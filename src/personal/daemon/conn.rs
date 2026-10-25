//! One client connection: parses requests, runs each in its own task and writes replies plus
//! subscribed events through a single writer task.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::hub::{Delivery, LAGGED_FRAME, Subscription};
use super::{Daemon, InflightGuard, peer_allowed};
use crate::personal::ipc;
use crate::personal::link::{Line, encode_line, read_line};
use crate::personal::protocol::{
    CODE_TOO_MANY_REQUESTS, CODE_TOO_MANY_WAITERS, Reply, ReplyError, Request,
};

/// Requests in flight per connection before new ones are refused.
const MAX_IN_FLIGHT: usize = 32;
/// Replies a connection may owe at once (running requests, refusals queued); past it the client
/// is refused at once, and dropped when even a refusal cannot be queued.
const MAX_OUTSTANDING: usize = 64;
/// Concurrent `wait_for_message` requests per connection.
const MAX_WAITERS: usize = 16;
const REPLY_QUEUE: usize = 64;

type EventRx = Subscription;

/// A reply line for the writer; `flushed` fires once the line is on the socket.
struct Outgoing {
    line: Vec<u8>,
    flushed: oneshot::Sender<()>,
}

/// Checks who the peer is, then serves the connection until the client leaves.
pub async fn handle(stream: ipc::ServerStream, daemon: Arc<Daemon>, own: Arc<ipc::Identity>) {
    match ipc::peer_identity(&stream) {
        Ok(peer) if peer_allowed(&peer, &*own) => {}
        Ok(peer) => {
            #[cfg(unix)]
            tracing::warn!(uid = peer, "rejected connection from another uid");
            #[cfg(not(unix))]
            tracing::warn!(sid = %peer, "rejected connection from another user");
            return;
        }
        // macOS reports ENOTCONN when a probing client already hung up.
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
            tracing::debug!("peer left before its credentials were read");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "rejected connection: peer credentials unavailable");
            return;
        }
    }
    serve(stream, daemon).await;
}

/// A request task the reader can abort when the client leaves.
struct Pending {
    handle: JoinHandle<()>,
    /// Writes and login changes must finish (audit, state) even after a disconnect.
    protected: bool,
}

/// Holds one of the connection's waiter slots until the task ends or is aborted.
struct WaiterSlot(Arc<AtomicUsize>);

/// Counts one outstanding reply until the request task ends or is aborted.
struct OutstandingSlot(Arc<AtomicUsize>);

impl Drop for OutstandingSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for WaiterSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn abort_pending(pending: &[Pending]) {
    for p in pending
        .iter()
        .filter(|p| !p.protected && !p.handle.is_finished())
    {
        p.handle.abort();
    }
}

fn refusal(id: u64, code: &str, message: String) -> Reply {
    Reply {
        id,
        ok: false,
        result: None,
        error: Some(ReplyError {
            code: code.to_owned(),
            message,
        }),
    }
}

fn is_protected(op: &str) -> bool {
    matches!(
        op,
        "send_text"
            | "send_media"
            | "send_reaction"
            | "login_start"
            | "login_finish"
            | "logout"
            | "restart"
    )
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S, daemon: Arc<Daemon>) {
    let (read_half, write_half) = tokio::io::split(stream);
    let (tx, rx) = mpsc::channel::<Outgoing>(REPLY_QUEUE);
    let (sub_tx, sub_rx) = mpsc::unbounded_channel::<EventRx>();
    let writer = tokio::spawn(write_loop(write_half, rx, sub_rx));

    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::new();
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let waiters = Arc::new(AtomicUsize::new(0));
    let outstanding = Arc::new(AtomicUsize::new(0));
    let mut pending: Vec<Pending> = Vec::new();
    loop {
        buf.clear();
        match read_line(&mut reader, &mut buf).await {
            Ok(Line::Eof) => break,
            Ok(Line::Blank) => continue,
            Ok(Line::Data) => {}
            Ok(Line::TooLong) => {
                tracing::warn!("client line exceeded 16 MiB, disconnecting");
                writer.abort();
                abort_pending(&pending);
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "client read failed");
                writer.abort();
                abort_pending(&pending);
                return;
            }
        }
        let Ok(req) = serde_json::from_slice::<Request>(&buf) else {
            tracing::warn!("malformed client frame, disconnecting");
            writer.abort();
            abort_pending(&pending);
            return;
        };
        if pending.len() >= 2 * MAX_OUTSTANDING {
            pending.retain(|p| !p.handle.is_finished());
        }

        // Subscribing installs the receiver before the reply is queued, so no event is missed.
        if req.op == "subscribe" {
            let _ = sub_tx.send(daemon.events.subscribe());
            queue_reply(
                &tx,
                Reply {
                    id: req.id,
                    ok: true,
                    result: Some(serde_json::json!({})),
                    error: None,
                },
            )
            .await;
            continue;
        }

        let is_waiter = req.op == "wait_for_message";
        let mut permit = None;
        let mut slot = None;
        let refused: Option<(&str, String)> =
            if outstanding.load(Ordering::SeqCst) >= MAX_OUTSTANDING {
                Some((
                    CODE_TOO_MANY_REQUESTS,
                    format!("at most {MAX_OUTSTANDING} replies outstanding per connection"),
                ))
            } else if is_waiter {
                if waiters.fetch_add(1, Ordering::SeqCst) >= MAX_WAITERS {
                    waiters.fetch_sub(1, Ordering::SeqCst);
                    Some((
                        CODE_TOO_MANY_WAITERS,
                        format!("at most {MAX_WAITERS} wait_for_message requests per connection"),
                    ))
                } else {
                    slot = Some(WaiterSlot(Arc::clone(&waiters)));
                    None
                }
            } else {
                match Arc::clone(&in_flight).try_acquire_owned() {
                    Ok(p) => {
                        permit = Some(p);
                        None
                    }
                    Err(_) => Some((
                        CODE_TOO_MANY_REQUESTS,
                        format!("at most {MAX_IN_FLIGHT} requests in flight per connection"),
                    )),
                }
            };
        if let Some((code, message)) = refused {
            // A refusal is answered inline; a full reply queue means the client does not read.
            let (flushed, _) = oneshot::channel();
            let out = Outgoing {
                line: reply_line(&refusal(req.id, code, message)),
                flushed,
            };
            if tx.try_send(out).is_err() {
                tracing::warn!("client does not read its replies, disconnecting");
                writer.abort();
                abort_pending(&pending);
                return;
            }
            continue;
        }
        let protected = is_protected(&req.op);
        let guard: InflightGuard = daemon.track();
        outstanding.fetch_add(1, Ordering::SeqCst);
        let owed = OutstandingSlot(Arc::clone(&outstanding));
        let task_daemon = Arc::clone(&daemon);
        let tx = tx.clone();
        let handle = tokio::spawn(async move {
            let reply = task_daemon.handle(req).await;
            queue_reply(&tx, reply).await;
            drop(permit);
            drop(slot);
            drop(owed);
            drop(guard);
        });
        pending.push(Pending { handle, protected });
    }
    // Leaving drops the unprotected tasks; the writer ends once the survivors have replied.
    abort_pending(&pending);
}

/// Queues a reply and waits until it is on the socket.
async fn queue_reply(tx: &mpsc::Sender<Outgoing>, reply: Reply) {
    let (flushed, written) = oneshot::channel();
    let out = Outgoing {
        line: reply_line(&reply),
        flushed,
    };
    if tx.send(out).await.is_ok() {
        let _ = written.await;
    }
}

/// Encodes a reply, swapping in an error reply when it would exceed the line limit.
fn reply_line(reply: &Reply) -> Vec<u8> {
    encode_line(reply).unwrap_or_else(|_| {
        let fallback = refusal(
            reply.id,
            "reply_too_large",
            "the reply exceeds the 16 MiB line limit".to_owned(),
        );
        encode_line(&fallback).unwrap_or_default()
    })
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut replies: mpsc::Receiver<Outgoing>,
    mut subs: mpsc::UnboundedReceiver<EventRx>,
) {
    let mut events: Option<EventRx> = None;
    let mut subs_open = true;
    loop {
        let mut flushed = None;
        let line: Arc<Vec<u8>> = tokio::select! {
            biased;
            sub = subs.recv(), if subs_open => {
                match sub {
                    Some(rx) => events = Some(rx),
                    None => subs_open = false,
                }
                continue;
            }
            r = replies.recv() => match r {
                Some(out) => {
                    flushed = Some(out.flushed);
                    Arc::new(out.line)
                }
                None => return,
            },
            ev = async {
                match events.as_mut() {
                    Some(sub) => sub.next().await,
                    None => std::future::pending().await,
                }
            } => match ev {
                Some(Delivery::Frame(line)) => line,
                Some(Delivery::Lagged) => {
                    tracing::warn!("client too slow, subscription dropped");
                    events = None;
                    Arc::new(LAGGED_FRAME.to_vec())
                }
                None => {
                    events = None;
                    continue;
                }
            },
        };
        if writer.write_all(&line).await.is_err() || writer.flush().await.is_err() {
            return;
        }
        if let Some(flushed) = flushed {
            let _ = flushed.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    #[cfg(unix)]
    use tokio::net::UnixStream;

    #[cfg(unix)]
    use crate::config::Config;
    #[cfg(unix)]
    use crate::inbox::Inbox;
    #[cfg(unix)]
    use crate::personal::audit::AuditLog;

    #[cfg(unix)]
    fn daemon(dir: &std::path::Path) -> Arc<Daemon> {
        let cfg = Config::from_lookup(|_| None, Some(dir.join("home"))).expect("config");
        let inbox = Inbox::open(&dir.join("inbox.db")).expect("inbox");
        let audit = AuditLog::open(&dir.join("audit.jsonl")).expect("audit");
        Daemon::new(cfg, inbox, audit, vec![Duration::from_secs(1)])
    }

    #[test]
    fn only_writes_and_login_changes_survive_a_disconnect() {
        for op in [
            "send_text",
            "send_media",
            "send_reaction",
            "login_start",
            "login_finish",
            "logout",
            "restart",
        ] {
            assert!(is_protected(op), "{op}");
        }
        for op in [
            "status",
            "recent",
            "subscribe",
            "wait_for_message",
            "list_threads",
            "search_users",
            "fetch_messages",
            "download_attachment",
            "mark_read",
            "set_typing",
            "login_cancel",
            "shutdown",
        ] {
            assert!(!is_protected(op), "{op}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_peer_with_another_uid_is_dropped_without_a_reply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = daemon(dir.path());
        let own = ipc::own_identity().await.expect("uid");
        let (server, mut client) = UnixStream::pair().expect("pair");
        tokio::time::timeout(
            Duration::from_secs(2),
            handle(server, d, Arc::new(own.wrapping_add(1))),
        )
        .await
        .expect("handle returns at once for a foreign uid");
        let _ = client.write_all(b"{\"id\":1,\"op\":\"status\"}\n").await;
        let mut buf = Vec::new();
        let n = tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut buf))
            .await
            .expect("no hang")
            .unwrap_or(0);
        assert_eq!((n, buf.len()), (0, 0), "nothing may be served: {buf:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_peer_with_our_uid_is_served() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = daemon(dir.path());
        let own = ipc::own_identity().await.expect("uid");
        let (server, mut client) = UnixStream::pair().expect("pair");
        tokio::spawn(handle(server, d, Arc::new(own)));
        client
            .write_all(b"{\"id\":7,\"op\":\"status\"}\n")
            .await
            .expect("write");
        let mut line = String::new();
        let mut reader = BufReader::new(&mut client);
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("no hang")
            .expect("read");
        assert!(
            line.contains("\"id\":7") && line.contains("\"ok\":true"),
            "{line}"
        );
    }
}
