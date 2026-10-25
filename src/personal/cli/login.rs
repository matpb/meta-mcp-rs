//! `personal login messenger` and `personal login whatsapp`.

use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::time::Instant;

use super::cookies::{CookieError, parse_cookies};
use super::{account_line, clean, connect, describe, qr, scrub};
use crate::personal::PersonalPlatform;
use crate::personal::client::DaemonClient;
use crate::personal::protocol::Event;

const START_TIMEOUT: Duration = Duration::from_secs(70);
const FINISH_TIMEOUT: Duration = Duration::from_secs(130);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
const WHATSAPP_BUDGET: Duration = Duration::from_secs(300);
const MAX_INPUT_BYTES: usize = 256 * 1024;
const EXIT_USAGE: u8 = 2;
#[cfg(unix)]
type Interrupt = tokio::signal::unix::Signal;
#[cfg(windows)]
type Interrupt = tokio::signal::windows::CtrlC;

/// Listens for Ctrl-C: SIGINT on Unix, the console event on Windows.
#[cfg(unix)]
fn interrupt() -> io::Result<Interrupt> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
}

#[cfg(windows)]
fn interrupt() -> io::Result<Interrupt> {
    tokio::signal::windows::ctrl_c()
}

const NO_QR_SHOWN: &str = "no QR code was shown: the sidecar could not reach WhatsApp; check the network and run the command again";

/// Options of `personal login whatsapp`.
#[derive(Debug, Clone)]
pub struct WhatsAppLogin {
    pub pair_code: Option<String>,
    pub full_history: bool,
    pub device_name: String,
}

/// Runs a blocking read on a detached thread, so Ctrl-C and exit never wait for it.
async fn blocking_input<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await
        .unwrap_or_else(|_| Err(io::Error::other("the input thread stopped")))
}

/// One line from stdin: hidden on a terminal when `hidden`, plain when piped.
fn read_line_now(hidden: bool) -> io::Result<String> {
    if hidden && io::stdin().is_terminal() {
        let line = rpassword::read_password()?;
        eprintln!();
        return Ok(line);
    }
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "input closed before an answer arrived",
        ));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// The pasted cookies: hidden lines up to one not ending in a backslash on a terminal, else every
/// line up to EOF or a blank line.
fn read_paste_now() -> io::Result<String> {
    let tty = io::stdin().is_terminal();
    let mut out = String::new();
    loop {
        let line = if tty {
            read_line_now(true)?
        } else {
            let mut l = String::new();
            if io::stdin().lock().read_line(&mut l)? == 0 {
                break;
            }
            l.trim_end_matches(['\r', '\n']).to_owned()
        };
        if !tty && line.trim().is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        let continued = line.trim_end().ends_with('\\');
        out.push_str(&line);
        if (tty && !continued) || out.len() > MAX_INPUT_BYTES {
            break;
        }
        out.push('\n');
    }
    Ok(out)
}

fn prompt(text: &str) {
    use std::io::Write;
    let mut err = io::stderr().lock();
    let _ = write!(err, "{text}");
    let _ = err.flush();
}

async fn cancel(client: &DaemonClient, platform: PersonalPlatform) {
    let _ = client
        .request(
            "login_cancel",
            json!({"platform": platform.as_str()}),
            CANCEL_TIMEOUT,
        )
        .await;
}

fn cookie_failure(e: &CookieError) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(EXIT_USAGE)
}

pub async fn login_messenger(cookies_file: Option<PathBuf>) -> ExitCode {
    let preset = match cookies_file {
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                return ExitCode::from(EXIT_USAGE);
            }
        },
        None => None,
    };
    let preset = match preset.map(|t| parse_cookies(&t)).transpose() {
        Ok(p) => p,
        Err(e) => return cookie_failure(&e),
    };
    let client = match connect().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    let _events = match client.subscribe().await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            return ExitCode::FAILURE;
        }
    };
    let platform = PersonalPlatform::Messenger;
    let started = client
        .request(
            "login_start",
            json!({"platform": platform.as_str(), "method": "cookies"}),
            START_TIMEOUT,
        )
        .await;
    let started = match started {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            return ExitCode::FAILURE;
        }
    };
    if let Some(text) = started.get("instructions").and_then(Value::as_str) {
        println!("{}", clean(text));
    }
    let parsed = match preset {
        Some(p) => p,
        None => {
            println!(
                "Paste the cURL command or the cookies JSON, then press Enter (nothing is shown; a terminal cuts a pasted line at 4096 characters, so for a longer paste use --cookies-file):"
            );
            let input = match blocking_input(read_paste_now).await {
                Ok(s) => s,
                Err(e) => {
                    cancel(&client, platform).await;
                    eprintln!("error: could not read the pasted input: {e}");
                    return ExitCode::from(EXIT_USAGE);
                }
            };
            match parse_cookies(&input) {
                Ok(p) => p,
                Err(e) => {
                    cancel(&client, platform).await;
                    return cookie_failure(&e);
                }
            }
        }
    };
    let secrets: Vec<String> = parsed.cookies.values().cloned().collect();
    let mut params = json!({"platform": platform.as_str(), "cookies": parsed.cookies});
    if let Some(ua) = &parsed.user_agent {
        params["user_agent"] = json!(ua);
    }
    match client.request("login_finish", params, FINISH_TIMEOUT).await {
        Ok(v) => {
            match v.get("account").filter(|a| a.is_object()) {
                Some(a) => println!("Logged in to Messenger as {}", account_line(a)),
                None => println!("Logged in to Messenger."),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Login failed: {}", describe(&e, &secrets));
            ExitCode::FAILURE
        }
    }
}

/// Digits only, 6 to 15 of them; accepts `+`, spaces, dashes and brackets as typed.
fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .filter(|c| !matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
        .collect();
    let ok = (6..=15).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit());
    ok.then_some(digits)
}

struct Flow<'a> {
    client: &'a DaemonClient,
    out_tty: bool,
    qr_count: u32,
    qr_lines: usize,
}

pub async fn login_whatsapp(opts: WhatsAppLogin) -> ExitCode {
    let platform = PersonalPlatform::WhatsApp;
    let phone = match opts.pair_code.as_deref().map(normalize_phone) {
        Some(Some(p)) => Some(p),
        Some(None) => {
            eprintln!(
                "error: --pair-code needs the phone number in international form, digits only"
            );
            return ExitCode::from(EXIT_USAGE);
        }
        None => None,
    };
    let name = opts.device_name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        eprintln!("error: --device-name must be 1 to 64 printable characters");
        return ExitCode::from(EXIT_USAGE);
    }
    let client = match connect().await {
        Ok(c) => c,
        Err(code) => return code,
    };
    let mut events = match client.subscribe().await {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            return ExitCode::FAILURE;
        }
    };
    let mut sigint = match interrupt() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot listen for Ctrl-C: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut params = json!({
        "platform": platform.as_str(),
        "method": if phone.is_some() { "pair_code" } else { "qr" },
        "full_history": opts.full_history,
        "device_name": name,
    });
    if let Some(p) = &phone {
        params["phone"] = json!(p);
    }
    let started = match client.request("login_start", params, START_TIMEOUT).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {}", describe(&e, &[]));
            return ExitCode::FAILURE;
        }
    };
    if let Some(code) = started.get("code").and_then(Value::as_str) {
        println!("Pairing code: {}", clean(code));
        println!("WhatsApp on the phone: Linked devices, Link with phone number");
    }
    let mut flow = Flow {
        client: &client,
        out_tty: io::stdout().is_terminal(),
        qr_count: 0,
        qr_lines: 0,
    };
    let deadline = Instant::now() + WHATSAPP_BUDGET;
    loop {
        let next = tokio::select! {
            biased;
            _ = sigint.recv() => {
                cancel(&client, platform).await;
                eprintln!("\nCancelled.");
                return ExitCode::FAILURE;
            }
            r = tokio::time::timeout_at(deadline, events.recv()) => r,
        };
        let ev = match next {
            Err(_) => {
                cancel(&client, platform).await;
                eprintln!("Timed out after 5 minutes: run the command again.");
                return ExitCode::FAILURE;
            }
            Ok(None) => {
                eprintln!("Lost the connection to the daemon before the link finished.");
                return ExitCode::FAILURE;
            }
            Ok(Some(ev)) => ev,
        };
        let for_us = ev
            .data
            .get("platform")
            .and_then(Value::as_str)
            .is_none_or(|p| p == platform.as_str());
        if !for_us {
            continue;
        }
        if let Some(code) = flow.handle(ev, &mut sigint).await {
            return code;
        }
    }
}

impl Flow<'_> {
    /// Acts on one event; `Some(code)` ends the command.
    async fn handle(&mut self, ev: Event, sigint: &mut Interrupt) -> Option<ExitCode> {
        let platform = PersonalPlatform::WhatsApp;
        match ev.event.as_str() {
            "qr" => {
                self.show_qr(&ev.data);
                None
            }
            "passkey_request" => self.passkey(&ev.data, sigint).await,
            "passkey_confirmation" => {
                let code = ev.data.get("code").and_then(Value::as_str).unwrap_or("");
                if code.is_empty() {
                    println!("Confirm the link on your phone.");
                } else {
                    println!(
                        "Compare this code with the one on your phone: {}",
                        clean(code)
                    );
                }
                prompt("Confirm? [y/N] ");
                let answer = tokio::select! {
                    r = blocking_input(|| read_line_now(false)) => r.unwrap_or_default(),
                    _ = sigint.recv() => return Some(self.abort().await),
                };
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    cancel(self.client, platform).await;
                    eprintln!("Cancelled: the link was not confirmed.");
                    return Some(ExitCode::FAILURE);
                }
                let params = json!({"platform": platform.as_str(), "confirm": true});
                match self
                    .client
                    .request("login_finish", params, FINISH_TIMEOUT)
                    .await
                {
                    Ok(_) => None,
                    Err(e) => {
                        eprintln!("Login failed: {}", describe(&e, &[]));
                        Some(ExitCode::FAILURE)
                    }
                }
            }
            "logged_in" => {
                match ev.data.get("account").filter(|a| a.is_object()) {
                    Some(a) => println!("Logged in to WhatsApp as {}", account_line(a)),
                    None => println!("Logged in to WhatsApp."),
                }
                Some(ExitCode::SUCCESS)
            }
            "connection" => {
                let state = ev.data.get("state").and_then(Value::as_str);
                let reason = ev.data.get("reason").and_then(Value::as_str);
                match (state, reason) {
                    (Some("disconnected"), Some("qr_timeout")) if self.qr_count == 0 => {
                        eprintln!("{NO_QR_SHOWN}");
                        Some(ExitCode::FAILURE)
                    }
                    (Some("disconnected"), Some("qr_timeout")) => {
                        eprintln!("The QR codes expired: run the command again.");
                        Some(ExitCode::FAILURE)
                    }
                    (Some("fatal"), r) => {
                        eprintln!(
                            "The connection failed: {}",
                            clean(r.unwrap_or("unknown reason"))
                        );
                        if self.qr_count == 0 {
                            eprintln!("{NO_QR_SHOWN}");
                        }
                        Some(ExitCode::FAILURE)
                    }
                    _ => None,
                }
            }
            "logged_out" => {
                let reason = ev.data.get("reason").and_then(Value::as_str);
                eprintln!("Logged out: {}", clean(reason.unwrap_or("unknown reason")));
                Some(ExitCode::FAILURE)
            }
            "error" => {
                let code = ev
                    .data
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let msg = ev.data.get("message").and_then(Value::as_str).unwrap_or("");
                eprintln!("error: {}: {}", clean(code), scrub(msg, &[]));
                None
            }
            _ => None,
        }
    }

    async fn abort(&self) -> ExitCode {
        cancel(self.client, PersonalPlatform::WhatsApp).await;
        eprintln!("\nCancelled.");
        ExitCode::FAILURE
    }

    fn show_qr(&mut self, data: &Value) {
        let Some(code) = data.get("code").and_then(Value::as_str) else {
            return;
        };
        let art = match qr::render(code) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("error: cannot draw this QR code: {e}");
                return;
            }
        };
        self.qr_count += 1;
        let valid = data
            .get("expires_in_s")
            .and_then(Value::as_i64)
            .map_or(String::new(), |s| format!(", valid {s}s"));
        if self.out_tty && self.qr_lines > 0 {
            print!("\x1b[{}A\x1b[J", self.qr_lines);
        }
        println!(
            "Scan with WhatsApp: Settings, Linked devices, Link a device (code {}{valid})",
            self.qr_count
        );
        println!("{art}");
        self.qr_lines = 1 + art.lines().count();
    }

    async fn passkey(&self, data: &Value, sigint: &mut Interrupt) -> Option<ExitCode> {
        let platform = PersonalPlatform::WhatsApp;
        if let Some(text) = data.get("instructions").and_then(Value::as_str) {
            println!("{}", clean(text));
        }
        if let Some(pk) = data.get("public_key") {
            println!(
                "{}",
                serde_json::to_string_pretty(pk).unwrap_or_else(|_| pk.to_string())
            );
        }
        prompt("Paste the assertion JSON (nothing is shown), then press Enter: ");
        let raw = tokio::select! {
            r = blocking_input(|| read_line_now(true)) => r,
            _ = sigint.recv() => return Some(self.abort().await),
        };
        let raw = match raw {
            Ok(r) => r,
            Err(e) => {
                cancel(self.client, platform).await;
                eprintln!("error: could not read the assertion: {e}");
                return Some(ExitCode::from(EXIT_USAGE));
            }
        };
        let assertion = match serde_json::from_str::<Map<String, Value>>(raw.trim()) {
            Ok(m) => m,
            Err(_) => {
                cancel(self.client, platform).await;
                eprintln!("error: the assertion is not a JSON object");
                return Some(ExitCode::from(EXIT_USAGE));
            }
        };
        let params = json!({"platform": platform.as_str(), "passkey_response": assertion});
        match self
            .client
            .request("login_finish", params, FINISH_TIMEOUT)
            .await
        {
            Ok(_) => None,
            Err(e) => {
                cancel(self.client, platform).await;
                eprintln!("Login failed: {}", describe(&e, &[raw.trim().to_owned()]));
                Some(ExitCode::FAILURE)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_numbers_are_normalised_to_digits() {
        assert_eq!(
            normalize_phone("+1 (555) 010-1234").as_deref(),
            Some("15550101234")
        );
        assert_eq!(
            normalize_phone("33612345678").as_deref(),
            Some("33612345678")
        );
        for bad in ["", "12", "abc123456", "+1234567890123456", "555-CALL-NOW"] {
            assert_eq!(normalize_phone(bad), None, "{bad}");
        }
    }
}
