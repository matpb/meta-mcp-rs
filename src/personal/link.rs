//! Line-framed request/reply link over any async byte stream pair: the sidecar's pipes and the
//! daemon socket. Frame bodies are never logged.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::personal::protocol::{Event, Frame, Reply, Request};

pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("link I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("link protocol violation: {0}")]
    Protocol(String),
    #[error("the link is closed")]
    Closed,
    #[error("no answer to `{0}` in time")]
    Timeout(String),
    #[error("`{op}` was rejected with {code}: {message}")]
    Remote {
        op: String,
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
enum ClosedReason {
    Eof,
    Failed(String),
}

impl ClosedReason {
    fn to_error(&self) -> LinkError {
        match self {
            ClosedReason::Eof => LinkError::Closed,
            ClosedReason::Failed(m) => LinkError::Protocol(m.clone()),
        }
    }
}

type Waiter = oneshot::Sender<Result<Reply, LinkError>>;

#[derive(Default)]
struct PendingTable {
    waiters: HashMap<u64, Waiter>,
    closed: Option<ClosedReason>,
}

struct Shared {
    table: Mutex<PendingTable>,
    failed: watch::Sender<bool>,
    closed: watch::Sender<bool>,
    closing: AtomicBool,
}

/// Runs its closure on drop unless disarmed; used to undo state when a request future is dropped.
pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    pub(crate) fn new(f: F) -> Self {
        Self(Some(f))
    }

    pub(crate) fn disarm(mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, PendingTable> {
        self.table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register(&self, id: u64) -> Result<oneshot::Receiver<Result<Reply, LinkError>>, LinkError> {
        let mut t = self.lock();
        if let Some(reason) = &t.closed {
            return Err(reason.to_error());
        }
        let (tx, rx) = oneshot::channel();
        t.waiters.insert(id, tx);
        Ok(rx)
    }

    fn forget(&self, id: u64) {
        self.lock().waiters.remove(&id);
    }

    fn take(&self, id: u64) -> Option<Waiter> {
        self.lock().waiters.remove(&id)
    }

    /// Marks the connection dead and fails every pending request.
    fn close(&self, reason: &ClosedReason) {
        let waiters = {
            let mut t = self.lock();
            if t.closed.is_none() {
                t.closed = Some(reason.clone());
                if matches!(reason, ClosedReason::Failed(_)) {
                    self.failed.send_replace(true);
                }
                self.closed.send_replace(true);
            }
            std::mem::take(&mut t.waiters)
        };
        for (_, w) in waiters {
            let _ = w.send(Err(reason.to_error()));
        }
    }
}

pub(crate) enum Line {
    Eof,
    TooLong,
    Blank,
    Data,
}

/// Reads one line into `buf` (cleared by the caller), enforcing the 16 MiB limit.
pub(crate) async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> io::Result<Line> {
    let cap = MAX_LINE_BYTES as u64 + 1;
    let n = (&mut *reader).take(cap).read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.len() > MAX_LINE_BYTES {
        return Ok(Line::TooLong);
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    if buf.iter().all(u8::is_ascii_whitespace) {
        return Ok(Line::Blank);
    }
    Ok(Line::Data)
}

/// Serialises one frame as a line, refusing anything over the 16 MiB limit.
pub(crate) fn encode_line<T: Serialize>(frame: &T) -> Result<Vec<u8>, LinkError> {
    let mut line = serde_json::to_vec(frame)
        .map_err(|e| LinkError::Protocol(format!("could not encode frame: {e}")))?;
    line.push(b'\n');
    if line.len() > MAX_LINE_BYTES {
        return Err(LinkError::Protocol(
            "frame exceeds the 16 MiB line limit".into(),
        ));
    }
    Ok(line)
}

async fn read_loop<R: AsyncRead + Unpin>(
    reader: R,
    shared: Arc<Shared>,
    events: UnboundedSender<Event>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let reason = loop {
        buf.clear();
        match read_line(&mut reader, &mut buf).await {
            Ok(Line::Eof) => break ClosedReason::Eof,
            Ok(Line::TooLong) => {
                break ClosedReason::Failed("a peer line exceeded 16 MiB".into());
            }
            Ok(Line::Blank) => continue,
            Ok(Line::Data) => {}
            Err(e) => break ClosedReason::Failed(format!("reading from the peer failed: {e}")),
        }
        match Frame::parse(&buf) {
            Ok(Frame::Reply(reply)) => {
                let id = reply.id;
                match shared.take(id) {
                    Some(w) => {
                        let _ = w.send(Ok(reply));
                    }
                    None => tracing::warn!(id, "reply with no pending request"),
                }
            }
            Ok(Frame::Event(ev)) => {
                // Fails only once the receiver is gone, at teardown.
                let _ = events.send(ev);
            }
            Err(e) => {
                break ClosedReason::Failed(format!(
                    "unparseable peer frame ({:?} at line {} column {})",
                    e.classify(),
                    e.line(),
                    e.column()
                ));
            }
        }
    };
    match &reason {
        ClosedReason::Failed(m) => tracing::error!("{m}"),
        ClosedReason::Eof if shared.closing.load(Ordering::SeqCst) => {
            tracing::debug!("peer output closed");
        }
        ClosedReason::Eof => tracing::warn!("peer output closed unexpectedly"),
    }
    shared.close(&reason);
}

type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

pub struct Link {
    /// Aborted on drop, so a link over a split stream closes the whole connection.
    read_task: tokio::task::JoinHandle<()>,
    writer: AsyncMutex<Option<BoxWriter>>,
    shared: Arc<Shared>,
    events: AsyncMutex<Option<UnboundedReceiver<Event>>>,
    next_id: AtomicU64,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.read_task.abort();
    }
}

impl Link {
    /// Starts the read task on the current tokio runtime.
    pub fn new<R, W>(reader: R, writer: W) -> Link
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let shared = Arc::new(Shared {
            table: Mutex::new(PendingTable::default()),
            failed: watch::channel(false).0,
            closed: watch::channel(false).0,
            closing: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let read_task = tokio::spawn(read_loop(reader, Arc::clone(&shared), tx));
        Link {
            read_task,
            writer: AsyncMutex::new(Some(Box::new(writer))),
            shared,
            events: AsyncMutex::new(Some(rx)),
            next_id: AtomicU64::new(0),
        }
    }

    pub async fn request(&self, op: &str, params: Value) -> Result<Value, LinkError> {
        self.request_with_timeout(op, params, REQUEST_TIMEOUT).await
    }

    pub async fn request_with_timeout(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, LinkError> {
        let deadline = Instant::now() + timeout;
        let timed_out = || LinkError::Timeout(op.to_string());

        // Id allocation and the write share the writer lock so ids hit the wire in order.
        let (rx, forget_guard) = {
            let mut guard = tokio::time::timeout_at(deadline, self.writer.lock())
                .await
                .map_err(|_| timed_out())?;
            let writer = guard.as_mut().ok_or(LinkError::Closed)?;
            let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            let line = encode_line(&Request {
                id,
                op: op.to_string(),
                params,
            })
            .map_err(|e| match e {
                LinkError::Protocol(m) => LinkError::Protocol(format!("request `{op}`: {m}")),
                other => other,
            })?;
            let rx = self.shared.register(id)?;
            let shared = &self.shared;
            let forget = OnDrop::new(move || shared.forget(id));
            tracing::debug!(id, op, "link request");
            // A half-written line corrupts the stream, so a dropped write kills the connection.
            let broken = OnDrop::new(|| {
                shared.close(&ClosedReason::Failed("link write interrupted".into()));
            });
            let write = async {
                writer.write_all(&line).await?;
                writer.flush().await
            };
            match tokio::time::timeout_at(deadline, write).await {
                Ok(Ok(())) => broken.disarm(),
                Ok(Err(e)) => {
                    broken.disarm();
                    return Err(LinkError::Io(e));
                }
                Err(_) => return Err(timed_out()),
            }
            (rx, forget)
        };

        let reply = match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(r)) => r?,
            Ok(Err(_)) => return Err(LinkError::Closed),
            Err(_) => return Err(timed_out()),
        };
        forget_guard.disarm();
        if reply.ok {
            Ok(reply.result.unwrap_or(Value::Null))
        } else {
            let (code, message) = reply.error.map_or_else(
                || ("unknown".to_string(), "no error detail".to_string()),
                |e| (e.code, e.message),
            );
            Err(LinkError::Remote {
                op: op.to_string(),
                code,
                message,
            })
        }
    }

    /// Resolves once the connection died on a protocol violation or a read error.
    pub async fn failed(&self) {
        let mut rx = self.shared.failed.subscribe();
        if rx.wait_for(|failed| *failed).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// True once the connection is gone for any reason.
    pub fn is_closed(&self) -> bool {
        *self.shared.closed.borrow()
    }

    /// Resolves once the connection is gone for any reason, a clean EOF included.
    pub async fn closed(&self) {
        let mut rx = self.shared.closed.subscribe();
        if rx.wait_for(|closed| *closed).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// Hands the event receiver to the caller; `None` once taken.
    pub fn take_events(&self) -> Option<UnboundedReceiver<Event>> {
        self.events.try_lock().ok()?.take()
    }

    /// Waits for the next event; for startup handshakes before `take_events`.
    pub async fn recv_event(&self, timeout: Duration, what: &str) -> Result<Event, LinkError> {
        let mut guard = self.events.lock().await;
        let rx = guard.as_mut().ok_or(LinkError::Closed)?;
        match tokio::time::timeout(timeout, rx.recv()).await {
            Err(_) => Err(LinkError::Timeout(what.to_string())),
            Ok(None) => Err(LinkError::Closed),
            Ok(Some(ev)) => Ok(ev),
        }
    }

    /// Quiets the log line for an expected EOF.
    pub fn expect_close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
    }

    /// Drops the write half so the peer sees EOF. Gives up after `timeout` if a write is stuck.
    pub async fn close_write(&self, timeout: Duration) {
        if let Ok(mut guard) = tokio::time::timeout(timeout, self.writer.lock()).await
            && let Some(mut w) = guard.take()
        {
            let _ = tokio::time::timeout(timeout, w.shutdown()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{DuplexStream, duplex};

    const WAIT: Duration = Duration::from_secs(10);

    /// A link whose peer is the returned duplex end, split into a line reader and writer.
    fn pair() -> (
        Link,
        BufReader<tokio::io::ReadHalf<DuplexStream>>,
        tokio::io::WriteHalf<DuplexStream>,
    ) {
        let (a, b) = duplex(64 * 1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        (Link::new(ar, aw), BufReader::new(br), bw)
    }

    async fn next_request(peer: &mut BufReader<tokio::io::ReadHalf<DuplexStream>>) -> Request {
        let mut line = String::new();
        peer.read_line(&mut line).await.expect("peer read");
        serde_json::from_str(&line).expect("request")
    }

    #[tokio::test]
    async fn request_reply_and_increasing_ids() {
        let (link, mut peer_r, mut peer_w) = pair();
        let peer = tokio::spawn(async move {
            let mut ids = Vec::new();
            for _ in 0..3 {
                let r = next_request(&mut peer_r).await;
                ids.push(r.id);
                let line = format!(
                    "{{\"id\":{},\"ok\":true,\"result\":{{\"n\":{}}}}}\n",
                    r.id, r.id
                );
                peer_w.write_all(line.as_bytes()).await.expect("write");
            }
            ids
        });
        for n in 1..=3 {
            let v = link.request("ping", Value::Null).await.expect("reply");
            assert_eq!(v["n"], n);
        }
        assert_eq!(peer.await.expect("peer"), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn remote_error_carries_code_and_message() {
        let (link, mut peer_r, mut peer_w) = pair();
        tokio::spawn(async move {
            let r = next_request(&mut peer_r).await;
            let line = format!(
                "{{\"id\":{},\"ok\":false,\"error\":{{\"code\":\"nope\",\"message\":\"m\"}}}}\n",
                r.id
            );
            peer_w.write_all(line.as_bytes()).await.expect("write");
        });
        let err = link.request("x", Value::Null).await.expect_err("remote");
        assert!(
            matches!(&err, LinkError::Remote { code, message, .. } if code == "nope" && message == "m"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn timeout_forgets_the_waiter() {
        let (link, _peer_r, _peer_w) = pair();
        let err = link
            .request_with_timeout("ping", Value::Null, Duration::from_millis(100))
            .await
            .expect_err("timeout");
        assert!(matches!(err, LinkError::Timeout(_)), "{err:?}");
        assert!(link.shared.lock().waiters.is_empty());
    }

    #[tokio::test]
    async fn eof_closes_without_failing() {
        let (link, _peer_r, mut peer_w) = pair();
        peer_w.shutdown().await.expect("shutdown");
        tokio::time::timeout(WAIT, link.closed())
            .await
            .expect("closed() resolves");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), link.failed())
                .await
                .is_err(),
            "EOF must not resolve failed()"
        );
        let err = link.request("ping", Value::Null).await.expect_err("closed");
        assert!(matches!(err, LinkError::Closed), "{err:?}");
    }

    #[tokio::test]
    async fn garbage_fails_and_closes() {
        let (link, _peer_r, mut peer_w) = pair();
        peer_w.write_all(b"not json\n").await.expect("write");
        tokio::time::timeout(WAIT, link.failed())
            .await
            .expect("failed() resolves");
        tokio::time::timeout(WAIT, link.closed())
            .await
            .expect("closed() resolves");
        let err = link
            .request("ping", Value::Null)
            .await
            .expect_err("protocol");
        assert!(matches!(err, LinkError::Protocol(_)), "{err:?}");
    }

    #[tokio::test]
    async fn oversized_incoming_line_fails() {
        let (link, _peer_r, mut peer_w) = pair();
        tokio::spawn(async move {
            let chunk = vec![b'a'; 1024 * 1024];
            for _ in 0..17 {
                if peer_w.write_all(&chunk).await.is_err() {
                    return;
                }
            }
            let _ = peer_w.write_all(b"\n").await;
        });
        tokio::time::timeout(WAIT, link.failed())
            .await
            .expect("failed() resolves");
    }

    #[tokio::test]
    async fn oversized_request_is_refused_before_writing() {
        let (link, mut peer_r, mut peer_w) = pair();
        tokio::spawn(async move {
            let r = next_request(&mut peer_r).await;
            let line = format!("{{\"id\":{},\"ok\":true,\"result\":{{}}}}\n", r.id);
            peer_w.write_all(line.as_bytes()).await.expect("write");
        });
        let big = "a".repeat(17 * 1024 * 1024);
        let err = link
            .request("ping", json!({ "x": big }))
            .await
            .expect_err("refused");
        assert!(matches!(err, LinkError::Protocol(_)), "{err:?}");
        link.request("ping", Value::Null).await.expect("after");
    }

    #[tokio::test]
    async fn blank_lines_and_events_are_handled() {
        let (link, mut peer_r, mut peer_w) = pair();
        tokio::spawn(async move {
            peer_w
                .write_all(b"\n  \r\n{\"event\":\"hi\",\"data\":{\"n\":1}}\n")
                .await
                .expect("write");
            let r = next_request(&mut peer_r).await;
            let line = format!("\n{{\"id\":{},\"ok\":true,\"result\":null}}\n", r.id);
            peer_w.write_all(line.as_bytes()).await.expect("write");
        });
        let ev = link.recv_event(WAIT, "hi").await.expect("event");
        assert_eq!(ev.event, "hi");
        link.request("ping", Value::Null).await.expect("reply");
    }

    #[tokio::test]
    async fn events_are_taken_once_and_in_order() {
        let (link, _peer_r, mut peer_w) = pair();
        for n in 0..1000 {
            let line = format!("{{\"event\":\"e\",\"data\":{{\"n\":{n}}}}}\n");
            peer_w.write_all(line.as_bytes()).await.expect("write");
        }
        let mut rx = link.take_events().expect("events");
        assert!(link.take_events().is_none());
        for n in 0..1000 {
            let ev = tokio::time::timeout(WAIT, rx.recv())
                .await
                .expect("in time")
                .expect("event");
            assert_eq!(ev.data["n"], n);
        }
    }

    #[tokio::test]
    async fn close_write_gives_the_peer_eof() {
        let (link, mut peer_r, _peer_w) = pair();
        link.close_write(Duration::from_secs(1)).await;
        let mut line = String::new();
        let n = peer_r.read_line(&mut line).await.expect("read");
        assert_eq!(n, 0);
        let err = link.request("ping", Value::Null).await.expect_err("closed");
        assert!(matches!(err, LinkError::Closed), "{err:?}");
    }
}
