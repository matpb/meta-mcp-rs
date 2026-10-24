//! Integration tests for `meta-mcp personal daemon`, driven against a fake bash sidecar.

mod common;
#[path = "common/personal.rs"]
mod personal_common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use meta_mcp::inbox::{EventKind, ListFilter, Platform};
use meta_mcp::mcp::{ALL_TOOL_NAMES, READ_ONLY_TOOL_NAMES};
use meta_mcp::personal::client::{ClientError, DaemonClient};
use meta_mcp::personal::daemon::peer_allowed;
use meta_mcp::personal::protocol::Event;
use personal_common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedReceiver;

const LONG: Duration = Duration::from_secs(15);

async fn call(c: &DaemonClient, op: &str, params: Value) -> Result<Value, ClientError> {
    c.request(op, params, REQUEST_TIMEOUT).await
}

async fn ok(c: &DaemonClient, op: &str, params: Value) -> Value {
    call(c, op, params)
        .await
        .unwrap_or_else(|e| panic!("`{op}` failed: {e}"))
}

/// Polls `recent` until it lists `n` messages: the inbox row lands just before the ring entry.
async fn wait_recent(c: &DaemonClient, n: usize) {
    for _ in 0..500 {
        let seen = ok(c, "recent", json!({"since_ts": 0})).await["messages"]
            .as_array()
            .map_or(0, Vec::len);
        if seen >= n {
            assert_eq!(seen, n, "recent lists more messages than expected");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("recent never listed {n} messages");
}

async fn err(c: &DaemonClient, op: &str, params: Value) -> ClientError {
    match call(c, op, params).await {
        Ok(v) => panic!("`{op}` should have failed, got {v}"),
        Err(e) => e,
    }
}

fn sha_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

fn rows(env: &TestEnv, platform: Platform) -> Vec<meta_mcp::inbox::Event> {
    env.inbox()
        .list(&ListFilter {
            platform: Some(platform),
            ..ListFilter::default()
        })
        .expect("list inbox")
}

async fn next_event(rx: &mut UnboundedReceiver<Event>, what: &str) -> Event {
    tokio::time::timeout(LONG, rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("event stream closed waiting for {what}"))
}

/// Collects events until one named `last` arrives.
async fn events_until(rx: &mut UnboundedReceiver<Event>, last: &str) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let ev = next_event(rx, last).await;
        let done = ev.event == last;
        seen.push(ev);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn daemon_hello_and_status() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let daemon = env.start();
    let client = env.connect().await;
    let st = wait_status(&client, "messenger connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;

    assert_eq!(st["daemon"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(st["daemon"]["pid"], daemon.pid());
    assert_eq!(st["daemon"]["read_only"], false);
    assert_eq!(st["daemon"]["files_dir_configured"], false);
    let m = &st["platforms"]["messenger"];
    assert_eq!(m["logged_in"], true);
    assert_eq!(m["account"], json!({"id": ACCOUNT_ID, "name": "Test User"}));
    assert_eq!(m["sidecar_running"], true);
    assert_eq!(m["sidecar_version"], "0.0.0-fake");
    assert_eq!(
        m["library"],
        json!({"name": "fakelib", "version": "v0.0.1"})
    );
    assert_eq!(m["sending_blocked"], false);
    assert!(m["blocked_reason"].is_null());
    assert!(m["reconnect_in_s"].is_null());
    assert!(m["last_error"].is_null());
    let w = &st["platforms"]["whatsapp"];
    assert_eq!(w["logged_in"], false);
    assert_eq!(w["connection"], "stopped");
    assert_eq!(w["sidecar_running"], false);
    assert!(w["account"].is_null());
    assert_eq!(
        env.request_count("whatsapp", "hello"),
        0,
        "no state, no sidecar"
    );

    let hello = env.last_request("messenger", "hello").expect("hello");
    assert_eq!(hello["params"]["protocol"], 1);
    assert_eq!(hello["params"]["platform"], "messenger");
    assert_eq!(
        hello["params"]["state"],
        json!({"session": "seeded-messenger"})
    );
    let data_dir = hello["params"]["data_dir"].as_str().expect("data_dir");
    assert_eq!(
        std::path::PathBuf::from(data_dir),
        env.personal_dir().join("messenger/data")
    );
    assert_eq!(mode(&env.personal_dir().join("messenger/data")), 0o700);
    assert_eq!(mode(&env.personal_dir().join("messenger")), 0o700);
    assert_eq!(mode(&env.personal_dir()), 0o700);
    assert_eq!(env.request_count("messenger", "connect"), 1);
    assert_eq!(mode(&env.socket()), 0o600);

    for op in ["hello", "shutdown", "connect", "disconnect", "nonsense"] {
        assert_eq!(
            err(&client, op, json!({"platform": "messenger"}))
                .await
                .code,
            "unknown_op"
        );
    }
    assert_eq!(env.request_count("messenger", "shutdown"), 0);
}

#[tokio::test]
async fn login_messenger_cookies_roundtrip_persists_state() {
    let env = TestEnv::new();
    let daemon = env.start();
    let client = env.connect().await;

    let start = ok(
        &client,
        "login_start",
        json!({"platform": "messenger", "method": "cookies"}),
    )
    .await;
    assert_eq!(start["needs"], json!(["cookies"]));
    assert_eq!(
        env.last_request("messenger", "hello").expect("hello")["params"]["state"],
        Value::Null
    );
    assert!(
        !env.last_request("messenger", "login_start")
            .expect("login_start")
            .to_string()
            .contains("\"platform\""),
        "platform is stripped before the sidecar"
    );

    let cookies =
        json!({"c_user": ACCOUNT_ID, "xs": COOKIE_SECRET, "datr": "d", "sb": "s", "fr": "f"});
    let done = ok(
        &client,
        "login_finish",
        json!({"platform": "messenger", "cookies": cookies, "user_agent": "UA/1.0"}),
    )
    .await;
    assert_eq!(done["account"]["id"], ACCOUNT_ID);

    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(
        st["platforms"]["messenger"]["logged_in"], true,
        "events before the reply are applied"
    );
    assert_eq!(st["platforms"]["messenger"]["account"]["name"], "Test User");
    let state_path = env.state_path("messenger");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&state_path).expect("state file")).expect("json");
    assert_eq!(saved, json!({"session": "fake-messenger"}));
    assert_eq!(mode(&state_path), 0o600);
    assert_eq!(mode(state_path.parent().expect("dir")), 0o700);
    assert_eq!(mode(&env.personal_dir()), 0o700);
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    assert_eq!(
        env.request_count("messenger", "connect"),
        1,
        "login_finish and logged_in share one connect"
    );

    let log = env.daemon_log();
    assert!(!log.contains(COOKIE_SECRET), "cookies never reach the log");
    assert!(
        !log.contains("fake-messenger"),
        "state never reaches the log"
    );

    daemon.terminate();
    let _again = env.start();
    let client = env.connect().await;
    wait_status(&client, "connected after restart", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    let hello = env.last_request("messenger", "hello").expect("hello");
    assert_eq!(
        hello["params"]["state"],
        json!({"session": "fake-messenger"}),
        "state is passed back"
    );
    assert_eq!(env.request_count("messenger", "hello"), 2);
}

#[tokio::test]
async fn login_whatsapp_qr_events_reach_client() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let client = env.connect().await;
    let mut events = client.subscribe().await.expect("subscribe");

    let reply = ok(
        &client,
        "login_start",
        json!({"platform": "whatsapp", "method": "qr", "full_history": true, "device_name": "laptop"}),
    )
    .await;
    assert_eq!(reply, json!({}));
    let seen = events_until(&mut events, "connection").await;
    let names: Vec<&str> = seen.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        names,
        ["qr", "qr", "logged_in", "connection"],
        "ready and state_updated are not forwarded"
    );
    assert_eq!(seen[0].data["code"], "QR-CODE-ONE");
    assert_eq!(seen[1].data["expires_in_s"], 20);
    for ev in &seen {
        assert_eq!(ev.data["platform"], "whatsapp", "{ev:?}");
    }
    assert_eq!(seen[2].data["account"]["id"], ACCOUNT_ID);
    let request = env
        .last_request("whatsapp", "login_start")
        .expect("login_start");
    assert_eq!(
        request["params"],
        json!({"method": "qr", "full_history": true, "device_name": "laptop"})
    );
    let st = wait_status(&client, "whatsapp connected", LONG, |st| {
        st["platforms"]["whatsapp"]["connection"] == "connected"
    })
    .await;
    assert_eq!(st["platforms"]["whatsapp"]["logged_in"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(env.state_path("whatsapp")).expect("state"))
            .expect("json"),
        json!({"session": "fake-whatsapp"})
    );
}

#[tokio::test]
async fn whatsapp_passkey_flow_forwards_events_and_keeps_secrets_out_of_the_log() {
    let env = TestEnv::new();
    env.knob("whatsapp", "passkey", true);
    let _daemon = env.start();
    let client = env.connect().await;
    let mut events = client.subscribe().await.expect("subscribe");

    ok(
        &client,
        "login_start",
        json!({"platform": "whatsapp", "method": "qr"}),
    )
    .await;
    let req = next_event(&mut events, "passkey_request").await;
    assert_eq!(req.event, "passkey_request");
    assert_eq!(req.data["public_key"]["challenge"], "abc");
    ok(
        &client,
        "login_finish",
        json!({"platform": "whatsapp", "passkey_response": {"id": "cred", "signature": "SIG-SECRET-xyz"}}),
    )
    .await;
    let conf = next_event(&mut events, "passkey_confirmation").await;
    assert_eq!(conf.data["code"], "123456");
    ok(
        &client,
        "login_finish",
        json!({"platform": "whatsapp", "confirm": true}),
    )
    .await;
    events_until(&mut events, "logged_in").await;
    wait_status(&client, "logged in", LONG, |st| {
        st["platforms"]["whatsapp"]["logged_in"] == true
    })
    .await;
    assert!(env.state_path("whatsapp").is_file());
    assert!(!env.daemon_log().contains("SIG-SECRET-xyz"));
}

#[tokio::test]
async fn inbound_message_event_lands_in_inbox_with_platform_messenger_personal() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;

    let line = message_event("mid.in1", "t1", "in", "hello from Alice");
    env.emit("messenger", &line);
    wait_until("the message in the inbox", LONG, || {
        !rows(&env, Platform::MessengerPersonal).is_empty()
    })
    .await;
    let all = rows(&env, Platform::MessengerPersonal);
    assert_eq!(all.len(), 1);
    let row = &all[0];
    assert_eq!(row.id, "messenger_personal:t1:mid.in1");
    assert_eq!(row.platform, Platform::MessengerPersonal);
    assert_eq!(row.kind, EventKind::Message);
    assert_eq!(row.account_id, ACCOUNT_ID);
    assert_eq!(row.sender, "u2");
    assert_eq!(row.recipient, "t1");
    assert_eq!(row.text.as_deref(), Some("hello from Alice"));
    assert_eq!(row.ts, 1_700_000_100);
    assert_eq!(row.attachments.len(), 1);
    assert_eq!(row.attachments[0].media_id.as_deref(), Some("a1"));
    assert_eq!(row.attachments[0].mime_type.as_deref(), Some("image/jpeg"));
    assert!(!row.processed);
    assert!(rows(&env, Platform::WhatsAppPersonal).is_empty());
    assert!(
        rows(&env, Platform::Messenger).is_empty(),
        "never the Page platform"
    );

    // The same event again is idempotent; the other kinds map too.
    env.emit("messenger", &line);
    env.emit(
        "messenger",
        &message_event("mid.out1", "t1", "out", "sent elsewhere"),
    );
    env.emit(
        "messenger",
        &json!({"event": "reaction", "data": {"thread_id": "t1", "message_id": "mid.in1",
            "sender_id": "u2", "emoji": "\u{1F44D}", "ts": 1_700_000_200}})
        .to_string(),
    );
    env.emit(
        "messenger",
        &json!({"event": "receipt", "data": {"thread_id": "t1", "kind": "read",
            "sender_id": "u2", "up_to_ts": 1_700_000_300}})
        .to_string(),
    );
    env.emit(
        "messenger",
        &json!({"event": "history", "data": {"thread_id": "t9", "complete": true, "messages": [
            {"id": "h1", "ts": 5, "sender": {"id": "u3", "is_me": false}, "text": "old"},
            {"id": "h2", "ts": 6, "sender": {"id": "me", "is_me": true}, "text": "older"}]}})
        .to_string(),
    );
    wait_until("all kinds in the inbox", LONG, || {
        rows(&env, Platform::MessengerPersonal).len() == 6
    })
    .await;
    let all = rows(&env, Platform::MessengerPersonal);
    let kind_of = |id: &str| {
        all.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("{id}"))
            .kind
    };
    assert_eq!(kind_of("messenger_personal:t1:mid.out1"), EventKind::Echo);
    assert_eq!(
        kind_of("messenger_personal:reaction:t1:mid.in1:u2:1700000200"),
        EventKind::Reaction
    );
    assert_eq!(
        kind_of("messenger_personal:read:t1:u2:1700000300"),
        EventKind::Read
    );
    assert_eq!(kind_of("messenger_personal:t9:h1"), EventKind::Message);
    assert_eq!(kind_of("messenger_personal:t9:h2"), EventKind::Echo);

    let recent = ok(&client, "recent", json!({"since_ts": 0})).await;
    let ids: Vec<&str> = recent["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids.len(), 2, "live message events only: {recent}");
    assert!(
        ids.contains(&"messenger_personal:t1:mid.in1")
            && ids.contains(&"messenger_personal:t1:mid.out1")
    );
    assert_eq!(recent["truncated"], false);
    assert!(
        !recent.to_string().contains("hello from Alice"),
        "the ring never holds text"
    );
    let first = &recent["messages"][0];
    assert_eq!(first["platform"], "messenger_personal");
    assert_eq!(first["thread_id"], "t1");
    assert_eq!(first["ts"], 1_700_000_100);
    let later = ok(
        &client,
        "recent",
        json!({"since_ts": 1_700_000_101, "platform": "messenger"}),
    )
    .await;
    assert_eq!(later["messages"], json!([]));
    let st = ok(&client, "status", Value::Null).await;
    assert!(st["platforms"]["messenger"]["last_event_at"].is_i64());
    assert!(
        !env.daemon_log().contains("hello from Alice"),
        "message text never reaches the log"
    );
}

#[tokio::test]
async fn wait_for_message_returns_new_event() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let client = Arc::new(client);

    env.emit(
        "messenger",
        &message_event("mid.old", "t1", "in", "before the call"),
    );
    wait_until("the old message stored", LONG, || {
        !rows(&env, Platform::MessengerPersonal).is_empty()
    })
    .await;
    wait_recent(&client, 1).await;

    let waiter = {
        let c = Arc::clone(&client);
        tokio::spawn(async move { ok(&c, "wait_for_message", json!({"timeout_ms": 20_000})).await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    env.emit(
        "messenger",
        &message_event("mid.echo", "t1", "out", "my own"),
    );
    env.emit(
        "messenger",
        &json!({"event": "history", "data": {"thread_id": "t1", "messages": [{"id": "hh", "ts": 1, "text": "hist"}]}}).to_string(),
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    env.emit(
        "messenger",
        &message_event("mid.new", "t1", "in", "fresh one"),
    );
    let got = tokio::time::timeout(LONG, waiter)
        .await
        .expect("waiter in time")
        .expect("join");
    assert_eq!(got["timed_out"], false);
    assert_eq!(got["lagged"], false);
    assert_eq!(got["message"]["id"], "messenger_personal:t1:mid.new");
    assert_eq!(got["message"]["text"], "fresh one");
    assert_eq!(got["message"]["platform"], "messenger_personal");
    assert!(got["message"].get("raw").is_none(), "raw is removed");

    let timeout = ok(&client, "wait_for_message", json!({"timeout_ms": 150})).await;
    assert_eq!(timeout["timed_out"], true);
    assert!(timeout["message"].is_null());
    let other = ok(
        &client,
        "wait_for_message",
        json!({"timeout_ms": 600, "platform": "whatsapp"}),
    )
    .await;
    assert_eq!(other["timed_out"], true);
    for bad in [
        json!({}),
        json!({"timeout_ms": 0}),
        json!({"timeout_ms": 300_001}),
        json!({"timeout_ms": 1.5}),
    ] {
        assert_eq!(
            err(&client, "wait_for_message", bad).await.code,
            "bad_params"
        );
    }
    assert_eq!(err(&client, "recent", json!({})).await.code, "bad_params");
}

#[tokio::test]
async fn too_many_waiters_on_one_connection() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let client = Arc::new(env.connect().await);
    let mut handles = Vec::new();
    for _ in 0..16 {
        let c = Arc::clone(&client);
        handles.push(tokio::spawn(async move {
            call(&c, "wait_for_message", json!({"timeout_ms": 3000})).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let refused = err(&client, "wait_for_message", json!({"timeout_ms": 3000})).await;
    assert_eq!(refused.code, "too_many_waiters");
    assert_eq!(
        ok(&client, "status", Value::Null).await["daemon"]["read_only"],
        false,
        "other ops still work"
    );
    for h in handles {
        assert_eq!(h.await.expect("join").expect("waiter")["timed_out"], true);
    }
}

#[tokio::test]
async fn send_text_is_audited_and_proxied() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;

    let out = ok(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "  hello world \r\nsecond line  ", "reply_to": "mid.0"}),
    )
    .await;
    assert_eq!(out["message_id"], "mid.1");
    assert_eq!(out["ts"], 1_700_000_000);
    let audit_id = out["audit_id"].as_u64().expect("audit_id");
    assert!(out.get("audit_warning").is_none());

    let sent = env
        .last_request("messenger", "send_text")
        .expect("send_text");
    assert_eq!(
        sent["params"],
        json!({"thread_id": "t1", "text": "hello world \nsecond line", "reply_to": "mid.0"}),
        "cleaned text, no platform"
    );

    let lines = env.audit_lines();
    assert_eq!(lines.len(), 2);
    let (attempt, done) = (&lines[0], &lines[1]);
    assert_eq!(attempt["id"], 1);
    assert_eq!(attempt["outcome"], "attempt");
    assert_eq!(attempt["platform"], "messenger");
    assert_eq!(attempt["thread_id"], "t1");
    assert_eq!(attempt["kind"], "text");
    assert_eq!(
        attempt["body_sha256"],
        sha_hex(b"hello world \nsecond line")
    );
    assert_eq!(attempt["body_len"], 24);
    assert!(attempt.get("file_sha256").is_none());
    assert!(attempt["ts"].as_str().expect("ts").ends_with('Z'));
    assert_eq!(done["id"], audit_id);
    assert_eq!(done["outcome"], "sent");
    assert_eq!(done["attempt_id"], 1);
    assert_eq!(done["message_id"], "mid.1");
    assert_eq!(mode(&env.audit_path()), 0o600);
    let raw = std::fs::read_to_string(env.audit_path()).expect("audit");
    assert!(
        !raw.contains("hello world"),
        "the audit log never holds the text"
    );
    assert!(!env.daemon_log().contains("hello world"));

    let second = ok(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "again"}),
    )
    .await;
    assert_eq!(second["audit_id"], 4, "ids keep growing");
    assert!(
        env.last_request("messenger", "send_text").expect("req")["params"]
            .get("reply_to")
            .is_none()
    );
}

#[tokio::test]
async fn failed_and_uncertain_sends_are_audited_with_their_codes() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let params = json!({"platform": "messenger", "thread_id": "t1", "text": "x"});

    env.knob("messenger", "send_fail", true);
    let failed = err(&client, "send_text", params.clone()).await;
    assert_eq!(failed.code, "send_failed", "sidecar codes pass through");
    env.knob("messenger", "send_fail", false);

    env.knob("messenger", "send_unknown", true);
    let unknown = err(&client, "send_text", params).await;
    assert_eq!(unknown.code, "send_unknown");
    assert!(unknown.message.contains("may have been sent"), "{unknown}");
    assert!(
        unknown.message.contains("read the thread before retrying"),
        "{unknown}"
    );

    let lines = env.audit_lines();
    let outcomes: Vec<(&str, Option<&str>)> = lines
        .iter()
        .map(|l| {
            (
                l["outcome"].as_str().expect("outcome"),
                l["error_code"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("attempt", None),
            ("failed", Some("send_failed")),
            ("attempt", None),
            ("unknown", Some("send_unknown"))
        ]
    );
    assert_eq!(lines[1]["attempt_id"], 1);
    assert_eq!(lines[3]["attempt_id"], 3);
}

#[tokio::test]
async fn invalid_and_unavailable_sends_are_refused_before_the_sidecar() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let sends_before = env.request_count("messenger", "send_text");

    let cases = [
        json!({"platform": "messenger", "thread_id": "t1", "text": ""}),
        json!({"platform": "messenger", "thread_id": "t1", "text": "tab\there"}),
        json!({"platform": "messenger", "thread_id": "t1", "text": "a".repeat(4001)}),
        json!({"platform": "messenger", "thread_id": "bad id", "text": "x"}),
        json!({"platform": "messenger", "thread_id": "t1", "text": "x", "reply_to": "no spaces allowed"}),
        json!({"platform": "messenger", "text": "x"}),
        json!({"platform": "messenger", "thread_id": "t1", "text": 5}),
    ];
    for c in cases {
        assert_eq!(
            err(&client, "send_text", c.clone()).await.code,
            "bad_params",
            "{c}"
        );
    }
    assert_eq!(env.request_count("messenger", "send_text"), sends_before);
    let lines = env.audit_lines();
    assert_eq!(lines.len(), 7);
    assert!(lines.iter().all(|l| l["outcome"] == "invalid"), "{lines:?}");
    assert!(
        lines[3]["thread_id"].is_null(),
        "an invalid thread id is not recorded"
    );
    assert_eq!(lines[0]["thread_id"], "t1");
    assert_eq!(lines[0]["body_len"], 0);

    // Not logged in: refused and audited, never forwarded.
    let nl = err(
        &client,
        "send_text",
        json!({"platform": "whatsapp", "thread_id": "t1", "text": "x"}),
    )
    .await;
    assert_eq!(nl.code, "not_logged_in");
    assert_eq!(
        env.audit_lines().last().expect("line")["outcome"],
        "refused"
    );
    assert_eq!(
        err(&client, "list_threads", json!({"platform": "whatsapp"}))
            .await
            .code,
        "not_logged_in"
    );
    assert_eq!(
        err(
            &client,
            "send_text",
            json!({"platform": "sms", "thread_id": "t", "text": "x"})
        )
        .await
        .code,
        "bad_params"
    );
    assert_eq!(
        err(&client, "send_text", json!({"thread_id": "t", "text": "x"}))
            .await
            .code,
        "bad_params"
    );
    assert_eq!(
        err(&client, "send_text", json!("nope")).await.code,
        "bad_params"
    );
}

#[tokio::test]
async fn reactions_reads_and_typing_are_proxied() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;

    let r = ok(&client, "send_reaction", json!({"platform": "messenger", "thread_id": "t1", "message_id": "m1", "emoji": "\u{1F44D}"})).await;
    assert!(r["audit_id"].is_u64());
    ok(
        &client,
        "send_reaction",
        json!({"platform": "messenger", "thread_id": "t1", "message_id": "m1", "emoji": ""}),
    )
    .await;
    for bad in ["a", "\u{1F44D}x", "\u{1F44D} ", "1"] {
        let e = err(
            &client,
            "send_reaction",
            json!({"platform": "messenger", "thread_id": "t1", "message_id": "m1", "emoji": bad}),
        )
        .await;
        assert_eq!(e.code, "bad_params", "{bad:?}");
    }
    let lines = env.audit_lines();
    assert_eq!(lines.len(), 8);
    assert_eq!(lines[0]["kind"], "reaction");
    assert_eq!(lines[0]["body_len"], 1);
    assert_eq!(lines[2]["body_len"], 0, "the empty emoji is hashed");
    assert_eq!(lines[2]["body_sha256"], sha_hex(b""));
    assert_eq!(lines[4]["outcome"], "invalid");

    let before = env.audit_lines().len();
    ok(
        &client,
        "mark_read",
        json!({"platform": "messenger", "thread_id": "t1", "up_to_ts_ms": 1700000000000_i64}),
    )
    .await;
    ok(
        &client,
        "set_typing",
        json!({"platform": "messenger", "thread_id": "t1", "on": true}),
    )
    .await;
    assert_eq!(
        env.audit_lines().len(),
        before,
        "mark_read and set_typing are not audited"
    );
    let typing = env.last_request("messenger", "set_typing").expect("typing");
    assert_eq!(typing["params"], json!({"thread_id": "t1", "on": true}));
    assert_eq!(
        err(
            &client,
            "set_typing",
            json!({"platform": "messenger", "thread_id": "t1"})
        )
        .await
        .code,
        "bad_params"
    );
    assert_eq!(
        err(
            &client,
            "mark_read",
            json!({"platform": "messenger", "thread_id": "bad id"})
        )
        .await
        .code,
        "bad_params"
    );

    let threads = ok(
        &client,
        "list_threads",
        json!({"platform": "messenger", "limit": 10}),
    )
    .await;
    assert_eq!(threads["threads"][0]["id"], "t1");
    assert_eq!(
        env.last_request("messenger", "list_threads").expect("req")["params"],
        json!({"limit": 10})
    );
    let msgs = ok(
        &client,
        "fetch_messages",
        json!({"platform": "messenger", "thread_id": "t1", "limit": 5}),
    )
    .await;
    assert_eq!(msgs["messages"][0]["text"], "hello");
    assert_eq!(
        err(
            &client,
            "fetch_messages",
            json!({"platform": "messenger", "thread_id": ""})
        )
        .await
        .code,
        "bad_params"
    );
}

#[tokio::test]
async fn send_refused_in_read_only_daemon_client() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let _daemon = env.start_with(&[("META_MCP_READ_ONLY", "true")]);
    let client = env.connect().await;
    let st = wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    assert_eq!(st["daemon"]["read_only"], true);

    let e = err(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "hi"}),
    )
    .await;
    assert_eq!(e.code, "read_only");
    assert_eq!(err(&client, "send_reaction", json!({"platform": "messenger", "thread_id": "t1", "message_id": "m", "emoji": "\u{1F44D}"})).await.code, "read_only");
    assert_eq!(
        err(
            &client,
            "mark_read",
            json!({"platform": "messenger", "thread_id": "t1"})
        )
        .await
        .code,
        "read_only"
    );
    assert_eq!(env.request_count("messenger", "send_text"), 0);
    let lines = env.audit_lines();
    assert_eq!(lines.len(), 2, "mark_read is not audited");
    assert!(
        lines
            .iter()
            .all(|l| l["outcome"] == "refused" && l["error_code"] == "read_only")
    );
    ok(&client, "list_threads", json!({"platform": "messenger"})).await;
}

#[tokio::test]
async fn two_clients_share_one_sidecar() {
    let env = TestEnv::new();
    let (_daemon, first) = env.start_logged_in("messenger").await;
    let second = env.connect().await;
    let mut e1 = first.subscribe().await.expect("subscribe 1");
    let mut e2 = second.subscribe().await.expect("subscribe 2");

    env.emit(
        "messenger",
        &message_event("mid.shared", "t1", "in", "for both"),
    );
    for rx in [&mut e1, &mut e2] {
        let ev = next_event(rx, "message").await;
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data["platform"], "messenger");
        assert_eq!(ev.data["message"]["id"], "mid.shared");
    }
    let (a, b) = tokio::join!(
        call(&first, "list_threads", json!({"platform": "messenger"})),
        call(&second, "list_threads", json!({"platform": "messenger"}))
    );
    assert_eq!(a.expect("first"), b.expect("second"));
    assert_eq!(
        ok(&second, "status", Value::Null).await["platforms"],
        ok(&first, "status", Value::Null).await["platforms"]
    );
    assert_eq!(
        env.request_count("messenger", "hello"),
        1,
        "one sidecar for both clients"
    );
    assert_eq!(
        env.requests("messenger")
            .iter()
            .filter(|l| l.contains("\"op\":\"list_threads\""))
            .count(),
        2
    );
}

#[tokio::test]
async fn unsubscribed_connections_get_no_events_and_malformed_frames_disconnect() {
    let env = TestEnv::new();
    let (_daemon, _client) = env.start_logged_in("messenger").await;
    let stream = tokio::net::UnixStream::connect(env.socket())
        .await
        .expect("connect");
    let (r, w) = stream.into_split();
    let link = meta_mcp::personal::link::Link::new(r, w);
    let mut events = link.take_events().expect("events");
    link.request("status", Value::Null).await.expect("status");
    env.emit("messenger", &message_event("mid.x", "t1", "in", "ignored"));
    wait_until("the event stored", LONG, || {
        !rows(&env, Platform::MessengerPersonal).is_empty()
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .is_err(),
        "no events without subscribe"
    );

    use tokio::io::AsyncWriteExt;
    let mut raw = tokio::net::UnixStream::connect(env.socket())
        .await
        .expect("connect");
    raw.write_all(b"this is not json\n").await.expect("write");
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(LONG, tokio::io::AsyncReadExt::read(&mut raw, &mut buf))
        .await
        .expect("closed in time")
        .expect("read");
    assert_eq!(n, 0, "the daemon hangs up");
}

#[test]
fn socket_rejects_other_uid() {
    let own = current_uid();
    assert!(peer_allowed(own, own));
    assert!(!peer_allowed(own + 1, own));
    assert!(!peer_allowed(0, 1000));
}

#[tokio::test]
async fn socket_serves_our_own_uid() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let client = env.connect().await;
    assert!(ok(&client, "status", Value::Null).await["daemon"]["pid"].is_u64());
    assert!(!client.is_closed());
}

#[tokio::test]
async fn logout_clears_state() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    env.emit("messenger", &message_event("mid.l1", "t1", "in", "x"));
    wait_until("ring filled", LONG, || {
        !rows(&env, Platform::MessengerPersonal).is_empty()
    })
    .await;
    wait_recent(&client, 1).await;
    assert!(env.state_path("messenger").is_file());

    let out = ok(&client, "logout", json!({"platform": "messenger"})).await;
    assert_eq!(out, json!({"state_cleared": true}));
    assert_eq!(env.request_count("messenger", "logout"), 1);
    assert!(!env.state_path("messenger").exists());
    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(st["platforms"]["messenger"]["logged_in"], false);
    assert!(st["platforms"]["messenger"]["account"].is_null());
    assert_eq!(
        ok(&client, "recent", json!({"since_ts": 0})).await["messages"],
        json!([]),
        "ring cleared"
    );

    // Late state writes are refused until the next login_start.
    env.emit(
        "messenger",
        r#"{"event":"state_updated","data":{"state":{"session":"late"}}}"#,
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !env.state_path("messenger").exists(),
        "no state after logout"
    );
    ok(
        &client,
        "login_start",
        json!({"platform": "messenger", "method": "cookies"}),
    )
    .await;
    env.emit(
        "messenger",
        r#"{"event":"state_updated","data":{"state":{"session":"fresh"}}}"#,
    );
    wait_until("state written after login_start", LONG, || {
        env.state_path("messenger").is_file()
    })
    .await;

    // Logging out a platform that never ran still clears and reports no remote error.
    let none = ok(&client, "logout", json!({"platform": "whatsapp"})).await;
    assert_eq!(none, json!({"state_cleared": true}));
    // A null state from the sidecar deletes the file.
    env.emit(
        "messenger",
        r#"{"event":"state_updated","data":{"state":null}}"#,
    );
    wait_until("state deleted by null", LONG, || {
        !env.state_path("messenger").exists()
    })
    .await;
}

#[tokio::test]
async fn logged_out_event_clears_the_session() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let mut events = client.subscribe().await.expect("subscribe");
    env.emit(
        "messenger",
        r#"{"event":"logged_out","data":{"reason":"token_invalidated"}}"#,
    );
    let ev = next_event(&mut events, "logged_out").await;
    assert_eq!(ev.event, "logged_out");
    assert_eq!(ev.data["platform"], "messenger");
    let st = wait_status(&client, "logged out", LONG, |st| {
        st["platforms"]["messenger"]["logged_in"] == false
    })
    .await;
    assert_eq!(st["platforms"]["messenger"]["reason"], "token_invalidated");
    assert!(!env.state_path("messenger").exists());
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        env.request_count("messenger", "hello"),
        1,
        "no reconnect after logged_out"
    );
}

#[tokio::test]
async fn reconnect_after_sidecar_crash_with_backoff() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let _daemon = env.start_with(&[("META_MCP_PERSONAL_BACKOFF_MS", "1500,3000")]);
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    let first_pid = env.sidecar_pid("messenger").expect("pid");

    env.kill_sidecar("messenger");
    let waiting = wait_status(&client, "reconnect scheduled", LONG, |st| {
        !st["platforms"]["messenger"]["reconnect_in_s"].is_null()
    })
    .await;
    let m = &waiting["platforms"]["messenger"];
    assert_eq!(m["sidecar_running"], false);
    assert_eq!(m["connection"], "disconnected");
    assert!(
        (1..=2).contains(&m["reconnect_in_s"].as_u64().expect("secs")),
        "{m}"
    );
    assert_eq!(m["last_error"], "sidecar_exited");
    wait_status(&client, "reconnected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
            && st["platforms"]["messenger"]["sidecar_running"] == true
    })
    .await;
    assert_ne!(env.sidecar_pid("messenger").expect("pid"), first_pid);
    assert_eq!(env.request_count("messenger", "hello"), 2);
    assert_eq!(
        env.last_request("messenger", "hello").expect("hello")["params"]["state"],
        json!({"session": "seeded-messenger"})
    );

    // Connected reset the backoff: the next crash waits the first step again, not the second.
    env.kill_sidecar("messenger");
    let waiting = wait_status(&client, "second reconnect scheduled", LONG, |st| {
        !st["platforms"]["messenger"]["reconnect_in_s"].is_null()
    })
    .await;
    assert!(
        waiting["platforms"]["messenger"]["reconnect_in_s"]
            .as_u64()
            .expect("secs")
            <= 2,
        "{waiting}"
    );
    wait_status(&client, "reconnected again", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    assert_eq!(env.request_count("messenger", "hello"), 3);
}

#[tokio::test]
async fn a_transient_fatal_event_triggers_a_reconnect() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    env.emit(
        "messenger",
        r#"{"event":"connection","data":{"state":"fatal","reason":"network_down"}}"#,
    );
    wait_until("a second hello", LONG, || {
        env.request_count("messenger", "hello") == 2
    })
    .await;
    let st = wait_status(&client, "connected again", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    assert_eq!(st["platforms"]["messenger"]["sending_blocked"], false);
}

#[tokio::test]
async fn sending_blocked_after_fatal_event() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("whatsapp").await;
    env.emit(
        "whatsapp",
        r#"{"event":"connection","data":{"state":"fatal","reason":"temporary_ban"}}"#,
    );
    let st = wait_status(&client, "blocked", LONG, |st| {
        st["platforms"]["whatsapp"]["sending_blocked"] == true
    })
    .await;
    let w = &st["platforms"]["whatsapp"];
    assert_eq!(w["blocked_reason"], "temporary_ban");
    assert_eq!(w["connection"], "fatal");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        env.request_count("whatsapp", "hello"),
        1,
        "no automatic reconnect after a ban"
    );

    let e = err(
        &client,
        "send_text",
        json!({"platform": "whatsapp", "thread_id": "123@s.whatsapp.net", "text": "hi"}),
    )
    .await;
    assert_eq!(e.code, "sending_blocked");
    assert!(e.message.contains("temporary_ban"), "{e}");
    assert!(
        e.message.contains("meta-mcp personal restart whatsapp"),
        "{e}"
    );
    assert_eq!(env.request_count("whatsapp", "send_text"), 0);
    let last = env.audit_lines().pop().expect("audit line");
    assert_eq!(last["outcome"], "refused");
    assert_eq!(last["error_code"], "sending_blocked");
    assert_eq!(last["platform"], "whatsapp");
    for reason in [
        "reachout_timelock",
        "banned",
        "device_removed",
        "client_outdated",
        "stream_replaced",
    ] {
        env.emit(
            "whatsapp",
            &json!({"event": "connection", "data": {"state": "fatal", "reason": reason}})
                .to_string(),
        );
        wait_status(&client, reason, LONG, |st| {
            st["platforms"]["whatsapp"]["blocked_reason"] == reason
        })
        .await;
    }
    assert_eq!(
        err(
            &client,
            "mark_read",
            json!({"platform": "whatsapp", "thread_id": "t"})
        )
        .await
        .code,
        "sending_blocked"
    );
}

#[tokio::test]
async fn restart_clears_sending_blocked() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("whatsapp").await;
    env.emit(
        "whatsapp",
        r#"{"event":"connection","data":{"state":"fatal","reason":"stream_replaced"}}"#,
    );
    wait_status(&client, "blocked", LONG, |st| {
        st["platforms"]["whatsapp"]["sending_blocked"] == true
    })
    .await;

    let p = ok(&client, "restart", json!({"platform": "whatsapp"})).await;
    assert_eq!(p["sending_blocked"], false);
    assert_eq!(p["logged_in"], true);
    assert_eq!(p["sidecar_running"], true);
    assert!(p["blocked_reason"].is_null());
    assert_eq!(
        env.request_count("whatsapp", "shutdown"),
        1,
        "the old sidecar is shut down"
    );
    assert_eq!(env.request_count("whatsapp", "hello"), 2);
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["whatsapp"]["connection"] == "connected"
    })
    .await;
    let sent = ok(
        &client,
        "send_text",
        json!({"platform": "whatsapp", "thread_id": "123@s.whatsapp.net", "text": "back"}),
    )
    .await;
    assert_eq!(sent["message_id"], "mid.1");

    // A platform with no stored session stays stopped.
    let m = ok(&client, "restart", json!({"platform": "messenger"})).await;
    assert_eq!(m["connection"], "stopped");
    assert_eq!(m["sidecar_running"], false);
    assert_eq!(env.request_count("messenger", "hello"), 0);
}

#[tokio::test]
async fn send_media_copies_into_data_dir_and_refuses_inbound() {
    let env = TestEnv::with_files();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let content = b"jpeg-bytes-for-test";
    std::fs::write(env.files_dir().join("pic.jpg"), content).expect("file");

    let out = ok(
        &client,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": "pic.jpg", "caption": " look at this "}),
    )
    .await;
    assert_eq!(out["message_id"], "mid.2");
    assert!(out["audit_id"].is_u64());
    let req = env
        .last_request("messenger", "send_media")
        .expect("send_media");
    let p = &req["params"];
    let sent_path = std::path::PathBuf::from(p["path"].as_str().expect("path"));
    let outbox = env.personal_dir().join("messenger/data/outbox");
    assert_eq!(sent_path.parent().expect("parent"), outbox);
    assert_eq!(sent_path.extension().and_then(|e| e.to_str()), Some("jpg"));
    assert_eq!(
        sent_path.file_stem().expect("stem").len(),
        16,
        "random_hex(8)"
    );
    assert_eq!(p["mime"], "image/jpeg");
    assert_eq!(p["kind"], "image");
    assert_eq!(p["filename"], "pic.jpg");
    assert_eq!(p["caption"], "look at this");
    assert_eq!(p["thread_id"], "t1");
    assert_eq!(
        std::fs::read(env.fake_dir().join("messenger.media.copy")).expect("copy"),
        content
    );
    assert_eq!(
        std::fs::read_to_string(env.fake_dir().join("messenger.media.mode"))
            .expect("mode")
            .trim(),
        "600"
    );
    assert!(!sent_path.exists(), "the copy is deleted after the reply");
    assert_eq!(std::fs::read_dir(&outbox).expect("outbox").count(), 0);
    assert_eq!(mode(&outbox), 0o700);

    let lines = env.audit_lines();
    assert_eq!(lines[0]["kind"], "media");
    assert_eq!(lines[0]["file_sha256"], sha_hex(content));
    assert_eq!(lines[0]["body_sha256"], sha_hex(b"look at this"));
    assert_eq!(lines[0]["body_len"], 12);
    assert_eq!(lines[1]["outcome"], "sent");
    assert_eq!(lines[1]["file_sha256"], sha_hex(content));

    // Explicit kind, filename and no caption.
    ok(&client, "send_media", json!({"platform": "messenger", "thread_id": "t1", "path": "pic.jpg", "kind": "file", "filename": "scan.jpg"})).await;
    let p = env.last_request("messenger", "send_media").expect("req")["params"].clone();
    assert_eq!(
        (p["kind"].as_str(), p["filename"].as_str()),
        (Some("file"), Some("scan.jpg"))
    );
    assert!(p.get("caption").is_none());

    // Files under inbound/ need allow_inbound.
    let inbound = env.files_dir().join("inbound/messenger_personal");
    std::fs::create_dir_all(&inbound).expect("inbound dir");
    std::fs::write(inbound.join("got.png"), b"png").expect("inbound file");
    let before = env.request_count("messenger", "send_media");
    let refused = err(&client, "send_media", json!({"platform": "messenger", "thread_id": "t1", "path": "inbound/messenger_personal/got.png"})).await;
    assert_eq!(refused.code, "file_refused");
    ok(&client, "send_media", json!({"platform": "messenger", "thread_id": "t1", "path": "inbound/messenger_personal/got.png", "allow_inbound": true})).await;
    assert_eq!(env.request_count("messenger", "send_media"), before + 1);

    // Outside the jail, missing, bad kind.
    for path in ["../outside.txt", "/etc/passwd", "missing.jpg"] {
        let e = err(
            &client,
            "send_media",
            json!({"platform": "messenger", "thread_id": "t1", "path": path}),
        )
        .await;
        assert_eq!(e.code, "file_refused", "{path}");
    }
    let e = err(
        &client,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": "pic.jpg", "kind": "sticker"}),
    )
    .await;
    assert_eq!(e.code, "bad_params");
    assert_eq!(env.request_count("messenger", "send_media"), before + 1);
    let refused_lines = env
        .audit_lines()
        .iter()
        .filter(|l| l["outcome"] == "refused")
        .count();
    assert_eq!(refused_lines, 4);
    assert_eq!(std::fs::read_dir(&outbox).expect("outbox").count(), 0);
}

#[tokio::test]
async fn send_media_enforces_the_size_limit_and_needs_a_files_dir() {
    let env = TestEnv::with_files();
    env.preseed_session("messenger");
    let _daemon = env.start_with(&[("META_MCP_MAX_FILE_BYTES", "8")]);
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    std::fs::write(env.files_dir().join("big.bin"), b"123456789").expect("big");
    std::fs::write(env.files_dir().join("small.bin"), b"12345678").expect("small");
    let e = err(
        &client,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": "big.bin"}),
    )
    .await;
    assert_eq!(e.code, "file_refused");
    ok(
        &client,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": "small.bin"}),
    )
    .await;
    let p = env.last_request("messenger", "send_media").expect("req")["params"].clone();
    assert_eq!(
        (p["mime"].as_str(), p["kind"].as_str()),
        (Some("application/octet-stream"), Some("file"))
    );

    let bare = TestEnv::new();
    let (_d2, c2) = bare.start_logged_in("messenger").await;
    let e = err(
        &c2,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": "x.jpg"}),
    )
    .await;
    assert_eq!(e.code, "no_files_dir");
    let e = err(&c2, "download_attachment", json!({"platform": "messenger", "thread_id": "t1", "message_id": "m1", "attachment_id": "a1"})).await;
    assert_eq!(e.code, "no_files_dir");
}

#[tokio::test]
async fn download_attachment_lands_in_inbound_folder() {
    let env = TestEnv::with_files();
    let (_daemon, client) = env.start_logged_in("messenger").await;

    let out = ok(&client, "download_attachment", json!({"platform": "messenger", "thread_id": "t1", "message_id": "mid.7", "attachment_id": "a/1"})).await;
    let rel = out["files_path"].as_str().expect("files_path");
    assert!(
        rel.starts_with("inbound/messenger_personal/mid.7_a_1_"),
        "{rel}"
    );
    assert_eq!(out["mime"], "image/jpeg");
    assert_eq!(out["bytes"], 16);
    let abs = env.files_dir().join(rel);
    assert_eq!(
        std::fs::read(&abs).expect("downloaded"),
        b"attachment-bytes"
    );
    assert_eq!(out["sha256"], sha_hex(b"attachment-bytes"));
    assert_eq!(mode(abs.parent().expect("dir")), 0o700);
    let req = env
        .last_request("messenger", "download_attachment")
        .expect("req");
    let dest = std::path::PathBuf::from(req["params"]["dest_path"].as_str().expect("dest_path"));
    assert!(dest.is_absolute());
    assert_eq!(
        dest.strip_prefix(std::fs::canonicalize(env.files_dir()).expect("canon"))
            .expect("inside"),
        std::path::Path::new(rel)
    );
    assert!(req["params"].get("platform").is_none());

    // A download is not sendable without allow_inbound.
    let e = err(
        &client,
        "send_media",
        json!({"platform": "messenger", "thread_id": "t1", "path": rel}),
    )
    .await;
    assert_eq!(e.code, "file_refused");
    let second = ok(&client, "download_attachment", json!({"platform": "messenger", "thread_id": "t1", "message_id": "mid.7", "attachment_id": "a/1"})).await;
    assert_ne!(
        second["files_path"], out["files_path"],
        "random suffix per download"
    );
    for bad in [
        json!({"platform": "messenger", "thread_id": "t1", "message_id": "m", "attachment_id": "has space"}),
        json!({"platform": "messenger", "thread_id": "t1", "message_id": "", "attachment_id": "a"}),
        json!({"platform": "messenger", "message_id": "m", "attachment_id": "a"}),
    ] {
        assert_eq!(
            err(&client, "download_attachment", bad).await.code,
            "bad_params"
        );
    }
}

#[tokio::test]
async fn missing_sidecar_leaves_the_platform_stopped_and_the_daemon_serving() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let empty = env.root().join("empty");
    std::fs::create_dir_all(&empty).expect("empty dir");
    let _daemon = env.start_with(&[("META_MCP_SIDECAR_DIR", empty.to_str().expect("utf8"))]);
    let client = env.connect().await;
    let st = wait_status(&client, "stopped with an error", LONG, |st| {
        !st["platforms"]["messenger"]["last_error"].is_null()
    })
    .await;
    let m = &st["platforms"]["messenger"];
    assert_eq!(m["connection"], "stopped");
    assert_eq!(m["sidecar_running"], false);
    assert_eq!(m["last_error"], "sidecar_spawn_failed");
    let e = err(
        &client,
        "login_start",
        json!({"platform": "whatsapp", "method": "qr"}),
    )
    .await;
    assert_eq!(e.code, "sidecar_unavailable");
    assert!(e.message.contains("meta-whatsapp-sidecar"), "{e}");
    assert_eq!(
        err(&client, "list_threads", json!({"platform": "messenger"}))
            .await
            .code,
        "not_logged_in"
    );
    assert!(
        env.state_path("messenger").is_file(),
        "the stored session is kept"
    );
}

#[tokio::test]
async fn a_corrupt_state_file_is_moved_aside() {
    let env = TestEnv::new();
    let dir = env.personal_dir().join("messenger");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("state.json"), b"{broken").expect("corrupt");
    let _daemon = env.start();
    let client = env.connect().await;
    wait_until("the file moved aside", LONG, || {
        dir.join("state.json.corrupt").exists()
    })
    .await;
    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(st["platforms"]["messenger"]["connection"], "stopped");
    assert_eq!(env.request_count("messenger", "hello"), 0);
    assert!(!env.daemon_log().contains("broken"));
}

#[tokio::test]
async fn a_second_daemon_on_the_same_socket_refuses_to_start() {
    let env = TestEnv::new();
    let mut first = env.start();
    let mut second = env.spawn(&[]);
    let code = wait_exit(&mut second);
    assert_eq!(code, Some(1));
    assert!(
        env.daemon_log().contains("already uses"),
        "{}",
        env.daemon_log()
    );
    assert!(first.is_running());
    let client = env.connect().await;
    ok(&client, "status", Value::Null).await;
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let env = TestEnv::new();
    drop(std::os::unix::net::UnixListener::bind(env.socket()).expect("bind stale"));
    // A process forked by a parallel test may hold the listener until it execs.
    wait_until("the stale socket refuses", LONG, || {
        std::os::unix::net::UnixStream::connect(env.socket()).is_err()
    })
    .await;
    let _daemon = env.start();
    ok(&env.connect().await, "status", Value::Null).await;
}

#[tokio::test]
async fn sigterm_stops_sidecars_and_removes_the_socket() {
    let env = TestEnv::new();
    let (daemon, _client) = env.start_logged_in("messenger").await;
    let pid = env.sidecar_pid("messenger").expect("pid");
    assert_eq!(daemon.terminate(), Some(0));
    assert!(!env.socket().exists(), "the socket file is removed");
    assert_eq!(env.request_count("messenger", "shutdown"), 1);
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists() || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
        },
        "the sidecar is gone"
    );
}

#[tokio::test]
async fn too_many_requests_in_flight_are_refused_at_once() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let client = Arc::new(client);
    env.knob("messenger", "list_slow", true);
    let spawn_list = || {
        let c = Arc::clone(&client);
        tokio::spawn(
            async move { call(&c, "list_threads", json!({"platform": "messenger"})).await },
        );
    };
    for _ in 0..32 {
        spawn_list();
    }
    // The fake answers one list every 3 s, so slots stay taken. Add one list per round and let it
    // arrive before polling, so a poll never holds the last slot against it, until the poll is refused.
    let deadline = std::time::Instant::now() + LONG;
    loop {
        spawn_list();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        match call(&client, "status", Value::Null).await {
            Err(e) => {
                assert_eq!(e.code, "too_many_requests");
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "refused without waiting"
                );
                break;
            }
            Ok(_) => assert!(
                std::time::Instant::now() < deadline,
                "the in-flight cap never refused a request"
            ),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_65th_connection_is_dropped() {
    let env = TestEnv::new();
    let _daemon = env.start();
    // Each held connection proves it got a slot by answering; a refused one is retried.
    let mut held = Vec::new();
    let deadline = std::time::Instant::now() + LONG;
    while held.len() < 64 {
        assert!(
            std::time::Instant::now() < deadline,
            "could not fill 64 slots"
        );
        let client = env.connect().await;
        match call(&client, "status", Value::Null).await {
            Ok(_) => held.push(client),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    let mut extra = tokio::net::UnixStream::connect(env.socket())
        .await
        .expect("connect 65");
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(LONG, tokio::io::AsyncReadExt::read(&mut extra, &mut buf))
        .await
        .expect("dropped in time")
        .unwrap_or(0);
    assert_eq!(n, 0);

    drop(held.pop());
    let deadline = std::time::Instant::now() + LONG;
    loop {
        let client = env.connect().await;
        if call(&client, "status", Value::Null).await.is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the freed slot never came back"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_failed_attempt_line_refuses_the_send() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let daemon = env.start_with(&[("TEST_IGNORE_XFSZ", "1")]);
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    daemon.limit_file_size(64);
    let e = err(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "never sent"}),
    )
    .await;
    assert_eq!(e.code, "audit_failed");
    assert_eq!(
        env.request_count("messenger", "send_text"),
        0,
        "nothing is sent without the write-ahead line"
    );
}

#[tokio::test]
async fn a_failed_completion_line_still_replies_with_a_warning() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let daemon = env.start_with(&[("TEST_IGNORE_XFSZ", "1")]);
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    daemon.limit_file_size(400);
    let out = ok(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "sent anyway"}),
    )
    .await;
    assert_eq!(out["message_id"], "mid.1");
    assert!(out["audit_id"].is_null());
    assert!(
        out["audit_warning"]
            .as_str()
            .expect("warning")
            .contains("audit")
    );
    assert_eq!(env.request_count("messenger", "send_text"), 1);
}

const OFFLINE: &str = "http://127.0.0.1:9";

const PERSONAL_TOOLS: [&str; 18] = [
    "messenger_personal_download_attachment",
    "messenger_personal_list_threads",
    "messenger_personal_mark_read",
    "messenger_personal_react",
    "messenger_personal_read_thread",
    "messenger_personal_send_media",
    "messenger_personal_send_text",
    "messenger_personal_typing",
    "personal_status",
    "personal_wait_for_message",
    "whatsapp_personal_download_media",
    "whatsapp_personal_list_chats",
    "whatsapp_personal_mark_read",
    "whatsapp_personal_react",
    "whatsapp_personal_read_chat",
    "whatsapp_personal_send_media",
    "whatsapp_personal_send_text",
    "whatsapp_personal_typing",
];

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"] == json!(true)
}

fn session_for(home: &common::TempHome, env: &TestEnv, extra: &[(&str, &str)]) -> common::Session {
    let socket = env.socket();
    let mut envs = vec![(
        "META_MCP_PERSONAL_SOCKET",
        socket.to_str().expect("utf8 socket path"),
    )];
    envs.extend_from_slice(extra);
    common::Session::start(home, OFFLINE, &envs)
}

#[test]
fn mcp_tools_list_includes_personal_tools() {
    let home = common::TempHome::new();
    let mut s = common::Session::start(&home, OFFLINE, &[]);
    let names = s.tool_names();
    assert_eq!(names, ALL_TOOL_NAMES);
    for tool in PERSONAL_TOOLS {
        assert!(names.iter().any(|n| n == tool), "{tool} missing");
    }
    let listed = s.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools");
    let read = tools
        .iter()
        .find(|t| t["name"] == "whatsapp_personal_read_chat")
        .expect("read chat tool");
    let props = read["inputSchema"]["properties"]
        .as_object()
        .expect("props");
    for field in ["chat_id", "limit", "before_ts_ms"] {
        assert!(props.contains_key(field), "{field} in {props:?}");
    }
    assert_eq!(read["inputSchema"]["required"], json!(["chat_id"]));
}

#[test]
fn personal_tool_errors_when_daemon_absent() {
    let env = TestEnv::new();
    let home = common::TempHome::new();
    let mut s = session_for(&home, &env, &[]);
    for (tool, args) in [
        ("personal_status", json!({})),
        ("messenger_personal_list_threads", json!({})),
        (
            "whatsapp_personal_send_text",
            json!({"chat_id": "c", "text": "hi"}),
        ),
        ("personal_wait_for_message", json!({"timeout_seconds": 1})),
    ] {
        let resp = s.call_tool(tool, args);
        assert!(is_error(&resp), "{tool}: {resp}");
        let text = common::tool_text(&resp);
        assert!(text.contains("personal mode not running"), "{tool}: {text}");
        assert!(text.contains("meta-mcp personal daemon"), "{tool}: {text}");
    }
}

#[tokio::test]
async fn personal_tools_proxy_through_daemon() {
    let env = TestEnv::new();
    let (_daemon, _client) = env.start_logged_in("messenger").await;
    let socket = env.socket();
    let out = common::blocking(move || {
        let home = common::TempHome::new();
        let mut s = common::Session::start(
            &home,
            OFFLINE,
            &[(
                "META_MCP_PERSONAL_SOCKET",
                socket.to_str().expect("utf8 socket path"),
            )],
        );
        let status = common::tool_json(&s.call_tool("personal_status", json!({})));
        let list = common::tool_json(&s.call_tool(
            "messenger_personal_list_threads",
            json!({"limit": 5, "cursor": "c1"}),
        ));
        let read = common::tool_json(&s.call_tool(
            "messenger_personal_read_thread",
            json!({"thread_id": "t1", "before_ts_ms": 1_700_000_000_000_i64}),
        ));
        let sent = common::tool_json(&s.call_tool(
            "messenger_personal_send_text",
            json!({"thread_id": "t1", "text": "  hello there ", "reply_to": "m1"}),
        ));
        let not_in = s.call_tool("whatsapp_personal_list_chats", json!({}));
        let bad = s.call_tool(
            "messenger_personal_send_text",
            json!({"thread_id": "t1", "text": "   "}),
        );
        let react = common::tool_json(&s.call_tool(
            "messenger_personal_react",
            json!({"thread_id": "t1", "message_id": "m1", "emoji": "\u{1F44D}"}),
        ));
        (status, list, read, sent, not_in, bad, react)
    })
    .await;
    let (status, list, read, sent, not_in, bad, react) = out;
    assert_eq!(
        status["platforms"]["messenger"]["logged_in"], true,
        "{status}"
    );
    assert_eq!(
        status["platforms"]["messenger"]["account"]["name"],
        "Test User"
    );
    assert_eq!(status["platforms"]["whatsapp"]["logged_in"], false);
    assert_eq!(list["threads"][0]["id"], "t1", "{list}");
    assert_eq!(read["messages"][0]["text"], "hello", "{read}");
    assert_eq!(sent["message_id"], "mid.1", "{sent}");
    assert!(sent["audit_id"].is_number(), "{sent}");
    assert!(react["audit_id"].is_number(), "{react}");
    assert!(is_error(&not_in), "{not_in}");
    assert!(
        common::tool_text(&not_in).contains("not_logged_in"),
        "{not_in}"
    );
    assert!(is_error(&bad), "{bad}");
    assert!(common::tool_text(&bad).contains("bad_params"), "{bad}");

    let list_req = env
        .last_request("messenger", "list_threads")
        .expect("list request");
    assert_eq!(list_req["params"]["limit"], 5);
    assert_eq!(list_req["params"]["cursor"], "c1");
    assert!(list_req["params"].get("platform").is_none(), "{list_req}");
    let read_req = env
        .last_request("messenger", "fetch_messages")
        .expect("read request");
    assert_eq!(read_req["params"]["limit"], 20, "default limit");
    assert_eq!(read_req["params"]["before_ts_ms"], 1_700_000_000_000_i64);
    let send_req = env
        .last_request("messenger", "send_text")
        .expect("send request");
    assert_eq!(
        send_req["params"]["text"], "hello there",
        "trimmed by the daemon"
    );
    assert_eq!(send_req["params"]["reply_to"], "m1");
    assert_eq!(env.request_count("whatsapp", "list_threads"), 0);
}

#[tokio::test]
async fn personal_wait_tool_returns_the_next_live_message() {
    let env = TestEnv::new();
    let (_daemon, _client) = env.start_logged_in("whatsapp").await;
    let socket = env.socket();
    let waiter = common::blocking(move || {
        let home = common::TempHome::new();
        let mut s = common::Session::start(
            &home,
            OFFLINE,
            &[(
                "META_MCP_PERSONAL_SOCKET",
                socket.to_str().expect("utf8 socket path"),
            )],
        );
        let quiet = common::tool_json(&s.call_tool(
            "personal_wait_for_message",
            json!({"timeout_seconds": 1, "platform": "messenger"}),
        ));
        let wrong = s.call_tool("personal_wait_for_message", json!({"platform": "sms"}));
        (quiet, wrong, s)
    });
    let (quiet, wrong, mut s) = waiter.await;
    assert_eq!(quiet["timed_out"], true, "{quiet}");
    assert!(is_error(&wrong), "{wrong}");
    let emitter = {
        let line = message_event("wm1", "chat@s.whatsapp.net", "in", "ping");
        let env_dir = env.fake_dir();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(env_dir.join("whatsapp.emit"))
                .expect("emit file");
            writeln!(f, "{line}").expect("emit");
        })
    };
    let got = common::blocking(move || {
        common::tool_json(&s.call_tool("personal_wait_for_message", json!({"timeout_seconds": 10})))
    })
    .await;
    emitter.await.expect("emitter");
    assert_eq!(got["timed_out"], false, "{got}");
    assert_eq!(got["message"]["text"], "ping", "{got}");
}

#[tokio::test]
async fn personal_download_inline_returns_small_pictures_only() {
    let env = TestEnv::with_files();
    let (_daemon, _client) = env.start_logged_in("messenger").await;
    let src = env.fake_dir().join("messenger.download_src");
    let socket = env.socket();
    let files = env.files_dir();
    let png = common::png_image();
    std::fs::write(&src, &png).expect("png source");
    let big_src = env.root().join("big.png");
    let mut big = png.clone();
    big.resize(2 * 1024 * 1024 + 1, 0);
    std::fs::write(&big_src, &big).expect("big source");
    let (src2, big2) = (src.clone(), big_src.clone());
    let args =
        json!({"thread_id": "t1", "message_id": "m1", "attachment_id": "a1", "inline": true});
    let out = common::blocking(move || {
        let home = common::TempHome::new();
        let sock = socket.to_str().expect("utf8 socket path");
        let mut s = common::Session::start(
            &home,
            OFFLINE,
            &[
                ("META_MCP_PERSONAL_SOCKET", sock),
                ("META_MCP_FILES_DIR", files.to_str().expect("utf8")),
            ],
        );
        let picture = s.call_tool("messenger_personal_download_attachment", args.clone());
        let not_inline = s.call_tool(
            "messenger_personal_download_attachment",
            json!({"thread_id": "t1", "message_id": "m2", "attachment_id": "a1"}),
        );
        std::fs::copy(&big2, &src2).expect("swap in the big file");
        let too_big = s.call_tool("messenger_personal_download_attachment", args.clone());
        std::fs::remove_file(&src2).expect("remove source");
        let text = s.call_tool("messenger_personal_download_attachment", args.clone());
        let mut no_jail =
            common::Session::start(&home, OFFLINE, &[("META_MCP_PERSONAL_SOCKET", sock)]);
        let unreadable = no_jail.call_tool("messenger_personal_download_attachment", args);
        (picture, not_inline, too_big, text, unreadable)
    })
    .await;
    let (picture, not_inline, too_big, text, unreadable) = out;
    let content = picture["result"]["content"].as_array().expect("content");
    assert_eq!(content.len(), 2, "{picture}");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    let meta = common::tool_json(&picture);
    assert!(
        meta["files_path"]
            .as_str()
            .expect("files_path")
            .starts_with("inbound/"),
        "{meta}"
    );
    assert!(meta.get("inline_skipped").is_none(), "{meta}");

    assert_eq!(
        not_inline["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    assert!(
        common::tool_json(&not_inline)
            .get("inline_skipped")
            .is_none()
    );
    let reason = |resp: &Value| -> String {
        assert_eq!(
            resp["result"]["content"].as_array().expect("content").len(),
            1,
            "{resp}"
        );
        common::tool_json(resp)["inline_skipped"]
            .as_str()
            .expect("inline_skipped")
            .to_owned()
    };
    assert!(reason(&too_big).contains("larger than 2 MiB"), "{too_big}");
    assert!(reason(&text).contains("not a PNG"), "{text}");
    assert!(
        reason(&unreadable).contains("META_MCP_FILES_DIR"),
        "{unreadable}"
    );
}

#[test]
fn personal_write_tools_hidden_in_read_only() {
    let env = TestEnv::new();
    let home = common::TempHome::new();
    let mut s = session_for(&home, &env, &[("META_MCP_READ_ONLY", "1")]);
    let names = s.tool_names();
    assert_eq!(names, READ_ONLY_TOOL_NAMES);
    let left: Vec<&str> = PERSONAL_TOOLS
        .iter()
        .copied()
        .filter(|t| names.iter().any(|n| n == t))
        .collect();
    assert_eq!(left.len(), 8, "{left:?}");
    for tool in PERSONAL_TOOLS {
        let writes = ["send_text", "send_media", "react", "mark_read", "typing"];
        let is_write = writes.iter().any(|w| tool.ends_with(w));
        assert_eq!(names.iter().any(|n| n == tool), !is_write, "{tool}");
    }
    let resp = s.call_tool(
        "messenger_personal_send_text",
        json!({"thread_id": "t", "text": "x"}),
    );
    assert!(is_error(&resp) || resp["error"].is_object(), "{resp}");
}

fn field(v: &Value, key: &str) -> Value {
    v[key].clone()
}

#[tokio::test]
async fn cli_login_messenger_with_cookies_json() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let cookies = format!(
        r#"{{"c_user":"100001","xs":"{COOKIE_SECRET}","datr":"datr-value-1","sb":"sb-1","junk":"drop-me"}}"#
    );
    let run = env
        .cli(
            &["personal", "login", "messenger", "--cookies-json", &cookies],
            "",
        )
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    assert!(
        run.stdout
            .contains("Logged in to Messenger as Test User (100001)"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("Paste the cookies."),
        "daemon instructions: {run:?}"
    );
    for text in [&run.stdout, &run.stderr, &env.daemon_log()] {
        assert!(!text.contains(COOKIE_SECRET), "cookie leaked: {text}");
    }
    let start = env
        .last_request("messenger", "login_start")
        .expect("login_start");
    assert_eq!(start["params"], json!({"method": "cookies"}));
    let fin = env
        .last_request("messenger", "login_finish")
        .expect("login_finish");
    assert_eq!(fin["params"]["cookies"]["xs"], COOKIE_SECRET);
    assert_eq!(fin["params"]["cookies"]["sb"], "sb-1");
    assert!(fin["params"]["cookies"].get("junk").is_none(), "{fin}");
    assert!(fin["params"].get("user_agent").is_none(), "{fin}");
    assert_eq!(mode(&env.state_path("messenger")), 0o600);
    let client = env.connect().await;
    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(st["platforms"]["messenger"]["logged_in"], true);
}

#[tokio::test]
async fn cli_login_messenger_reads_a_pasted_curl_and_a_cookies_file() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let pasted = format!(
        "curl 'https://www.facebook.com/' \\\n  -H 'User-Agent: TestAgent/1.0' \\\n  -H 'Cookie: c_user=100001; xs={COOKIE_SECRET}; datr=d1; locale=en_US' \\\n  -H 'Accept: */*'\n"
    );
    let run = env.cli(&["personal", "login", "messenger"], &pasted).await;
    assert_eq!(run.code, 0, "{run:?}");
    let fin = env
        .last_request("messenger", "login_finish")
        .expect("login_finish");
    assert_eq!(fin["params"]["user_agent"], "TestAgent/1.0");
    assert_eq!(fin["params"]["cookies"]["datr"], "d1");
    assert!(!run.stdout.contains(COOKIE_SECRET) && !run.stderr.contains(COOKIE_SECRET));

    let file = env.root().join("cookies.json");
    std::fs::write(
        &file,
        format!(r#"[{{"name":"c_user","value":"100001"}},{{"name":"xs","value":"{COOKIE_SECRET}"}},{{"name":"datr","value":"d2"}}]"#),
    )
    .expect("cookies file");
    let from_file = env
        .cli(
            &[
                "personal",
                "login",
                "messenger",
                "--cookies-file",
                file.to_str().expect("utf8"),
            ],
            "",
        )
        .await;
    assert_eq!(from_file.code, 0, "{from_file:?}");
    let fin = env
        .last_request("messenger", "login_finish")
        .expect("login_finish");
    assert_eq!(fin["params"]["cookies"]["datr"], "d2");
}

#[tokio::test]
async fn cli_login_messenger_rejects_incomplete_input() {
    let env = TestEnv::new();
    let secret = "XSVALUE-should-never-print-123";
    let json = format!(r#"{{"c_user":"1","xs":"{secret}"}}"#);
    let run = env
        .cli(
            &["personal", "login", "messenger", "--cookies-json", &json],
            "",
        )
        .await;
    assert_eq!(run.code, 2, "{run:?}");
    assert!(run.stderr.contains("datr"), "{run:?}");
    assert!(
        !run.stderr.contains(secret) && !run.stdout.contains(secret),
        "{run:?}"
    );
    assert!(
        !run.stderr.contains("not running"),
        "parse errors come first: {run:?}"
    );

    let _daemon = env.start();
    let garbage = env
        .cli(&["personal", "login", "messenger"], "just some words\n")
        .await;
    assert_eq!(garbage.code, 2, "{garbage:?}");
    assert_eq!(env.request_count("messenger", "login_cancel"), 1);
    let missing_file = env
        .cli(
            &[
                "personal",
                "login",
                "messenger",
                "--cookies-file",
                "/nonexistent/c.txt",
            ],
            "",
        )
        .await;
    assert_eq!(missing_file.code, 2, "{missing_file:?}");
}

const ASSERTION: &str = r#"{"id":"cred1","type":"public-key","response":{"signature":"ASSERTION-SECRET-sig","clientDataJSON":"x"}}"#;

fn finish_requests(env: &TestEnv) -> Vec<Value> {
    env.requests("whatsapp")
        .iter()
        .filter(|l| l.contains("\"op\":\"login_finish\""))
        .map(|l| serde_json::from_str(l).expect("request json"))
        .collect()
}

#[tokio::test]
async fn passkey_flow_roundtrip() {
    let env = TestEnv::new();
    env.knob("whatsapp", "passkey", true);
    let _daemon = env.start();
    let run = env
        .cli(
            &["personal", "login", "whatsapp"],
            &format!("{ASSERTION}\ny\n"),
        )
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    assert!(
        run.stdout
            .contains("Logged in to WhatsApp as Test User (100001)"),
        "{run:?}"
    );
    assert!(
        run.stdout.contains("web.whatsapp.com"),
        "public key printed: {run:?}"
    );
    assert!(
        run.stdout
            .contains("Compare this code with the one on your phone: 123456"),
        "{run:?}"
    );
    let finishes = finish_requests(&env);
    assert_eq!(finishes.len(), 2, "{finishes:?}");
    assert_eq!(finishes[0]["params"]["passkey_response"]["id"], "cred1");
    assert!(finishes[0]["params"].get("confirm").is_none());
    assert_eq!(finishes[1]["params"]["confirm"], true);
    for text in [&run.stdout, &run.stderr, &env.daemon_log()] {
        assert!(
            !text.contains("ASSERTION-SECRET"),
            "assertion leaked: {text}"
        );
    }
    let start = env
        .last_request("whatsapp", "login_start")
        .expect("login_start");
    assert_eq!(start["params"]["method"], "qr");
    assert_eq!(start["params"]["device_name"], "meta-mcp");
    assert_eq!(field(&start["params"], "full_history"), json!(false));
}

#[tokio::test]
async fn passkey_flow_declined_confirmation_cancels() {
    let env = TestEnv::new();
    env.knob("whatsapp", "passkey", true);
    let _daemon = env.start();
    let run = env
        .cli(
            &["personal", "login", "whatsapp"],
            &format!("{ASSERTION}\nn\n"),
        )
        .await;
    assert_eq!(run.code, 1, "{run:?}");
    assert!(!run.stdout.contains("Logged in"), "{run:?}");
    assert_eq!(finish_requests(&env).len(), 1, "no confirm was sent");
    assert_eq!(env.request_count("whatsapp", "login_cancel"), 1);

    let not_json = env
        .cli(&["personal", "login", "whatsapp"], "not json\n")
        .await;
    assert_eq!(not_json.code, 2, "{not_json:?}");
    assert!(
        not_json.stderr.contains("not a JSON object"),
        "{not_json:?}"
    );
    assert_eq!(env.request_count("whatsapp", "login_cancel"), 2);
}

#[tokio::test]
async fn cli_login_whatsapp_prints_qr_codes_and_pair_codes() {
    let env = TestEnv::new();
    let _daemon = env.start();
    let run = env
        .cli(
            &[
                "personal",
                "login",
                "whatsapp",
                "--full-history",
                "--device-name",
                "My Box",
            ],
            "",
        )
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    for want in [
        "Scan with WhatsApp: Settings, Linked devices, Link a device (code 1, valid 60s)",
        "(code 2, valid 20s)",
        "Logged in to WhatsApp as Test User (100001)",
    ] {
        assert!(run.stdout.contains(want), "{want}: {run:?}");
    }
    assert!(run.stdout.contains('\u{2588}'), "block characters: {run:?}");
    assert!(!run.stdout.contains("\x1b"), "no ANSI when piped: {run:?}");
    let start = env
        .last_request("whatsapp", "login_start")
        .expect("login_start");
    assert_eq!(start["params"]["full_history"], true);
    assert_eq!(start["params"]["device_name"], "My Box");
    assert!(start["params"].get("phone").is_none());

    let paired = env
        .cli(
            &[
                "personal",
                "login",
                "whatsapp",
                "--pair-code",
                "+1 (555) 010-1234",
            ],
            "",
        )
        .await;
    assert_eq!(paired.code, 0, "{paired:?}");
    assert!(
        paired.stdout.contains("Pairing code: ABCD-EFGH"),
        "{paired:?}"
    );
    assert!(
        paired.stdout.contains("Link with phone number"),
        "{paired:?}"
    );
    let start = env
        .last_request("whatsapp", "login_start")
        .expect("login_start");
    assert_eq!(start["params"]["method"], "pair_code");
    assert_eq!(start["params"]["phone"], "15550101234");

    let bad = env
        .cli(&["personal", "login", "whatsapp", "--pair-code", "abc"], "")
        .await;
    assert_eq!(bad.code, 2, "{bad:?}");
}

#[tokio::test]
async fn cli_login_whatsapp_ends_on_qr_timeout_and_logout() {
    for (event, want) in [
        (
            r#"{"event":"connection","data":{"state":"disconnected","reason":"qr_timeout","action":"retry"}}"#,
            "The QR codes expired: run the command again",
        ),
        (
            r#"{"event":"connection","data":{"state":"fatal","reason":"banned"}}"#,
            "banned",
        ),
        (
            r#"{"event":"logged_out","data":{"reason":"device_removed"}}"#,
            "device_removed",
        ),
    ] {
        let env = TestEnv::new();
        env.knob("whatsapp", "qr_only", true);
        let _daemon = env.start();
        let cmd = env.cli_command(&["personal", "login", "whatsapp"]);
        let running = tokio::task::spawn_blocking(move || run_command(cmd, ""));
        wait_until("login_start", LONG, || {
            env.request_count("whatsapp", "login_start") == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        env.emit("whatsapp", event);
        let run = running.await.expect("join");
        assert_eq!(run.code, 1, "{run:?}");
        assert!(run.stderr.contains(want), "{want}: {run:?}");
    }
}

#[tokio::test]
async fn cli_status_json() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let run = env.cli(&["personal", "status", "--json"], "").await;
    assert_eq!(run.code, 0, "{run:?}");
    let parsed: Value = serde_json::from_str(&run.stdout).expect("status json");
    assert_eq!(
        parsed["platforms"]["messenger"]["logged_in"], true,
        "{parsed}"
    );
    assert_eq!(parsed["platforms"]["messenger"]["connection"], "connected");
    assert_eq!(parsed["platforms"]["whatsapp"]["logged_in"], false);
    let direct = ok(&client, "status", Value::Null).await;
    assert_eq!(parsed["daemon"]["pid"], direct["daemon"]["pid"]);

    let text = env.cli(&["personal", "status"], "").await;
    assert_eq!(text.code, 0, "{text:?}");
    for want in [
        "Messenger",
        "logged in: yes as Test User (100001)",
        "connection: connected",
        "sending: ok",
        "WhatsApp",
        "logged in: no",
        "sidecar: running, version 0.0.0-fake, library fakelib v0.0.1",
    ] {
        assert!(text.stdout.contains(want), "{want}: {text:?}");
    }
}

#[tokio::test]
async fn cli_status_without_a_daemon_fails_with_the_hint() {
    let env = TestEnv::new();
    let run = env.cli(&["personal", "status"], "").await;
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("personal mode not running"), "{run:?}");
    let json = env.cli(&["personal", "status", "--json"], "").await;
    assert_eq!(json.code, 1, "{json:?}");
}

#[tokio::test]
async fn cli_restart_platform() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    assert_eq!(env.request_count("messenger", "hello"), 1);
    let run = env.cli(&["personal", "restart", "messenger"], "").await;
    assert_eq!(run.code, 0, "{run:?}");
    assert!(
        run.stdout.contains("Restarted the Messenger sidecar"),
        "{run:?}"
    );
    assert!(run.stdout.contains("logged in: yes"), "{run:?}");
    assert_eq!(env.request_count("messenger", "hello"), 2);
    wait_status(&client, "reconnected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    let bad = env.cli(&["personal", "restart", "sms"], "").await;
    assert_eq!(bad.code, 2, "{bad:?}");
}

#[tokio::test]
async fn cli_logout_platform() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    assert!(env.state_path("messenger").exists());
    let run = env.cli(&["personal", "logout", "messenger"], "").await;
    assert_eq!(run.code, 0, "{run:?}");
    assert!(run.stdout.contains("Logged out of Messenger"), "{run:?}");
    assert!(!env.state_path("messenger").exists());
    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(st["platforms"]["messenger"]["logged_in"], false);
}

#[tokio::test]
async fn cli_install_dry_run_prints_units() {
    let env = TestEnv::with_files();
    let mut cmd = env.cli_command(&["personal", "install", "--dry-run"]);
    cmd.env("META_MCP_FILES_DIR", env.files_dir())
        .env("META_MCP_READ_ONLY", "1");
    let run = env.cli_with(cmd, "").await;
    assert_eq!(run.code, 0, "{run:?}");
    let exe = std::fs::canonicalize(common::BIN).expect("canonical exe");
    let socket = env.socket();
    for want in [
        format!("ListenStream={}", socket.display()),
        "SocketMode=0600".to_owned(),
        "RemoveOnStop=yes".to_owned(),
        "WantedBy=sockets.target".to_owned(),
        "Requires=meta-mcp-personal.socket".to_owned(),
        format!("ExecStart={} personal daemon", exe.display()),
        format!("Environment=\"META_MCP_HOME={}\"", env.home().display()),
        format!(
            "Environment=\"META_MCP_FILES_DIR={}\"",
            env.files_dir().display()
        ),
        format!(
            "Environment=\"META_MCP_SIDECAR_DIR={}\"",
            env.sidecar_dir().display()
        ),
        "Environment=\"META_MCP_READ_ONLY=1\"".to_owned(),
        "NoNewPrivileges=yes".to_owned(),
        "WantedBy=default.target".to_owned(),
        "systemctl --user daemon-reload".to_owned(),
        "systemctl --user enable --now meta-mcp-personal.socket meta-mcp-personal.service"
            .to_owned(),
        format!(
            "{}/systemd/user/meta-mcp-personal.service",
            env.xdg_dir().display()
        ),
    ] {
        assert!(run.stdout.contains(&want), "{want}\n{}", run.stdout);
    }
    assert!(!env.xdg_dir().exists(), "a dry run writes nothing");

    let mut cmd = env.cli_command(&["personal", "install", "--dry-run"]);
    cmd.env_remove("META_MCP_PERSONAL_SOCKET")
        .env("META_MCP_SIDECAR_DIR", env.root().join("none"));
    let default = env.cli_with(cmd, "").await;
    assert_eq!(default.code, 0, "{default:?}");
    assert!(
        default
            .stdout
            .contains("ListenStream=%t/meta-mcp/personal.sock"),
        "{default:?}"
    );
    assert!(
        default
            .stdout
            .contains("warning: meta-messenger-sidecar not found"),
        "{default:?}"
    );
}

#[tokio::test]
async fn cli_install_refuses_without_sidecars_and_uninstall_dry_run_lists_steps() {
    let env = TestEnv::new();
    let mut cmd = env.cli_command(&["personal", "install"]);
    cmd.env("META_MCP_SIDECAR_DIR", env.root().join("none"));
    let run = env.cli_with(cmd, "").await;
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("cannot be located"), "{run:?}");
    assert!(!env.xdg_dir().exists(), "nothing written when refusing");

    let un = env.cli(&["personal", "uninstall", "--dry-run"], "").await;
    assert_eq!(un.code, 0, "{un:?}");
    for want in [
        "systemctl --user disable --now meta-mcp-personal.service meta-mcp-personal.socket",
        "systemctl --user daemon-reload",
        "sessions and the audit log are kept",
    ] {
        assert!(un.stdout.contains(want), "{want}: {un:?}");
    }
}

#[tokio::test]
async fn personal_help_lists_subcommands() {
    let env = TestEnv::new();
    let run = env.cli(&["personal", "--help"], "").await;
    assert_eq!(run.code, 0, "{run:?}");
    for sub in [
        "daemon",
        "login",
        "status",
        "logout",
        "restart",
        "install",
        "uninstall",
    ] {
        assert!(
            run.stdout.lines().any(|l| l.trim_start().starts_with(sub)),
            "{sub}: {}",
            run.stdout
        );
    }
    let login = env.cli(&["personal", "login", "--help"], "").await;
    assert!(
        login.stdout.contains("messenger") && login.stdout.contains("whatsapp"),
        "{login:?}"
    );
    let usage = env.cli(&["personal"], "").await;
    assert_eq!(usage.code, 2, "{usage:?}");
}

#[tokio::test]
async fn state_rotated_during_hello_is_kept_at_startup_and_restart() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    env.knob("messenger", "hello_rotate", true);
    let _daemon = env.start();
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    let rotated = || {
        std::fs::read_to_string(env.state_path("messenger"))
            .is_ok_and(|s| s.contains("rotated-messenger"))
    };
    wait_until("the rotated state at startup", LONG, rotated).await;

    std::fs::write(env.state_path("messenger"), r#"{"session":"seeded-again"}"#).expect("reset");
    ok(&client, "restart", json!({"platform": "messenger"})).await;
    wait_until("the rotated state after restart", LONG, rotated).await;
}

#[tokio::test]
async fn a_second_daemon_on_another_socket_but_the_same_home_exits() {
    let env = TestEnv::new();
    let mut first = env.start();
    let other = env.root().join("o.sock");
    let mut second = env.spawn_on(&other, &[]);
    assert_eq!(wait_exit(&mut second), Some(1));
    let log = env.daemon_log();
    assert!(
        log.contains("another personal daemon already uses"),
        "{log}"
    );
    assert!(
        log.contains(env.personal_dir().to_str().expect("utf8")),
        "{log}"
    );
    assert!(!other.exists());
    assert_eq!(mode(&env.personal_dir().join("daemon.lock")), 0o600);
    assert!(first.is_running());
    ok(&env.connect().await, "status", Value::Null).await;

    // The lock goes with the process: a new daemon can start after the first one stops.
    assert_eq!(first.terminate(), Some(0));
    let _third = env.start();
    ok(&env.connect().await, "status", Value::Null).await;
}

#[tokio::test]
async fn a_reconnect_sleep_does_not_kill_a_login_started_meanwhile() {
    let env = TestEnv::new();
    env.preseed_session("messenger");
    let _daemon = env.start_with(&[("META_MCP_PERSONAL_BACKOFF_MS", "1500,3000")]);
    let client = env.connect().await;
    wait_status(&client, "connected", LONG, |st| {
        st["platforms"]["messenger"]["connection"] == "connected"
    })
    .await;
    env.kill_sidecar("messenger");
    wait_status(&client, "reconnect scheduled", LONG, |st| {
        !st["platforms"]["messenger"]["reconnect_in_s"].is_null()
    })
    .await;
    ok(&client, "logout", json!({"platform": "messenger"})).await;
    ok(
        &client,
        "login_start",
        json!({"platform": "messenger", "method": "cookies"}),
    )
    .await;
    let hellos = env.request_count("messenger", "hello");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        env.request_count("messenger", "hello"),
        hellos,
        "no second respawn"
    );
    assert_eq!(
        env.request_count("messenger", "shutdown"),
        0,
        "the login sidecar was left alone"
    );
    let st = ok(&client, "status", Value::Null).await;
    assert_eq!(st["platforms"]["messenger"]["sidecar_running"], true);
    assert!(st["platforms"]["messenger"]["reconnect_in_s"].is_null());
}

#[tokio::test]
async fn login_start_is_refused_while_sending_is_blocked() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("whatsapp").await;
    env.emit(
        "whatsapp",
        r#"{"event":"connection","data":{"state":"fatal","reason":"banned"}}"#,
    );
    wait_status(&client, "blocked", LONG, |st| {
        st["platforms"]["whatsapp"]["sending_blocked"] == true
    })
    .await;
    let e = err(
        &client,
        "login_start",
        json!({"platform": "whatsapp", "method": "qr"}),
    )
    .await;
    assert_eq!(e.code, "sending_blocked");
    assert!(
        e.message.contains("meta-mcp personal restart whatsapp"),
        "{e}"
    );
    assert_eq!(env.request_count("whatsapp", "login_start"), 0);
    ok(&client, "restart", json!({"platform": "whatsapp"})).await;
    ok(
        &client,
        "login_start",
        json!({"platform": "whatsapp", "method": "qr"}),
    )
    .await;
}

#[tokio::test]
async fn a_symlinked_download_folder_outside_the_files_root_is_refused() {
    use std::os::unix::fs::symlink;
    let env = TestEnv::with_files();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    let outside = env.root().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let inbound = env.files_dir().join("inbound");
    std::fs::create_dir_all(&inbound).expect("inbound");
    symlink(&outside, inbound.join("messenger_personal")).expect("symlink");
    let params = json!({"platform": "messenger", "thread_id": "t1", "message_id": "m1", "attachment_id": "a1"});

    let e = err(&client, "download_attachment", params.clone()).await;
    assert_eq!(e.code, "file_refused");
    assert_eq!(
        mode(&outside),
        0o755,
        "a folder outside the root is not chmodded"
    );
    assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
    assert_eq!(env.request_count("messenger", "download_attachment"), 0);

    // `inbound` itself pointing elsewhere: nothing is created there either.
    std::fs::remove_dir_all(&inbound).expect("remove inbound");
    symlink(&outside, &inbound).expect("symlink inbound");
    let e = err(&client, "download_attachment", params).await;
    assert_eq!(e.code, "file_refused");
    assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
    assert_eq!(mode(&outside), 0o755);
    assert_eq!(env.request_count("messenger", "download_attachment"), 0);
}

#[tokio::test]
async fn sidecar_error_codes_are_sanitised_for_the_client_and_the_audit() {
    let env = TestEnv::new();
    let (_daemon, client) = env.start_logged_in("messenger").await;
    env.knob("messenger", "send_weird", true);
    let e = err(
        &client,
        "send_text",
        json!({"platform": "messenger", "thread_id": "t1", "text": "x"}),
    )
    .await;
    assert_eq!(
        e.code, "sidecar_error",
        "a code that is not a plain word is replaced"
    );
    assert_eq!(e.message, "nope");
    let last = env.audit_lines().pop().expect("audit line");
    assert_eq!(last["outcome"], "failed");
    assert_eq!(last["error_code"], "weird_code_");
}
