//! Shared harness: stdio MCP sessions, a temp `META_MCP_HOME`, and wiremock helpers.
#![allow(dead_code)]

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::MockServer;

pub const BIN: &str = env!("CARGO_BIN_EXE_meta-mcp");
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// A temp directory used as `META_MCP_HOME`, with the inbox database inside it.
pub struct TempHome {
    dir: tempfile::TempDir,
}

impl TempHome {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn inbox_db(&self) -> PathBuf {
        self.dir.path().join("inbox.db")
    }

    /// Writes a `credentials.json` fixture with private permissions.
    pub fn write_credentials(&self, creds: &Value) {
        let path = self.dir.path().join("credentials.json");
        std::fs::write(&path, serde_json::to_vec_pretty(creds).expect("json")).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
    }
}

/// A command for the binary with a cleared environment and test-safe defaults.
pub fn command(home: &TempHome, graph_base: &str, envs: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("META_MCP_HOME", home.path())
        .env("META_MCP_INBOX_DB", home.inbox_db())
        .env("META_GRAPH_BASE_URL", graph_base);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

/// A running `meta-mcp stdio` process speaking JSON-RPC lines.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Session {
    pub fn start(home: &TempHome, graph_base: &str, envs: &[(&str, &str)]) -> Self {
        let mut cmd = command(home, graph_base, envs);
        cmd.arg("stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn meta-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Self {
            child,
            stdin,
            lines,
            next_id: 1,
        };
        let init = s.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(
            init["result"]["serverInfo"]["name"],
            json!("meta-mcp"),
            "{init}"
        );
        s.notify("notifications/initialized");
        s
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(REPLY_TIMEOUT)
                .unwrap_or_else(|e| panic!("no reply to {method} within {REPLY_TIMEOUT:?}: {e}"));
            let v: Value = serde_json::from_str(&line).expect("json line");
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// Sorted tool names from `tools/list`.
    pub fn tool_names(&mut self) -> Vec<String> {
        let resp = self.request("tools/list", json!({}));
        let mut names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools array in {resp}"))
            .iter()
            .map(|t| t["name"].as_str().expect("name").to_string())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text of the first content block of a `tools/call` response.
pub fn tool_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("text content in {resp}"))
        .to_string()
}

/// The first content block parsed as JSON.
pub fn tool_json(resp: &Value) -> Value {
    serde_json::from_str(&tool_text(resp)).unwrap_or_else(|e| panic!("json text ({e}) in {resp}"))
}

static USED_PORTS: Mutex<Vec<u16>> = Mutex::new(Vec::new());

/// A loopback port that no other call in this test process has been given.
pub fn free_port() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let mut used = USED_PORTS.lock().expect("port registry");
        if !used.contains(&port) {
            used.push(port);
            return port;
        }
    }
}

pub async fn mock_graph() -> MockServer {
    MockServer::start().await
}

/// Runs blocking test code (a `Session`) off the async runtime so wiremock keeps serving.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("join")
}

/// The Graph base URL to give the server under test for a mock at `uri`: the same mock under the
/// name `localhost`, so its `127.0.0.1` media URLs are on another host than the Graph API.
pub fn graph_base(uri: &str) -> String {
    uri.replacen("127.0.0.1", "localhost", 1)
}

/// A second mock server on another loopback address, so its host differs from `127.0.0.1` and
/// `localhost`. Gives the server and its host as written in URLs; `None` where none can be bound.
pub async fn mock_on_other_loopback() -> Option<(MockServer, String)> {
    for host in ["127.0.0.2", "[::1]"] {
        let bare = host.trim_matches(['[', ']']);
        if let Ok(listener) = TcpListener::bind((bare, 0)) {
            let server = MockServer::builder().listener(listener).start().await;
            return Some((server, host.to_string()));
        }
    }
    None
}

// Real 2x2 pictures written by Pillow.
const PNG_B64: &str = concat!(
    "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR42mM8ISfHwMDAxMDAwMDAAAANBAEIf62CUQAAAABJ",
    "RU5ErkJggg=="
);
const JPEG_B64: &str = concat!(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDABALDA4MChAODQ4SERATGCgaGBYWGDEjJR0oOjM9PDkzODdASFxOQERXRTc4UG1R",
    "V19iZ2hnPk1xeXBkeFxlZ2P/2wBDARESEhgVGC8aGi9jQjhCY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2Nj",
    "Y2NjY2NjY2NjY2NjY2P/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAA",
    "AgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6",
    "Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXG",
    "x8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREA",
    "AgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5",
    "OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPE",
    "xcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDHooorhPqD/9k="
);
const JPEG_PROGRESSIVE_B64: &str = concat!(
    "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDABALDA4MChAODQ4SERATGCgaGBYWGDEjJR0oOjM9PDkzODdASFxOQERXRTc4UG1R",
    "V19iZ2hnPk1xeXBkeFxlZ2P/2wBDARESEhgVGC8aGi9jQjhCY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2Nj",
    "Y2NjY2NjY2NjY2NjY2P/wgARCAACAAIDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAT/xAAVAQEBAAAAAAAAAAAA",
    "AAAAAAADBf/aAAwDAQACEAMQAAABjAVP/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAA",
    "AAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAA",
    "AP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABAD/8QAFBEBAAAAAAAA",
    "AAAAAAAAAAAAAP/aAAgBAwEBPxB//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPxB//8QAFBABAAAAAAAAAAAAAAAA",
    "AAAAAP/aAAgBAQABPxB//9k="
);
const GIF_B64: &str = "R0lGODdhAgACAIEAAMgeHgAAAAAAAAAAACwAAAAAAgACAAAIBgABCAQQEAA7";
const WEBP_LOSSY_B64: &str =
    "UklGRjYAAABXRUJQVlA4ICoAAACQAQCdASoCAAIAAsBMJaACdLoAA5gA/u2QP4hd7G2//PTP62/j/xkAAAA=";
const WEBP_LOSSLESS_B64: &str = "UklGRh4AAABXRUJQVlA4TBEAAAAvAUAAAAdQjyLXo/+BiOh/AAA=";
const WEBP_ALPHA_B64: &str = concat!(
    "UklGRlYAAABXRUJQVlA4WAoAAAAQAAAAAQAAAQAAQUxQSAUAAAAAgICAgABWUDggKgAAAJABAJ0BKgIAAgACwEwloAJ0ugAD",
    "mAD+7ZA/iF3sbb/89M/rb+P/GQAAAA=="
);

fn decode(b64: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("image fixture")
}

pub fn png_image() -> Vec<u8> {
    decode(PNG_B64)
}

pub fn jpeg_image() -> Vec<u8> {
    decode(JPEG_B64)
}

pub fn gif_image() -> Vec<u8> {
    decode(GIF_B64)
}

pub fn webp_image() -> Vec<u8> {
    decode(WEBP_LOSSY_B64)
}

/// Bodies that start like a picture and are not one, as found by the review: (label, bytes).
pub fn polyglot_bodies() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "png_html",
            [
                b"\x89PNG\r\n\x1a\n".as_slice(),
                b"<html><script>alert(1)</script>",
            ]
            .concat(),
        ),
        (
            "jpeg_text",
            [b"\xFF\xD8\xFF".as_slice(), b"not really a jpeg, just text"].concat(),
        ),
        (
            "gif_php",
            [b"GIF89a".as_slice(), b"<?php system($_GET[0]); ?>"].concat(),
        ),
        (
            "webp_text",
            [b"RIFF\0\0\0\0WEBP".as_slice(), b"garbage garbage"].concat(),
        ),
    ]
}
