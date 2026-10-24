//! The binary over stdio: tool listing, read-only mode, a status call and the inbox tools.

mod common;

use std::time::{Duration, Instant};

use common::{Session, TempHome, blocking, command, mock_graph, tool_json, tool_text};
use meta_mcp::inbox::{Event, EventKind, Inbox, ListFilter, Platform};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use meta_mcp::mcp::{ALL_TOOL_NAMES, READ_ONLY_TOOL_NAMES};
const OFFLINE: &str = "http://127.0.0.1:9";

fn sample_event(id: &str, ts: i64) -> Event {
    Event {
        ts,
        account_id: "PAGE1".into(),
        sender: "PSID1".into(),
        recipient: "PAGE1".into(),
        text: Some(format!("hello {id}")),
        raw: json!({"mid": id}),
        received_at: ts,
        ..Event::new(
            format!("messenger:{id}"),
            Platform::Messenger,
            EventKind::Message,
        )
    }
}

#[test]
fn lists_core_tools() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    assert_eq!(s.tool_names(), ALL_TOOL_NAMES);
}

#[test]
fn no_arguments_means_stdio() {
    use std::io::{BufRead, BufReader, Write};
    let home = TempHome::new();
    let mut child = command(&home, OFFLINE, &[])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}});
    writeln!(stdin, "{init}").expect("write");
    stdin.flush().expect("flush");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read");
    let reply: Value = serde_json::from_str(&line).expect("json");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        json!("meta-mcp"),
        "{reply}"
    );
    drop(stdin);
    let status = child.wait().expect("exit after stdin closes");
    assert!(status.success());
}

#[test]
fn read_only_hides_write_tools() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[("META_MCP_READ_ONLY", "1")]);
    let names = s.tool_names();
    assert!(
        !names.contains(&"inbox_mark_processed".to_string()),
        "{names:?}"
    );
    assert_eq!(names, READ_ONLY_TOOL_NAMES);
}

#[test]
fn initialize_reports_identity_and_instructions() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    let init = s.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    let info = &init["result"];
    assert_eq!(info["serverInfo"]["name"], json!("meta-mcp"));
    assert_eq!(
        info["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    let text = info["instructions"].as_str().expect("instructions");
    assert!(
        text.starts_with("Meta Graph API MCP server: Facebook Messenger (Page inbox)"),
        "{text}"
    );
    assert!(text.contains("untrusted third-party content"));
    assert!(info["capabilities"]["tools"].is_object());
}

#[test]
fn tool_metadata_is_annotated() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    let resp = s.request("tools/list", json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools");
    let by_name = |n: &str| {
        tools
            .iter()
            .find(|t| t["name"] == json!(n))
            .unwrap_or_else(|| panic!("{n}"))
    };
    for read_only in ["meta_status", "inbox_list", "inbox_wait"] {
        assert_eq!(
            by_name(read_only)["annotations"]["readOnlyHint"],
            json!(true),
            "{read_only}"
        );
    }
    let mark = &by_name("inbox_mark_processed")["annotations"];
    assert_eq!(mark["readOnlyHint"], json!(false));
    assert_eq!(mark["destructiveHint"], json!(false));
    assert_eq!(mark["idempotentHint"], json!(true));
    for t in tools {
        let d = t["description"].as_str().expect("description");
        assert!(
            d.len() < 1000,
            "{} description is {} chars",
            t["name"],
            d.len()
        );
        assert_eq!(t["inputSchema"]["type"], json!("object"));
    }
    let list = &by_name("inbox_list")["inputSchema"]["properties"];
    for p in [
        "platform",
        "kinds",
        "since",
        "unprocessed_only",
        "account_id",
        "limit",
        "include_raw",
    ] {
        assert!(
            list[p]["description"].is_string(),
            "inbox_list.{p} lacks a description: {list}"
        );
    }
    assert_eq!(
        by_name("inbox_mark_processed")["inputSchema"]["required"],
        json!(["ids"])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_status_via_wiremock() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PAGE1"))
        .and(header("authorization", "Bearer EAAPAGETOKEN0123456789"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "PAGE1", "name": "Mock Page"})),
        )
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PAGE1/subscribed_apps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id": "APP1", "name": "App", "subscribed_fields": ["messages", "messaging_postbacks"]}
        ]})))
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let status = blocking(move || {
        let home = TempHome::new();
        let mut s = Session::start(
            &home,
            &uri,
            &[
                ("META_PAGE_ACCESS_TOKEN", "EAAPAGETOKEN0123456789"),
                ("META_PAGE_ID", "PAGE1"),
                ("META_APP_SECRET", "APP-SECRET-VALUE"),
                ("META_APP_ID", "APP1"),
            ],
        );
        let resp = s.call_tool("meta_status", json!({}));
        assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
        (tool_text(&resp), tool_json(&resp))
    })
    .await;
    let (text, v) = status;
    assert_eq!(v["pages"][0]["id"], json!("PAGE1"));
    assert_eq!(v["pages"][0]["name"], json!("Mock Page"));
    assert_eq!(v["pages"][0]["token_ok"], json!(true));
    assert_eq!(
        v["pages"][0]["subscribed_fields"],
        json!(["messages", "messaging_postbacks"])
    );
    assert!(v["pages"][0].get("error").is_none());
    assert_eq!(v["app_id"], json!("APP1"));
    assert_eq!(v["graph_version"], json!("v26.0"));
    assert_eq!(v["read_only"], json!(false));
    assert!(v["whatsapp"].is_null() && v["user_token"].is_null());
    assert_eq!(v["webhook"]["verify_token_set"], json!(false));
    assert_eq!(v["webhook"]["listen"], json!("127.0.0.1:8787"));
    assert_eq!(v["inbox"]["total"], json!(0));
    assert!(v["files_dir"].is_null());
    for secret in ["EAAPAGETOKEN0123456789", "APP-SECRET-VALUE"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let reqs = graph.received_requests().await.expect("recording");
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs.iter()
            .all(|r| r.url.query_pairs().any(|(k, _)| k == "appsecret_proof"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_status_reports_failures_without_failing_the_call() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PAGE1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Error validating access token", "type": "OAuthException", "code": 190, "fbtrace_id": "T"}})))
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PN1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "display_phone_number": "+1 555 0100", "verified_name": "Shop", "quality_rating": "GREEN"})))
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let v = blocking(move || {
        let home = TempHome::new();
        home.write_credentials(&json!({
            "version": 1, "app_id": "APP1",
            "user_token": {"token": "EAAUSERTOKEN0123456789", "expires_at": 4_102_444_800_i64, "scopes": ["pages_messaging"], "obtained_at": 1_700_000_000},
            "pages": [{"id": "PAGE1", "name": "Stored Page", "token": "EAABADTOKEN0123456789"}],
            "whatsapp": {"waba_id": "W1", "token": "EAAWATOKEN0123456789", "token_kind": "system_user",
                         "phone_numbers": [{"id": "PN1", "display_phone_number": "", "verified_name": ""}]},
            "webhook": {"verify_token": "vt-secret", "public_url": "https://hooks.example.org/webhook"}
        }));
        let mut s = Session::start(&home, &uri, &[]);
        let resp = s.call_tool("meta_status", json!({}));
        assert_ne!(resp["result"]["isError"], json!(true), "{resp}");
        tool_json(&resp)
    })
    .await;
    let page = &v["pages"][0];
    assert_eq!(page["token_ok"], json!(false));
    assert_eq!(page["name"], json!("Stored Page"));
    assert!(page["subscribed_fields"].is_null());
    let err = page["error"].as_str().expect("error string");
    assert!(
        err.contains("190") && err.contains("meta-mcp setup"),
        "{err}"
    );
    let phone = &v["whatsapp"]["phone_numbers"][0];
    assert_eq!(phone["token_ok"], json!(true));
    assert_eq!(phone["display_phone_number"], json!("+1 555 0100"));
    assert_eq!(phone["quality_rating"], json!("GREEN"));
    assert_eq!(v["whatsapp"]["waba_id"], json!("W1"));
    assert_eq!(v["user_token"]["scopes"], json!(["pages_messaging"]));
    assert!(v["user_token"]["days_left"].as_i64().expect("days") > 1000);
    assert_eq!(
        v["webhook"]["public_url"],
        json!("https://hooks.example.org/webhook")
    );
    assert_eq!(v["webhook"]["verify_token_set"], json!(true));
    let text = v.to_string();
    for secret in [
        "EAAUSERTOKEN0123456789",
        "EAABADTOKEN0123456789",
        "EAAWATOKEN0123456789",
        "vt-secret",
    ] {
        assert!(!text.contains(secret), "{secret} leaked");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn page_id_is_resolved_with_get_me_when_missing() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me"))
        .and(header("authorization", "Bearer EAAPAGETOKEN0123456789"))
        .and(query_param_is_missing("access_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "RESOLVED1", "name": "Auto Page"})),
        )
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/RESOLVED1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "RESOLVED1", "name": "Auto Page"})),
        )
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/RESOLVED1/subscribed_apps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let v = blocking(move || {
        let home = TempHome::new();
        let mut s = Session::start(
            &home,
            &uri,
            &[("META_PAGE_ACCESS_TOKEN", "EAAPAGETOKEN0123456789")],
        );
        tool_json(&s.call_tool("meta_status", json!({})))
    })
    .await;
    assert_eq!(v["pages"][0]["id"], json!("RESOLVED1"));
    assert_eq!(v["pages"][0]["token_ok"], json!(true));
    assert_eq!(v["pages"][0]["subscribed_fields"], Value::Null);
}

#[test]
fn unresolvable_page_token_fails_fast() {
    let home = TempHome::new();
    let out = command(
        &home,
        OFFLINE,
        &[("META_PAGE_ACCESS_TOKEN", "EAAPAGETOKEN0123456789")],
    )
    .arg("stdio")
    .stdin(std::process::Stdio::null())
    .output()
    .expect("run");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("META_PAGE_ID"), "{err}");
    assert!(!err.contains("EAAPAGETOKEN0123456789"), "{err}");
}

#[test]
fn inbox_roundtrip() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    let inbox = Inbox::open(&home.inbox_db()).expect("open inbox");
    inbox
        .insert(&[
            sample_event("m1", 1_700_000_100),
            sample_event("m2", 1_700_000_200),
        ])
        .expect("insert");

    let listed = tool_json(&s.call_tool("inbox_list", json!({"unprocessed_only": true})));
    assert_eq!(listed["count"], json!(2));
    assert_eq!(
        listed["events"][0]["id"],
        json!("messenger:m2"),
        "newest first"
    );
    assert_eq!(listed["events"][0]["text"], json!("hello m2"));
    assert!(listed["events"][0].get("raw").is_none(), "raw is opt-in");
    assert_eq!(listed["webhook_configured"], json!(false));
    assert!(listed.get("note").is_none());

    let raw = tool_json(&s.call_tool("inbox_list", json!({"include_raw": true, "limit": 1})));
    assert_eq!(raw["count"], json!(1));
    assert_eq!(raw["events"][0]["raw"]["mid"], json!("m2"));

    let marked = tool_json(&s.call_tool("inbox_mark_processed", json!({"ids": ["messenger:m2"]})));
    assert_eq!(marked["updated"], json!(1));
    let again = tool_json(&s.call_tool(
        "inbox_mark_processed",
        json!({"ids": ["messenger:m2", "nope"]}),
    ));
    assert_eq!(again["updated"], json!(0));

    let unprocessed = tool_json(&s.call_tool("inbox_list", json!({"unprocessed_only": true})));
    assert_eq!(unprocessed["count"], json!(1));
    assert_eq!(unprocessed["events"][0]["id"], json!("messenger:m1"));
    let everything = tool_json(&s.call_tool("inbox_list", json!({})));
    assert_eq!(everything["count"], json!(2));
    let processed: Vec<bool> = everything["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["processed"].as_bool().expect("bool"))
        .collect();
    assert_eq!(processed, [true, false]);

    let since = tool_json(&s.call_tool(
        "inbox_list",
        json!({"since": 1_700_000_150, "kinds": ["message"], "platform": "messenger"}),
    ));
    assert_eq!(since["count"], json!(1));
    let none = tool_json(&s.call_tool("inbox_list", json!({"platform": "whatsapp"})));
    assert_eq!(none["count"], json!(0));
    assert!(
        none.get("note").is_none(),
        "the inbox is not empty, so no setup note"
    );
    assert_eq!(
        inbox
            .list(&ListFilter {
                unprocessed_only: true,
                ..ListFilter::default()
            })
            .expect("list")
            .len(),
        1
    );
}

#[test]
fn empty_inbox_without_webhook_explains_itself() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    let listed = tool_json(&s.call_tool("inbox_list", json!({})));
    assert_eq!(listed["count"], json!(0));
    assert_eq!(listed["webhook_configured"], json!(false));
    assert!(
        listed["note"]
            .as_str()
            .expect("note")
            .contains("meta-mcp webhook"),
        "{listed}"
    );

    let home = TempHome::new();
    let mut s = Session::start(
        &home,
        OFFLINE,
        &[
            ("META_WEBHOOK_VERIFY_TOKEN", "vt"),
            (
                "META_WEBHOOK_PUBLIC_URL",
                "https://hooks.example.org/webhook",
            ),
        ],
    );
    let listed = tool_json(&s.call_tool("inbox_list", json!({})));
    assert_eq!(listed["webhook_configured"], json!(true));
    assert!(listed.get("note").is_none());
}

#[test]
fn inbox_wait_sees_events_from_another_process() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    let inbox = Inbox::open(&home.inbox_db()).expect("open inbox");
    inbox
        .insert(&[sample_event("old", 1_700_000_000)])
        .expect("seed");
    let writer = inbox.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1200));
        writer
            .insert(&[sample_event("fresh", 1_700_000_300)])
            .expect("insert");
    });
    let started = Instant::now();
    let waited = tool_json(&s.call_tool("inbox_wait", json!({"timeout_seconds": 20})));
    handle.join().expect("join");
    assert_eq!(waited["count"], json!(1), "{waited}");
    assert_eq!(waited["events"][0]["id"], json!("messenger:fresh"));
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "wait did not return early"
    );

    let timed_out = tool_json(&s.call_tool(
        "inbox_wait",
        json!({"timeout_seconds": 1, "platform": "whatsapp"}),
    ));
    assert_eq!(timed_out["count"], json!(0));
}

#[test]
fn bad_tool_arguments_are_tool_errors() {
    let home = TempHome::new();
    let mut s = Session::start(&home, OFFLINE, &[]);
    for (tool, args, needle) in [
        ("inbox_list", json!({"platform": "sms"}), "unknown platform"),
        (
            "inbox_list",
            json!({"kinds": ["nope"]}),
            "unknown event kind",
        ),
        (
            "inbox_wait",
            json!({"kinds": ["nope"]}),
            "unknown event kind",
        ),
        ("inbox_mark_processed", json!({"ids": []}), "at least one"),
    ] {
        let resp = s.call_tool(tool, args);
        assert_eq!(resp["result"]["isError"], json!(true), "{tool}: {resp}");
        assert!(tool_text(&resp).contains(needle), "{tool}: {resp}");
    }
    let still_alive = s.call_tool("inbox_list", json!({}));
    assert_ne!(still_alive["result"]["isError"], json!(true));
}

#[test]
fn tools_subcommand_honours_read_only() {
    let home = TempHome::new();
    let run = |envs: &[(&str, &str)]| {
        let out = command(&home, OFFLINE, envs)
            .arg("tools")
            .output()
            .expect("run");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("utf8")
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(&[]), ALL_TOOL_NAMES);
    assert_eq!(run(&[("META_MCP_READ_ONLY", "true")]), READ_ONLY_TOOL_NAMES);
    assert!(
        !home.inbox_db().exists(),
        "listing tools must not create the inbox"
    );
}

#[test]
fn cli_exit_codes() {
    let home = TempHome::new();
    let code = |args: &[&str], envs: &[(&str, &str)]| {
        let out = command(&home, OFFLINE, envs)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (c, out, _) = code(&["--version"], &[]);
    assert_eq!(c, Some(0));
    assert_eq!(
        out.trim(),
        format!("meta-mcp {}", env!("CARGO_PKG_VERSION"))
    );
    let (c, out, _) = code(&["--help"], &[]);
    assert_eq!(c, Some(0));
    for sub in [
        "stdio", "http", "webhook", "setup", "doctor", "token", "tools",
    ] {
        assert!(out.contains(sub), "--help lacks {sub}: {out}");
    }
    let (c, _, err) = code(&["setup"], &[]);
    assert_eq!(
        c,
        Some(2),
        "setup without a terminal needs its values as flags"
    );
    assert!(err.contains("--app-id"), "{err}");
    let (c, _, err) = code(
        &["setup", "--webhook-url", "https://x.example/webhook"],
        &[],
    );
    assert_eq!(c, Some(2));
    assert!(err.contains("--app-id"), "{err}");
    let (c, _, _) = code(&["token"], &[]);
    assert_eq!(c, Some(2), "token needs a subcommand");
    let (c, out, _) = code(&["doctor"], &[]);
    assert_eq!(c, Some(1), "nothing configured is a failure");
    assert!(out.contains("meta-mcp setup"), "{out}");
    let (c, _, _) = code(&["bogus"], &[]);
    assert_eq!(c, Some(2));
    let (c, _, err) = code(&["tools"], &[("META_MCP_READ_ONLY", "maybe")]);
    assert_eq!(c, Some(1));
    assert!(err.contains("META_MCP_READ_ONLY"), "{err}");
    let (c, _, err) = code(&["webhook"], &[]);
    assert_eq!(c, Some(1));
    assert!(
        err.contains("META_APP_SECRET") && err.contains("META_WEBHOOK_VERIFY_TOKEN"),
        "{err}"
    );
}
