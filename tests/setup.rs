//! `meta-mcp setup`, `doctor` and `token` run as real processes against wiremock and loopback.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::{TempHome, blocking, command, free_port, mock_graph};
use meta_mcp::config::Secret;
use meta_mcp::inbox::Inbox;
use meta_mcp::webhook::{WebhookState, router};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const APP_SECRET: &str = "app-secret-s3cr3t-value";
const PAGE_TOKEN: &str = "EAApagetoken0123456789abcdefghijklmnop";
const USER_TOKEN: &str = "EAAusertoken0123456789abcdefghijklmnop";
const ALL_MESSENGER: [&str; 5] = [
    "pages_show_list",
    "pages_manage_metadata",
    "pages_messaging",
    "pages_read_engagement",
    "business_management",
];
const WAIT: Duration = Duration::from_secs(40);

fn now() -> i64 {
    meta_mcp::inbox::now_secs()
}

async fn get_json(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn post_json(server: &MockServer, p: &str, body: Value) {
    Mock::given(method("POST"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

fn debug_body(kind: &str, scopes: &[&str], expires_at: i64) -> Value {
    json!({"data": {"app_id": "1", "type": kind, "application": "Test App", "is_valid": true,
        "scopes": scopes, "expires_at": expires_at, "issued_at": now() - 100, "user_id": "7"}})
}

async fn app_mock(server: &MockServer) {
    get_json(
        server,
        "/v26.0/1",
        json!({"id": "1", "name": "Test App", "link": "https://www.facebook.com/games/?app_id=1"}),
    )
    .await;
}

async fn debug_mock(server: &MockServer, kind: &str, scopes: &[&str], expires_at: i64) {
    get_json(
        server,
        "/v26.0/debug_token",
        debug_body(kind, scopes, expires_at),
    )
    .await;
}

async fn page_mocks(server: &MockServer, id: &str, name: &str) {
    get_json(
        server,
        &format!("/v26.0/{id}"),
        json!({"id": id, "name": name}),
    )
    .await;
    get_json(
        server,
        &format!("/v26.0/{id}/conversations"),
        json!({"data": []}),
    )
    .await;
    get_json(
        server,
        &format!("/v26.0/{id}/subscribed_apps"),
        json!({"data": []}),
    )
    .await;
}

fn creds_json(home: &TempHome) -> Value {
    let raw = std::fs::read_to_string(home.path().join("credentials.json")).expect("credentials");
    serde_json::from_str(&raw).expect("credentials json")
}

fn creds_text(home: &TempHome) -> String {
    std::fs::read_to_string(home.path().join("credentials.json")).expect("credentials")
}

#[cfg(unix)]
fn mode_of(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).expect("meta").permissions().mode() & 0o777
}

async fn run(mut cmd: Command) -> Output {
    cmd.stdin(Stdio::null());
    blocking(move || cmd.output().expect("run meta-mcp")).await
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn cmd_with(home: &TempHome, server: &MockServer, args: &[&str]) -> Command {
    let mut cmd = command(home, &server.uri(), &[]);
    cmd.args(args);
    cmd
}

fn requests_to<'a>(all: &'a [wiremock::Request], p: &str) -> Vec<&'a wiremock::Request> {
    all.iter().filter(|r| r.url.path() == p).collect()
}

fn query_of(req: &wiremock::Request) -> Vec<(String, String)> {
    req.url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn form_of(req: &wiremock::Request) -> Vec<(String, String)> {
    url::form_urlencoded::parse(&req.body)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn pair<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A running wizard with its stdout streamed line by line and stderr collected.
struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    err: Receiver<String>,
    seen: Vec<String>,
}

impl Proc {
    fn spawn(mut cmd: Command, piped_stdin: bool) -> Self {
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = cmd.spawn().expect("spawn meta-mcp");
        let stdout = child.stdout.take().expect("stdout");
        let mut stderr = child.stderr.take().expect("stderr");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let (etx, err) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            let _ = etx.send(text);
        });
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            lines,
            err,
            seen: Vec::new(),
        }
    }

    /// Blocks until a stdout line contains `needle` and returns it.
    fn read_until(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|e| panic!("no line with `{needle}` ({e}); saw {:#?}", self.seen));
            self.seen.push(line.clone());
            if line.contains(needle) {
                return line;
            }
        }
    }

    fn write_stdin(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("piped stdin");
        stdin.write_all(text.as_bytes()).expect("write stdin");
        stdin.flush().expect("flush stdin");
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for exit, then returns (exit code, all stdout, stderr).
    fn finish(mut self) -> (i32, String, String) {
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(s) = self.child.try_wait().expect("wait") {
                break s;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("meta-mcp did not exit; saw {:#?}", self.seen);
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(500)) {
            self.seen.push(line);
        }
        let stderr = self.err.recv_timeout(WAIT).unwrap_or_default();
        (status.code().unwrap_or(-1), self.seen.join("\n"), stderr)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn dialog_params(line: &str) -> Vec<(String, String)> {
    let start = line.find("https://").expect("a URL in the line");
    let url = Url::parse(line[start..].trim()).expect("dialog URL");
    assert!(url.path().ends_with("/dialog/oauth"), "{url}");
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn mock_login_chain(server: &MockServer, long_token: &str, expect_exchange: Option<u64>) {
    mock_login_chain_scoped(server, long_token, expect_exchange, &ALL_MESSENGER).await;
}

async fn mock_login_chain_scoped(
    server: &MockServer,
    long_token: &str,
    expect_exchange: Option<u64>,
    scopes: &[&str],
) {
    let two_pages = json!({"data": [
        {"id": "42", "name": "Page One", "category": "Shop", "tasks": ["MESSAGING", "MODERATE", "MANAGE"],
         "access_token": "EAAPAGEONE0123456789abcdefghijklmnop"},
        {"id": "43", "name": "Page Two", "category": "Blog", "tasks": ["MESSAGING", "MODERATE"],
         "access_token": "EAAPAGETWO0123456789abcdefghijklmnop"}
    ]});
    mock_login_chain_with(server, long_token, expect_exchange, scopes, two_pages).await;
    page_mocks(server, "42", "Page One").await;
    page_mocks(server, "43", "Page Two").await;
}

async fn mock_login_chain_with(
    server: &MockServer,
    long_token: &str,
    expect_exchange: Option<u64>,
    scopes: &[&str],
    accounts: Value,
) {
    app_mock(server).await;
    let short = Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("code", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": "SHORTTOKEN0123456789abcdefghijkl", "token_type": "bearer", "expires_in": 3600}),
        ));
    let long = Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("grant_type", "fb_exchange_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": long_token, "token_type": "bearer", "expires_in": 5_184_000}),
        ));
    match expect_exchange {
        Some(n) => {
            short.expect(n).mount(server).await;
            long.expect(n).mount(server).await;
        }
        None => {
            short.mount(server).await;
            long.mount(server).await;
        }
    }
    debug_mock(server, "USER", scopes, now() + 5_184_000).await;
    get_json(server, "/v26.0/me/accounts", accounts).await;
}

fn login_args<'a>(port: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "setup",
        "--app-id",
        "1",
        "--app-secret",
        APP_SECRET,
        "--login",
        "--no-browser",
        "--all-pages",
        "--non-interactive",
        "--port",
        port,
    ];
    args.extend_from_slice(extra);
    args
}

#[tokio::test]
async fn setup_page_token_path_writes_credentials_and_doctor_passes() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(
        &server,
        "PAGE",
        &[
            ALL_MESSENGER[1],
            ALL_MESSENGER[2],
            ALL_MESSENGER[3],
            ALL_MESSENGER[0],
        ],
        0,
    )
    .await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page", "category": "Retail"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;

    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("Test Page"), "{text}");
    assert!(text.contains("claude mcp add meta-mcp"), "{text}");
    assert!(text.contains("0 failures"), "{text}");

    let file = home.path().join("credentials.json");
    assert!(file.exists());
    #[cfg(unix)]
    assert_eq!(mode_of(&file), 0o600);
    let creds = creds_json(&home);
    assert_eq!(creds["app_id"], json!("1"));
    assert_eq!(creds["app_secret"], json!(APP_SECRET));
    assert_eq!(creds["pages"][0]["id"], json!("42"));
    assert_eq!(creds["pages"][0]["name"], json!("Test Page"));
    assert_eq!(creds["pages"][0]["token"], json!(PAGE_TOKEN));
    assert!(
        creds["user_token"].is_null(),
        "no user token for a Page token"
    );
    let raw = creds_text(&home);
    assert_eq!(
        raw.matches(APP_SECRET).count(),
        1,
        "the secret only in app_secret"
    );
    assert_eq!(raw.matches(PAGE_TOKEN).count(), 1);
    for name in std::fs::read_dir(home.path()).expect("dir") {
        let name = name
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(!name.ends_with(".tmp"), "leftover {name}");
    }

    let d = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&d), 0, "{}\n{}", out(&d), err(&d));
    assert!(out(&d).contains("Test Page"), "{}", out(&d));
    assert!(out(&d).contains("0 failures"), "{}", out(&d));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_oauth_loopback_full_chain() {
    let server = mock_graph().await;
    let long = "EAALONGTOKEN0123456789abcdefghijklmnop";
    mock_login_chain(&server, long, Some(1)).await;
    let home = TempHome::new();
    let port = free_port().to_string();
    let mut proc = Proc::spawn(cmd_with(&home, &server, &login_args(&port, &[])), false);

    let line = proc.read_until("dialog/oauth");
    let params = dialog_params(&line);
    let state = pair(&params, "state").expect("state").to_string();
    assert_eq!(state.len(), 64);
    assert_eq!(pair(&params, "client_id"), Some("1"));
    assert_eq!(pair(&params, "response_type"), Some("code"));
    let redirect = format!("http://localhost:{port}/callback");
    assert_eq!(pair(&params, "redirect_uri"), Some(redirect.as_str()));
    let scope = pair(&params, "scope").expect("scope");
    for wanted in ALL_MESSENGER {
        assert!(scope.split(',').any(|s| s == wanted), "{wanted} in {scope}");
    }

    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state={state}"
        ))
        .send()
        .await
        .expect("callback");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.text()
            .await
            .expect("body")
            .contains("Signed in. You can close this tab.")
    );

    let (status, stdout, stderr) = proc.finish();
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(!stdout.contains(APP_SECRET) && !stderr.contains(APP_SECRET));
    let creds = creds_json(&home);
    assert_eq!(creds["user_token"]["token"], json!(long));
    let expires = creds["user_token"]["expires_at"]
        .as_i64()
        .expect("expires_at");
    assert!((expires - (now() + 5_184_000)).abs() < 600, "{expires}");
    let pages = creds["pages"].as_array().expect("pages");
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0]["id"], json!("42"));
    assert_eq!(
        pages[0]["token"],
        json!("EAAPAGEONE0123456789abcdefghijklmnop")
    );
    assert_eq!(pages[1]["id"], json!("43"));
    assert_eq!(
        pages[1]["token"],
        json!("EAAPAGETWO0123456789abcdefghijklmnop")
    );

    let all = server.received_requests().await.expect("recording");
    let exchanges = requests_to(&all, "/v26.0/oauth/access_token");
    assert_eq!(exchanges.len(), 2);
    let first = query_of(exchanges[0]);
    assert_eq!(
        pair(&first, "redirect_uri"),
        Some(redirect.as_str()),
        "byte-identical to the dialog"
    );
    assert_eq!(pair(&first, "code"), Some("abc"));
    assert_eq!(pair(&first, "client_secret"), Some(APP_SECRET));
    for req in &exchanges {
        assert!(
            req.headers.get("authorization").is_none(),
            "no Bearer on the exchange"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_oauth_rejects_bad_state() {
    let server = mock_graph().await;
    mock_login_chain(&server, "EALONG0123456789abcdefghijklmnop", Some(0)).await;
    let home = TempHome::new();
    let port = free_port().to_string();
    let mut proc = Proc::spawn(
        cmd_with(
            &home,
            &server,
            &login_args(&port, &["--timeout-seconds", "3"]),
        ),
        false,
    );
    let line = proc.read_until("dialog/oauth");
    let state = pair(&dialog_params(&line), "state")
        .expect("state")
        .to_string();
    let http = reqwest::Client::new();
    let bad = http
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state=not-{state}"
        ))
        .send()
        .await
        .expect("callback");
    assert_eq!(bad.status(), 400);
    let none = http
        .get(format!("http://127.0.0.1:{port}/callback?code=abc"))
        .send()
        .await
        .expect("callback");
    assert_eq!(none.status(), 400);

    let (status, stdout, _) = proc.finish();
    assert_eq!(status, 1, "{stdout}");
    assert!(stdout.contains("no callback received"), "{stdout}");
    assert!(stdout.contains("wrong state"), "{stdout}");
    assert!(stdout.contains("Valid OAuth Redirect URIs"), "{stdout}");
    let all = server.received_requests().await.expect("recording");
    assert!(requests_to(&all, "/v26.0/oauth/access_token").is_empty());
    assert!(!home.path().join("credentials.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_oauth_paste_mode_parses_redirect_url() {
    let server = mock_graph().await;
    let long = "EAALONGTOKEN0123456789abcdefghijklmnop";
    mock_login_chain(&server, long, Some(1)).await;
    let home = TempHome::new();
    let cmd = cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--login",
            "--redirect-mode",
            "paste",
            "--no-browser",
            "--all-pages",
            "--non-interactive",
        ],
    );
    let mut proc = Proc::spawn(cmd, true);
    let line = proc.read_until("dialog/oauth");
    let params = dialog_params(&line);
    let state = pair(&params, "state").expect("state").to_string();
    assert_eq!(
        pair(&params, "redirect_uri"),
        Some("https://localhost:8766/callback")
    );
    proc.write_stdin(&format!(
        "https://localhost:8766/callback?code=abc&state={state}#_=_\n"
    ));
    proc.close_stdin();

    let (status, stdout, stderr) = proc.finish();
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("Paste mode"), "{stdout}");
    let creds = creds_json(&home);
    assert_eq!(creds["user_token"]["token"], json!(long));
    assert_eq!(creds["pages"].as_array().expect("pages").len(), 2);
    let all = server.received_requests().await.expect("recording");
    let first = query_of(requests_to(&all, "/v26.0/oauth/access_token")[0]);
    assert_eq!(
        pair(&first, "redirect_uri"),
        Some("https://localhost:8766/callback")
    );
    assert_eq!(pair(&first, "code"), Some("abc"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_oauth_paste_mode_refuses_a_foreign_state() {
    let server = mock_graph().await;
    mock_login_chain(&server, "EALONG0123456789abcdefghijklmnop", Some(0)).await;
    let home = TempHome::new();
    let mut proc = Proc::spawn(
        cmd_with(
            &home,
            &server,
            &[
                "setup",
                "--app-id",
                "1",
                "--app-secret",
                APP_SECRET,
                "--login",
                "--redirect-mode",
                "paste",
                "--no-browser",
                "--all-pages",
                "--non-interactive",
            ],
        ),
        true,
    );
    proc.read_until("dialog/oauth");
    proc.write_stdin("https://localhost:8766/callback?code=abc&state=somebody-elses-state\n");
    proc.close_stdin();
    let (status, stdout, _) = proc.finish();
    assert_eq!(status, 1, "{stdout}");
    assert!(stdout.contains("state"), "{stdout}");
    let all = server.received_requests().await.expect("recording");
    assert!(requests_to(&all, "/v26.0/oauth/access_token").is_empty());
}

#[tokio::test]
async fn setup_whatsapp_discovery_stores_phone_numbers() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(
        &server,
        "USER",
        &[
            "business_management",
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        now() + 50 * 86_400,
    )
    .await;
    get_json(
        &server,
        "/v26.0/me/businesses",
        json!({"data": [{"id": "B1", "name": "Biz"}]}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/B1/owned_whatsapp_business_accounts",
        json!({"data": [{"id": "W1", "name": "My WABA"}]}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/W1/phone_numbers",
        json!({"data": [
            {"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop", "quality_rating": "GREEN"},
            {"id": "P2", "display_phone_number": "+1 555 0200", "verified_name": "Other"}
        ]}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "quality_rating": "GREEN",
               "code_verification_status": "VERIFIED", "status": "CONNECTED"}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/W1/subscribed_apps",
        json!({"data": [{"whatsapp_business_api_data": {"id": "1", "name": "Test App"}}]}),
    )
    .await;

    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--user-token",
            USER_TOKEN,
            "--whatsapp",
            "--non-interactive",
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
            "--no-messenger",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("+1 555 0100"), "{text}");
    assert!(text.contains("user tokens expire in 60 days"), "{text}");
    assert!(!text.contains("Step 3: Pages"), "no Messenger step: {text}");
    let creds = creds_json(&home);
    let wa = &creds["whatsapp"];
    assert_eq!(wa["phone_numbers"].as_array().expect("numbers").len(), 1);
    assert_eq!(wa["phone_numbers"][0]["id"], json!("P1"));
    assert_eq!(
        wa["phone_numbers"][0]["display_phone_number"],
        json!("+1 555 0100")
    );
    assert_eq!(wa["phone_numbers"][0]["verified_name"], json!("Shop"));
    assert_eq!(wa["token_kind"], json!("user"));
    assert_eq!(wa["waba_id"], json!("W1"));
    assert_eq!(wa["token"], json!(USER_TOKEN));
    assert_eq!(creds["user_token"]["token"], json!(USER_TOKEN));
    assert_eq!(creds["pages"], json!([]));
    let all = server.received_requests().await.expect("recording");
    assert!(
        !requests_to(&all, "/v26.0/me/businesses").is_empty(),
        "discovery ran"
    );
    assert!(
        !requests_to(&all, "/v26.0/W1/phone_numbers").is_empty(),
        "the numbers were listed"
    );
}

async fn start_receiver(verify_token: &str) -> (u16, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    let inbox = Inbox::open(&dir.path().join("inbox.db")).expect("inbox");
    let state = Arc::new(WebhookState::new(
        Secret::new(APP_SECRET),
        Secret::new(verify_token),
        inbox,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.ok();
    });
    (port, dir)
}

const ALL_PAGE_FIELDS: [&str; 12] = [
    "messages",
    "message_echoes",
    "message_reads",
    "message_deliveries",
    "message_reactions",
    "message_edits",
    "messaging_postbacks",
    "messaging_referrals",
    "messaging_optins",
    "messaging_handovers",
    "standby",
    "messaging_policy_enforcement",
];

async fn page_token_mocks(server: &MockServer) {
    app_mock(server).await;
    debug_mock(server, "PAGE", &ALL_MESSENGER, 0).await;
    get_json(
        server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page", "category": "Retail"}),
    )
    .await;
    get_json(
        server,
        "/v26.0/42",
        json!({"id": "42", "name": "Test Page"}),
    )
    .await;
    get_json(server, "/v26.0/42/conversations", json!({"data": []})).await;
}

#[tokio::test]
async fn setup_webhook_subscribes_app_and_page() {
    let (port, _dir) = start_receiver("v").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    get_json(
        &server,
        "/v26.0/1/subscriptions",
        json!({"data": [{"object": "page", "callback_url": url, "active": true,
            "fields": ALL_PAGE_FIELDS.iter().map(|f| json!({"name": f, "version": "v26.0"})).collect::<Vec<_>>()}]}),
    )
    .await;
    post_json(
        &server,
        "/v26.0/42/subscribed_apps",
        json!({"success": true}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/42/subscribed_apps",
        json!({"data": [{"id": "1", "name": "Test App", "subscribed_fields": ALL_PAGE_FIELDS}]}),
    )
    .await;

    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--webhook-url",
            &url,
            "--verify-token",
            "v",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("callback reachable"), "{text}");
    assert!(
        text.contains("message_echoes"),
        "read-back fields are printed: {text}"
    );

    let all = server.received_requests().await.expect("recording");
    let app_post = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/1/subscriptions")
        .expect("app subscription");
    let form = form_of(app_post);
    assert_eq!(pair(&form, "object"), Some("page"));
    assert_eq!(pair(&form, "callback_url"), Some(url.as_str()));
    assert_eq!(pair(&form, "verify_token"), Some("v"));
    assert_eq!(pair(&form, "include_values"), Some("true"));
    let fields = pair(&form, "fields").expect("fields");
    assert!(fields.split(',').any(|f| f == "messages"), "{fields}");
    assert_eq!(
        app_post
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer 1|{APP_SECRET}").as_str())
    );

    let page_post = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/42/subscribed_apps")
        .expect("page subscription");
    let form = form_of(page_post);
    let subscribed = pair(&form, "subscribed_fields").expect("subscribed_fields");
    for f in ALL_PAGE_FIELDS {
        assert!(subscribed.split(',').any(|s| s == f), "{f} in {subscribed}");
    }
    assert!(!subscribed.contains("include_values"));
    assert_eq!(
        page_post
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {PAGE_TOKEN}").as_str())
    );

    let creds = creds_json(&home);
    assert_eq!(creds["webhook"]["public_url"], json!(url));
    assert_eq!(creds["webhook"]["verify_token"], json!("v"));
}

#[tokio::test]
async fn setup_webhook_fails_fast_when_callback_unreachable() {
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v26.0/42/subscribed_apps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    let url = "http://127.0.0.1:9/webhook".to_string();
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--webhook-url",
            &url,
            "--verify-token",
            "v",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("meta-mcp webhook"), "{text}");
    assert!(text.contains("not reachable"), "{text}");
    let creds = creds_json(&home);
    assert_eq!(
        creds["pages"][0]["id"],
        json!("42"),
        "progress is kept: {text}"
    );
    assert_eq!(creds["webhook"]["verify_token"], json!("v"));
    assert!(text.contains("progress so far is saved"), "{text}");
}

#[tokio::test]
async fn setup_missing_scopes_are_reported() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(
        &server,
        "PAGE",
        &[
            "pages_messaging",
            "pages_manage_metadata",
            "pages_show_list",
        ],
        0,
    )
    .await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("missing scope pages_read_engagement"),
        "{text}"
    );
    assert!(!text.contains("missing scope pages_messaging"), "{text}");
}

#[tokio::test]
async fn setup_wrong_app_secret_is_reported_with_the_dashboard_path() {
    let server = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Invalid OAuth access token - Cannot parse access token", "type": "OAuthException", "code": 190}})))
        .mount(&server)
        .await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1);
    let text = out(&o);
    assert!(text.contains("❌ App ID or App Secret rejected"), "{text}");
    assert!(text.contains("Settings > Basic"), "{text}");
    assert!(text.contains("Fix:"), "{text}");
    assert!(!home.path().join("credentials.json").exists());
}

#[tokio::test]
async fn setup_non_interactive_missing_values_are_usage_errors() {
    let server = mock_graph().await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &["setup", "--non-interactive"])).await;
    assert_eq!(code(&o), 2, "{}", out(&o));
    assert!(err(&o).contains("--app-id"), "{}", err(&o));

    let o = run(cmd_with(
        &home,
        &server,
        &["setup", "--non-interactive", "--app-id", "1"],
    ))
    .await;
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("--app-secret-env"), "{}", err(&o));

    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--non-interactive",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--webhook-url",
            "http://hooks.example.org/webhook",
        ],
    ))
    .await;
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("https"), "{}", err(&o));
    assert!(!err(&o).contains(APP_SECRET) && !out(&o).contains(APP_SECRET));
    let all = server.received_requests().await.expect("recording");
    assert!(all.is_empty(), "usage errors happen before any request");
}

#[tokio::test]
async fn setup_page_selection_is_required_non_interactively() {
    let server = mock_graph().await;
    mock_login_chain(&server, "EALONG0123456789abcdefghijklmnop", None).await;
    let home = TempHome::new();
    let mut cmd = cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret-env",
            "THE_SECRET",
            "--user-token",
            USER_TOKEN,
            "--non-interactive",
        ],
    );
    cmd.env("THE_SECRET", APP_SECRET);
    let o = run(cmd).await;
    assert_eq!(code(&o), 2, "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("--all-pages"), "{}", err(&o));
    assert!(!err(&o).contains(APP_SECRET));
    let all = server.received_requests().await.expect("recording");
    assert!(
        all.is_empty(),
        "no sign-in is wasted on a run that cannot pick"
    );
}

fn fixture(user_expires: Option<i64>) -> Value {
    json!({
        "version": 1,
        "app_id": "1",
        "app_secret": APP_SECRET,
        "user_token": {"token": USER_TOKEN, "expires_at": user_expires,
                       "scopes": ALL_MESSENGER, "obtained_at": now() - 86_400},
        "pages": [{"id": "42", "name": "Test Page", "token": PAGE_TOKEN}],
    })
}

#[tokio::test]
async fn setup_stored_tokens_rerun_adds_the_webhook() {
    let (port, _dir) = start_receiver("stored-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    page_mocks(&server, "42", "Test Page").await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    post_json(
        &server,
        "/v26.0/42/subscribed_apps",
        json!({"success": true}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["webhook"] = json!({"verify_token": "stored-vt"});
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--webhook-url",
            &url,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let after = creds_json(&home);
    assert_eq!(
        after["pages"][0]["token"],
        json!(PAGE_TOKEN),
        "stored page kept"
    );
    assert_eq!(after["webhook"]["public_url"], json!(url));
    assert_eq!(
        after["webhook"]["verify_token"],
        json!("stored-vt"),
        "stored verify token reused"
    );
    let all = server.received_requests().await.expect("recording");
    let form = form_of(
        all.iter()
            .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/1/subscriptions")
            .expect("subscription"),
    );
    assert_eq!(pair(&form, "verify_token"), Some("stored-vt"));
    assert!(
        !requests_to(&all, "/v26.0/oauth/access_token")
            .iter()
            .any(|_| true),
        "no login happened"
    );
}

async fn healthy_graph(server: &MockServer) {
    app_mock(server).await;
    debug_mock(server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    page_mocks(server, "42", "Test Page").await;
}

#[tokio::test]
async fn doctor_reports_expiring_user_token() {
    let server = mock_graph().await;
    healthy_graph(&server).await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 3 * 86_400 + 600)));
    let o = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&o), 0, "warning only: {}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("token refresh"), "{text}");
    assert!(text.contains("⚠️ user token"), "{text}");
    assert!(
        text.contains("1 warnings") || text.contains("2 warnings"),
        "{text}"
    );
    assert!(text.contains("0 failures"), "{text}");
}

#[tokio::test]
async fn doctor_exit_code_1_on_invalid_page_token() {
    let server = mock_graph().await;
    app_mock(&server).await;
    get_json(
        &server,
        "/v26.0/debug_token",
        json!({"data": {"app_id": "1", "type": "PAGE", "is_valid": false, "scopes": [],
                        "error": {"code": 190, "message": "Error validating access token"}}}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let mut creds = fixture(None);
    creds["user_token"] = Value::Null;
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("❌ page 42: the Page token is no longer valid\n   Fix: meta-mcp setup"),
        "{text}"
    );
    assert!(text.contains("1 failures"), "{text}");
}

#[tokio::test]
async fn doctor_json_output() {
    let server = mock_graph().await;
    healthy_graph(&server).await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 40 * 86_400)));
    let o = run(cmd_with(&home, &server, &["doctor", "--json"])).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let v: Value = serde_json::from_str(&out(&o)).expect("one JSON document");
    assert_eq!(v["ok"], json!(true), "{v}");
    let checks = v["checks"].as_array().expect("checks");
    assert!(checks.len() >= 6, "{v}");
    for c in checks {
        assert!(c["name"].is_string() && c["detail"].is_string(), "{c}");
        assert!(["ok", "warn", "fail", "info"].contains(&c["status"].as_str().expect("status")));
    }
    assert!(
        checks
            .iter()
            .any(|c| c["name"] == json!("app") && c["status"] == json!("ok"))
    );
    assert_eq!(v["summary"]["failures"], json!(0));
    let raw = out(&o);
    for secret in [APP_SECRET, PAGE_TOKEN, USER_TOKEN] {
        assert!(!raw.contains(secret), "{secret} leaked");
    }

    let mut bad = fixture(None);
    bad["app_id"] = json!("1");
    let broken = TempHome::new();
    broken.write_credentials(&bad);
    let server2 = mock_graph().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Invalid OAuth access token", "type": "OAuthException", "code": 190}})))
        .mount(&server2)
        .await;
    let o = run(cmd_with(&broken, &server2, &["doctor", "--json"])).await;
    assert_eq!(code(&o), 1);
    let v: Value = serde_json::from_str(&out(&o)).expect("one JSON document");
    assert_eq!(v["ok"], json!(false));
}

#[tokio::test]
async fn doctor_webhook_flag_tests_the_callback() {
    let (port, _dir) = start_receiver("hook-vt").await;
    let server = mock_graph().await;
    healthy_graph(&server).await;
    get_json(
        &server,
        "/v26.0/1/subscriptions",
        json!({"data": [{"object": "page", "callback_url": format!("http://127.0.0.1:{port}/webhook"),
                         "fields": ["messages"], "active": true}]}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["webhook"] = json!({"verify_token": "hook-vt", "public_url": format!("http://127.0.0.1:{port}/webhook")});
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor", "--webhook"])).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("answered the verification handshake"),
        "{}",
        out(&o)
    );

    creds["webhook"]["verify_token"] = json!("another-token");
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor", "--webhook"])).await;
    assert_eq!(code(&o), 1, "{}", out(&o));
    assert!(out(&o).contains("webhook reachability"), "{}", out(&o));
    assert!(out(&o).contains("meta-mcp webhook"), "{}", out(&o));
}

#[tokio::test]
async fn token_refresh_replaces_user_token() {
    let server = mock_graph().await;
    let new_token = "EAAnewusertoken0123456789abcdefghijkl";
    let new_page = "EAAp2pagetoken0123456789abcdefghijklm";
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("grant_type", "fb_exchange_token"))
        .and(query_param("fb_exchange_token", USER_TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": new_token, "token_type": "bearer", "expires_in": 5_184_000}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    get_json(
        &server,
        "/v26.0/me/accounts",
        json!({"data": [{"id": "42", "name": "Test Page Renamed", "access_token": new_page},
                        {"id": "99", "name": "Not stored", "access_token": "EAAother0123456789abcdefghijklmnopqr"}]}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 2 * 86_400)));
    let o = run(cmd_with(&home, &server, &["token", "refresh"])).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let creds = creds_json(&home);
    assert_eq!(creds["user_token"]["token"], json!(new_token));
    let expires = creds["user_token"]["expires_at"]
        .as_i64()
        .expect("expires_at");
    assert!((expires - (now() + 5_184_000)).abs() < 600, "{expires}");
    assert_eq!(
        creds["pages"].as_array().expect("pages").len(),
        1,
        "unstored pages are not added"
    );
    assert_eq!(creds["pages"][0]["token"], json!(new_page));
    assert_eq!(creds["pages"][0]["name"], json!("Test Page Renamed"));
    assert_eq!(creds["app_secret"], json!(APP_SECRET));
    let text = out(&o);
    assert!(!text.contains(new_token) && !text.contains(new_page) && !text.contains(USER_TOKEN));
    assert!(text.contains("refreshed"), "{text}");
}

#[tokio::test]
async fn token_refresh_whatsapp_uses_the_sixty_day_flag_for_system_users() {
    let server = mock_graph().await;
    let fresh = "EAAfreshwatoken0123456789abcdefghijklm";
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param(
            "fb_exchange_token",
            "EAAsystemuser0123456789abcdefghijklmnop",
        ))
        .and(query_param("set_token_expires_in_60_days", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": fresh, "expires_in": 5_184_000})),
        )
        .expect(1)
        .mount(&server)
        .await;
    debug_mock(
        &server,
        "SYSTEM_USER",
        &[
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        now() + 20 * 86_400,
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&json!({
        "version": 1, "app_id": "1", "app_secret": APP_SECRET,
        "whatsapp": {"waba_id": "W1", "token": "EAAsystemuser0123456789abcdefghijklmnop", "token_kind": "system_user",
                     "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]}
    }));
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "refresh", "--whatsapp"],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let creds = creds_json(&home);
    assert_eq!(creds["whatsapp"]["token"], json!(fresh));
    assert_eq!(creds["whatsapp"]["phone_numbers"][0]["id"], json!("P1"));
}

#[tokio::test]
async fn token_refresh_with_a_dead_user_token_points_to_setup() {
    let server = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Error validating access token: Session has expired", "type": "OAuthException", "code": 190}})))
        .mount(&server)
        .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() - 86_400)));
    let before = creds_text(&home);
    let o = run(cmd_with(&home, &server, &["token", "refresh"])).await;
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("meta-mcp setup --login"), "{}", out(&o));
    assert_eq!(creds_text(&home), before, "nothing is rewritten on failure");
}

#[tokio::test]
async fn token_import_page_token_validates_with_debug_token() {
    let server = mock_graph().await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, 0).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "77", "name": "Imported Page", "category": "Shop"}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 40 * 86_400)));
    let imported = "EAAimported0123456789abcdefghijklmnopqrs";
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "import", "--page-token", imported],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let creds = creds_json(&home);
    let pages = creds["pages"].as_array().expect("pages");
    assert_eq!(pages.len(), 2, "added next to the stored page");
    assert_eq!(pages[1]["id"], json!("77"));
    assert_eq!(pages[1]["name"], json!("Imported Page"));
    assert_eq!(pages[1]["token"], json!(imported));
    assert_eq!(pages[0]["token"], json!(PAGE_TOKEN));
    let all = server.received_requests().await.expect("recording");
    let inspected = requests_to(&all, "/v26.0/debug_token");
    assert_eq!(inspected.len(), 1);
    assert_eq!(pair(&query_of(inspected[0]), "input_token"), Some(imported));
    assert_eq!(
        inspected[0]
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer 1|{APP_SECRET}").as_str())
    );
    assert!(!out(&o).contains(imported));
}

#[tokio::test]
async fn token_import_rejects_an_invalid_token_and_stores_nothing() {
    let server = mock_graph().await;
    get_json(
        &server,
        "/v26.0/debug_token",
        json!({"data": {"app_id": "1", "is_valid": false, "scopes": [], "error": {"code": 190, "message": "expired"}}}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(None));
    let before = creds_text(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "token",
            "import",
            "--user-token",
            "EAAbad0123456789abcdefghijklmnopqrstu",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("not valid"), "{}", out(&o));
    assert_eq!(creds_text(&home), before);
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "import", "--page-token", "EAAx", "--waba-id", "W1"],
    ))
    .await;
    assert_eq!(code(&o), 2, "{}", err(&o));
}

#[tokio::test]
async fn token_import_whatsapp_token_with_a_phone_number() {
    let server = mock_graph().await;
    debug_mock(
        &server,
        "SYSTEM_USER",
        &[
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        0,
    )
    .await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "5", "name": "Bot System User"}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/P9",
        json!({"display_phone_number": "+1 555 0900", "verified_name": "Nine", "status": "CONNECTED"}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(None));
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "token",
            "import",
            "--whatsapp-token",
            "EAAwa0123456789abcdefghijklmnopqrstuvwx",
            "--waba-id",
            "W9",
            "--phone-number-id",
            "P9",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let wa = &creds_json(&home)["whatsapp"];
    assert_eq!(wa["token_kind"], json!("system_user"));
    assert_eq!(wa["waba_id"], json!("W9"));
    assert_eq!(wa["phone_numbers"][0]["id"], json!("P9"));
    assert_eq!(
        wa["phone_numbers"][0]["display_phone_number"],
        json!("+1 555 0900")
    );
}

#[tokio::test]
async fn token_show_never_prints_secrets() {
    let server = mock_graph().await;
    let home = TempHome::new();
    home.write_credentials(&json!({
        "version": 1,
        "app_id": "1",
        "app_secret": "SECRETMARKERappsecret",
        "user_token": {"token": "SECRETMARKERuser0123456789abcdef", "expires_at": now() + 30 * 86_400,
                       "scopes": ["pages_messaging", "pages_show_list"], "obtained_at": now() - 86_400},
        "pages": [{"id": "42", "name": "Test Page", "token": "SECRETMARKERpage0123456789abcdef"}],
        "whatsapp": {"waba_id": "W1", "token": "SECRETMARKERwa0123456789abcdefgh", "token_kind": "system_user",
                     "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]},
        "webhook": {"verify_token": "SECRETMARKERverify", "public_url": "https://hooks.example.org/webhook"}
    }));
    let text = run(cmd_with(&home, &server, &["token", "show"])).await;
    assert_eq!(code(&text), 0, "{}", err(&text));
    let shown = out(&text);
    assert!(
        !shown.contains("SECRETMARKER") && !err(&text).contains("SECRETMARKER"),
        "{shown}"
    );
    assert!(
        shown.contains("42") && shown.contains("Test Page") && shown.contains("P1"),
        "{shown}"
    );
    assert!(
        shown.contains("days") && shown.contains("pages_messaging"),
        "{shown}"
    );
    assert!(shown.contains("verify token set"), "{shown}");

    let j = run(cmd_with(&home, &server, &["token", "show", "--json"])).await;
    assert_eq!(code(&j), 0);
    assert!(!out(&j).contains("SECRETMARKER"), "{}", out(&j));
    let v: Value = serde_json::from_str(&out(&j)).expect("json");
    assert_eq!(v["app_id"], json!("1"));
    assert_eq!(v["pages"][0]["name"], json!("Test Page"));
    assert_eq!(v["user_token"]["kind"], json!("user"));
    assert_eq!(v["whatsapp"]["token_kind"], json!("system_user"));
    assert_eq!(v["webhook"]["verify_token_set"], json!(true));
    let all = server.received_requests().await.expect("recording");
    assert!(all.is_empty(), "token show is purely local");
}

#[tokio::test]
async fn setup_never_logs_secrets() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, 0).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let mut cmd = command(&home, &server.uri(), &[("META_MCP_LOG", "trace")]);
    cmd.args([
        "setup",
        "--app-id",
        "1",
        "--app-secret",
        APP_SECRET,
        "--page-token",
        PAGE_TOKEN,
        "--non-interactive",
    ]);
    let o = run(cmd).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let app_token = format!("1|{APP_SECRET}");
    for (name, text) in [("stdout", out(&o)), ("stderr", err(&o))] {
        for secret in [PAGE_TOKEN, APP_SECRET, app_token.as_str()] {
            assert!(!text.contains(secret), "{secret} leaked on {name}:\n{text}");
        }
    }
    assert!(
        out(&o).contains("shell history"),
        "the secret flag is warned about: {}",
        out(&o)
    );

    let doctor =
        run(command(&home, &server.uri(), &[("META_MCP_LOG", "trace")]).tap_args(&["doctor"]))
            .await;
    assert_eq!(code(&doctor), 0, "{}", out(&doctor));
    for text in [out(&doctor), err(&doctor)] {
        assert!(
            !text.contains(PAGE_TOKEN) && !text.contains(APP_SECRET),
            "{text}"
        );
    }
}

trait TapArgs {
    fn tap_args(self, args: &[&str]) -> Self;
}

impl TapArgs for Command {
    fn tap_args(mut self, args: &[&str]) -> Self {
        self.args(args);
        self
    }
}

async fn whatsapp_webhook_mocks(server: &MockServer, override_uri: Option<&str>) {
    app_mock(server).await;
    debug_mock(
        server,
        "SYSTEM_USER",
        &[
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        0,
    )
    .await;
    get_json(
        server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    post_json(
        server,
        "/v26.0/W1/subscribed_apps",
        json!({"success": true}),
    )
    .await;
    let mut entry = json!({"whatsapp_business_api_data": {"id": "1", "name": "Test App"}});
    if let Some(uri) = override_uri {
        entry["override_callback_uri"] = json!(uri);
    }
    get_json(
        server,
        "/v26.0/W1/subscribed_apps",
        json!({"data": [entry]}),
    )
    .await;
}

fn whatsapp_args<'a>(url: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "setup",
        "--app-id",
        "1",
        "--app-secret",
        APP_SECRET,
        "--whatsapp-token",
        USER_TOKEN,
        "--waba-id",
        "W1",
        "--phone-number-id",
        "P1",
        "--no-messenger",
        "--webhook-url",
        url,
        "--verify-token",
        "wa-vt",
        "--non-interactive",
    ];
    args.extend_from_slice(extra);
    args
}

#[tokio::test]
async fn setup_whatsapp_webhook_explains_the_dashboard_when_the_api_refuses() {
    let (port, _dir) = start_receiver("wa-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "WhatsApp webhooks are configured in the dashboard", "type": "OAuthException", "code": 100}})))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &whatsapp_args(&url, &[]))).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("Configure it by hand"), "{text}");
    assert!(
        text.contains("App Dashboard > WhatsApp > Configuration"),
        "{text}"
    );
    assert!(
        !text.contains("wa-vt"),
        "the verify token is never printed: {text}"
    );
    assert!(
        text.contains("WhatsApp account W1: this app is subscribed"),
        "{text}"
    );
    let all = server.received_requests().await.expect("recording");
    let attempt = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/1/subscriptions")
        .expect("app-level attempt");
    let form = form_of(attempt);
    assert_eq!(pair(&form, "object"), Some("whatsapp_business_account"));
    assert_eq!(pair(&form, "fields"), Some("messages"));
    assert_eq!(pair(&form, "verify_token"), Some("wa-vt"));
    let account = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps")
        .expect("account subscription");
    assert!(
        account.body.is_empty(),
        "no override without --webhook-override"
    );
    assert_eq!(
        account
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {USER_TOKEN}").as_str())
    );
    let creds = creds_json(&home);
    assert_eq!(creds["whatsapp"]["token_kind"], json!("system_user"));
    assert_eq!(creds["whatsapp"]["phone_numbers"][0]["id"], json!("P1"));
}

#[tokio::test]
async fn setup_webhook_override_targets_only_the_whatsapp_account() {
    let (port, _dir) = start_receiver("wa-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, Some(&url)).await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &whatsapp_args(&url, &["--webhook-override"]),
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("left untouched"), "{}", out(&o));
    assert!(
        out(&o).contains("subscribed with the callback override"),
        "{}",
        out(&o)
    );
    let all = server.received_requests().await.expect("recording");
    let account = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps")
        .expect("account subscription");
    let body: Value =
        serde_json::from_slice(&account.body).expect("a JSON body, as Meta documents");
    assert_eq!(body["override_callback_uri"], json!(url));
    assert_eq!(body["verify_token"], json!("wa-vt"));
}

#[tokio::test]
async fn setup_stored_rerun_subscribes_the_stored_whatsapp_account() {
    let (port, _dir) = start_receiver("stored-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    page_mocks(&server, "42", "Test Page").await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    post_json(
        &server,
        "/v26.0/42/subscribed_apps",
        json!({"success": true}),
    )
    .await;
    post_json(
        &server,
        "/v26.0/W1/subscribed_apps",
        json!({"success": true}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/W1/subscribed_apps",
        json!({"data": [{"whatsapp_business_api_data": {"id": "1"}}]}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["webhook"] = json!({"verify_token": "stored-vt"});
    creds["whatsapp"] = json!({"waba_id": "W1", "token": "stored-whatsapp-system-token-0123456789", "token_kind": "system_user",
        "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]});
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--stored",
            "--webhook-url",
            &url,
            "--app-id",
            "1",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let all = server.received_requests().await.expect("recording");
    let account = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps")
        .expect("the stored WhatsApp account is subscribed too");
    assert_eq!(
        account
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some("Bearer stored-whatsapp-system-token-0123456789")
    );
    assert_eq!(
        creds_json(&home)["whatsapp"]["phone_numbers"][0]["id"],
        json!("P1")
    );
}

async fn callback_conflict_mocks(server: &MockServer, posts: u64) {
    page_token_mocks(server).await;
    get_json(server, "/v26.0/42/subscribed_apps", json!({"data": []})).await;
    get_json(
        server,
        "/v26.0/1/subscriptions",
        json!({"data": [{"object": "page", "callback_url": "https://other.example.org/hook",
                         "fields": ["messages"], "active": true}]}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(posts)
        .mount(server)
        .await;
    post_json(
        server,
        "/v26.0/42/subscribed_apps",
        json!({"success": true}),
    )
    .await;
}

fn webhook_args<'a>(url: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "setup",
        "--app-id",
        "1",
        "--app-secret",
        APP_SECRET,
        "--page-token",
        PAGE_TOKEN,
        "--webhook-url",
        url,
        "--verify-token",
        "v",
        "--non-interactive",
    ];
    args.extend_from_slice(extra);
    args
}

#[tokio::test]
async fn setup_refuses_to_replace_a_different_callback_without_yes() {
    let (port, _dir) = start_receiver("v").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    callback_conflict_mocks(&server, 0).await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &webhook_args(&url, &[]))).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("https://other.example.org/hook"), "{text}");
    assert!(text.contains("--yes"), "{text}");
    assert!(
        text.contains("left the existing page webhook callback in place"),
        "{text}"
    );
    let creds = creds_json(&home);
    assert_eq!(creds["pages"][0]["id"], json!("42"), "progress is kept");
    assert_eq!(creds["webhook"]["verify_token"], json!("v"));
    assert!(
        creds["webhook"]["public_url"].is_null(),
        "a URL Meta is not using is not recorded: {creds}"
    );
}

#[tokio::test]
async fn setup_replaces_a_different_callback_when_told_to() {
    let (port, _dir) = start_receiver("v").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    callback_conflict_mocks(&server, 1).await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &webhook_args(&url, &["--yes"]))).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("this run replaces that callback"),
        "{}",
        out(&o)
    );
}

#[tokio::test]
async fn setup_refuses_to_switch_apps_without_replace() {
    let server = mock_graph().await;
    get_json(
        &server,
        "/v26.0/2",
        json!({"id": "2", "name": "Other App", "link": "https://example.invalid"}),
    )
    .await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, 0).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "99", "name": "New Page"}),
    )
    .await;
    page_mocks(&server, "99", "New Page").await;
    let home = TempHome::new();
    let mut stored = fixture(Some(now() + 40 * 86_400));
    stored["webhook"] =
        json!({"verify_token": "old-verify", "public_url": "https://old.example.org/webhook"});
    home.write_credentials(&stored);
    let before = creds_text(&home);
    let args = [
        "setup",
        "--app-id",
        "2",
        "--app-secret",
        APP_SECRET,
        "--page-token",
        "EAAnewapppage0123456789abcdefghijklmnop",
        "--non-interactive",
    ];
    let o = run(cmd_with(&home, &server, &args)).await;
    assert_eq!(code(&o), 2, "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("--replace"), "{}", err(&o));
    assert!(out(&o).contains("belongs to app 1"), "{}", out(&o));
    assert_eq!(
        creds_text(&home),
        before,
        "nothing is written without consent"
    );

    let mut replace = args.to_vec();
    replace.push("--replace");
    let o = run(cmd_with(&home, &server, &replace)).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let creds = creds_json(&home);
    assert_eq!(creds["app_id"], json!("2"));
    let pages = creds["pages"].as_array().expect("pages");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["id"], json!("99"));
    assert!(
        creds["user_token"].is_null(),
        "tokens of the old app are gone"
    );
    assert!(creds["webhook"]["public_url"].is_null());
    let token = creds["webhook"]["verify_token"]
        .as_str()
        .expect("a fresh verify token");
    assert_eq!(token.len(), 64, "the receiver of the new app can start");
}

#[tokio::test]
async fn setup_and_doctor_point_to_the_move_command_for_a_corrupt_credentials_file() {
    let server = mock_graph().await;
    let home = TempHome::new();
    std::fs::write(home.path().join("credentials.json"), "{ this is not json").expect("write");
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let file = home.path().join("credentials.json");
    let expected = format!("mv {} {}.broken", file.display(), file.display());
    assert!(out(&o).contains(&expected), "{}", out(&o));
    let d = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&d), 1);
    assert!(out(&d).contains("❌ config"), "{}", out(&d));
    assert!(out(&d).contains(&expected), "{}", out(&d));
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "{ this is not json"
    );
}

#[tokio::test]
async fn setup_oauth_reports_a_busy_port() {
    let server = mock_graph().await;
    mock_login_chain(&server, "EALONG0123456789abcdefghijklmnop", Some(0)).await;
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = taken.local_addr().expect("addr").port().to_string();
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &login_args(&port, &[]))).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains(&format!("port {port} is already in use")),
        "{text}"
    );
    assert!(text.contains("--port"), "{text}");
    drop(taken);
}

#[tokio::test]
async fn setup_rejects_an_https_loopback_redirect_uri_before_any_request() {
    let server = mock_graph().await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--login",
            "--redirect-uri",
            "https://localhost:8766/callback",
            "--all-pages",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 2, "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("--redirect-mode paste"), "{}", err(&o));
    let all = server.received_requests().await.expect("recording");
    assert!(all.is_empty());
}

#[tokio::test]
async fn setup_reads_the_app_secret_from_a_file_and_the_environment() {
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let file = home.path().join("secret.txt");
    std::fs::write(&file, format!("{APP_SECRET}\n")).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let file_arg = file.to_str().expect("utf8").to_string();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret-file",
            &file_arg,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(!out(&o).contains("shell history"), "{}", out(&o));
    assert_eq!(creds_json(&home)["app_secret"], json!(APP_SECRET));

    let other = TempHome::new();
    let mut cmd = cmd_with(
        &other,
        &server,
        &["setup", "--page-token", PAGE_TOKEN, "--non-interactive"],
    );
    cmd.env("META_APP_ID", "1")
        .env("META_APP_SECRET", APP_SECRET);
    let o = run(cmd).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert_eq!(creds_json(&other)["app_secret"], json!(APP_SECRET));
    assert!(!out(&o).contains(APP_SECRET));
}

#[tokio::test]
async fn setup_creates_a_verify_token_so_the_receiver_can_start() {
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let args = [
        "setup",
        "--app-id",
        "1",
        "--app-secret",
        APP_SECRET,
        "--page-token",
        PAGE_TOKEN,
        "--non-interactive",
    ];
    let o = run(cmd_with(&home, &server, &args)).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let token = creds_json(&home)["webhook"]["verify_token"]
        .as_str()
        .expect("a verify token")
        .to_string();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!out(&o).contains(&token), "never printed");
    assert!(
        out(&o).contains("created a webhook verify token"),
        "{}",
        out(&o)
    );
    assert!(creds_json(&home)["webhook"]["public_url"].is_null());

    let again = run(cmd_with(&home, &server, &args)).await;
    assert_eq!(code(&again), 0);
    assert_eq!(
        creds_json(&home)["webhook"]["verify_token"],
        json!(token),
        "kept on the next run"
    );
    assert!(!out(&again).contains("created a webhook verify token"));
}

#[tokio::test]
async fn setup_checks_the_token_type_for_page_and_user_routes() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, 0).await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--user-token",
            USER_TOKEN,
            "--all-pages",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("this is a Page token, not a user token"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("--page-token"), "{}", out(&o));

    let user_server = mock_graph().await;
    app_mock(&user_server).await;
    debug_mock(&user_server, "USER", &ALL_MESSENGER, now() + 86_400).await;
    let o = run(cmd_with(
        &home,
        &user_server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("this is a user token, not a Page token"),
        "{}",
        out(&o)
    );
    assert!(!home.path().join("credentials.json").exists());
}

#[tokio::test]
async fn setup_stored_replace_keeps_only_the_selected_pages() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["pages"] = json!([
        {"id": "42", "name": "Test Page", "token": PAGE_TOKEN},
        {"id": "43", "name": "Other Page", "token": "EAAotherpage0123456789abcdefghijklmnop"}
    ]);
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--page-id",
            "42",
            "--replace",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let pages = creds_json(&home)["pages"].clone();
    assert_eq!(pages.as_array().expect("pages").len(), 1, "{pages}");
    assert_eq!(pages[0]["id"], json!("42"));
}

#[tokio::test]
async fn doctor_warns_about_an_inactive_app_subscription() {
    let server = mock_graph().await;
    healthy_graph(&server).await;
    let url = "https://hooks.example.org/webhook";
    get_json(
        &server,
        "/v26.0/1/subscriptions",
        json!({"data": [{"object": "page", "callback_url": url, "fields": ["messages"], "active": false}]}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["webhook"] = json!({"verify_token": "hook-vt", "public_url": url});
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&o), 0, "a warning only: {}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("object=page is inactive"), "{text}");
    assert!(
        text.contains(&format!("meta-mcp setup --stored --webhook-url {url}")),
        "{text}"
    );
}

#[tokio::test]
async fn token_refresh_whatsapp_leaves_a_non_expiring_token_alone() {
    let server = mock_graph().await;
    debug_mock(
        &server,
        "SYSTEM_USER",
        &[
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        0,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token": "never-used"})),
        )
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    home.write_credentials(&json!({
        "version": 1, "app_id": "1", "app_secret": APP_SECRET,
        "whatsapp": {"waba_id": "W1", "token": "EAAneverexpires0123456789abcdefghijklmn", "token_kind": "system_user",
                     "phone_numbers": []}
    }));
    let before = creds_text(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "refresh", "--whatsapp"],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("never expires: nothing to refresh"),
        "{}",
        out(&o)
    );
    assert!(creds_text(&home).contains("EAAneverexpires0123456789abcdefghijklmn"));
    assert_eq!(
        creds_json(&home),
        serde_json::from_str::<Value>(&before).expect("json")
    );
}

#[tokio::test]
async fn token_refresh_keeps_the_user_refresh_when_the_whatsapp_step_fails() {
    let server = mock_graph().await;
    let new_user = "EAAnewusertoken0123456789abcdefghijkl";
    let new_page = "EAAp2pagetoken0123456789abcdefghijklm";
    let wa_token = "EAAwhatsappsystem0123456789abcdefghijk";
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("fb_exchange_token", USER_TOKEN))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": new_user, "expires_in": 5_184_000})),
        )
        .mount(&server)
        .await;
    get_json(
        &server,
        "/v26.0/me/accounts",
        json!({"data": [{"id": "42", "name": "Test Page", "access_token": new_page}]}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/debug_token"))
        .and(query_param("input_token", wa_token))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": {
            "message": "temporarily unavailable", "type": "OAuthException", "code": 2}})))
        .mount(&server)
        .await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 5_184_000).await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 2 * 86_400));
    creds["whatsapp"] = json!({"waba_id": "W1", "token": wa_token, "token_kind": "system_user", "phone_numbers": []});
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "refresh", "--whatsapp"],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("cannot tell whether the WhatsApp token expires"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("refreshed and saved before this failure"),
        "{}",
        out(&o)
    );
    let after = creds_json(&home);
    assert_eq!(after["user_token"]["token"], json!(new_user));
    assert_eq!(after["pages"][0]["token"], json!(new_page));
    assert_eq!(after["whatsapp"]["token"], json!(wa_token));
}

#[tokio::test]
async fn token_show_says_when_a_user_token_has_no_recorded_expiry() {
    let server = mock_graph().await;
    let home = TempHome::new();
    let mut creds = fixture(None);
    creds["user_token"]["expires_at"] = Value::Null;
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["token", "show"])).await;
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("expiry not recorded"), "{}", out(&o));
    assert!(!out(&o).contains("never expires"), "{}", out(&o));
}

#[tokio::test]
async fn setup_stays_put_when_the_current_subscriptions_cannot_be_read() {
    let (port, _dir) = start_receiver("v").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    get_json(&server, "/v26.0/42/subscribed_apps", json!({"data": []})).await;
    Mock::given(method("GET"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "(#200) permission denied", "type": "OAuthException", "code": 200}})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &webhook_args(&url, &[]))).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("could not be read, so it was left alone"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("--yes"), "{}", out(&o));
}

#[tokio::test]
async fn setup_whatsapp_app_level_attempt_respects_an_existing_callback() {
    let (port, _dir) = start_receiver("wa-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    get_json(
        &server,
        "/v26.0/1/subscriptions",
        json!({"data": [{"object": "whatsapp_business_account", "callback_url": "https://other.example.org/wa",
                         "fields": ["messages"], "active": true}]}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &whatsapp_args(&url, &[]))).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(text.contains("https://other.example.org/wa"), "{text}");
    assert!(
        text.contains("no app-level WhatsApp change was made"),
        "{text}"
    );
    assert!(
        !text.contains("Configure it by hand"),
        "the dashboard steps would do the very replacement that was refused: {text}"
    );
    assert!(text.contains("rerun with --webhook-override"), "{text}");
    assert!(
        text.contains("no subscription of this run sends events to"),
        "nothing of this run sends events to it: {text}"
    );
    let all = server.received_requests().await.expect("recording");
    assert!(
        !all.iter()
            .any(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps"),
        "the account is left alone"
    );
    assert!(creds_json(&home)["webhook"]["public_url"].is_null());
}

fn whatsapp_credentials(token: &str) -> Value {
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["whatsapp"] = json!({"waba_id": "W1", "token": token, "token_kind": "system_user",
        "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]});
    creds
}

#[tokio::test]
async fn setup_keeps_a_stored_system_user_token_when_whatsapp_is_set_up_again() {
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let system = "stored-system-user-token-0123456789";
    home.write_credentials(&whatsapp_credentials(system));
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--whatsapp",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("keeping the stored System User token"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("reusing the WhatsApp numbers already stored"),
        "{}",
        out(&o)
    );
    let wa = creds_json(&home)["whatsapp"].clone();
    assert_eq!(wa["token"], json!(system), "{wa}");
    assert_eq!(wa["token_kind"], json!("system_user"));
    assert_eq!(wa["phone_numbers"][0]["id"], json!("P1"));
    assert_eq!(wa["waba_id"], json!("W1"));
    let all = server.received_requests().await.expect("recording");
    let number_reads = requests_to(&all, "/v26.0/P1");
    assert!(!number_reads.is_empty());
    for read in number_reads {
        assert_eq!(
            read.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some(format!("Bearer {system}").as_str()),
            "the number is read with the System User token"
        );
    }
}

#[tokio::test]
async fn setup_stored_whatsapp_only_credentials_can_subscribe_the_webhook() {
    let (port, _dir) = start_receiver("wa-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    let home = TempHome::new();
    home.write_credentials(&json!({
        "version": 1, "app_id": "1", "app_secret": APP_SECRET,
        "whatsapp": {"waba_id": "W1", "token": USER_TOKEN, "token_kind": "system_user",
                     "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]},
        "webhook": {"verify_token": "wa-vt"}
    }));
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--webhook-url",
            &url,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("Messenger is skipped"), "{}", out(&o));
    let all = server.received_requests().await.expect("recording");
    assert!(
        all.iter()
            .any(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps"),
        "the account is subscribed"
    );
    let creds = creds_json(&home);
    assert_eq!(creds["webhook"]["public_url"], json!(url));
    assert_eq!(creds["whatsapp"]["token"], json!(USER_TOKEN));
}

#[tokio::test]
async fn setup_does_not_borrow_another_apps_stored_secret() {
    let server = mock_graph().await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 40 * 86_400)));
    let before = creds_text(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "2",
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 2, "{}\n{}", out(&o), err(&o));
    assert!(err(&o).contains("--app-secret-env"), "{}", err(&o));
    assert_eq!(creds_text(&home), before);
    let all = server.received_requests().await.expect("recording");
    assert!(all.is_empty());
}

#[tokio::test]
async fn setup_stored_rejects_an_unknown_page_id() {
    let server = mock_graph().await;
    app_mock(&server).await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 40 * 86_400)));
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--page-id",
            "99",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("Page 99 is not stored (stored: 42)"),
        "{}",
        out(&o)
    );
}

#[tokio::test]
async fn setup_rejects_any_non_page_token_type_as_a_page_token() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "SYSTEM_USER", &ALL_MESSENGER, 0).await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("this is a system_user token, not a Page token"),
        "{}",
        out(&o)
    );
    assert!(!home.path().join("credentials.json").exists());
}

#[tokio::test]
async fn doctor_flags_a_stale_whatsapp_override() {
    let server = mock_graph().await;
    healthy_graph(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    get_json(
        &server,
        "/v26.0/W1/subscribed_apps",
        json!({"data": [{"whatsapp_business_api_data": {"id": "1"},
                         "override_callback_uri": "https://old.example.org/webhook"}]}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = whatsapp_credentials("stored-system-user-token-0123456789");
    creds["webhook"] =
        json!({"verify_token": "hook-vt", "public_url": "https://hooks.example.org/webhook"});
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&o), 0, "a warning only: {}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("callback override https://old.example.org/webhook sends events there instead of https://hooks.example.org/webhook"),
        "{text}"
    );
    assert!(text.contains("--webhook-override"), "{text}");
}

#[tokio::test]
async fn setup_falls_back_to_the_user_token_when_the_stored_system_token_is_dead() {
    let server = mock_graph().await;
    let dead = "dead-stored-system-user-token-0123456789";
    app_mock(&server).await;
    Mock::given(method("GET"))
        .and(path("/v26.0/debug_token"))
        .and(query_param("input_token", dead))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
            "app_id": "1", "is_valid": false, "scopes": [],
            "error": {"code": 190, "message": "token revoked"}}})))
        .mount(&server)
        .await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let mut stored = whatsapp_credentials(dead);
    stored["user_token"]["scopes"] = json!(wa_scopes());
    home.write_credentials(&stored);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--whatsapp",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("no longer works"), "{}", out(&o));
    assert!(
        out(&o).contains("using your user token instead"),
        "{}",
        out(&o)
    );
    let wa = creds_json(&home)["whatsapp"].clone();
    assert_eq!(wa["token_kind"], json!("user"), "{wa}");
    assert_eq!(wa["token"], json!(USER_TOKEN));
    assert_eq!(wa["phone_numbers"][0]["id"], json!("P1"));
}

#[tokio::test]
async fn setup_names_the_whatsapp_token_flag_when_the_stored_token_is_dead_and_there_is_no_user_token()
 {
    let server = mock_graph().await;
    let dead = "dead-stored-system-user-token-0123456789";
    app_mock(&server).await;
    get_json(
        &server,
        "/v26.0/debug_token",
        json!({"data": {"app_id": "1", "is_valid": false, "scopes": [],
                        "error": {"code": 190, "message": "token revoked"}}}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&json!({
        "version": 1, "app_id": "1", "app_secret": APP_SECRET,
        "whatsapp": {"waba_id": "W1", "token": dead, "token_kind": "system_user",
                     "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]}
    }));
    let before = creds_text(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--whatsapp",
            "--no-messenger",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("the stored System User token no longer works"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("--whatsapp-token"), "{}", out(&o));
    assert_eq!(creds_text(&home), before, "nothing is replaced");
}

#[tokio::test]
async fn setup_sees_credentials_written_by_token_import() {
    let (port, _dir) = start_receiver("wa-vt").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "5", "name": "Bot System User"}),
    )
    .await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    let home = TempHome::new();
    let env_app = |mut cmd: Command| {
        cmd.env("META_APP_ID", "1")
            .env("META_APP_SECRET", APP_SECRET);
        cmd
    };
    let system = "imported-system-user-token-0123456789";
    let imported = run(env_app(cmd_with(
        &home,
        &server,
        &[
            "token",
            "import",
            "--whatsapp-token",
            system,
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
        ],
    )))
    .await;
    assert_eq!(code(&imported), 0, "{}\n{}", out(&imported), err(&imported));
    assert_eq!(
        creds_json(&home)["app_id"],
        json!("1"),
        "import records the app it belongs to"
    );

    let o = run(env_app(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--stored",
            "--webhook-url",
            &url,
            "--verify-token",
            "wa-vt",
            "--non-interactive",
        ],
    )))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let all = server.received_requests().await.expect("recording");
    let account = all
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/v26.0/W1/subscribed_apps")
        .expect("the imported account is subscribed");
    assert_eq!(
        account
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {system}").as_str())
    );
    assert_eq!(creds_json(&home)["whatsapp"]["token"], json!(system));
}

#[tokio::test]
async fn setup_with_nothing_to_subscribe_is_an_error_and_records_no_url() {
    let url = "http://127.0.0.1:9/webhook".to_string();
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    Mock::given(method("POST"))
        .and(path("/v26.0/1/subscriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["pages"] = json!([]);
    creds["webhook"] = json!({"verify_token": "v"});
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--no-messenger",
            "--webhook-url",
            &url,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("there is nothing to subscribe"),
        "{}",
        out(&o)
    );
    assert!(
        !out(&o).contains("not reachable"),
        "the check comes before the probe: {}",
        out(&o)
    );
    assert!(creds_json(&home)["webhook"]["public_url"].is_null());
}

#[tokio::test]
async fn setup_app_switch_with_a_webhook_does_not_reuse_the_old_verify_token() {
    let (port, _dir) = start_receiver("old-verify-token").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    get_json(
        &server,
        "/v26.0/2",
        json!({"id": "2", "name": "Other App", "link": "https://example.invalid"}),
    )
    .await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, 0).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "99", "name": "New Page"}),
    )
    .await;
    page_mocks(&server, "99", "New Page").await;
    let home = TempHome::new();
    let mut stored = fixture(Some(now() + 40 * 86_400));
    stored["webhook"] = json!({"verify_token": "old-verify-token", "public_url": "https://old.example.org/webhook"});
    home.write_credentials(&stored);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "2",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            "EAAnewapppage0123456789abcdefghijklmnop",
            "--replace",
            "--webhook-url",
            &url,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("403"),
        "the old app's token is not sent: {}",
        out(&o)
    );
    let token = creds_json(&home)["webhook"]["verify_token"]
        .as_str()
        .expect("a fresh token")
        .to_string();
    assert_eq!(token.len(), 64);
    assert_ne!(token, "old-verify-token");
}

#[tokio::test]
async fn token_refresh_does_not_pair_the_env_app_id_with_another_apps_secret() {
    let server = mock_graph().await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 2 * 86_400)));
    let mut cmd = cmd_with(&home, &server, &["token", "refresh"]);
    cmd.env("META_APP_ID", "2");
    let o = run(cmd).await;
    assert_eq!(code(&o), 2, "{}\n{}", out(&o), err(&o));
    assert!(
        err(&o).contains("belongs to app 1 but META_APP_ID is 2"),
        "{}",
        err(&o)
    );
    let all = server.received_requests().await.expect("recording");
    assert!(
        all.is_empty(),
        "the stored secret never leaves the machine for app 2"
    );
}

#[tokio::test]
async fn setup_login_skips_whatsapp_scopes_when_a_system_user_token_is_kept() {
    let server = mock_graph().await;
    let long = "EAALONGTOKEN0123456789abcdefghijklmnop";
    mock_login_chain(&server, long, Some(1)).await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    let home = TempHome::new();
    let system = "stored-system-user-token-0123456789";
    home.write_credentials(&whatsapp_credentials(system));
    let port = free_port().to_string();
    let mut proc = Proc::spawn(
        cmd_with(&home, &server, &login_args(&port, &["--whatsapp"])),
        false,
    );
    let line = proc.read_until("dialog/oauth");
    let params = dialog_params(&line);
    let scope = pair(&params, "scope").expect("scope").to_string();
    assert!(!scope.contains("whatsapp"), "{scope}");
    assert!(scope.contains("pages_messaging"), "{scope}");
    let state = pair(&params, "state").expect("state").to_string();
    let resp = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state={state}"
        ))
        .send()
        .await
        .expect("callback");
    assert_eq!(resp.status(), 200);
    let (status, stdout, stderr) = proc.finish();
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert_eq!(creds_json(&home)["whatsapp"]["token"], json!(system));
}

fn wa_scopes() -> Vec<&'static str> {
    let mut scopes = ALL_MESSENGER.to_vec();
    scopes.extend([
        "whatsapp_business_messaging",
        "whatsapp_business_management",
    ]);
    scopes
}

async fn dead_token_mock(server: &MockServer, dead: &str) {
    Mock::given(method("GET"))
        .and(path("/v26.0/debug_token"))
        .and(query_param("input_token", dead))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
            "app_id": "1", "is_valid": false, "scopes": [],
            "error": {"code": 190, "message": "token revoked"}}})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn setup_login_asks_for_whatsapp_scopes_again_when_the_stored_system_token_is_dead() {
    let server = mock_graph().await;
    let dead = "dead-stored-system-user-token-0123456789";
    dead_token_mock(&server, dead).await;
    let long = "EAALONGTOKEN0123456789abcdefghijklmnop";
    mock_login_chain_scoped(&server, long, Some(1), &wa_scopes()).await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&whatsapp_credentials(dead));
    let port = free_port().to_string();
    let mut proc = Proc::spawn(
        cmd_with(&home, &server, &login_args(&port, &["--whatsapp"])),
        false,
    );
    let line = proc.read_until("dialog/oauth");
    let params = dialog_params(&line);
    let scope = pair(&params, "scope").expect("scope").to_string();
    assert!(scope.contains("whatsapp_business_management"), "{scope}");
    let state = pair(&params, "state").expect("state").to_string();
    reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state={state}"
        ))
        .send()
        .await
        .expect("callback");
    let (status, stdout, stderr) = proc.finish();
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("using your user token instead"), "{stdout}");
    let wa = creds_json(&home)["whatsapp"].clone();
    assert_eq!(wa["token_kind"], json!("user"), "{wa}");
    assert_eq!(wa["token"], json!(long));
}

#[tokio::test]
async fn setup_refuses_the_user_token_fallback_when_it_has_no_whatsapp_permissions() {
    let server = mock_graph().await;
    let dead = "dead-stored-system-user-token-0123456789";
    dead_token_mock(&server, dead).await;
    mock_login_chain(&server, "EAALONGTOKEN0123456789abcdefghijklmnop", Some(1)).await;
    let home = TempHome::new();
    home.write_credentials(&whatsapp_credentials(dead));
    let port = free_port().to_string();
    let mut proc = Proc::spawn(
        cmd_with(&home, &server, &login_args(&port, &["--whatsapp"])),
        false,
    );
    let line = proc.read_until("dialog/oauth");
    let state = pair(&dialog_params(&line), "state")
        .expect("state")
        .to_string();
    reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state={state}"
        ))
        .send()
        .await
        .expect("callback");
    let (status, stdout, _) = proc.finish();
    assert_eq!(status, 1, "{stdout}");
    assert!(
        stdout.contains("your user token has no WhatsApp permissions"),
        "{stdout}"
    );
    assert!(stdout.contains("--rerequest"), "{stdout}");
    assert_eq!(
        creds_json(&home)["whatsapp"]["token"],
        json!(dead),
        "the stored token stays"
    );
}

#[tokio::test]
async fn setup_skips_a_sign_in_that_would_ask_for_nothing() {
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    page_mocks(&server, "42", "Test Page").await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": "never"})))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempHome::new();
    let system = "stored-system-user-token-0123456789";
    home.write_credentials(&whatsapp_credentials(system));
    let before = creds_json(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--login",
            "--no-messenger",
            "--whatsapp",
            "--no-browser",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("no sign-in is needed"), "{}", out(&o));
    let after = creds_json(&home);
    assert_eq!(
        after["user_token"], before["user_token"],
        "the stored user token is untouched"
    );
    assert_eq!(after["whatsapp"]["token"], json!(system));
}

#[tokio::test]
async fn setup_accepts_a_credentials_file_without_an_app_id() {
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "5", "name": "Bot System User"}),
    )
    .await;
    let home = TempHome::new();
    let system = "imported-system-user-token-0123456789";
    let imported = run(cmd_with(
        &home,
        &server,
        &[
            "token",
            "import",
            "--whatsapp-token",
            system,
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
        ],
    ))
    .await;
    assert_eq!(code(&imported), 0, "{}\n{}", out(&imported), err(&imported));
    assert!(
        creds_json(&home)["app_id"].is_null(),
        "no app was configured during the import"
    );

    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--stored",
            "--whatsapp",
            "--no-messenger",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let creds = creds_json(&home);
    assert_eq!(creds["app_id"], json!("1"));
    assert_eq!(
        creds["whatsapp"]["token"],
        json!(system),
        "the imported token is kept"
    );
    assert_eq!(creds["whatsapp"]["token_kind"], json!("system_user"));
}

async fn discovery_mocks(server: &MockServer) {
    get_json(
        server,
        "/v26.0/me/businesses",
        json!({"data": [{"id": "B1", "name": "Biz"}]}),
    )
    .await;
    get_json(
        server,
        "/v26.0/B1/owned_whatsapp_business_accounts",
        json!({"data": [{"id": "W1", "name": "My WABA"}]}),
    )
    .await;
    get_json(
        server,
        "/v26.0/W1/phone_numbers",
        json!({"data": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]}),
    )
    .await;
    get_json(
        server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    get_json(
        server,
        "/v26.0/W1/subscribed_apps",
        json!({"data": [{"whatsapp_business_api_data": {"id": "1"}}]}),
    )
    .await;
}

#[tokio::test]
async fn setup_names_the_secret_flags_when_the_stored_secret_was_rejected() {
    let server = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Invalid OAuth access token - Cannot parse access token", "type": "OAuthException", "code": 190}})))
        .mount(&server)
        .await;
    let home = TempHome::new();
    home.write_credentials(&fixture(Some(now() + 40 * 86_400)));
    let before = creds_text(&home);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains("stored in credentials.json was rejected"),
        "{text}"
    );
    assert!(text.contains("--app-secret-env"), "{text}");
    assert_eq!(creds_text(&home), before);
}

#[tokio::test]
async fn setup_webhook_failure_hint_keeps_the_flags_that_shaped_the_run() {
    let server = mock_graph().await;
    whatsapp_webhook_mocks(&server, None).await;
    let url = "http://127.0.0.1:9/webhook".to_string();
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &whatsapp_args(&url, &["--webhook-override", "--yes"]),
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains(&format!(
            "rerun `meta-mcp setup --stored --webhook-url {url} --no-messenger --webhook-override --yes`"
        )),
        "{text}"
    );
    assert!(!text.contains("--verify-token"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_login_without_pages_continues_with_whatsapp() {
    let server = mock_graph().await;
    let long = "EAALONGTOKEN0123456789abcdefghijklmnop";
    mock_login_chain_with(&server, long, Some(1), &wa_scopes(), json!({"data": []})).await;
    discovery_mocks(&server).await;
    let home = TempHome::new();
    let port = free_port().to_string();
    let mut proc = Proc::spawn(
        cmd_with(
            &home,
            &server,
            &login_args(
                &port,
                &["--whatsapp", "--waba-id", "W1", "--phone-number-id", "P1"],
            ),
        ),
        false,
    );
    let line = proc.read_until("dialog/oauth");
    let state = pair(&dialog_params(&line), "state")
        .expect("state")
        .to_string();
    reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{port}/callback?code=abc&state={state}"
        ))
        .send()
        .await
        .expect("callback");
    let (status, stdout, stderr) = proc.finish();
    assert_eq!(status, 0, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("Messenger is skipped, WhatsApp continues"),
        "{stdout}"
    );
    let creds = creds_json(&home);
    assert_eq!(creds["pages"], json!([]));
    assert_eq!(creds["whatsapp"]["phone_numbers"][0]["id"], json!("P1"));
    assert_eq!(creds["whatsapp"]["token"], json!(long));
}

#[tokio::test]
async fn setup_stored_user_token_alone_can_set_up_whatsapp() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &wa_scopes(), now() + 40 * 86_400).await;
    discovery_mocks(&server).await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["pages"] = json!([]);
    creds["user_token"]["scopes"] = json!(wa_scopes());
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--stored",
            "--whatsapp",
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("Messenger is skipped"), "{}", out(&o));
    let after = creds_json(&home);
    assert_eq!(after["whatsapp"]["phone_numbers"][0]["id"], json!("P1"));
    assert_eq!(after["whatsapp"]["token_kind"], json!("user"));
    assert_eq!(after["whatsapp"]["token"], json!(USER_TOKEN));
}

#[tokio::test]
async fn token_refresh_whatsapp_runs_even_when_the_user_token_is_dead() {
    let server = mock_graph().await;
    let fresh = "EAAfreshwatoken0123456789abcdefghijklm";
    let system = "EAAsystemuser0123456789abcdefghijklmnop";
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("fb_exchange_token", USER_TOKEN))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Session has expired", "type": "OAuthException", "code": 190}})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("fb_exchange_token", system))
        .and(query_param("set_token_expires_in_60_days", "true"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": fresh, "expires_in": 5_184_000})),
        )
        .expect(1)
        .mount(&server)
        .await;
    debug_mock(
        &server,
        "SYSTEM_USER",
        &[
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ],
        now() + 20 * 86_400,
    )
    .await;
    let home = TempHome::new();
    home.write_credentials(&whatsapp_credentials(system));
    let o = run(cmd_with(
        &home,
        &server,
        &["token", "refresh", "--whatsapp"],
    ))
    .await;
    assert_eq!(
        code(&o),
        1,
        "the dead user token still fails the command: {}\n{}",
        out(&o),
        err(&o)
    );
    assert!(
        out(&o).contains("user token is no longer valid"),
        "{}",
        out(&o)
    );
    let creds = creds_json(&home);
    assert_eq!(
        creds["whatsapp"]["token"],
        json!(fresh),
        "the WhatsApp token was refreshed anyway"
    );
    assert_eq!(
        creds["user_token"]["token"],
        json!(USER_TOKEN),
        "the dead user token is untouched"
    );
}

#[tokio::test]
async fn setup_does_not_record_a_url_when_every_page_subscription_fails() {
    let (port, _dir) = start_receiver("v").await;
    let url = format!("http://127.0.0.1:{port}/webhook");
    let server = mock_graph().await;
    page_token_mocks(&server).await;
    get_json(&server, "/v26.0/1/subscriptions", json!({"data": []})).await;
    post_json(&server, "/v26.0/1/subscriptions", json!({"success": true})).await;
    Mock::given(method("POST"))
        .and(path("/v26.0/42/subscribed_apps"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "(#200) pages_manage_metadata is missing", "type": "OAuthException", "code": 200}})))
        .mount(&server)
        .await;
    get_json(&server, "/v26.0/42/subscribed_apps", json!({"data": []})).await;
    let home = TempHome::new();
    let o = run(cmd_with(&home, &server, &webhook_args(&url, &[]))).await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("subscribing failed"), "{}", out(&o));
    assert!(
        out(&o).contains("no subscription of this run sends events"),
        "{}",
        out(&o)
    );
    assert!(creds_json(&home)["webhook"]["public_url"].is_null());
}

#[tokio::test]
async fn setup_user_token_route_names_the_login_command_for_missing_scopes() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &["pages_messaging"], now() + 40 * 86_400).await;
    get_json(
        &server,
        "/v26.0/me/accounts",
        json!({"data": [{"id": "42", "name": "Test Page", "tasks": ["MESSAGING"], "access_token": PAGE_TOKEN}]}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--user-token",
            USER_TOKEN,
            "--all-pages",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let text = out(&o);
    assert!(
        text.contains(
            "missing scope pages_show_list: sign in with `meta-mcp setup --login --rerequest`"
        ),
        "{text}"
    );
    assert!(!text.contains("rerun with --rerequest"), "{text}");
}

#[tokio::test]
async fn setup_stops_before_discovery_when_the_user_token_has_no_whatsapp_scopes() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(
        &server,
        "USER",
        &["business_management"],
        now() + 40 * 86_400,
    )
    .await;
    discovery_mocks(&server).await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--user-token",
            USER_TOKEN,
            "--whatsapp",
            "--no-messenger",
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 1, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("your user token has no WhatsApp permissions"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("--whatsapp --rerequest"), "{}", out(&o));
    let all = server.received_requests().await.expect("recording");
    assert!(
        requests_to(&all, "/v26.0/me/businesses").is_empty(),
        "no discovery was attempted"
    );
}

#[tokio::test]
async fn setup_warns_about_a_page_token_that_expires_and_doctor_names_the_right_fix() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "PAGE", &ALL_MESSENGER, now() + 3_000).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    let home = TempHome::new();
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--app-secret",
            APP_SECRET,
            "--page-token",
            PAGE_TOKEN,
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("Page token is valid but expires within the hour"),
        "{}",
        out(&o)
    );
    assert!(!out(&o).contains("✅ Page token is valid"), "{}", out(&o));

    let mut creds = fixture(None);
    creds["user_token"] = Value::Null;
    let other = TempHome::new();
    other.write_credentials(&creds);
    let soon = mock_graph().await;
    app_mock(&soon).await;
    debug_mock(&soon, "PAGE", &ALL_MESSENGER, now() + 3 * 86_400).await;
    page_mocks(&soon, "42", "Test Page").await;
    let d = run(cmd_with(&other, &soon, &["doctor"])).await;
    assert_eq!(code(&d), 0, "{}\n{}", out(&d), err(&d));
    assert!(
        out(&d).contains("meta-mcp setup --login --page-id 42"),
        "{}",
        out(&d)
    );
    assert!(
        !out(&d).contains("meta-mcp token refresh"),
        "no user token to refresh with: {}",
        out(&d)
    );
}

#[tokio::test]
async fn doctor_scope_fix_asks_for_whatsapp_when_the_user_token_serves_it() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "USER", &ALL_MESSENGER, now() + 40 * 86_400).await;
    page_mocks(&server, "42", "Test Page").await;
    get_json(
        &server,
        "/v26.0/P1",
        json!({"display_phone_number": "+1 555 0100", "verified_name": "Shop", "status": "CONNECTED"}),
    )
    .await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["whatsapp"] = json!({"waba_id": "W1", "token": USER_TOKEN, "token_kind": "user",
        "phone_numbers": [{"id": "P1", "display_phone_number": "+1 555 0100", "verified_name": "Shop"}]});
    home.write_credentials(&creds);
    let o = run(cmd_with(&home, &server, &["doctor"])).await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    assert!(
        out(&o).contains("Fix: meta-mcp setup --login --rerequest --whatsapp"),
        "{}",
        out(&o)
    );
}

#[tokio::test]
async fn setup_page_token_route_can_use_the_stored_user_token_for_whatsapp() {
    let server = mock_graph().await;
    app_mock(&server).await;
    debug_mock(&server, "PAGE", &wa_scopes(), 0).await;
    get_json(
        &server,
        "/v26.0/me",
        json!({"id": "42", "name": "Test Page"}),
    )
    .await;
    page_mocks(&server, "42", "Test Page").await;
    discovery_mocks(&server).await;
    let home = TempHome::new();
    let mut creds = fixture(Some(now() + 40 * 86_400));
    creds["pages"] = json!([]);
    creds["user_token"]["scopes"] = json!(wa_scopes());
    home.write_credentials(&creds);
    let o = run(cmd_with(
        &home,
        &server,
        &[
            "setup",
            "--app-id",
            "1",
            "--page-token",
            PAGE_TOKEN,
            "--whatsapp",
            "--waba-id",
            "W1",
            "--phone-number-id",
            "P1",
            "--non-interactive",
        ],
    ))
    .await;
    assert_eq!(code(&o), 0, "{}\n{}", out(&o), err(&o));
    let after = creds_json(&home);
    assert_eq!(after["pages"][0]["id"], json!("42"));
    assert_eq!(
        after["whatsapp"]["token"],
        json!(USER_TOKEN),
        "the stored user token serves WhatsApp"
    );
    assert_eq!(after["whatsapp"]["token_kind"], json!("user"));
}
