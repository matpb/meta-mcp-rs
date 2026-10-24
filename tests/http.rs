//! The binary over streamable HTTP: API key gate, Host guard, health and the optional webhook.

mod common;

use std::io::Read;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::{TempHome, command, free_port};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const APP_SECRET: &str = "http-app-secret";
const VERIFY_TOKEN: &str = "http-verify-token";

struct Server {
    child: Child,
    port: u16,
    home: TempHome,
}

impl Server {
    fn start(args: &[&str], envs: &[(&str, &str)]) -> Self {
        let home = TempHome::new();
        let port = free_port();
        let mut cmd = command(&home, "http://127.0.0.1:9", envs);
        cmd.arg("http")
            .args(args)
            .env("META_MCP_PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn");
        let mut server = Self { child, port, home };
        wait_for_port(&mut server.child, server.port);
        server
    }

    fn with_key(args: &[&str], envs: &[(&str, &str)]) -> Self {
        let mut all = vec![("META_MCP_API_KEYS", KEY)];
        all.extend_from_slice(envs);
        Self::start(args, &all)
    }

    fn url(&self, p: &str) -> String {
        format!("http://127.0.0.1:{}{p}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// True once `GET /health` on the port answers with HTTP 200.
fn health_answers(port: u16) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200")
        || String::from_utf8_lossy(&buf).starts_with("HTTP/1.0 200")
}

/// Waits until the server answers `/health`, not only until the port accepts connections.
fn wait_for_port(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("wait") {
            panic!("the server exited early with {status}");
        }
        if health_answers(port) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not start serving on {port}");
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("client")
}

fn rpc(method: &str, id: u64, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn init_body() -> Value {
    rpc(
        "initialize",
        1,
        json!({"protocolVersion": "2025-06-18", "capabilities": {},
               "clientInfo": {"name": "test", "version": "0"}}),
    )
}

async fn post_mcp(
    s: &Server,
    body: &Value,
    bearer: Option<&str>,
    host: Option<&str>,
    origin: Option<&str>,
) -> reqwest::Response {
    let mut req = client()
        .post(s.url("/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .json(body);
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(h) = host {
        req = req.header("Host", h);
    }
    if let Some(o) = origin {
        req = req.header("Origin", o);
    }
    req.send().await.expect("send")
}

fn sign(body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(APP_SECRET.as_bytes()).expect("key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn exit_stderr(mut child: Child) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().expect("wait") {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the process kept running");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut err = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .expect("read");
    (status.code(), err)
}

#[tokio::test]
async fn rejects_without_key() {
    let s = Server::with_key(&[], &[]);
    let resp = post_mcp(&s, &init_body(), None, None, None).await;
    assert_eq!(resp.status(), 401);
    assert_eq!(resp.headers()["www-authenticate"], "Bearer");
    let wrong = post_mcp(
        &s,
        &init_body(),
        Some("wrong-key-wrong-key-wrong-key-wrong"),
        None,
        None,
    )
    .await;
    assert_eq!(wrong.status(), 401);
    let short = post_mcp(&s, &init_body(), Some(&KEY[..31]), None, None).await;
    assert_eq!(short.status(), 401);
}

#[tokio::test]
async fn accepts_with_key() {
    let s = Server::with_key(&[], &[]);
    let resp = post_mcp(&s, &init_body(), Some(KEY), None, None).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["result"]["serverInfo"]["name"],
        json!("meta-mcp"),
        "{body}"
    );

    let via_header = client()
        .post(s.url("/mcp"))
        .header("Accept", "application/json, text/event-stream")
        .header("X-API-Key", KEY)
        .json(&init_body())
        .send()
        .await
        .expect("send");
    assert_eq!(via_header.status(), 200);

    let listed = post_mcp(&s, &rpc("tools/list", 2, json!({})), Some(KEY), None, None).await;
    assert_eq!(listed.status(), 200);
    let body: Value = listed.json().await.expect("json");
    let mut names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools in {body}"))
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    names.sort_unstable();
    assert_eq!(names, meta_mcp::mcp::ALL_TOOL_NAMES);
}

#[tokio::test]
async fn host_guard_rejects_foreign_host() {
    let s = Server::with_key(&[], &[]);
    let resp = post_mcp(&s, &init_body(), Some(KEY), Some("evil.example"), None).await;
    let status = resp.status().as_u16();
    assert!(status == 403 || status == 421, "unexpected {status}");
    assert_ne!(status, 200);
    let origin = post_mcp(
        &s,
        &init_body(),
        Some(KEY),
        None,
        Some("https://evil.example"),
    )
    .await;
    assert_eq!(origin.status(), 403);
    let local = format!("http://127.0.0.1:{}", s.port);
    let ok = post_mcp(&s, &init_body(), Some(KEY), None, Some(&local)).await;
    assert_eq!(ok.status(), 200);
    let health = client()
        .get(s.url("/health"))
        .header("Host", "evil.example")
        .send()
        .await
        .expect("send");
    assert_eq!(health.status(), 403, "the guard covers /health too");
}

#[tokio::test]
async fn health_open() {
    let s = Server::with_key(&[], &[]);
    let resp = client().get(s.url("/health")).send().await.expect("send");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], json!(true));
}

#[tokio::test]
async fn with_webhook_mounts_verify_route() {
    let s = Server::with_key(
        &["--with-webhook"],
        &[
            ("META_APP_SECRET", APP_SECRET),
            ("META_WEBHOOK_VERIFY_TOKEN", VERIFY_TOKEN),
        ],
    );
    let url = s.url(&format!(
        "/webhook?hub.mode=subscribe&hub.verify_token={VERIFY_TOKEN}&hub.challenge=42"
    ));
    let resp = client().get(&url).send().await.expect("send");
    assert_eq!(resp.status(), 200, "the webhook route needs no API key");
    assert_eq!(resp.text().await.expect("body"), "42");

    let bad = s.url("/webhook?hub.mode=subscribe&hub.verify_token=nope&hub.challenge=42");
    assert_eq!(client().get(&bad).send().await.expect("send").status(), 403);
    assert_eq!(
        client()
            .get(s.url("/health"))
            .send()
            .await
            .expect("send")
            .status(),
        200
    );
    assert_eq!(
        post_mcp(&s, &init_body(), None, None, None).await.status(),
        401,
        "/mcp keeps its gate"
    );
}

#[tokio::test]
async fn webhook_events_reach_the_mcp_inbox_over_http() {
    let s = Server::with_key(
        &["--with-webhook"],
        &[
            ("META_APP_SECRET", APP_SECRET),
            ("META_WEBHOOK_VERIFY_TOKEN", VERIFY_TOKEN),
        ],
    );
    let payload = serde_json::to_vec(&json!({"object": "whatsapp_business_account", "entry": [{"id": "W", "changes": [{
        "field": "messages", "value": {"messaging_product": "whatsapp",
            "metadata": {"phone_number_id": "PN1"},
            "messages": [{"from": "5511", "id": "wamid.HTTP1", "timestamp": "1750000000", "type": "text", "text": {"body": "via http"}}]}
    }]}]}))
    .expect("json");
    let unsigned = client()
        .post(s.url("/webhook"))
        .body(payload.clone())
        .send()
        .await
        .expect("send");
    assert_eq!(unsigned.status(), 401);
    let signed = client()
        .post(s.url("/webhook"))
        .header("X-Hub-Signature-256", sign(&payload))
        .body(payload)
        .send()
        .await
        .expect("send");
    assert_eq!(signed.status(), 200);

    let call = rpc(
        "tools/call",
        3,
        json!({"name": "inbox_list", "arguments": {"platform": "whatsapp"}}),
    );
    let resp = post_mcp(&s, &call, Some(KEY), None, None).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{body}"));
    let listed: Value = serde_json::from_str(text).expect("tool json");
    assert_eq!(listed["count"], json!(1), "{listed}");
    assert_eq!(listed["events"][0]["text"], json!("via http"));
    assert!(s.home.inbox_db().exists());
}

#[tokio::test]
async fn webhook_route_is_absent_without_the_flag() {
    let s = Server::with_key(
        &[],
        &[
            ("META_APP_SECRET", APP_SECRET),
            ("META_WEBHOOK_VERIFY_TOKEN", VERIFY_TOKEN),
        ],
    );
    let resp = client()
        .get(s.url(&format!(
            "/webhook?hub.mode=subscribe&hub.verify_token={VERIFY_TOKEN}&hub.challenge=1"
        )))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn public_webhook_host_passes_the_host_guard() {
    let env = [
        ("META_APP_SECRET", APP_SECRET),
        ("META_WEBHOOK_VERIFY_TOKEN", VERIFY_TOKEN),
        (
            "META_WEBHOOK_PUBLIC_URL",
            "https://hooks.example.org/webhook",
        ),
    ];
    let with_url = Server::with_key(&["--with-webhook"], &env);
    let verify =
        format!("/webhook?hub.mode=subscribe&hub.verify_token={VERIFY_TOKEN}&hub.challenge=7");
    let resp = client()
        .get(with_url.url(&verify))
        .header("Host", "hooks.example.org")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "7");

    let without_url = Server::with_key(&["--with-webhook"], &env[..2]);
    let resp = client()
        .get(without_url.url(&verify))
        .header("Host", "hooks.example.org")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 403);
}

#[test]
fn refuses_to_start_without_or_with_weak_keys() {
    let home = TempHome::new();
    let spawn = |envs: &[(&str, &str)]| {
        command(&home, "http://127.0.0.1:9", envs)
            .arg("http")
            .env("META_MCP_PORT", free_port().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let (code, err) = exit_stderr(spawn(&[]));
    assert_eq!(code, Some(1));
    assert!(err.contains("META_MCP_API_KEYS"), "{err}");
    let (code, err) = exit_stderr(spawn(&[("META_MCP_API_KEYS", "too-short")]));
    assert_eq!(code, Some(1));
    assert!(err.contains("32") && !err.contains("too-short"), "{err}");
}

#[test]
fn with_webhook_needs_its_secrets() {
    let home = TempHome::new();
    let child = command(&home, "http://127.0.0.1:9", &[("META_MCP_API_KEYS", KEY)])
        .args(["http", "--with-webhook"])
        .env("META_MCP_PORT", free_port().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let (code, err) = exit_stderr(child);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("META_APP_SECRET") && err.contains("META_WEBHOOK_VERIFY_TOKEN"),
        "{err}"
    );
}

#[tokio::test]
async fn standalone_webhook_process_serves_verify_and_health() {
    let home = TempHome::new();
    let port = free_port();
    let listen = format!("127.0.0.1:{port}");
    let mut guard = KillOnDrop(
        command(
            &home,
            "http://127.0.0.1:9",
            &[
                ("META_APP_SECRET", APP_SECRET),
                ("META_WEBHOOK_VERIFY_TOKEN", VERIFY_TOKEN),
                ("META_MCP_WEBHOOK_LISTEN", &listen),
            ],
        )
        .arg("webhook")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn"),
    );
    wait_for_port(&mut guard.0, port);
    let base = format!("http://{listen}");
    let verify = client()
        .get(format!(
            "{base}/webhook?hub.mode=subscribe&hub.verify_token={VERIFY_TOKEN}&hub.challenge=99"
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(verify.text().await.expect("body"), "99");
    let health: Value = client()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(
        (health["ok"].clone(), health["events"].clone()),
        (json!(true), json!(0))
    );
    let payload = br#"{"object":"page","entry":[{"id":"P","messaging":[{"sender":{"id":"U"},"recipient":{"id":"P"},"timestamp":1700000000000,"message":{"mid":"m_x","text":"hi"}}]}]}"#;
    let stored = client()
        .post(format!("{base}/webhook"))
        .header("X-Hub-Signature-256", sign(payload))
        .body(payload.to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(stored.status(), 200);
    let health: Value = client()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(health["events"], json!(1));
}
