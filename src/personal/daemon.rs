//! The personal daemon: owns the Messenger and WhatsApp sidecars, their session state, the inbox
//! writes, the send audit and the unix socket. It never logs text, cookies, state or numbers.

mod conn;
mod events;
mod inbox_map;
mod listen;
mod login;
mod ops;
mod platform;
mod ring;
mod write;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Semaphore, broadcast, watch};
use tokio::time::Instant;

use super::audit::AuditLog;
use super::link::LinkError;
use super::protocol::{CODE_BAD_PARAMS, CODE_SIDECAR_UNAVAILABLE, CODE_TIMEOUT};
use super::{PersonalPlatform, default_socket_path, personal_dir};
use crate::config::Config;
use crate::inbox::Inbox;
use platform::PlatformRt;

pub use inbox_map::inbox_events;
pub use listen::{ListenError, systemd_passed_one_fd};

pub const MAX_CONNECTIONS: usize = 64;
pub const ENV_BACKOFF_MS: &str = "META_MCP_PERSONAL_BACKOFF_MS";
const DEFAULT_BACKOFF_SECS: [u64; 4] = [10, 30, 60, 300];
const SHUTDOWN_TOTAL: Duration = Duration::from_secs(15);
const DRAIN_WRITES: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
pub struct DaemonOptions {
    pub socket: Option<PathBuf>,
}

/// Whether a connecting peer may be served: only our own uid.
pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

fn own_uid() -> std::io::Result<u32> {
    Ok(std::fs::metadata("/proc/self")?.uid())
}

/// An error answer for the client: a stable code and a message with no secrets.
#[derive(Debug, Clone, PartialEq)]
pub struct OpError {
    pub code: String,
    pub message: String,
}

impl OpError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    pub fn bad_params(message: impl Into<String>) -> Self {
        Self::new(CODE_BAD_PARAMS, message)
    }

    pub fn sidecar_unavailable(platform: PersonalPlatform) -> Self {
        Self::new(
            CODE_SIDECAR_UNAVAILABLE,
            format!(
                "the {} sidecar is not running; check `status`",
                platform.as_str()
            ),
        )
    }
}

/// Code given to the client when a sidecar's own code is not a plain `[a-z0-9_]` word.
pub const CODE_SIDECAR_ERROR: &str = "sidecar_error";
const MAX_CODE_CHARS: usize = 40;

/// A sidecar-supplied word reduced to at most 40 chars of `[a-z0-9_]`, for status, audit and logs.
pub(crate) fn sanitize_code(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .take(MAX_CODE_CHARS)
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '_') => c,
            _ => '_',
        })
        .collect();
    if clean.is_empty() {
        CODE_SIDECAR_ERROR.to_owned()
    } else {
        clean
    }
}

/// The code a client sees: the sidecar's own when it already has the plain shape.
pub(crate) fn reply_code(raw: &str) -> String {
    if raw.chars().count() <= MAX_CODE_CHARS && sanitize_code(raw) == raw {
        raw.to_owned()
    } else {
        CODE_SIDECAR_ERROR.to_owned()
    }
}

impl From<LinkError> for OpError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::Remote { code, message, .. } => OpError {
                code: reply_code(&code),
                message,
            },
            LinkError::Timeout(op) => Self::new(
                CODE_TIMEOUT,
                format!("the sidecar did not answer `{op}` in time"),
            ),
            _ => Self::new(CODE_SIDECAR_UNAVAILABLE, "the sidecar is not available"),
        }
    }
}

/// A one-way flag with an async wait, used to stop loops and waiters.
#[derive(Clone)]
pub(crate) struct Cancel(Arc<watch::Sender<bool>>);

impl Cancel {
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
}

/// A live inbound message, handed to `wait_for_message` callers.
#[derive(Debug, Clone)]
pub(crate) struct Incoming {
    pub platform: PersonalPlatform,
    pub message: Value,
}

/// Keeps the daemon's in-flight request count honest until the reply is written.
pub(crate) struct InflightGuard(Arc<Daemon>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Daemon {
    cfg: Config,
    inbox: Inbox,
    audit: Arc<AuditLog>,
    platforms: [Arc<PlatformRt>; 2],
    events: broadcast::Sender<Arc<Vec<u8>>>,
    incoming: broadcast::Sender<Arc<Incoming>>,
    cancel: Cancel,
    inflight: AtomicUsize,
    backoff: Vec<Duration>,
}

/// `META_MCP_PERSONAL_BACKOFF_MS` (comma list of milliseconds), else 10 s, 30 s, 60 s, 300 s.
pub fn backoff_schedule(raw: Option<&str>) -> Vec<Duration> {
    let parsed: Option<Vec<Duration>> = raw
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().parse::<u64>().ok().filter(|n| *n > 0))
                .map(|n| n.map(Duration::from_millis))
                .collect()
        })
        .and_then(|v: Option<Vec<Duration>>| v.filter(|v| !v.is_empty()));
    parsed.unwrap_or_else(|| DEFAULT_BACKOFF_SECS.map(Duration::from_secs).to_vec())
}

impl Daemon {
    pub fn new(cfg: Config, inbox: Inbox, audit: AuditLog, backoff: Vec<Duration>) -> Arc<Daemon> {
        let platforms = PersonalPlatform::ALL.map(|p| PlatformRt::new(p, &cfg.home));
        Arc::new(Daemon {
            cfg,
            inbox,
            audit: Arc::new(audit),
            platforms,
            events: broadcast::channel(1024).0,
            incoming: broadcast::channel(256).0,
            cancel: Cancel::new(),
            inflight: AtomicUsize::new(0),
            backoff,
        })
    }

    fn rt(&self, platform: PersonalPlatform) -> &Arc<PlatformRt> {
        match platform {
            PersonalPlatform::Messenger => &self.platforms[0],
            PersonalPlatform::WhatsApp => &self.platforms[1],
        }
    }

    pub(crate) fn track(self: &Arc<Self>) -> InflightGuard {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        InflightGuard(Arc::clone(self))
    }

    /// Stops waiters and loops, lets writes finish, then stops every sidecar within 15 s.
    async fn shutdown(self: &Arc<Self>) {
        self.cancel.cancel();
        let deadline = Instant::now() + SHUTDOWN_TOTAL;
        let drain_until = Instant::now() + DRAIN_WRITES;
        while self.inflight.load(Ordering::SeqCst) > 0 && Instant::now() < drain_until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let stops = self.platforms.iter().map(|rt| {
            let left = deadline.saturating_duration_since(Instant::now());
            self.stop_for_shutdown(rt, left)
        });
        futures_util::future::join_all(stops).await;
    }
}

/// Runs the daemon until SIGTERM or SIGINT; the value is the process exit code.
pub async fn run(opts: DaemonOptions) -> ExitCode {
    crate::server::init_tracing();
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    match serve(cfg, opts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

/// Takes the exclusive per-home lock that makes the daemon a single instance.
fn lock_instance(personal: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = personal.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "another personal daemon already uses {}",
            personal.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

async fn serve(cfg: Config, opts: DaemonOptions) -> Result<(), String> {
    let own = own_uid().map_err(|e| format!("cannot read the daemon uid: {e}"))?;
    let inbox = Inbox::open(&cfg.inbox_db).map_err(|e| e.to_string())?;
    let personal = personal_dir(&cfg.home);
    platform::private_dir(&personal)
        .map_err(|e| format!("cannot secure {}: {e}", personal.display()))?;
    // Held until this function returns; the descriptor is close-on-exec, so no sidecar keeps it.
    let _instance_lock = lock_instance(&personal)?;
    let audit = AuditLog::open(&personal.join("audit.jsonl"))
        .map_err(|e| format!("cannot open the audit log: {e}"))?;
    // The listener first: an inherited fd is duplicated close-on-exec before any sidecar exists.
    let socket = opts.socket.unwrap_or_else(default_socket_path);
    let listener = listen::acquire(&socket).map_err(|e| e.to_string())?;
    let (Ok(mut sigterm), Ok(mut sigint)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return Err("cannot install signal handlers".to_owned());
    };

    let backoff = backoff_schedule(std::env::var(ENV_BACKOFF_MS).ok().as_deref());
    let daemon = Daemon::new(cfg, inbox, audit, backoff);
    tracing::info!(
        socket_activated = listener.bound_path.is_none(),
        read_only = daemon.cfg.read_only,
        "personal daemon accepting connections"
    );
    daemon.start_platforms();

    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut cap_warned = false;
    loop {
        tokio::select! {
            _ = sigterm.recv() => { tracing::info!("SIGTERM, shutting down"); break; }
            _ = sigint.recv() => { tracing::info!("SIGINT, shutting down"); break; }
            accepted = listener.listener.accept() => match accepted {
                Ok((stream, _)) => match Arc::clone(&slots).try_acquire_owned() {
                    Ok(slot) => {
                        cap_warned = false;
                        let daemon = Arc::clone(&daemon);
                        tokio::spawn(async move {
                            conn::handle(stream, daemon, own).await;
                            drop(slot);
                        });
                    }
                    Err(_) => {
                        if !cap_warned {
                            tracing::warn!(max = MAX_CONNECTIONS, "connection limit reached, dropping connections");
                            cap_warned = true;
                        }
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }

    daemon.shutdown().await;
    if let Some(path) = &listener.bound_path {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_own_uid_is_allowed() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(peer_allowed(0, 0));
    }

    #[test]
    fn sidecar_codes_are_reduced_to_plain_words() {
        assert_eq!(sanitize_code("send_failed"), "send_failed");
        assert_eq!(sanitize_code("Bad Code!\n"), "bad_code__");
        assert_eq!(sanitize_code(""), CODE_SIDECAR_ERROR);
        assert_eq!(sanitize_code(&"a".repeat(100)).len(), 40);
        assert_eq!(reply_code("rate_limited"), "rate_limited");
        for bad in [
            "Rate",
            "a b",
            "",
            &"a".repeat(41),
            "caf\u{e9}",
            "x\u{1b}[0m",
        ] {
            assert_eq!(reply_code(bad), CODE_SIDECAR_ERROR, "{bad:?}");
        }
    }

    #[test]
    fn our_own_uid_is_readable() {
        let uid = own_uid().expect("uid");
        assert!(peer_allowed(uid, uid));
    }

    #[test]
    fn backoff_defaults_and_overrides() {
        let secs = |v: Vec<Duration>| v.iter().map(Duration::as_secs).collect::<Vec<_>>();
        assert_eq!(secs(backoff_schedule(None)), [10, 30, 60, 300]);
        let ms = backoff_schedule(Some("200, 400"));
        assert_eq!(ms, [Duration::from_millis(200), Duration::from_millis(400)]);
        for bad in ["", "x", "100,,200", "0", "-5"] {
            assert_eq!(
                secs(backoff_schedule(Some(bad))),
                [10, 30, 60, 300],
                "{bad:?}"
            );
        }
    }
}
