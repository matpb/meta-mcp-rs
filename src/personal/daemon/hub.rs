//! Event fan-out to subscribed connections through a ring capped by bytes; a subscriber that falls
//! behind the ring is told once and dropped from further events.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

pub const MAX_RING_BYTES: usize = 16 * 1024 * 1024;
const MAX_RING_FRAMES: usize = 1024;

/// The frame a lagged subscriber receives before its subscription ends.
pub const LAGGED_FRAME: &[u8] =
    b"{\"event\":\"error\",\"data\":{\"code\":\"lagged\",\"message\":\"lagged: resubscribe\"}}\n";

struct State {
    frames: VecDeque<Arc<Vec<u8>>>,
    /// Sequence number of `frames[0]`.
    first_seq: u64,
    bytes: usize,
}

pub struct EventHub {
    state: Mutex<State>,
    notify: Notify,
    max_bytes: usize,
}

/// What a subscriber gets next.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Frame(Arc<Vec<u8>>),
    /// The subscriber fell off the ring; nothing more will arrive.
    Lagged,
}

impl EventHub {
    pub fn new() -> Arc<Self> {
        Self::with_limit(MAX_RING_BYTES)
    }

    pub fn with_limit(max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                frames: VecDeque::new(),
                first_seq: 0,
                bytes: 0,
            }),
            notify: Notify::new(),
            max_bytes,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends a frame, evicting the oldest ones past the byte and count caps (the newest stays).
    pub fn publish(&self, frame: Arc<Vec<u8>>) {
        {
            let mut st = self.lock();
            st.bytes += frame.len();
            st.frames.push_back(frame);
            while st.frames.len() > 1
                && (st.bytes > self.max_bytes || st.frames.len() > MAX_RING_FRAMES)
            {
                if let Some(old) = st.frames.pop_front() {
                    st.bytes -= old.len();
                    st.first_seq += 1;
                }
            }
        }
        self.notify.notify_waiters();
    }

    /// A subscription that sees frames published from now on.
    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        let next = {
            let st = self.lock();
            st.first_seq + st.frames.len() as u64
        };
        Subscription {
            hub: Arc::clone(self),
            next,
            done: false,
        }
    }

    #[cfg(test)]
    fn buffered_bytes(&self) -> usize {
        self.lock().bytes
    }
}

pub struct Subscription {
    hub: Arc<EventHub>,
    next: u64,
    done: bool,
}

impl Subscription {
    /// The next frame, `Lagged` once, then `None` for good.
    pub async fn next(&mut self) -> Option<Delivery> {
        loop {
            if self.done {
                return None;
            }
            let notified = self.hub.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let st = self.hub.lock();
                if self.next < st.first_seq {
                    self.done = true;
                    return Some(Delivery::Lagged);
                }
                let idx = usize::try_from(self.next - st.first_seq).unwrap_or(usize::MAX);
                if let Some(frame) = st.frames.get(idx) {
                    self.next += 1;
                    return Some(Delivery::Frame(Arc::clone(frame)));
                }
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![b'x'; n])
    }

    async fn next(s: &mut Subscription) -> Option<Delivery> {
        tokio::time::timeout(Duration::from_secs(2), s.next())
            .await
            .expect("no hang")
    }

    #[tokio::test]
    async fn frames_reach_a_subscriber_in_order_and_only_from_subscription_on() {
        let hub = EventHub::with_limit(1000);
        hub.publish(frame(1));
        let mut s = hub.subscribe();
        hub.publish(frame(2));
        hub.publish(frame(3));
        assert_eq!(next(&mut s).await, Some(Delivery::Frame(frame(2))));
        assert_eq!(next(&mut s).await, Some(Delivery::Frame(frame(3))));
        let waiting = tokio::spawn(async move { next(&mut s).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        hub.publish(frame(4));
        assert_eq!(
            waiting.await.expect("join"),
            Some(Delivery::Frame(frame(4)))
        );
    }

    #[tokio::test]
    async fn the_ring_is_capped_by_bytes_and_a_slow_subscriber_is_told_once() {
        let hub = EventHub::with_limit(100);
        let mut slow = hub.subscribe();
        let mut fast = hub.subscribe();
        for _ in 0..10 {
            hub.publish(frame(30));
            assert!(matches!(next(&mut fast).await, Some(Delivery::Frame(_))));
        }
        assert!(hub.buffered_bytes() <= 100, "{}", hub.buffered_bytes());
        assert_eq!(next(&mut slow).await, Some(Delivery::Lagged));
        assert_eq!(next(&mut slow).await, None);
        hub.publish(frame(5));
        assert_eq!(next(&mut slow).await, None, "dropped from further events");
        assert!(matches!(next(&mut fast).await, Some(Delivery::Frame(_))));
    }

    #[tokio::test]
    async fn one_oversized_frame_is_kept_alone_and_resubscribing_works() {
        let hub = EventHub::with_limit(10);
        let mut s = hub.subscribe();
        hub.publish(frame(50));
        assert_eq!(next(&mut s).await, Some(Delivery::Frame(frame(50))));
        hub.publish(frame(50));
        hub.publish(frame(50));
        assert_eq!(next(&mut s).await, Some(Delivery::Lagged));
        let mut again = hub.subscribe();
        hub.publish(frame(7));
        assert_eq!(next(&mut again).await, Some(Delivery::Frame(frame(7))));
    }

    #[test]
    fn the_lagged_frame_is_one_valid_error_event() {
        let v: serde_json::Value = serde_json::from_slice(LAGGED_FRAME).expect("json");
        assert_eq!(v["event"], "error");
        assert_eq!(v["data"]["message"], "lagged: resubscribe");
        assert!(LAGGED_FRAME.ends_with(b"\n"));
    }
}
