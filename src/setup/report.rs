//! Status lines, secret scrubbing and expiry wording shared by setup, doctor and the token commands.

use std::io::Write;
use std::sync::Mutex;

use chrono::DateTime;
use serde::Serialize;

use crate::config::secret::{Secret, redact};

const MIN_GUARDED_LEN: usize = 8;
static GUARDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Outcome of one line of output; the emoji is the first thing on the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
}

impl Status {
    pub fn emoji(self) -> &'static str {
        match self {
            Self::Ok => "✅",
            Self::Warn => "⚠️",
            Self::Fail => "❌",
            Self::Info => "ℹ️",
        }
    }
}

/// Registers a secret so `scrub` removes it from everything printed; short values are ignored.
pub fn guard(secret: &Secret) {
    guard_str(secret.expose());
}

pub fn guard_str(value: &str) {
    if value.len() < MIN_GUARDED_LEN {
        return;
    }
    if let Ok(mut list) = GUARDED.lock()
        && !list.iter().any(|s| s == value)
    {
        list.push(value.to_string());
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

/// Token-shaped runs and every registered secret replaced, for anything about to be printed.
pub fn scrub(text: &str) -> String {
    let mut out = redact(text);
    if let Ok(list) = GUARDED.lock() {
        for secret in list.iter() {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "***");
            }
        }
    }
    out
}

pub fn status_line(status: Status, text: &str) -> String {
    format!("{} {text}", status.emoji())
}

pub fn fix_line(text: &str) -> String {
    format!("   Fix: {text}")
}

/// Prints one scrubbed line to stdout and flushes; a closed pipe is ignored.
pub fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", scrub(text));
    let _ = out.flush();
}

pub fn line(status: Status, text: &str) {
    say(&status_line(status, text));
}

pub fn fix(text: &str) {
    say(&fix_line(text));
}

pub fn step(number: usize, title: &str) {
    say("");
    say(&format!("Step {number}: {title}"));
}

pub fn plural(n: i64, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// `YYYY-MM-DD` in UTC.
pub fn day(ts: i64) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ts.to_string())
}

/// Whole days between `now` and `expires_at`; negative once expired.
pub fn days_left(expires_at: i64, now: i64) -> i64 {
    (expires_at - now).div_euclid(86_400)
}

/// Human wording for an expiry; `None` means the token never expires.
pub fn expiry_text(expires_at: Option<i64>, now: i64) -> String {
    match expires_at {
        None => "never expires".to_string(),
        Some(t) if t <= now => format!("expired on {}", day(t)),
        Some(t) => {
            let days = days_left(t, now);
            if days >= 1 {
                format!("expires in {} ({})", plural(days, "day"), day(t))
            } else if t - now < 3600 {
                "expires within the hour".to_string()
            } else {
                format!("expires in {}", plural((t - now) / 3600, "hour"))
            }
        }
    }
}

/// Expiry wording for a user token, where a missing expiry means "not recorded", never "never".
pub fn user_expiry_text(expires_at: Option<i64>, now: i64) -> String {
    match expires_at {
        None => "expiry not recorded".to_string(),
        some => expiry_text(some, now),
    }
}

/// Octal permission bits of a file, for printing; `None` when unreadable or not unix.
#[cfg(unix)]
pub fn file_mode(path: &std::path::Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub fn file_mode(_path: &std::path::Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_start_with_the_emoji() {
        assert_eq!(status_line(Status::Ok, "fine"), "✅ fine");
        assert_eq!(status_line(Status::Warn, "hmm"), "⚠️ hmm");
        assert_eq!(status_line(Status::Fail, "bad"), "❌ bad");
        assert_eq!(status_line(Status::Info, "fyi"), "ℹ️ fyi");
        assert_eq!(fix_line("run it"), "   Fix: run it");
    }

    #[test]
    fn scrub_removes_tokens_and_guarded_secrets_only_when_long_enough() {
        guard_str("short");
        guard_str("a-long-guarded-secret-value");
        let text = "token EAAB1234567890123456789012345 and a-long-guarded-secret-value, short";
        let clean = scrub(text);
        assert_eq!(clean, "token EAA*** and ***, short");
    }

    #[test]
    fn expiry_wording() {
        let now = 1_800_000_000;
        assert_eq!(expiry_text(None, now), "never expires");
        assert!(expiry_text(Some(now - 10), now).starts_with("expired on "));
        assert!(expiry_text(Some(now + 3 * 86_400 + 5), now).starts_with("expires in 3 days ("));
        assert!(expiry_text(Some(now + 86_400 + 5), now).starts_with("expires in 1 day ("));
        assert_eq!(expiry_text(Some(now + 7_300), now), "expires in 2 hours");
        assert_eq!(expiry_text(Some(now + 600), now), "expires within the hour");
        assert_eq!(user_expiry_text(None, now), "expiry not recorded");
        assert_eq!(
            user_expiry_text(Some(now + 600), now),
            "expires within the hour"
        );
        assert_eq!(days_left(now + 10 * 86_400, now), 10);
        assert_eq!(days_left(now - 1, now), -1);
        assert_eq!(day(0), "1970-01-01");
    }
}
