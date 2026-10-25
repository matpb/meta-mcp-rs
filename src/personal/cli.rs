//! `meta-mcp personal ...`: login flows, status, logout, restart and systemd install, all talking
//! to the daemon over its socket.

mod cookies;
mod install;
mod login;
mod qr;

use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use super::PersonalPlatform;
use super::client::{CODE_CONNECTION_LOST, CODE_NOT_RUNNING, ClientError, DaemonClient};

pub use install::{install, uninstall};
pub use login::{WhatsAppLogin, login_messenger, login_whatsapp};

const STATUS_TIMEOUT: Duration = Duration::from_secs(15);
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(130);
const RESTART_TIMEOUT: Duration = Duration::from_secs(90);
const MIN_TOKEN_RUN: usize = 20;
const MIN_SECRET_LEN: usize = 8;
const MAX_PRINTED_CHARS: usize = 300;

/// Display name of a platform.
pub fn pretty(p: PersonalPlatform) -> &'static str {
    match p {
        PersonalPlatform::Messenger => "Messenger",
        PersonalPlatform::WhatsApp => "WhatsApp",
    }
}

/// Control characters become `?` so remote text cannot drive the terminal.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A message safe to print: known secrets and runs of 20 or more token characters become
/// `[redacted]`, control characters `?`, cut to 300 characters.
pub fn scrub(message: &str, secrets: &[String]) -> String {
    let mut ordered: Vec<&String> = secrets
        .iter()
        .filter(|s| s.len() >= MIN_SECRET_LEN)
        .collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut text = message.to_owned();
    for secret in ordered {
        text = text.replace(secret.as_str(), "[redacted]");
    }
    let mut out = String::new();
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String| {
        if run.chars().count() >= MIN_TOKEN_RUN {
            out.push_str("[redacted]");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            run.push(c);
        } else {
            flush(&mut out, &mut run);
            out.push(if c.is_control() { '?' } else { c });
        }
    }
    flush(&mut out, &mut run);
    out.chars().take(MAX_PRINTED_CHARS).collect()
}

/// A daemon failure as one line for the terminal.
pub fn describe(e: &ClientError, secrets: &[String]) -> String {
    if e.code == CODE_NOT_RUNNING || e.code == CODE_CONNECTION_LOST {
        return e.message.clone();
    }
    format!("{}: {}", clean(&e.code), scrub(&e.message, secrets))
}

pub(crate) async fn connect() -> Result<DaemonClient, ExitCode> {
    DaemonClient::connect_default().await.map_err(|e| {
        eprintln!("{}", e.message);
        ExitCode::FAILURE
    })
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0).map_or_else(
        || secs.to_string(),
        |d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// `Test User (100001)`, from an `account` object.
pub(crate) fn account_line(account: &Value) -> String {
    let name = text(account, "name").unwrap_or("unknown name");
    match text(account, "id") {
        Some(id) => format!("{} ({})", clean(name), clean(id)),
        None => clean(name),
    }
}

/// One platform block of the status text.
fn platform_block(key: &str, p: &Value) -> String {
    let name =
        PersonalPlatform::parse(key).map_or_else(|| key.to_owned(), |p| pretty(p).to_owned());
    let mut out = format!("{name}\n");
    let logged_in = p["logged_in"].as_bool().unwrap_or(false);
    let account = p.get("account").filter(|a| a.is_object());
    out.push_str(&format!(
        "  logged in: {}{}\n",
        yes_no(logged_in),
        account.map_or(String::new(), |a| format!(" as {}", account_line(a)))
    ));
    let reason = text(p, "reason").map_or(String::new(), |r| format!(" ({})", clean(r)));
    out.push_str(&format!(
        "  connection: {}{reason}\n",
        clean(text(p, "connection").unwrap_or("unknown"))
    ));
    if let Some(url) = text(p, "reason_url") {
        out.push_str(&format!(
            "  resolve it in a browser, then run meta-mcp personal restart {key}: {}\n",
            clean(url)
        ));
    }
    if p["sending_blocked"].as_bool().unwrap_or(false) {
        out.push_str(&format!(
            "  sending: BLOCKED ({}), run meta-mcp personal restart {key}\n",
            clean(text(p, "blocked_reason").unwrap_or("unknown reason"))
        ));
    } else {
        out.push_str("  sending: ok\n");
    }
    let last = p["last_event_at"]
        .as_i64()
        .map_or("never".to_owned(), rfc3339);
    out.push_str(&format!("  last event: {last}\n"));
    if p["sidecar_running"].as_bool().unwrap_or(false) {
        let version = text(p, "sidecar_version").unwrap_or("unknown");
        let lib = p
            .get("library")
            .filter(|l| l.is_object())
            .map_or(String::new(), |l| {
                format!(
                    ", library {} {}",
                    clean(text(l, "name").unwrap_or("?")),
                    clean(text(l, "version").unwrap_or("?"))
                )
            });
        out.push_str(&format!(
            "  sidecar: running, version {}{lib}\n",
            clean(version)
        ));
    } else {
        out.push_str("  sidecar: not running\n");
    }
    if p["reconnect_halted"].as_bool().unwrap_or(false) {
        out.push_str(&format!(
            "  reconnect: stopped (the account needs attention), run meta-mcp personal restart {key}\n"
        ));
    } else if let Some(s) = p["reconnect_in_s"].as_i64() {
        out.push_str(&format!("  reconnect: in {s}s\n"));
    }
    if let Some(e) = text(p, "last_error") {
        out.push_str(&format!("  last error: {}\n", scrub(e, &[])));
    }
    out
}

/// The status result as text, one block per platform.
pub fn status_text(status: &Value) -> String {
    let d = &status["daemon"];
    let mut out = format!(
        "Daemon: meta-mcp {}, pid {}, read-only {}, files dir {}\n\n",
        clean(text(d, "version").unwrap_or("?")),
        d["pid"].as_u64().map_or("?".to_owned(), |p| p.to_string()),
        yes_no(d["read_only"].as_bool().unwrap_or(false)),
        if d["files_dir_configured"].as_bool().unwrap_or(false) {
            "configured"
        } else {
            "not configured"
        },
    );
    for p in PersonalPlatform::ALL {
        let entry = &status["platforms"][p.as_str()];
        if entry.is_object() {
            out.push_str(&platform_block(p.as_str(), entry));
            out.push('\n');
        }
    }
    out
}

pub async fn status(as_json: bool) -> ExitCode {
    let client = match connect().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    match client.request("status", Value::Null, STATUS_TIMEOUT).await {
        Ok(v) if as_json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
            );
            ExitCode::SUCCESS
        }
        Ok(v) => {
            print!("{}", status_text(&v));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            ExitCode::FAILURE
        }
    }
}

/// The logout report as text and whether everything worked (state cleared, data wiped, remote ok).
fn logout_report(platform: PersonalPlatform, v: &Value) -> (String, bool) {
    let cleared = v["state_cleared"].as_bool().unwrap_or(false);
    let wiped = v["data_wiped"].as_bool().unwrap_or(false);
    let remote = text(v, "remote_error");
    let ok = cleared && wiped && remote.is_none();
    let name = pretty(platform);
    let mut out = if ok {
        format!("Logged out of {name}.\n")
    } else {
        format!("Logout of {name} is incomplete:\n")
    };
    out.push_str(&format!(
        "  stored session: {}\n",
        if cleared {
            "cleared"
        } else {
            "NOT cleared (delete personal/<platform>/state.json under the meta-mcp home by hand)"
        }
    ));
    out.push_str(&format!(
        "  local data (databases, thread cache): {}\n",
        if wiped {
            "wiped"
        } else {
            "NOT fully wiped (empty personal/<platform>/data under the meta-mcp home by hand)"
        }
    ));
    match remote {
        None => out.push_str("  service: the device was unlinked\n"),
        Some(e) => out.push_str(&format!(
            "  service: could not be told to forget this device ({}): remove it from the account's linked devices or active sessions by hand\n",
            scrub(e, &[])
        )),
    }
    (out, ok)
}

pub async fn logout(platform: PersonalPlatform) -> ExitCode {
    let client = match connect().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    let params = json!({"platform": platform.as_str()});
    match client.request("logout", params, LOGOUT_TIMEOUT).await {
        Ok(v) => {
            let (report, ok) = logout_report(platform, &v);
            print!("{report}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            ExitCode::FAILURE
        }
    }
}

pub async fn restart(platform: PersonalPlatform) -> ExitCode {
    let client = match connect().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    let params = json!({"platform": platform.as_str()});
    match client.request("restart", params, RESTART_TIMEOUT).await {
        Ok(v) => {
            println!("Restarted the {} sidecar.", pretty(platform));
            print!("{}", platform_block(platform.as_str(), &v));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_secrets_and_long_tokens() {
        let secrets = vec!["short-secret".to_owned()];
        let msg = format!("bad short-secret and {} done\u{1b}[2J", "x".repeat(25));
        let out = scrub(&msg, &secrets);
        assert!(
            !out.contains("short-secret") && !out.contains("xxxxx"),
            "{out}"
        );
        assert!(
            out.contains("[redacted]") && out.contains("done?[2J"),
            "{out}"
        );
        assert_eq!(scrub(&"é ".repeat(400), &[]).chars().count(), 300);
    }

    #[test]
    fn logout_report_says_exactly_what_happened() {
        let p = PersonalPlatform::WhatsApp;
        let (all, ok) = logout_report(p, &json!({"state_cleared": true, "data_wiped": true}));
        assert!(ok && all.starts_with("Logged out of WhatsApp.\n"), "{all}");
        assert!(
            all.contains("stored session: cleared")
                && all.contains("data (databases, thread cache): wiped")
        );
        let (bad, ok) = logout_report(
            p,
            &json!({"state_cleared": false, "data_wiped": false, "remote_error": "sidecar_unavailable: gone"}),
        );
        assert!(
            !ok && bad.starts_with("Logout of WhatsApp is incomplete:"),
            "{bad}"
        );
        for want in [
            "NOT cleared",
            "NOT fully wiped",
            "sidecar_unavailable: gone",
            "by hand",
        ] {
            assert!(bad.contains(want), "missing {want:?} in {bad}");
        }
        assert!(
            !logout_report(p, &json!({"state_cleared": true})).1,
            "a missing field is a failure"
        );
    }

    #[test]
    fn status_text_covers_every_field() {
        let status = json!({
            "daemon": {"version": "0.1.0", "pid": 42, "read_only": false, "files_dir_configured": true},
            "platforms": {
                "messenger": {"logged_in": true, "account": {"id": "1", "name": "Ann"},
                    "connection": "connected", "reason": null, "sending_blocked": false,
                    "blocked_reason": null, "last_event_at": 1_700_000_000, "sidecar_running": true,
                    "sidecar_version": "0.2.0", "library": {"name": "lib", "version": "v1"},
                    "reconnect_in_s": null, "last_error": null},
                "whatsapp": {"logged_in": false, "account": null, "connection": "fatal",
                    "reason": "temporary_ban", "sending_blocked": true,
                    "blocked_reason": "temporary_ban", "last_event_at": null,
                    "sidecar_running": false, "sidecar_version": null, "library": null,
                    "reconnect_in_s": 10, "last_error": "boom"}
            }
        });
        let t = status_text(&status);
        for want in [
            "Daemon: meta-mcp 0.1.0, pid 42, read-only no, files dir configured",
            "Messenger\n  logged in: yes as Ann (1)",
            "connection: connected\n",
            "sending: ok",
            "last event: 2023-11-14T22:13:20Z",
            "sidecar: running, version 0.2.0, library lib v1",
            "WhatsApp\n  logged in: no\n",
            "connection: fatal (temporary_ban)",
            "sending: BLOCKED (temporary_ban), run meta-mcp personal restart whatsapp",
            "last event: never",
            "sidecar: not running",
            "reconnect: in 10s",
            "last error: boom",
        ] {
            assert!(t.contains(want), "missing {want:?} in:\n{t}");
        }
    }
}
