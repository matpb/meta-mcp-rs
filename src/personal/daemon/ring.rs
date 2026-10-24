//! In-memory ring of the last live message events (ids and timestamps only, never text).

use std::collections::VecDeque;

use serde::Serialize;

pub const RING_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentEntry {
    #[serde(skip)]
    pub message_id: String,
    pub id: String,
    pub platform: String,
    pub thread_id: String,
    pub ts: i64,
    pub direction: String,
}

#[derive(Debug, Default)]
pub struct MessageRing {
    entries: VecDeque<RecentEntry>,
    wrapped: bool,
}

impl MessageRing {
    /// Records `entry`; false when its id is already known.
    pub fn push(&mut self, entry: RecentEntry) -> bool {
        if self.entries.iter().any(|e| {
            (&e.platform, &e.thread_id, &e.message_id)
                == (&entry.platform, &entry.thread_id, &entry.message_id)
        }) {
            return false;
        }
        if self.entries.len() == RING_CAPACITY {
            self.entries.pop_front();
            self.wrapped = true;
        }
        self.entries.push_back(entry);
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.wrapped = false;
    }

    /// Entries at or after `since` (Unix seconds), and whether evicted ones may have qualified.
    pub fn recent(&self, since: i64) -> (Vec<RecentEntry>, bool) {
        let found = self
            .entries
            .iter()
            .filter(|e| e.ts >= since)
            .cloned()
            .collect();
        let truncated = self.wrapped
            && self
                .entries
                .front()
                .is_some_and(|oldest| oldest.ts >= since);
        (found, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: i64) -> RecentEntry {
        RecentEntry {
            message_id: format!("m{n}"),
            id: format!("messenger_personal:t:m{n}"),
            platform: "messenger_personal".into(),
            thread_id: "t".into(),
            ts: n,
            direction: "in".into(),
        }
    }

    #[test]
    fn recent_filters_and_ignores_duplicates() {
        let mut ring = MessageRing::default();
        assert!(ring.push(entry(10)));
        assert!(ring.push(entry(30)));
        assert!(!ring.push(entry(10)));
        let (found, truncated) = ring.recent(20);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ts, 30);
        assert!(!truncated);
    }

    #[test]
    fn dedupe_is_per_platform_thread_and_message() {
        let mut ring = MessageRing::default();
        let mut a = entry(10);
        a.message_id = "same".into();
        let mut b = a.clone();
        b.thread_id = "other".into();
        let mut c = a.clone();
        c.platform = "whatsapp_personal".into();
        assert!(ring.push(a.clone()));
        assert!(!ring.push(a));
        assert!(ring.push(b));
        assert!(ring.push(c));
    }

    #[test]
    fn wrapping_marks_old_requests_truncated() {
        let mut ring = MessageRing::default();
        let cap = i64::try_from(RING_CAPACITY).expect("fits");
        for n in 1..=cap + 1 {
            ring.push(entry(n * 10));
        }
        let (all, truncated) = ring.recent(0);
        assert_eq!(all.len(), RING_CAPACITY);
        assert!(truncated);
        assert!(!ring.recent(21).1);
        assert!(ring.recent(20).1);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut ring = MessageRing::default();
        let cap = i64::try_from(RING_CAPACITY).expect("fits");
        for n in 0..=cap {
            ring.push(entry(n));
        }
        ring.clear();
        let (found, truncated) = ring.recent(0);
        assert!(found.is_empty() && !truncated);
    }

    #[test]
    fn entries_serialize_without_text() {
        let json = serde_json::to_value(entry(5)).expect("json");
        assert_eq!(
            json,
            serde_json::json!({
                "id": "messenger_personal:t:m5", "platform": "messenger_personal", "thread_id": "t",
                "ts": 5, "direction": "in"
            })
        );
    }
}
