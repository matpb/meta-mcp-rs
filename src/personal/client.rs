//! Client side of the daemon socket: a `Link` over a unix stream.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::net::UnixStream;
use tokio::sync::mpsc::UnboundedReceiver;

use super::link::{Link, LinkError};
use super::protocol::{CODE_TIMEOUT, Event};

pub const CODE_NOT_RUNNING: &str = "daemon_not_running";
pub const CODE_CONNECTION_LOST: &str = "connection_lost";
pub const CODE_ALREADY_SUBSCRIBED: &str = "already_subscribed";
pub const CODE_UNTRUSTED_DAEMON: &str = "untrusted_daemon";

/// A failed daemon call: the daemon's own error code and message, or a local one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientError {
    pub code: String,
    pub message: String,
}

impl ClientError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ClientError {}

impl From<LinkError> for ClientError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::Remote { code, message, .. } => ClientError { code, message },
            LinkError::Timeout(op) => Self::new(
                CODE_TIMEOUT,
                format!("the daemon did not answer `{op}` in time"),
            ),
            LinkError::Closed | LinkError::Io(_) | LinkError::Protocol(_) => Self::new(
                CODE_CONNECTION_LOST,
                "the connection to the personal daemon was lost",
            ),
        }
    }
}

pub struct DaemonClient {
    link: Link,
    socket: PathBuf,
}

impl DaemonClient {
    /// Connects and checks that the process behind the socket runs as this user.
    pub async fn connect(path: &Path) -> Result<Self, ClientError> {
        let own = super::own_uid().await.map_err(|e| {
            ClientError::new(
                CODE_UNTRUSTED_DAEMON,
                format!("cannot read our own uid: {e}"),
            )
        })?;
        Self::connect_as(path, own).await
    }

    async fn connect_as(path: &Path, expected_uid: u32) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path).await.map_err(|e| {
            ClientError::new(
                CODE_NOT_RUNNING,
                format!(
                    "personal mode not running: start it with `meta-mcp personal install` or `meta-mcp personal daemon` (socket {}: {e})",
                    path.display()
                ),
            )
        })?;
        match stream.peer_cred() {
            Ok(cred) if super::daemon::peer_allowed(cred.uid(), expected_uid) => {}
            Ok(_) => {
                return Err(ClientError::new(
                    CODE_UNTRUSTED_DAEMON,
                    format!(
                        "the socket {} is served by another user: refusing to send anything to it",
                        path.display()
                    ),
                ));
            }
            Err(e) => {
                return Err(ClientError::new(
                    CODE_UNTRUSTED_DAEMON,
                    format!("cannot tell who serves {}: {e}", path.display()),
                ));
            }
        }
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            link: Link::new(read_half, write_half),
            socket: path.to_path_buf(),
        })
    }

    /// Connects to `default_socket_path()`.
    pub async fn connect_default() -> Result<Self, ClientError> {
        Self::connect(&super::default_socket_path()).await
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub async fn request(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        Ok(self.link.request_with_timeout(op, params, timeout).await?)
    }

    /// Asks the daemon for events and hands out the stream; once per client.
    pub async fn subscribe(&self) -> Result<UnboundedReceiver<Event>, ClientError> {
        let events = self.link.take_events().ok_or_else(|| {
            ClientError::new(CODE_ALREADY_SUBSCRIBED, "this client already subscribed")
        })?;
        self.request("subscribe", Value::Null, Duration::from_secs(10))
            .await?;
        Ok(events)
    }

    /// True once the daemon hung up.
    pub fn is_closed(&self) -> bool {
        self.link.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_socket_says_how_to_start_personal_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.sock");
        let err = DaemonClient::connect(&path).await.err().expect("error");
        assert_eq!(err.code, CODE_NOT_RUNNING);
        assert!(err.message.contains("personal mode not running"), "{err}");
        assert!(err.message.contains("meta-mcp personal daemon"), "{err}");
        assert!(err.message.contains(path.to_str().expect("utf8")), "{err}");
    }

    #[tokio::test]
    async fn a_server_running_as_another_uid_is_refused_before_anything_is_sent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let own = crate::personal::own_uid().await.expect("uid");
        let seen = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 1];
            tokio::time::timeout(
                Duration::from_millis(300),
                tokio::io::AsyncReadExt::read(&mut stream, &mut buf),
            )
            .await
        });
        let err = DaemonClient::connect_as(&path, own.wrapping_add(1))
            .await
            .err()
            .expect("refused");
        assert_eq!(err.code, CODE_UNTRUSTED_DAEMON);
        assert!(err.message.contains("another user"), "{err}");
        let read = seen.await.expect("join").expect("the server saw the close");
        assert_eq!(read.expect("read"), 0, "nothing was sent");
        let ours = dir.path().join("ours.sock");
        let _listener = tokio::net::UnixListener::bind(&ours).expect("bind");
        assert!(DaemonClient::connect_as(&ours, own).await.is_ok());
    }

    #[test]
    fn link_errors_keep_the_daemon_code() {
        let e: ClientError = LinkError::Remote {
            op: "send_text".into(),
            code: "read_only".into(),
            message: "no".into(),
        }
        .into();
        assert_eq!(e.code, "read_only");
        assert_eq!(e.to_string(), "no (read_only)");
        let t: ClientError = LinkError::Timeout("x".into()).into();
        assert_eq!(t.code, "timeout");
        let c: ClientError = LinkError::Closed.into();
        assert_eq!(c.code, CODE_CONNECTION_LOST);
    }
}
