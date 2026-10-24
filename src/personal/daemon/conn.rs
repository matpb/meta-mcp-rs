//! One client connection: parses requests, runs each in its own task and writes replies plus
//! subscribed events through a single writer task.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{Daemon, InflightGuard, peer_allowed};
use crate::personal::link::{Line, encode_line, read_line};
use crate::personal::protocol::{
    CODE_TOO_MANY_REQUESTS, CODE_TOO_MANY_WAITERS, Reply, ReplyError, Request,
};

/// Requests in flight per connection before new ones are refused.
const MAX_IN_FLIGHT: usize = 32;
/// Concurrent `wait_for_message` requests per connection.
const MAX_WAITERS: usize = 16;
const REPLY_QUEUE: usize = 64;

type EventRx = broadcast::Receiver<Arc<Vec<u8>>>;

/// A reply line for the writer; `flushed` fires once the line is on the socket.
struct Outgoing {
    line: Vec<u8>,
    flushed: oneshot::Sender<()>,
}

/// Checks the peer uid, then serves the connection until the client leaves.
pub async fn handle(stream: UnixStream, daemon: Arc<Daemon>, own_uid: u32) {
    match stream.peer_cred() {
        Ok(cred) if peer_allowed(cred.uid(), own_uid) => {}
        Ok(cred) => {
            tracing::warn!(uid = cred.uid(), "rejected connection from another uid");
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

async fn serve(stream: UnixStream, daemon: Arc<Daemon>) {
    let (read_half, write_half) = stream.into_split();
    let (tx, rx) = mpsc::channel::<Outgoing>(REPLY_QUEUE);
    let (sub_tx, sub_rx) = mpsc::unbounded_channel::<EventRx>();
    let writer = tokio::spawn(write_loop(write_half, rx, sub_rx));

    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::new();
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let waiters = Arc::new(AtomicUsize::new(0));
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
        pending.retain(|p| !p.handle.is_finished());

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
        let mut refused = None;
        if is_waiter {
            if waiters.fetch_add(1, Ordering::SeqCst) >= MAX_WAITERS {
                waiters.fetch_sub(1, Ordering::SeqCst);
                refused = Some(refusal(
                    req.id,
                    CODE_TOO_MANY_WAITERS,
                    format!("at most {MAX_WAITERS} wait_for_message requests per connection"),
                ));
            } else {
                slot = Some(WaiterSlot(Arc::clone(&waiters)));
            }
        } else {
            match Arc::clone(&in_flight).try_acquire_owned() {
                Ok(p) => permit = Some(p),
                Err(_) => {
                    refused = Some(refusal(
                        req.id,
                        CODE_TOO_MANY_REQUESTS,
                        format!("at most {MAX_IN_FLIGHT} requests in flight per connection"),
                    ));
                }
            }
        }
        let protected = is_protected(&req.op);
        let guard: InflightGuard = daemon.track();
        let task_daemon = Arc::clone(&daemon);
        let tx = tx.clone();
        let handle = tokio::spawn(async move {
            let reply = match refused {
                Some(reply) => reply,
                None => task_daemon.handle(req).await,
            };
            queue_reply(&tx, reply).await;
            drop(permit);
            drop(slot);
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
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => match ev {
                Ok(line) => line,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(lost = n, "client too slow, events dropped");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
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
