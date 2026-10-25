//! Append-only JSONL log of every write attempt: ids, hashes and counts, never the text.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::PersonalPlatform;
use super::secure::{private_open, set_private_file};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Media,
    Reaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Write-ahead line, appended before the sidecar is asked to send.
    Attempt,
    Sent,
    Failed,
    /// The send timed out or the link broke and the message may have gone out.
    Unknown,
    Invalid,
    Refused,
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: u64,
    pub ts: String,
    pub platform: String,
    pub thread_id: Option<String>,
    pub kind: Kind,
    pub body_sha256: String,
    pub body_len: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_sha256: Option<String>,
    pub outcome: Outcome,
    /// On a completion line, the id of its write-ahead `attempt` line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// An entry before the log assigns its id and timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEntry {
    pub platform: PersonalPlatform,
    pub thread_id: Option<String>,
    pub kind: Kind,
    pub body_sha256: String,
    pub body_len: usize,
    pub file_sha256: Option<String>,
    pub outcome: Outcome,
    pub attempt_id: Option<u64>,
    pub message_id: Option<String>,
    pub error_code: Option<String>,
}

/// Lower-case hex SHA-256 of the UTF-8 text and its length in Unicode scalar values.
pub fn fingerprint(text: &str) -> (String, usize) {
    (
        hex::encode(Sha256::digest(text.as_bytes())),
        text.chars().count(),
    )
}

impl NewEntry {
    /// An entry with the empty body hashed; the text itself is never kept.
    pub fn new(platform: PersonalPlatform, kind: Kind, outcome: Outcome) -> Self {
        let (body_sha256, body_len) = fingerprint("");
        Self {
            platform,
            thread_id: None,
            kind,
            body_sha256,
            body_len,
            file_sha256: None,
            outcome,
            attempt_id: None,
            message_id: None,
            error_code: None,
        }
    }

    pub fn thread(mut self, thread_id: Option<&str>) -> Self {
        self.thread_id = thread_id.map(str::to_owned);
        self
    }

    /// Records the hash and scalar count of `text` and drops the text.
    pub fn body(mut self, text: &str) -> Self {
        (self.body_sha256, self.body_len) = fingerprint(text);
        self
    }

    pub fn file(mut self, sha256: Option<String>) -> Self {
        self.file_sha256 = sha256;
        self
    }

    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }

    pub fn attempt(mut self, attempt_id: u64) -> Self {
        self.attempt_id = Some(attempt_id);
        self
    }

    pub fn message(mut self, message_id: Option<String>) -> Self {
        self.message_id = message_id;
        self
    }

    pub fn error(mut self, code: &str) -> Self {
        self.error_code = Some(code.to_owned());
        self
    }
}

struct Inner {
    file: File,
    next_id: u64,
}

pub struct AuditLog {
    path: PathBuf,
    inner: Mutex<Inner>,
}

fn parse_lines(data: &[u8]) -> Vec<AuditEntry> {
    data.split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice::<AuditEntry>(l).ok())
        .collect()
}

#[cfg(unix)]
fn read_byte_at(file: &File, buf: &mut [u8; 1], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

/// Reads one byte at `offset` without moving the append position.
#[cfg(windows)]
fn read_byte_at(file: &File, buf: &mut [u8; 1], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    loop {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl AuditLog {
    /// Opens or creates the log with mode 0600, reads the highest id and ends a torn last line
    /// so the next entry starts on its own line.
    pub fn open(path: &Path) -> io::Result<AuditLog> {
        let mut opts = OpenOptions::new();
        let mut file = private_open(&mut opts)
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        set_private_file(&file, path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let max_id = parse_lines(&data).iter().map(|e| e.id).max().unwrap_or(0);
        if data.last().is_some_and(|b| *b != b'\n') {
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        Ok(AuditLog {
            path: path.to_owned(),
            inner: Mutex::new(Inner {
                file,
                next_id: max_id + 1,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Appends `entry` and fsyncs it; blocking.
    pub fn append(&self, entry: NewEntry) -> io::Result<AuditEntry> {
        let mut inner = self.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        let stored = AuditEntry {
            id,
            ts: now_rfc3339(),
            platform: entry.platform.as_str().to_owned(),
            thread_id: entry.thread_id,
            kind: entry.kind,
            body_sha256: entry.body_sha256,
            body_len: entry.body_len,
            file_sha256: entry.file_sha256,
            outcome: entry.outcome,
            attempt_id: entry.attempt_id,
            message_id: entry.message_id,
            error_code: entry.error_code,
        };
        let mut line = serde_json::to_vec(&stored).map_err(io::Error::other)?;
        line.push(b'\n');
        // A torn last line gets its newline in the same write, so entries never merge.
        let len = inner.file.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            read_byte_at(&inner.file, &mut last, len - 1)?;
            if last[0] != b'\n' {
                line.insert(0, b'\n');
            }
        }
        inner.file.write_all(&line)?;
        inner.file.sync_all()?;
        Ok(stored)
    }

    /// The newest `limit` entries, newest first; a line that does not parse is skipped.
    pub fn tail(&self, limit: usize) -> io::Result<Vec<AuditEntry>> {
        let _inner = self.lock();
        let data = std::fs::read(&self.path)?;
        let mut entries = parse_lines(&data);
        entries.reverse();
        entries.truncate(limit);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(outcome: Outcome) -> NewEntry {
        NewEntry::new(PersonalPlatform::Messenger, Kind::Text, outcome)
            .thread(Some("t1"))
            .body("hello")
    }

    #[test]
    fn lines_have_the_documented_shape_and_never_the_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path).expect("open");
        let attempt = log.append(entry(Outcome::Attempt)).expect("attempt");
        let done = log
            .append(
                entry(Outcome::Sent)
                    .attempt(attempt.id)
                    .message(Some("mid.1".into())),
            )
            .expect("sent");
        assert_eq!((attempt.id, done.id), (1, 2));
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("hello"));
        let lines: Vec<serde_json::Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).expect("json"))
            .collect();
        assert_eq!(lines[0]["platform"], "messenger");
        assert_eq!(lines[0]["kind"], "text");
        assert_eq!(lines[0]["outcome"], "attempt");
        assert_eq!(lines[0]["body_len"], 5);
        assert_eq!(lines[0]["thread_id"], "t1");
        assert_eq!(
            lines[0]["body_sha256"],
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(lines[0].get("attempt_id").is_none());
        assert_eq!(lines[1]["attempt_id"], 1);
        assert_eq!(lines[1]["message_id"], "mid.1");
        let ts = lines[0]["ts"].as_str().expect("ts");
        assert!(ts.ends_with('Z') && ts.len() == 20, "{ts}");
    }

    #[test]
    fn a_missing_thread_id_is_null_and_the_empty_body_is_hashed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = AuditLog::open(&dir.path().join("a.jsonl")).expect("open");
        let e = log
            .append(NewEntry::new(
                PersonalPlatform::WhatsApp,
                Kind::Reaction,
                Outcome::Invalid,
            ))
            .expect("append");
        let v = serde_json::to_value(&e).expect("json");
        assert!(v["thread_id"].is_null());
        assert_eq!(
            v["body_sha256"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(v["body_len"], 0);
    }

    #[test]
    fn body_length_counts_scalar_values() {
        assert_eq!(fingerprint("h\u{e9}\u{1F600}").1, 3);
    }

    #[test]
    fn ids_stay_monotonic_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(&path).expect("open");
            for _ in 0..3 {
                log.append(entry(Outcome::Attempt)).expect("append");
            }
        }
        let log = AuditLog::open(&path).expect("reopen");
        assert_eq!(log.append(entry(Outcome::Sent)).expect("append").id, 4);
        let ids: Vec<u64> = log.tail(10).expect("tail").iter().map(|e| e.id).collect();
        assert_eq!(ids, [4, 3, 2, 1]);
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_reterminated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        {
            let log = AuditLog::open(&path).expect("open");
            log.append(entry(Outcome::Attempt)).expect("append");
            log.append(entry(Outcome::Sent)).expect("append");
        }
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open raw");
        f.write_all(br#"{"id":3,"ts":"2026-01-01T00:00:0"#)
            .expect("torn");
        drop(f);
        let log = AuditLog::open(&path).expect("reopen");
        let e = log.append(entry(Outcome::Failed)).expect("append");
        assert_eq!(e.id, 3);
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(raw.ends_with('\n'));
        assert_eq!(raw.lines().count(), 4);
        let ids: Vec<u64> = log.tail(10).expect("tail").iter().map(|e| e.id).collect();
        assert_eq!(ids, [3, 2, 1]);
    }

    #[test]
    fn append_after_a_torn_write_keeps_entries_on_their_own_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path).expect("open");
        log.append(entry(Outcome::Attempt)).expect("append");
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open raw");
        f.write_all(b"{\"id\":9").expect("torn");
        drop(f);
        log.append(entry(Outcome::Sent)).expect("append");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert_eq!(raw.lines().count(), 3);
        assert_eq!(log.tail(10).expect("tail").len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_even_when_it_existed_loose() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        std::fs::write(&path, b"").expect("create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        AuditLog::open(&path).expect("open");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
