//! The Go sidecar as a child process: spawned with an empty environment (HOME only), spoken to
//! over its stdin and stdout through a `Link`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use super::PersonalPlatform;
use super::link::{Link, LinkError};
use super::protocol::{Event, HelloParams, HelloResult, PROTOCOL_VERSION};

pub const ENV_SIDECAR_DIR: &str = "META_MCP_SIDECAR_DIR";
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum LocateError {
    #[error(
        "cannot determine the meta-mcp executable path to find {binary} (set {ENV_SIDECAR_DIR}): {source}"
    )]
    CurrentExe {
        binary: &'static str,
        source: io::Error,
    },
    #[error("the meta-mcp executable path has no parent directory (set {ENV_SIDECAR_DIR})")]
    NoParent,
}

/// `$META_MCP_SIDECAR_DIR/<binary>`, else the binary next to this executable.
pub fn locate(platform: PersonalPlatform) -> Result<PathBuf, LocateError> {
    resolve(
        std::env::var_os(ENV_SIDECAR_DIR),
        std::env::current_exe(),
        platform,
    )
}

fn resolve(
    env: Option<OsString>,
    exe: io::Result<PathBuf>,
    platform: PersonalPlatform,
) -> Result<PathBuf, LocateError> {
    let binary = platform.binary_name();
    if let Some(dir) = env.filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join(binary));
    }
    let exe = exe.map_err(|source| LocateError::CurrentExe { binary, source })?;
    let dir = exe.parent().ok_or(LocateError::NoParent)?;
    Ok(dir.join(binary))
}

#[derive(Debug, thiserror::Error)]
#[error("could not start the sidecar at {path}: {source}")]
pub struct SpawnError {
    pub path: String,
    pub source: io::Error,
}

pub struct Sidecar {
    link: Link,
    child: AsyncMutex<Child>,
    hello: OnceLock<HelloResult>,
}

impl Sidecar {
    pub fn spawn(path: &Path) -> Result<Sidecar, SpawnError> {
        let spawn_err = |source| SpawnError {
            path: path.display().to_string(),
            source,
        };
        // An absolute path keeps a bare name from being looked up on PATH.
        let abs = std::path::absolute(path).map_err(spawn_err)?;
        let mut cmd = Command::new(&abs);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_clear()
            .kill_on_drop(true);
        if let Some(home) = std::env::var_os("HOME") {
            cmd.env("HOME", home);
        }
        let mut child = cmd.spawn().map_err(|source| SpawnError {
            path: abs.display().to_string(),
            source,
        })?;
        let pipe_err = |what: &str| SpawnError {
            path: abs.display().to_string(),
            source: io::Error::other(format!("sidecar {what} was not piped")),
        };
        let stdin = child.stdin.take().ok_or_else(|| pipe_err("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| pipe_err("stdout"))?;
        Ok(Sidecar {
            link: Link::new(stdout, stdin),
            child: AsyncMutex::new(child),
            hello: OnceLock::new(),
        })
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    pub async fn request(&self, op: &str, params: Value) -> Result<Value, LinkError> {
        self.link.request(op, params).await
    }

    /// Resolves once the sidecar's output is gone for any reason.
    pub async fn closed(&self) {
        self.link.closed().await;
    }

    /// Hands the event receiver to the caller; `None` once taken.
    pub fn take_events(&self) -> Option<UnboundedReceiver<Event>> {
        self.link.take_events()
    }

    /// Waits for the `ready` event the sidecar emits before reading any request.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<Event, LinkError> {
        let ev = self.link.recv_event(timeout, "ready").await?;
        if ev.event == "ready" {
            Ok(ev)
        } else {
            Err(LinkError::Protocol(format!(
                "expected `ready` as the first event, got `{}`",
                ev.event
            )))
        }
    }

    /// Sends `hello` and caches the result; a different protocol version is an error.
    pub async fn hello(
        &self,
        platform: PersonalPlatform,
        data_dir: &Path,
        state: Option<Value>,
    ) -> Result<HelloResult, LinkError> {
        let params = serde_json::to_value(HelloParams {
            protocol: PROTOCOL_VERSION,
            platform: platform.as_str().to_owned(),
            data_dir: data_dir.display().to_string(),
            state: state.unwrap_or(Value::Null),
        })
        .map_err(|e| LinkError::Protocol(format!("could not encode hello: {e}")))?;
        let value = self
            .link
            .request_with_timeout("hello", params, HELLO_TIMEOUT)
            .await?;
        let result: HelloResult = serde_json::from_value(value)
            .map_err(|_| LinkError::Protocol("malformed hello result".into()))?;
        if result.protocol != PROTOCOL_VERSION {
            return Err(LinkError::Protocol(format!(
                "sidecar speaks protocol {}, expected {PROTOCOL_VERSION}",
                result.protocol
            )));
        }
        let _ = self.hello.set(result.clone());
        Ok(result)
    }

    /// The `hello` result captured at startup, if `hello()` has succeeded.
    pub fn hello_result(&self) -> Option<&HelloResult> {
        self.hello.get()
    }

    /// Kills the sidecar at once and reaps it.
    pub async fn kill(&self) {
        let _ = self.child.lock().await.kill().await;
    }

    /// Asks the sidecar to exit, closes its stdin, then kills it if it outlives `timeout` overall.
    pub async fn shutdown(&self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        self.link.expect_close();
        if let Err(e) = self
            .link
            .request_with_timeout("shutdown", Value::Null, timeout)
            .await
        {
            tracing::debug!(error = %e, "sidecar shutdown request failed");
        }
        // EOF on stdin ends a sidecar that refused `shutdown`.
        self.link
            .close_write(deadline.saturating_duration_since(Instant::now()))
            .await;
        let mut child = self.child.lock().await;
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                tracing::warn!("sidecar did not exit in time, killing it");
                child.kill().await?;
                child.wait().await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const READY: &str = r#"echo '{"event":"ready","data":{"pid":1,"protocol":1}}'"#;
    const ECHO_LOOP: &str = r#"
while read -r line; do
  id=${line#*\"id\":}; id=${id%%,*}
  case $line in
    *'"op":"hello"'*) echo "{\"id\":$id,\"ok\":true,\"result\":{\"protocol\":1,\"sidecar_version\":\"t\",\"library\":{\"name\":\"l\",\"version\":\"v\"},\"go\":\"g\",\"logged_in\":false,\"account\":null}}";;
    *) echo "{\"id\":$id,\"ok\":true,\"result\":{\"pong\":true}}";;
  esac
done"#;
    const WAIT: Duration = Duration::from_secs(10);

    fn fake(body: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-sidecars")
            .join(format!(
                "meta-fake-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("sidecar.sh");
        std::fs::write(&path, format!("#!/bin/sh\nPATH=/usr/bin:/bin\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    // A freshly written script can hit ETXTBSY while a parallel test forks.
    fn start(body: &str) -> Sidecar {
        let path = fake(body);
        for _ in 0..50 {
            match Sidecar::spawn(&path) {
                Err(SpawnError { source, .. }) if source.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                other => return other.expect("spawn fake sidecar"),
            }
        }
        panic!("fake sidecar stayed busy");
    }

    #[test]
    fn env_dir_wins_and_names_the_binary() {
        let p = resolve(
            Some(OsString::from("/opt/side")),
            Ok(PathBuf::from("/usr/bin/meta-mcp")),
            PersonalPlatform::WhatsApp,
        )
        .expect("path");
        assert_eq!(p, PathBuf::from("/opt/side/meta-whatsapp-sidecar"));
    }

    #[test]
    fn falls_back_to_the_exe_directory_and_empty_env_counts_as_unset() {
        for env in [None, Some(OsString::new())] {
            let p = resolve(
                env,
                Ok(PathBuf::from("/usr/local/bin/meta-mcp")),
                PersonalPlatform::Messenger,
            )
            .expect("path");
            assert_eq!(p, PathBuf::from("/usr/local/bin/meta-messenger-sidecar"));
        }
    }

    #[test]
    fn exe_failure_names_the_env_var_and_the_binary() {
        let err = resolve(
            None,
            Err(io::Error::other("no exe")),
            PersonalPlatform::Messenger,
        )
        .expect_err("error");
        let text = err.to_string();
        assert!(text.contains("META_MCP_SIDECAR_DIR"), "{text}");
        assert!(text.contains("meta-messenger-sidecar"), "{text}");
    }

    #[tokio::test]
    async fn ready_hello_ping_and_shutdown() {
        let s = start(&format!("{READY}\n{ECHO_LOOP}"));
        let ev = s.wait_ready(WAIT).await.expect("ready");
        assert_eq!(ev.data["protocol"], 1);
        let hello = s
            .hello(PersonalPlatform::Messenger, Path::new("/d"), None)
            .await
            .expect("hello");
        assert_eq!(hello.sidecar_version, "t");
        assert!(s.hello_result().is_some());
        let pong = s.request("ping", Value::Null).await.expect("ping");
        assert_eq!(pong["pong"], true);
        let status = s.shutdown(Duration::from_secs(2)).await.expect("shutdown");
        assert!(status.success(), "{status:?}");
    }

    #[tokio::test]
    async fn only_home_reaches_the_sidecar() {
        let body = format!(
            r#"{READY}
echo "{{\"event\":\"env\",\"data\":{{\"path\":\"${{GODEBUG-unset}}\",\"user\":\"${{USER-unset}}\"}}}}"
read -r l"#
        );
        let s = start(&body);
        s.wait_ready(WAIT).await.expect("ready");
        let mut rx = s.take_events().expect("events");
        let ev = tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("in time")
            .expect("env event");
        assert_eq!(ev.data["path"], "unset");
        assert_eq!(ev.data["user"], "unset");
    }

    #[tokio::test]
    async fn shutdown_closes_stdin_of_a_sidecar_that_refuses() {
        let body = format!(
            r#"{READY}
while read -r line; do
  id=${{line#*\"id\":}}; id=${{id%%,*}}
  echo "{{\"id\":$id,\"ok\":false,\"error\":{{\"code\":\"not_initialised\",\"message\":\"no\"}}}}"
done"#
        );
        let s = start(&body);
        s.wait_ready(WAIT).await.expect("ready");
        let t0 = Instant::now();
        let status = s.shutdown(Duration::from_secs(2)).await.expect("shutdown");
        assert!(status.success(), "{status:?}");
        assert!(t0.elapsed() < Duration::from_millis(1500), "was killed");
    }

    #[tokio::test]
    async fn a_sidecar_that_ignores_everything_is_killed_at_the_deadline() {
        let s = start(&format!("{READY}\nsleep 30"));
        s.wait_ready(WAIT).await.expect("ready");
        let t0 = Instant::now();
        let status = s
            .shutdown(Duration::from_millis(600))
            .await
            .expect("shutdown");
        assert!(!status.success());
        assert!(t0.elapsed() < Duration::from_secs(3));
    }
}
