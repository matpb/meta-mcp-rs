//! Per-platform runtime: the sidecar process, its status, the event pump and the reconnect loop.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use super::pace::{Pace, PaceClass, PaceConfig};
use super::ring::MessageRing;
use super::{Cancel, Daemon, OpError};
use crate::personal::PersonalPlatform;
use crate::personal::link::LinkError;
use crate::personal::protocol::{Account, CODE_INTERNAL, Event, HelloResult, Library};
use crate::personal::sidecar::{self, READY_TIMEOUT, Sidecar, SpawnError};
use crate::personal::state::StateStore;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const RESTART_STOP: Duration = Duration::from_secs(15);
const RECONNECT_STOP: Duration = Duration::from_secs(5);
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
/// Shortest reconnect delay after the sidecar asked to wait.
const UNRESPONSIVE: &str = "sidecar_unresponsive";
pub(super) const WAIT_FLOOR: Duration = Duration::from_secs(300);

pub(super) type Barrier = oneshot::Sender<()>;

pub(super) struct ReconnectLoop {
    pub serial: u64,
    pub cancel: Cancel,
}

pub(super) struct Inner {
    pub sidecar: Option<Arc<Sidecar>>,
    pub generation: u64,
    pub barrier: Option<UnboundedSender<Barrier>>,
    pub logged_in: bool,
    pub account: Option<Account>,
    /// A stored session exists and the daemon should keep it connected.
    pub want_online: bool,
    /// False after logout and `logged_out` until the next `login_start`.
    pub accept_state: bool,
    pub connection: &'static str,
    pub reason: Option<String>,
    /// A page the owner must open to resolve the account (checkpoint, challenge, consent).
    pub reason_url: Option<String>,
    pub sending_blocked: bool,
    pub blocked_reason: Option<String>,
    pub last_event_at: Option<i64>,
    pub sidecar_version: Option<String>,
    pub library: Option<Library>,
    pub reconnect_at: Option<Instant>,
    pub last_error: Option<String>,
    pub backoff_idx: usize,
    /// The sidecar said `none` or `relogin`: no reconnecting until `restart` or a login.
    pub halted: bool,
    /// The sidecar said `wait`: the next reconnect delay is at least `WAIT_FLOOR`.
    pub wait_floor: bool,
    /// When the current connection was first reported up.
    pub connected_since: Option<Instant>,
    pub reconnect: Option<ReconnectLoop>,
    pub reconnect_serial: u64,
    pub connect_sent: bool,
    pub ring: MessageRing,
}

pub(super) struct PlatformRt {
    pub platform: PersonalPlatform,
    pub store: StateStore,
    inner: Mutex<Inner>,
    pace: Mutex<Pace>,
    /// Serialises spawning, stopping and replacing the sidecar.
    pub lifecycle: AsyncMutex<()>,
    /// Orders state writes and deletes against logout.
    pub state_io: AsyncMutex<()>,
}

pub(super) struct StartError {
    pub error: OpError,
    pub retryable: bool,
}

impl StartError {
    fn fatal(error: OpError) -> Self {
        Self {
            error,
            retryable: false,
        }
    }

    fn retry(error: OpError) -> Self {
        Self {
            error,
            retryable: true,
        }
    }
}

/// Drops outbound media copies a crashed daemon left behind.
fn clean_outbox(data_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(data_dir.join("outbox")) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl PlatformRt {
    pub fn new(platform: PersonalPlatform, home: &Path, pace: PaceConfig) -> Arc<Self> {
        Arc::new(Self {
            platform,
            store: StateStore::new(home, platform),
            pace: Mutex::new(Pace::new(pace, Instant::now())),
            inner: Mutex::new(Inner {
                sidecar: None,
                generation: 0,
                barrier: None,
                logged_in: false,
                account: None,
                want_online: false,
                accept_state: false,
                connection: "stopped",
                reason: None,
                reason_url: None,
                sending_blocked: false,
                blocked_reason: None,
                last_event_at: None,
                sidecar_version: None,
                library: None,
                reconnect_at: None,
                last_error: None,
                backoff_idx: 0,
                halted: false,
                wait_floor: false,
                connected_since: None,
                reconnect: None,
                reconnect_serial: 0,
                connect_sent: false,
                ring: MessageRing::default(),
            }),
            lifecycle: AsyncMutex::new(()),
            state_io: AsyncMutex::new(()),
        })
    }

    pub fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes a write token; the error says how long to wait.
    pub fn take_token(&self, class: PaceClass) -> Result<(), Duration> {
        self.pace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(class, Instant::now())
    }

    /// The refusal for a platform whose account may not send until `restart`.
    pub fn blocked_error(&self) -> Option<OpError> {
        let g = self.inner();
        g.sending_blocked.then(|| {
            OpError::new(
                crate::personal::protocol::CODE_SENDING_BLOCKED,
                format!(
                    "sending is blocked on {} ({}): fix the account, then run `meta-mcp personal restart {}`",
                    self.platform.as_str(),
                    g.blocked_reason.as_deref().unwrap_or("unknown"),
                    self.platform.as_str()
                ),
            )
        })
    }

    /// The running sidecar with its generation.
    pub fn current(&self) -> Option<(Arc<Sidecar>, u64)> {
        let g = self.inner();
        g.sidecar.clone().map(|s| (s, g.generation))
    }

    /// The `status` entry of this platform.
    pub fn status(&self) -> Value {
        let g = self.inner();
        let reconnect_in = g.reconnect_at.map(|at| {
            at.saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil() as u64
        });
        json!({
            "logged_in": g.logged_in,
            "account": g.account.as_ref().map(|a| json!({"id": a.id, "name": a.name})),
            "connection": g.connection,
            "reason": g.reason,
            "reason_url": g.reason_url,
            "sending_blocked": g.sending_blocked,
            "blocked_reason": g.blocked_reason,
            "last_event_at": g.last_event_at,
            "sidecar_running": g.sidecar.is_some(),
            "sidecar_version": g.sidecar_version,
            "library": g.library.as_ref().map(|l| json!({"name": l.name, "version": l.version})),
            "reconnect_in_s": reconnect_in,
            "reconnect_halted": g.halted,
            "last_error": g.last_error,
        })
    }
}

impl Daemon {
    /// Starts every platform that has a stored session, in the background.
    pub(super) fn start_platforms(self: &Arc<Self>) {
        for rt in &self.platforms {
            let daemon = Arc::clone(self);
            let rt = Arc::clone(rt);
            tokio::spawn(async move { daemon.startup_platform(&rt).await });
        }
    }

    async fn startup_platform(self: &Arc<Self>, rt: &Arc<PlatformRt>) {
        let _life = rt.lifecycle.lock().await;
        let Some(state) = rt.store.load() else {
            return;
        };
        rt.inner().want_online = true;
        if let Err(e) = self.start_sidecar_locked(rt, Some(state)).await {
            tracing::warn!(platform = rt.platform.as_str(), code = %e.error.code, "sidecar startup failed");
            if e.retryable {
                self.start_reconnect_loop(rt);
            }
        }
    }

    fn set_stopped(rt: &PlatformRt, last_error: &str) {
        let mut g = rt.inner();
        g.sidecar = None;
        g.barrier = None;
        g.generation += 1;
        g.connection = "stopped";
        g.last_error = Some(last_error.to_owned());
    }

    /// Spawns the sidecar, says `hello` with `state` and connects when it is logged in.
    /// The caller holds the lifecycle lock.
    pub(super) async fn start_sidecar_locked(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        state: Option<Value>,
    ) -> Result<HelloResult, StartError> {
        self.start_sidecar_with(rt, state, true).await
    }

    /// Like `start_sidecar_locked`; `connect: false` stops after `hello` (used to log out).
    pub(super) async fn start_sidecar_with(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        state: Option<Value>,
        connect: bool,
    ) -> Result<HelloResult, StartError> {
        let platform = rt.platform;
        if self.cancel.is_cancelled() {
            return Err(StartError::fatal(OpError::new(
                CODE_INTERNAL,
                "the daemon is shutting down",
            )));
        }
        let data_dir = rt
            .store
            .ensure_dirs()
            .and_then(|()| std::path::absolute(rt.store.data_dir()))
            .map_err(|e| {
                tracing::error!(platform = platform.as_str(), error = %e, "cannot prepare the state folders");
                Self::set_stopped(rt, "state_dir_failed");
                StartError::fatal(OpError::new(CODE_INTERNAL, "cannot prepare the state folders"))
            })?;
        clean_outbox(&data_dir);
        let path = sidecar::locate(platform).map_err(|e| {
            Self::set_stopped(rt, "sidecar_not_found");
            StartError::fatal(OpError::new(
                crate::personal::protocol::CODE_SIDECAR_UNAVAILABLE,
                e.to_string(),
            ))
        })?;
        let sc = Self::spawn_sidecar(&path).await.map_err(|e| {
            Self::set_stopped(rt, "sidecar_spawn_failed");
            StartError::fatal(OpError::new(
                crate::personal::protocol::CODE_SIDECAR_UNAVAILABLE,
                e.to_string(),
            ))
        })?;
        let sc = Arc::new(sc);
        let (barrier_tx, barrier_rx) = mpsc::unbounded_channel();
        let epoch = {
            let mut g = rt.inner();
            g.generation += 1;
            g.sidecar = Some(Arc::clone(&sc));
            g.barrier = Some(barrier_tx);
            g.connection = "starting";
            g.reason = None;
            g.reason_url = None;
            g.connect_sent = false;
            g.logged_in = false;
            g.account = None;
            g.sidecar_version = None;
            g.library = None;
            g.generation
        };
        let abort = |code: &'static str, message: &'static str| {
            let sc = Arc::clone(&sc);
            let rt = Arc::clone(rt);
            async move {
                {
                    let mut g = rt.inner();
                    if g.generation == epoch {
                        g.sidecar = None;
                        g.barrier = None;
                        g.generation += 1;
                        g.connection = "stopped";
                        g.last_error = Some(code.to_owned());
                    }
                }
                sc.kill().await;
                StartError::retry(OpError::new(
                    crate::personal::protocol::CODE_SIDECAR_UNAVAILABLE,
                    message,
                ))
            }
        };
        if sc.wait_ready(READY_TIMEOUT).await.is_err() {
            return Err(abort("sidecar_not_ready", "the sidecar did not report ready").await);
        }
        let Some(events) = sc.take_events() else {
            return Err(abort("sidecar_not_ready", "the sidecar events were taken").await);
        };
        let had_state = state.is_some();
        if had_state {
            // A stored session may rotate its state during or right after hello.
            let _io = rt.state_io.lock().await;
            rt.inner().accept_state = true;
        }
        tokio::spawn(Arc::clone(self).pump(Arc::clone(rt), epoch, events, barrier_rx));
        let hello = match sc.hello(platform, &data_dir, state).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(platform = platform.as_str(), error = %e, "sidecar hello failed");
                return Err(abort("hello_failed", "the sidecar did not accept hello").await);
            }
        };
        {
            let mut g = rt.inner();
            if g.generation != epoch {
                return Err(StartError::retry(OpError::new(
                    CODE_INTERNAL,
                    "the sidecar was replaced while starting",
                )));
            }
            g.logged_in = hello.logged_in;
            g.account = hello.account.clone().filter(|a| !a.id.is_empty());
            g.sidecar_version = Some(hello.sidecar_version.clone());
            g.library = Some(hello.library.clone());
            if hello.logged_in {
                g.want_online = true;
                g.accept_state = true;
                g.last_error = None;
            } else {
                g.connection = "disconnected";
                g.reason = Some("not_logged_in".to_owned());
                if had_state {
                    g.want_online = false;
                    g.accept_state = false;
                }
            }
        }
        tokio::spawn(Arc::clone(self).heartbeat(Arc::clone(rt), epoch, Arc::clone(&sc)));
        tracing::info!(
            platform = platform.as_str(),
            sidecar_version = %hello.sidecar_version,
            library = %hello.library.name,
            logged_in = hello.logged_in,
            "sidecar ready"
        );
        if hello.logged_in
            && connect
            && let Err(e) = self.connect_sidecar(rt, epoch).await
        {
            tracing::warn!(platform = platform.as_str(), code = %e.code, "connect failed");
            return Err(abort("connect_failed", "the sidecar could not connect").await);
        }
        if self.cancel.is_cancelled() {
            return Err(StartError::fatal(OpError::new(
                CODE_INTERNAL,
                "the daemon is shutting down",
            )));
        }
        Ok(hello)
    }

    /// Spawns the sidecar, retrying briefly while the binary is still busy from being written (ETXTBSY).
    async fn spawn_sidecar(path: &Path) -> Result<Sidecar, SpawnError> {
        let mut tries = 0;
        loop {
            match Sidecar::spawn(path) {
                Err(e) if e.source.raw_os_error() == Some(26) && tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => return other,
            }
        }
    }

    /// Sends `connect` once per sidecar generation; a duplicate request is a no-op.
    pub(super) async fn connect_sidecar(
        &self,
        rt: &Arc<PlatformRt>,
        epoch: u64,
    ) -> Result<(), OpError> {
        let sc = {
            let mut g = rt.inner();
            if g.generation != epoch || g.connect_sent {
                return Ok(());
            }
            let Some(sc) = g.sidecar.clone() else {
                return Ok(());
            };
            g.connect_sent = true;
            sc
        };
        let result = sc
            .link()
            .request_with_timeout("connect", Value::Null, CONNECT_TIMEOUT)
            .await;
        let mut g = rt.inner();
        match result {
            Ok(_) => {
                if g.generation == epoch && g.connection == "starting" {
                    g.connection = "connecting";
                }
                Ok(())
            }
            Err(e) => {
                if g.generation == epoch {
                    g.connect_sent = false;
                }
                let err = OpError::from(e);
                g.last_error = Some(err.code.clone());
                Err(err)
            }
        }
    }

    /// Pings the sidecar; two misses in a row kill it so the crash path respawns it.
    async fn heartbeat(self: Arc<Self>, rt: Arc<PlatformRt>, epoch: u64, sc: Arc<Sidecar>) {
        let mut misses = 0u32;
        loop {
            tokio::select! {
                () = tokio::time::sleep(self.live.interval) => {}
                () = sc.closed() => return,
                () = self.cancel.cancelled() => return,
            }
            if rt.inner().generation != epoch {
                return;
            }
            let ping = sc
                .link()
                .request_with_timeout("ping", Value::Null, self.live.timeout)
                .await;
            match ping {
                Ok(_) | Err(LinkError::Remote { .. }) => misses = 0,
                Err(LinkError::Closed | LinkError::Protocol(_) | LinkError::Io(_)) => return,
                Err(LinkError::Timeout(_)) => misses += 1,
            }
            if misses < 2 {
                continue;
            }
            {
                let mut g = rt.inner();
                if g.generation != epoch {
                    return;
                }
                g.last_error = Some(UNRESPONSIVE.to_owned());
            }
            tracing::warn!(
                platform = rt.platform.as_str(),
                "sidecar missed two pings, killing it"
            );
            sc.kill().await;
            return;
        }
    }

    /// A connection that stayed up for the stable period earns a fresh backoff.
    pub(super) fn note_connection_ended(&self, g: &mut Inner) {
        if g.connected_since
            .take()
            .is_some_and(|since| since.elapsed() >= self.stable)
        {
            g.backoff_idx = 0;
        }
    }

    /// Takes the sidecar out of service and asks it to exit. The caller holds the lifecycle lock.
    pub(super) async fn stop_sidecar_locked(&self, rt: &Arc<PlatformRt>, timeout: Duration) {
        // Events already queued are handled before the generation changes and drops the rest.
        self.flush_events(rt).await;
        let sc = {
            let mut g = rt.inner();
            g.generation += 1;
            g.barrier = None;
            g.connection = "stopped";
            g.reason = None;
            g.reason_url = None;
            g.connect_sent = false;
            g.sidecar.take()
        };
        if let Some(sc) = sc {
            match sc.shutdown(timeout).await {
                Ok(status) => {
                    tracing::info!(platform = rt.platform.as_str(), %status, "sidecar exited");
                }
                Err(e) => tracing::warn!(error = %e, "sidecar shutdown failed"),
            }
        }
    }

    pub(super) async fn stop_for_shutdown(&self, rt: &Arc<PlatformRt>, timeout: Duration) {
        self.stop_reconnect(rt);
        self.stop_sidecar_locked(rt, timeout).await;
    }

    /// Waits until the pump has handled every event the sidecar sent before now.
    pub(super) async fn flush_events(&self, rt: &Arc<PlatformRt>) {
        let tx = rt.inner().barrier.clone();
        if let Some(tx) = tx {
            let (ack, done) = oneshot::channel();
            if tx.send(ack).is_ok() {
                let _ = tokio::time::timeout(FLUSH_TIMEOUT, done).await;
            }
        }
    }

    async fn pump(
        self: Arc<Self>,
        rt: Arc<PlatformRt>,
        epoch: u64,
        mut events: UnboundedReceiver<Event>,
        mut barrier: UnboundedReceiver<Barrier>,
    ) {
        let mut barrier_open = true;
        loop {
            tokio::select! {
                biased;
                ev = events.recv() => match ev {
                    Some(ev) => self.on_event(&rt, epoch, ev).await,
                    None => break,
                },
                b = barrier.recv(), if barrier_open => match b {
                    Some(ack) => {
                        while let Ok(ev) = events.try_recv() {
                            self.on_event(&rt, epoch, ev).await;
                        }
                        let _ = ack.send(());
                    }
                    None => barrier_open = false,
                },
            }
        }
        self.on_sidecar_closed(&rt, epoch).await;
    }

    /// The sidecar's output ended on its own: reap it and reconnect when a session should stay up.
    async fn on_sidecar_closed(self: &Arc<Self>, rt: &Arc<PlatformRt>, epoch: u64) {
        let sc = {
            let mut g = rt.inner();
            if g.generation != epoch {
                return;
            }
            self.note_connection_ended(&mut g);
            g.generation += 1;
            g.barrier = None;
            g.connection = "disconnected";
            let unresponsive = g.last_error.as_deref() == Some(UNRESPONSIVE);
            let why = if unresponsive {
                UNRESPONSIVE
            } else {
                "sidecar_exited"
            };
            g.reason = Some(why.to_owned());
            g.connect_sent = false;
            g.last_error = Some(why.to_owned());
            g.sidecar.take()
        };
        tracing::warn!(
            platform = rt.platform.as_str(),
            "sidecar exited unexpectedly"
        );
        if let Some(sc) = sc {
            sc.kill().await;
        }
        self.start_reconnect_loop(rt);
    }

    /// Starts the reconnect loop unless one runs, the session should not stay up, or sending is blocked.
    pub(super) fn start_reconnect_loop(self: &Arc<Self>, rt: &Arc<PlatformRt>) {
        if self.cancel.is_cancelled() {
            return;
        }
        let (serial, cancel) = {
            let mut g = rt.inner();
            if g.reconnect.is_some() || !g.want_online || g.sending_blocked || g.halted {
                return;
            }
            g.reconnect_serial += 1;
            let cancel = Cancel::new();
            g.reconnect = Some(ReconnectLoop {
                serial: g.reconnect_serial,
                cancel: cancel.clone(),
            });
            (g.reconnect_serial, cancel)
        };
        let daemon = Arc::clone(self);
        let rt = Arc::clone(rt);
        tokio::spawn(async move { daemon.reconnect_loop(&rt, serial, cancel).await });
    }

    pub(super) fn stop_reconnect(&self, rt: &PlatformRt) {
        let running = {
            let mut g = rt.inner();
            g.reconnect_at = None;
            g.reconnect.take()
        };
        if let Some(running) = running {
            running.cancel.cancel();
        }
    }

    async fn reconnect_loop(self: Arc<Self>, rt: &Arc<PlatformRt>, serial: u64, cancel: Cancel) {
        loop {
            let delay = {
                let mut g = rt.inner();
                if cancel.is_cancelled() {
                    break;
                }
                let i = g.backoff_idx.min(self.backoff.len() - 1);
                g.backoff_idx += 1;
                let mut delay = self.backoff[i];
                if std::mem::take(&mut g.wait_floor) {
                    delay = delay.max(WAIT_FLOOR);
                }
                g.reconnect_at = Some(Instant::now() + delay);
                delay
            };
            tokio::select! {
                () = cancel.cancelled() => break,
                () = self.cancel.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
            {
                let mut g = rt.inner();
                g.reconnect_at = None;
                if cancel.is_cancelled() || !g.want_online || g.sending_blocked || g.halted {
                    break;
                }
            }
            match self.bring_up(rt, &cancel).await {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!(platform = rt.platform.as_str(), code = %e.code, "reconnect attempt failed");
                    rt.inner().last_error = Some(e.code);
                }
            }
            let g = rt.inner();
            if cancel.is_cancelled() || !g.want_online || g.sending_blocked || g.halted {
                break;
            }
        }
        let mut g = rt.inner();
        if g.reconnect.as_ref().is_some_and(|l| l.serial == serial) {
            g.reconnect = None;
            g.reconnect_at = None;
        }
    }

    /// Replaces the sidecar with a fresh one running the stored session.
    async fn bring_up(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        cancel: &Cancel,
    ) -> Result<(), OpError> {
        let _life = rt.lifecycle.lock().await;
        let stale = {
            let g = rt.inner();
            !g.want_online || g.sending_blocked || g.halted
        };
        if cancel.is_cancelled() || self.cancel.is_cancelled() || stale {
            return Err(OpError::new(CODE_INTERNAL, "cancelled"));
        }
        self.stop_sidecar_locked(rt, RECONNECT_STOP).await;
        let Some(state) = rt.store.load() else {
            rt.inner().want_online = false;
            return Ok(());
        };
        self.start_sidecar_locked(rt, Some(state))
            .await
            .map(|_| ())
            .map_err(|e| e.error)
    }

    /// The `restart` op: fresh sidecar, stored session, sending unblocked, backoff reset.
    pub(super) async fn restart_platform(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
    ) -> Result<Value, OpError> {
        self.stop_reconnect(rt);
        let _life = rt.lifecycle.lock().await;
        self.stop_sidecar_locked(rt, RESTART_STOP).await;
        {
            let mut g = rt.inner();
            g.sending_blocked = false;
            g.blocked_reason = None;
            g.halted = false;
            g.wait_floor = false;
            g.connected_since = None;
            g.backoff_idx = 0;
            g.reconnect_at = None;
            g.last_error = None;
            g.connection = "stopped";
        }
        let Some(state) = rt.store.load() else {
            rt.inner().want_online = false;
            return Ok(rt.status());
        };
        rt.inner().want_online = true;
        match self.start_sidecar_locked(rt, Some(state)).await {
            Ok(_) => Ok(rt.status()),
            Err(e) => {
                if e.retryable {
                    self.start_reconnect_loop(rt);
                }
                Err(e.error)
            }
        }
    }
}
