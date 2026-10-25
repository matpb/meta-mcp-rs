//! Event model and the SQLite inbox fed by the webhook receiver.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: usize = 1000;
const MARK_CHUNK: usize = 400;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    platform TEXT NOT NULL,
    kind TEXT NOT NULL,
    ts INTEGER NOT NULL,
    account_id TEXT NOT NULL,
    sender TEXT NOT NULL,
    recipient TEXT NOT NULL,
    text TEXT,
    attachments TEXT NOT NULL,
    reply_to TEXT,
    raw TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    processed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_events_platform_ts ON events (platform, ts);
CREATE INDEX IF NOT EXISTS idx_events_processed_received ON events (processed, received_at);
";

const COLUMNS: &str = "id, platform, kind, ts, account_id, sender, recipient, text, attachments, \
                       reply_to, raw, received_at, processed";

/// Seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Where an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Messenger,
    WhatsApp,
    Instagram,
    #[serde(rename = "messenger_personal")]
    MessengerPersonal,
    #[serde(rename = "whatsapp_personal")]
    WhatsAppPersonal,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Messenger => "messenger",
            Self::WhatsApp => "whatsapp",
            Self::Instagram => "instagram",
            Self::MessengerPersonal => "messenger_personal",
            Self::WhatsAppPersonal => "whatsapp_personal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "messenger" => Some(Self::Messenger),
            "whatsapp" => Some(Self::WhatsApp),
            "instagram" => Some(Self::Instagram),
            "messenger_personal" => Some(Self::MessengerPersonal),
            "whatsapp_personal" => Some(Self::WhatsAppPersonal),
            _ => None,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Message,
    Echo,
    Delivery,
    Read,
    Reaction,
    Postback,
    Referral,
    Optin,
    Status,
    Standby,
    Other,
}

impl EventKind {
    pub const ALL: [EventKind; 11] = [
        Self::Message,
        Self::Echo,
        Self::Delivery,
        Self::Read,
        Self::Reaction,
        Self::Postback,
        Self::Referral,
        Self::Optin,
        Self::Status,
        Self::Standby,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Echo => "echo",
            Self::Delivery => "delivery",
            Self::Read => "read",
            Self::Reaction => "reaction",
            Self::Postback => "postback",
            Self::Referral => "referral",
            Self::Optin => "optin",
            Self::Status => "status",
            Self::Standby => "standby",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let wanted = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|k| k.as_str() == wanted)
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A media item or rich payload attached to a message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Attachment {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Self::default()
        }
    }
}

/// One normalised inbound or outbound event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub platform: Platform,
    pub kind: EventKind,
    pub ts: i64,
    pub account_id: String,
    pub sender: String,
    pub recipient: String,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<String>,
    pub raw: Value,
    pub received_at: i64,
    pub processed: bool,
}

impl Event {
    /// An empty event to fill with struct-update syntax.
    pub fn new(id: impl Into<String>, platform: Platform, kind: EventKind) -> Self {
        Self {
            id: id.into(),
            platform,
            kind,
            ts: 0,
            account_id: String::new(),
            sender: String::new(),
            recipient: String::new(),
            text: None,
            attachments: Vec::new(),
            reply_to: None,
            raw: Value::Null,
            received_at: 0,
            processed: false,
        }
    }
}

/// Filter for `Inbox::list`; every field is optional and they combine with AND.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub platform: Option<Platform>,
    pub kinds: Vec<EventKind>,
    pub since_ts: Option<i64>,
    pub since_received: Option<i64>,
    pub unprocessed_only: bool,
    pub account_id: Option<String>,
    pub limit: Option<usize>,
    /// Leave out the personal Messenger and WhatsApp rows.
    pub exclude_personal: bool,
}

/// Counters for status and health output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxStats {
    pub total: i64,
    pub unprocessed: i64,
    pub last_received_at: Option<i64>,
    pub by_platform: Vec<(String, i64)>,
}

impl InboxStats {
    /// JSON with `by_platform` as an object.
    pub fn to_json(&self) -> Value {
        let by_platform: serde_json::Map<String, Value> = self
            .by_platform
            .iter()
            .map(|(p, n)| (p.clone(), json!(n)))
            .collect();
        json!({
            "total": self.total,
            "unprocessed": self.unprocessed,
            "last_received_at": self.last_received_at,
            "by_platform": by_platform,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("inbox database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("inbox file error on {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("inbox data is corrupt: {0}")]
    Corrupt(String),
    #[error("inbox lock was poisoned by a crashed thread")]
    Poisoned,
    #[error("inbox task failed: {0}")]
    Task(String),
}

/// The SQLite event store; cloning shares one connection.
#[derive(Clone)]
pub struct Inbox {
    conn: Arc<Mutex<Connection>>,
}

impl fmt::Debug for Inbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Inbox")
    }
}

fn io_err(path: &Path, source: std::io::Error) -> InboxError {
    InboxError::Io {
        path: path.display().to_string(),
        source,
    }
}

fn prepare_files(path: &Path) -> Result<(), InboxError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let mut dirs = std::fs::DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            dirs.mode(0o700);
        }
        dirs.create(parent).map_err(|e| io_err(parent, e))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(io_err(path, e)),
    }
}

fn to_json_string(v: &impl Serialize) -> Result<String, InboxError> {
    serde_json::to_string(v).map_err(|e| InboxError::Corrupt(e.to_string()))
}

fn conversion_error(idx: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        format!("stored {what} is not valid").into(),
    )
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let platform: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let attachments: String = row.get(8)?;
    let raw: String = row.get(10)?;
    Ok(Event {
        id: row.get(0)?,
        platform: Platform::parse(&platform).ok_or_else(|| conversion_error(1, "platform"))?,
        kind: EventKind::parse(&kind).ok_or_else(|| conversion_error(2, "kind"))?,
        ts: row.get(3)?,
        account_id: row.get(4)?,
        sender: row.get(5)?,
        recipient: row.get(6)?,
        text: row.get(7)?,
        attachments: serde_json::from_str(&attachments)
            .map_err(|_| conversion_error(8, "attachments"))?,
        reply_to: row.get(9)?,
        raw: serde_json::from_str(&raw).map_err(|_| conversion_error(10, "raw event"))?,
        received_at: row.get(11)?,
        processed: row.get::<_, i64>(12)? != 0,
    })
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

impl Inbox {
    /// Opens (creating when missing) the database in WAL mode; new files are private.
    pub fn open(path: &Path) -> Result<Inbox, InboxError> {
        prepare_files(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA synchronous=NORMAL;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Inbox {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, InboxError> {
        self.conn.lock().map_err(|_| InboxError::Poisoned)
    }

    /// Stores new events, ignoring ids already present; returns how many were new.
    pub fn insert(&self, events: &[Event]) -> Result<usize, InboxError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(&format!(
                "INSERT OR IGNORE INTO events ({COLUMNS}) VALUES ({})",
                placeholders(13)
            ))?;
            for e in events {
                inserted += stmt.execute(rusqlite::params![
                    e.id,
                    e.platform.as_str(),
                    e.kind.as_str(),
                    e.ts,
                    e.account_id,
                    e.sender,
                    e.recipient,
                    e.text,
                    to_json_string(&e.attachments)?,
                    e.reply_to,
                    to_json_string(&e.raw)?,
                    e.received_at,
                    i64::from(e.processed),
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Newest events first, honouring every filter field.
    pub fn list(&self, filter: &ListFilter) -> Result<Vec<Event>, InboxError> {
        self.list_after(filter, None, &[])
    }

    /// Like `list`, keeping only events from one of `senders` (empty means anyone): the stored
    /// sender, or the `_sender_user_id` or `from_parent_user_id` recorded in `raw`.
    pub fn list_from(
        &self,
        filter: &ListFilter,
        senders: &[String],
    ) -> Result<Vec<Event>, InboxError> {
        self.list_after(filter, None, senders)
    }

    fn list_after(
        &self,
        f: &ListFilter,
        after_rowid: Option<i64>,
        senders: &[String],
    ) -> Result<Vec<Event>, InboxError> {
        let mut sql = format!("SELECT {COLUMNS} FROM events WHERE 1=1");
        let mut args: Vec<Sql> = Vec::new();
        if let Some(p) = f.platform {
            sql.push_str(" AND platform = ?");
            args.push(Sql::Text(p.as_str().into()));
        }
        if f.exclude_personal {
            sql.push_str(" AND platform NOT IN ('messenger_personal', 'whatsapp_personal')");
        }
        if !f.kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", placeholders(f.kinds.len())));
            args.extend(f.kinds.iter().map(|k| Sql::Text(k.as_str().into())));
        }
        if let Some(t) = f.since_ts {
            sql.push_str(" AND ts >= ?");
            args.push(Sql::Integer(t));
        }
        if let Some(t) = f.since_received {
            sql.push_str(" AND received_at >= ?");
            args.push(Sql::Integer(t));
        }
        if f.unprocessed_only {
            sql.push_str(" AND processed = 0");
        }
        if let Some(a) = &f.account_id {
            sql.push_str(" AND account_id = ?");
            args.push(Sql::Text(a.clone()));
        }
        if !senders.is_empty() {
            let marks = placeholders(senders.len());
            sql.push_str(&format!(
                " AND (sender IN ({marks}) \
                 OR json_extract(raw, '$._sender_user_id') IN ({marks}) \
                 OR json_extract(raw, '$.from_parent_user_id') IN ({marks}))"
            ));
            for _ in 0..3 {
                args.extend(senders.iter().map(|s| Sql::Text(s.clone())));
            }
        }
        if let Some(r) = after_rowid {
            sql.push_str(" AND rowid > ?");
            args.push(Sql::Integer(r));
        }
        sql.push_str(" ORDER BY ts DESC, received_at DESC, rowid DESC LIMIT ?");
        let limit = f
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        args.push(Sql::Integer(limit as i64));

        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Flags events as handled; returns how many changed from unprocessed to processed.
    pub fn mark_processed(&self, ids: &[String]) -> Result<usize, InboxError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut updated = 0;
        for chunk in ids.chunks(MARK_CHUNK) {
            let sql = format!(
                "UPDATE events SET processed = 1 WHERE processed = 0 AND id IN ({})",
                placeholders(chunk.len())
            );
            updated += tx.execute(&sql, params_from_iter(chunk.iter()))?;
        }
        tx.commit()?;
        Ok(updated)
    }

    pub fn stats(&self) -> Result<InboxStats, InboxError> {
        let conn = self.lock()?;
        let (total, unprocessed, last_received_at) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN processed = 0 THEN 1 ELSE 0 END), 0), \
             MAX(received_at) FROM events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut stmt = conn
            .prepare("SELECT platform, COUNT(*) FROM events GROUP BY platform ORDER BY platform")?;
        let by_platform = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<(String, i64)>, _>>()?;
        Ok(InboxStats {
            total,
            unprocessed,
            last_received_at,
            by_platform,
        })
    }

    fn max_rowid(&self) -> Result<i64, InboxError> {
        let conn = self.lock()?;
        Ok(
            conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM events", [], |r| {
                r.get(0)
            })?,
        )
    }

    async fn blocking<T, F>(&self, work: F) -> Result<T, InboxError>
    where
        T: Send + 'static,
        F: FnOnce(Inbox) -> Result<T, InboxError> + Send + 'static,
    {
        let inbox = self.clone();
        tokio::task::spawn_blocking(move || work(inbox))
            .await
            .map_err(|e| InboxError::Task(e.to_string()))?
    }

    pub async fn insert_async(&self, events: Vec<Event>) -> Result<usize, InboxError> {
        self.blocking(move |i| i.insert(&events)).await
    }

    pub async fn list_async(&self, filter: ListFilter) -> Result<Vec<Event>, InboxError> {
        self.blocking(move |i| i.list(&filter)).await
    }

    pub async fn list_from_async(
        &self,
        filter: ListFilter,
        senders: Vec<String>,
    ) -> Result<Vec<Event>, InboxError> {
        self.blocking(move |i| i.list_from(&filter, &senders)).await
    }

    pub async fn mark_processed_async(&self, ids: Vec<String>) -> Result<usize, InboxError> {
        self.blocking(move |i| i.mark_processed(&ids)).await
    }

    pub async fn stats_async(&self) -> Result<InboxStats, InboxError> {
        self.blocking(|i| i.stats()).await
    }

    /// Like `wait`, but database errors are returned instead of logged.
    pub async fn try_wait(
        &self,
        filter: &ListFilter,
        timeout: Duration,
        poll: Duration,
    ) -> Result<Vec<Event>, InboxError> {
        let baseline = self.blocking(|i| i.max_rowid()).await?;
        let deadline = Instant::now() + timeout;
        loop {
            let f = filter.clone();
            let found = self
                .blocking(move |i| i.list_after(&f, Some(baseline), &[]))
                .await?;
            if !found.is_empty() {
                return Ok(found);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Vec::new());
            }
            tokio::time::sleep(poll.min(left)).await;
        }
    }

    /// Events matching `filter` that arrive after this call starts; empty at `timeout`.
    pub async fn wait(&self, filter: &ListFilter, timeout: Duration, poll: Duration) -> Vec<Event> {
        match self.try_wait(filter, timeout, poll).await {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!("inbox wait failed: {e}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_tmp() -> (tempfile::TempDir, Inbox) {
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = Inbox::open(&dir.path().join("sub").join("inbox.db")).expect("open");
        (dir, inbox)
    }

    fn ev(id: &str, platform: Platform, kind: EventKind, ts: i64, account: &str) -> Event {
        Event {
            ts,
            account_id: account.into(),
            sender: "s".into(),
            recipient: "r".into(),
            text: Some(format!("text {id}")),
            raw: json!({"id": id}),
            received_at: ts + 1,
            ..Event::new(id, platform, kind)
        }
    }

    fn seed(inbox: &Inbox) {
        let events = vec![
            ev("m1", Platform::Messenger, EventKind::Message, 100, "page1"),
            ev("m2", Platform::Messenger, EventKind::Read, 200, "page1"),
            ev("w1", Platform::WhatsApp, EventKind::Message, 300, "phone1"),
            ev("w2", Platform::WhatsApp, EventKind::Status, 400, "phone2"),
            ev("w3", Platform::WhatsApp, EventKind::Message, 500, "phone1"),
        ];
        assert_eq!(inbox.insert(&events).expect("insert"), 5);
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn enums_round_trip_through_strings_and_serde() {
        for p in [
            Platform::Messenger,
            Platform::WhatsApp,
            Platform::Instagram,
            Platform::MessengerPersonal,
            Platform::WhatsAppPersonal,
        ] {
            assert_eq!(Platform::parse(p.as_str()), Some(p));
            let json = serde_json::to_value(p).expect("serialize");
            assert_eq!(json, serde_json::json!(p.as_str()));
            assert_eq!(serde_json::from_value::<Platform>(json).expect("de"), p);
            assert_eq!(serde_json::to_value(p).expect("json"), json!(p.as_str()));
        }
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(k));
            assert_eq!(serde_json::to_value(k).expect("json"), json!(k.as_str()));
        }
        assert_eq!(Platform::parse(" WhatsApp "), Some(Platform::WhatsApp));
        assert_eq!(Platform::parse("sms"), None);
        assert_eq!(EventKind::parse("nope"), None);
    }

    #[test]
    fn insert_dedupes_and_round_trips() {
        let (_dir, inbox) = open_tmp();
        let mut e = ev("x", Platform::Messenger, EventKind::Message, 10, "p");
        e.attachments = vec![Attachment {
            url: Some("https://cdn.example/a.jpg".into()),
            ..Attachment::new("image")
        }];
        e.reply_to = Some("m_0".into());
        assert_eq!(inbox.insert(std::slice::from_ref(&e)).expect("insert"), 1);
        assert_eq!(inbox.insert(std::slice::from_ref(&e)).expect("again"), 0);
        let back = inbox.list(&ListFilter::default()).expect("list");
        assert_eq!(back, vec![e]);
    }

    #[test]
    fn list_filters() {
        let (_dir, inbox) = open_tmp();
        seed(&inbox);
        let list = |f: ListFilter| inbox.list(&f).expect("list");

        assert_eq!(
            ids(&list(ListFilter::default())),
            ["w3", "w2", "w1", "m2", "m1"]
        );
        let wa = list(ListFilter {
            platform: Some(Platform::WhatsApp),
            ..ListFilter::default()
        });
        assert_eq!(ids(&wa), ["w3", "w2", "w1"]);
        let msgs = list(ListFilter {
            kinds: vec![EventKind::Message],
            ..ListFilter::default()
        });
        assert_eq!(ids(&msgs), ["w3", "w1", "m1"]);
        let multi = list(ListFilter {
            kinds: vec![EventKind::Read, EventKind::Status],
            ..ListFilter::default()
        });
        assert_eq!(ids(&multi), ["w2", "m2"]);
        let since = list(ListFilter {
            since_ts: Some(300),
            ..ListFilter::default()
        });
        assert_eq!(ids(&since), ["w3", "w2", "w1"]);
        let received = list(ListFilter {
            since_received: Some(401),
            ..ListFilter::default()
        });
        assert_eq!(ids(&received), ["w3", "w2"]);
        let account = list(ListFilter {
            account_id: Some("phone1".into()),
            ..ListFilter::default()
        });
        assert_eq!(ids(&account), ["w3", "w1"]);
        let limited = list(ListFilter {
            limit: Some(2),
            ..ListFilter::default()
        });
        assert_eq!(ids(&limited), ["w3", "w2"]);
        let combined = list(ListFilter {
            platform: Some(Platform::WhatsApp),
            kinds: vec![EventKind::Message],
            since_ts: Some(400),
            ..ListFilter::default()
        });
        assert_eq!(ids(&combined), ["w3"]);
        assert!(
            list(ListFilter {
                platform: Some(Platform::Instagram),
                ..ListFilter::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn mark_processed() {
        let (_dir, inbox) = open_tmp();
        seed(&inbox);
        let unprocessed = ListFilter {
            unprocessed_only: true,
            ..ListFilter::default()
        };
        assert_eq!(inbox.list(&unprocessed).expect("list").len(), 5);
        let n = inbox
            .mark_processed(&["m1".into(), "w2".into(), "missing".into()])
            .expect("mark");
        assert_eq!(n, 2);
        assert_eq!(
            ids(&inbox.list(&unprocessed).expect("list")),
            ["w3", "w1", "m2"]
        );
        assert_eq!(inbox.mark_processed(&["m1".into()]).expect("again"), 0);
        assert_eq!(inbox.mark_processed(&[]).expect("empty"), 0);
        let all = inbox.list(&ListFilter::default()).expect("list");
        assert!(all.iter().find(|e| e.id == "m1").expect("m1").processed);
        let many: Vec<String> = (0..1000).map(|i| format!("none{i}")).collect();
        assert_eq!(inbox.mark_processed(&many).expect("many"), 0);
    }

    #[test]
    fn stats_count_by_platform() {
        let (_dir, inbox) = open_tmp();
        let empty = inbox.stats().expect("stats");
        assert_eq!(
            (empty.total, empty.unprocessed, empty.last_received_at),
            (0, 0, None)
        );
        seed(&inbox);
        inbox.mark_processed(&["m1".into()]).expect("mark");
        let s = inbox.stats().expect("stats");
        assert_eq!((s.total, s.unprocessed), (5, 4));
        assert_eq!(s.last_received_at, Some(501));
        assert_eq!(
            s.by_platform,
            vec![("messenger".to_string(), 2), ("whatsapp".to_string(), 3)]
        );
        assert_eq!(s.to_json()["by_platform"]["whatsapp"], json!(3));
    }

    fn from_sender(id: &str, sender: &str, ts: i64, raw: Value) -> Event {
        Event {
            ts,
            account_id: "phone1".into(),
            sender: sender.into(),
            raw,
            received_at: ts + 1,
            ..Event::new(id, Platform::WhatsApp, EventKind::Message)
        }
    }

    #[test]
    fn list_from_matches_the_sender_and_the_recorded_user_ids() {
        let (_dir, inbox) = open_tmp();
        inbox
            .insert(&[
                from_sender("a", "16505551234", 100, json!({})),
                from_sender(
                    "b",
                    "447911123456",
                    200,
                    json!({"_sender_user_id": "US.55"}),
                ),
                from_sender(
                    "c",
                    "33612345678",
                    300,
                    json!({"from_parent_user_id": "US.ENT.9"}),
                ),
                from_sender("d", "US.77", 400, json!({"_sender_user_id": "US.77"})),
                from_sender("e", "49301234567", 500, json!({"_sender_user_id": 5})),
            ])
            .expect("insert");
        let from = |senders: &[&str]| -> Vec<String> {
            let senders: Vec<String> = senders.iter().map(|s| (*s).to_string()).collect();
            let found = inbox
                .list_from(&ListFilter::default(), &senders)
                .expect("list_from");
            found.into_iter().map(|e| e.id).collect()
        };
        assert_eq!(from(&["16505551234"]), ["a"]);
        assert_eq!(from(&["US.55"]), ["b"], "business-scoped id kept in raw");
        assert_eq!(from(&["US.ENT.9"]), ["c"], "parent id kept in raw");
        assert_eq!(from(&["US.77"]), ["d"]);
        assert_eq!(from(&["16505551234", "US.55", "US.ENT.9"]), ["c", "b", "a"]);
        assert_eq!(from(&["nobody"]), Vec::<String>::new());
        assert_eq!(
            from(&["1650555123"]),
            Vec::<String>::new(),
            "whole ids only"
        );
        assert_eq!(from(&["x') OR 1=1 --"]), Vec::<String>::new());
        assert_eq!(
            from(&[]),
            ["e", "d", "c", "b", "a"],
            "no senders means anyone"
        );
        let one = ListFilter {
            limit: Some(1),
            ..ListFilter::default()
        };
        let senders = ["US.55".to_string(), "16505551234".to_string()];
        let newest = inbox.list_from(&one, &senders).expect("limit");
        assert_eq!(
            ids(&newest),
            ["b"],
            "the limit applies after the sender match"
        );
        let other_account = ListFilter {
            account_id: Some("phone2".into()),
            ..ListFilter::default()
        };
        assert!(
            inbox
                .list_from(&other_account, &senders)
                .expect("account")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn list_from_async_finds_a_sender_behind_many_newer_events() {
        let (_dir, inbox) = open_tmp();
        let mut events = vec![from_sender("target", "16505551234", 10, json!({}))];
        events.extend((0..300).map(|i| {
            from_sender(
                &format!("other{i}"),
                &format!("4479{i:07}"),
                1000 + i,
                json!({}),
            )
        }));
        inbox.insert_async(events).await.expect("insert");
        let one = ListFilter {
            limit: Some(1),
            ..ListFilter::default()
        };
        let found = inbox
            .list_from_async(one, vec!["16505551234".into()])
            .await
            .expect("list");
        assert_eq!(ids(&found), ["target"]);
    }

    #[tokio::test]
    async fn wait_returns_early() {
        let (_dir, inbox) = open_tmp();
        inbox
            .insert(&[ev("old", Platform::Messenger, EventKind::Message, 1, "p")])
            .expect("seed");
        let writer = inbox.clone();
        let started = Instant::now();
        let insert = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            writer
                .insert_async(vec![ev(
                    "new",
                    Platform::Messenger,
                    EventKind::Message,
                    2,
                    "p",
                )])
                .await
                .expect("insert");
        });
        let found = inbox
            .wait(
                &ListFilter::default(),
                Duration::from_secs(10),
                Duration::from_millis(25),
            )
            .await;
        insert.await.expect("join");
        assert_eq!(ids(&found), ["new"]);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn wait_times_out_empty_and_ignores_old_events() {
        let (_dir, inbox) = open_tmp();
        seed(&inbox);
        let started = Instant::now();
        let found = inbox
            .wait(
                &ListFilter::default(),
                Duration::from_millis(200),
                Duration::from_millis(25),
            )
            .await;
        assert!(found.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(190));
    }

    #[tokio::test]
    async fn wait_respects_the_filter() {
        let (_dir, inbox) = open_tmp();
        let writer = inbox.clone();
        let insert = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let other = ev("ignored", Platform::Messenger, EventKind::Message, 1, "p");
            let wanted = ev("wanted", Platform::WhatsApp, EventKind::Message, 2, "p");
            writer
                .insert_async(vec![other, wanted])
                .await
                .expect("insert");
        });
        let filter = ListFilter {
            platform: Some(Platform::WhatsApp),
            ..ListFilter::default()
        };
        let found = inbox
            .wait(&filter, Duration::from_secs(10), Duration::from_millis(25))
            .await;
        insert.await.expect("join");
        assert_eq!(ids(&found), ["wanted"]);
    }

    #[cfg(unix)]
    #[test]
    fn new_database_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, inbox) = open_tmp();
        seed(&inbox);
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        let sub = dir.path().join("sub");
        assert_eq!(mode(&sub), 0o700);
        assert_eq!(mode(&sub.join("inbox.db")), 0o600);
    }
}
