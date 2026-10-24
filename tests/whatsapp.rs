//! WhatsApp tools over stdio against wiremock, plus WhatsApp webhook normalisation.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use common::{Session, TempHome, blocking, mock_graph, tool_json, tool_text};
use meta_mcp::config::Config;
use meta_mcp::graph::GraphClient;
use meta_mcp::inbox::{Event, EventKind, Inbox, Platform, now_secs};
use meta_mcp::webhook::normalize::normalize;
use meta_mcp::whatsapp::types::SendTextParams;
use meta_mcp::whatsapp::{WhatsApp, normalize_phone};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "EAAwatest0123456789ABCDEFGH";
const PNID: &str = "555000111";
const NOW: i64 = 1_800_000_000;
const WRITE_TOOLS: [&str; 7] = [
    "whatsapp_send_text",
    "whatsapp_send_media",
    "whatsapp_send_template",
    "whatsapp_send_reaction",
    "whatsapp_send_location",
    "whatsapp_mark_read",
    "whatsapp_upload_media",
];

/// A temp home and files directory, plus the environment of a configured stdio session.
struct Rig {
    home: TempHome,
    files: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            home: TempHome::new(),
            files: tempfile::tempdir().expect("files dir"),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.files.path().join(name)
    }

    fn session(&self, graph_uri: &str, extra: &[(&str, &str)]) -> Session {
        let files = self.files.path().display().to_string();
        let mut envs: Vec<(&str, &str)> = vec![
            ("META_WHATSAPP_ACCESS_TOKEN", TOKEN),
            ("META_WHATSAPP_PHONE_NUMBER_ID", PNID),
            ("META_APP_SECRET", "secret"),
            ("META_MCP_FILES_DIR", &files),
            ("META_MCP_DOWNLOAD_HOSTS", "127.0.0.1"),
        ];
        envs.extend_from_slice(extra);
        Session::start(&self.home, graph_uri, &envs)
    }

    fn insert(&self, events: &[Event]) {
        let inbox = Inbox::open(&self.home.inbox_db()).expect("inbox");
        inbox.insert(events).expect("insert");
    }
}

fn png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..96u8).map(|i| i.wrapping_mul(7)));
    bytes
}

fn sent(id: &str) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "+1 (650) 555-1234", "wa_id": "16505551234"}],
        "messages": [{"id": id}]
    })
}

fn wa_error(code: i64, message: &str, details: &str) -> Value {
    json!({"error": {
        "message": message, "type": "OAuthException", "code": code,
        "error_data": {"messaging_product": "whatsapp", "details": details},
        "fbtrace_id": "TRACE1"
    }})
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

/// A mock for `POST /{phone}/messages` that expects one request matching `partial`.
fn send_mock(partial: Value, answer: ResponseTemplate) -> Mock {
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .and(header("authorization", bearer().as_str()))
        .and(body_partial_json(partial))
        .respond_with(answer)
        .expect(1)
}

fn ok_sent(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(sent(id))
}

async fn requests(graph: &MockServer) -> Vec<Request> {
    graph.received_requests().await.expect("recording")
}

async fn requests_to(graph: &MockServer, suffix: &str) -> Vec<Request> {
    requests(graph)
        .await
        .into_iter()
        .filter(|r| r.url.path().ends_with(suffix))
        .collect()
}

fn body_of(req: &Request) -> Value {
    req.body_json().expect("json body")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"] == json!(true)
}

fn inbound(sender: &str, ts: i64) -> Event {
    Event {
        ts,
        account_id: PNID.into(),
        sender: sender.into(),
        recipient: PNID.into(),
        text: Some("hi".into()),
        raw: json!({"from": sender}),
        received_at: ts,
        ..Event::new(
            format!("whatsapp:wamid.IN.{sender}.{ts}"),
            Platform::WhatsApp,
            EventKind::Message,
        )
    }
}

fn multipart_with(needles: &'static [&'static str]) -> impl Fn(&Request) -> bool + Send + Sync {
    move |req: &Request| {
        let content_type = req
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&req.body);
        content_type.starts_with("multipart/form-data") && needles.iter().all(|n| body.contains(n))
    }
}

fn upload_mock(media_id: &str) -> Mock {
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/media")))
        .and(header("authorization", bearer().as_str()))
        .and(multipart_with(&[
            "name=\"file\"",
            "name=\"messaging_product\"",
            "name=\"type\"",
        ]))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": media_id})))
        .expect(1)
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_body_and_preview() {
    let graph = mock_graph().await;
    send_mock(
        json!({
            "messaging_product": "whatsapp", "recipient_type": "individual",
            "to": "16505551234", "type": "text",
            "text": {"body": "Hello from the test", "preview_url": true}
        }),
        ok_sent("wamid.OUT1"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let (out, text) = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_text",
            json!({"to": "+1 (650) 555-1234", "body": "Hello from the test", "preview_url": true}),
        );
        assert!(!is_error(&resp), "{resp}");
        (tool_json(&resp), tool_text(&resp))
    })
    .await;
    assert_eq!(out["message_id"], json!("wamid.OUT1"));
    assert_eq!(out["input"], json!("+1 (650) 555-1234"));
    assert_eq!(out["wa_id"], json!("16505551234"));
    assert_eq!(out["phone_number_id"], json!(PNID));
    assert!(out.get("window_warning").is_none(), "empty inbox: {out}");
    assert!(!text.contains(TOKEN));
    let reqs = requests(&graph).await;
    assert_eq!(reqs.len(), 1, "a send is never retried or repeated");
    assert!(
        reqs[0]
            .url
            .query_pairs()
            .any(|(k, _)| k == "appsecret_proof"),
        "{}",
        reqs[0].url
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_rejects_4097_chars_before_any_request() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_text",
            json!({"to": "16505551234", "body": "x".repeat(4097)}),
        );
        assert!(is_error(&resp), "{resp}");
        let text = tool_text(&resp);
        assert!(text.contains("4097") && text.contains("4096"), "{text}");
        for (to, body, needle) in [
            ("16505551234", "   ", "must not be empty"),
            ("not a number", "hi", "not a recipient"),
            ("12345", "hi", "not a recipient"),
        ] {
            let resp = s.call_tool("whatsapp_send_text", json!({"to": to, "body": body}));
            assert!(is_error(&resp), "{resp}");
            assert!(tool_text(&resp).contains(needle), "{resp}");
        }
    })
    .await;
    assert!(requests(&graph).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_reply_context() {
    let graph = mock_graph().await;
    send_mock(
        json!({"to": "16505551234", "type": "text", "context": {"message_id": "wamid.QUOTED=="}}),
        ok_sent("wamid.OUT2"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_text",
            json!({"to": "16505551234", "body": "Replying", "reply_to_message_id": "wamid.QUOTED=="}),
        ))
    })
    .await;
    assert_eq!(out["message_id"], json!("wamid.OUT2"));
    let body = body_of(&requests(&graph).await[0]);
    assert!(body["text"].get("preview_url").is_none(), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_window_warning_from_inbox() {
    let graph = mock_graph().await;
    send_mock(json!({"to": "16505551234"}), ok_sent("wamid.OUT3"))
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        rig.insert(&[inbound("16505551234", now_secs() - 30 * 3600)]);
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_text",
            json!({"to": "+16505551234", "body": "Are you there?"}),
        ))
    })
    .await;
    let warning = out["window_warning"].as_str().expect("warning");
    assert!(
        warning.contains("131047") && warning.contains("30 hours"),
        "{warning}"
    );
    assert!(warning.contains("template"), "{warning}");
    assert_eq!(
        out["message_id"],
        json!("wamid.OUT3"),
        "the send still happens"
    );
    assert_eq!(requests_to(&graph, "/messages").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn window_warning_states() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .respond_with(ok_sent("wamid.W"))
        .expect(4)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let outs = blocking(move || {
        let rig = Rig::new();
        rig.insert(&[
            inbound("16505551234", now_secs() - 3600),
            inbound("447911123456", now_secs() - 25 * 3600),
        ]);
        let mut s = rig.session(&uri, &[]);
        let mut send = |to: &str| {
            tool_json(&s.call_tool("whatsapp_send_text", json!({"to": to, "body": "hi"})))
        };
        [
            send("16505551234"),
            send("447911123456"),
            send("33612345678"),
            send("US.13491208655302741918"),
        ]
    })
    .await;
    assert!(
        outs[0].get("window_warning").is_none(),
        "fresh: {}",
        outs[0]
    );
    let stale = outs[1]["window_warning"].as_str().expect("stale");
    assert!(
        stale.contains("25 hours ago") && stale.contains("131047"),
        "{stale}"
    );
    for unknown in [&outs[2], &outs[3]] {
        let none = unknown["window_warning"].as_str().expect("no inbound");
        assert!(
            none.contains("no inbound message from this user") && none.contains("only templates"),
            "{none}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn window_warning_reads_only_whatsapp_messages() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .respond_with(ok_sent("wamid.W2"))
        .expect(3)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let outs = blocking(move || {
        let rig = Rig::new();
        let messenger = Event {
            ts: now_secs() - 40 * 3600,
            account_id: "PAGE1".into(),
            sender: "16505551234".into(),
            ..Event::new("messenger:m1", Platform::Messenger, EventKind::Message)
        };
        rig.insert(&[messenger]);
        let only_messenger = {
            let mut s = rig.session(&uri, &[]);
            let args = json!({"to": "16505551234", "body": "hi"});
            tool_json(&s.call_tool("whatsapp_send_text", args))
        };
        let status = Event {
            ts: now_secs(),
            account_id: PNID.into(),
            sender: PNID.into(),
            recipient: "16505551234".into(),
            ..Event::new(
                "whatsapp:status:w:sent",
                Platform::WhatsApp,
                EventKind::Status,
            )
        };
        let with_user_id = Event {
            raw: json!({"from": "447911123456", "_sender_user_id": "US.55"}),
            ..inbound("447911123456", now_secs() - 3600)
        };
        rig.insert(&[status, with_user_id]);
        let mut s = rig.session(&uri, &[]);
        let mut send = |to: &str| {
            tool_json(&s.call_tool("whatsapp_send_text", json!({"to": to, "body": "hi"})))
        };
        [only_messenger, send("16505551234"), send("US.55")]
    })
    .await;
    assert!(
        outs[0].get("window_warning").is_none(),
        "another platform says nothing about WhatsApp: {}",
        outs[0]
    );
    let none = outs[1]["window_warning"].as_str().expect("no inbound");
    assert!(none.contains("no inbound message from this user"), "{none}");
    assert!(
        outs[2].get("window_warning").is_none(),
        "a business-scoped id matches the stored sender id: {}",
        outs[2]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn send_to_a_business_scoped_id_uses_recipient() {
    let graph = mock_graph().await;
    let answer = json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}],
        "messages": [{"id": "wamid.BS", "message_status": "accepted"}]
    });
    send_mock(
        json!({"recipient": "US.13491208655302741918", "type": "text"}),
        ResponseTemplate::new(200).set_body_json(answer),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_text",
            json!({"to": "US.13491208655302741918", "body": "hi"}),
        ))
    })
    .await;
    assert_eq!(out["user_id"], json!("US.13491208655302741918"));
    assert_eq!(out["message_status"], json!("accepted"));
    assert!(out.get("wa_id").is_none());
    let body = body_of(&requests(&graph).await[0]);
    assert!(body.get("to").is_none(), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_by_path_uploads_then_sends() {
    let graph = mock_graph().await;
    upload_mock("777").mount(&graph).await;
    send_mock(
        json!({
            "to": "16505551234", "type": "image",
            "image": {"id": "777", "caption": "A tiny cat"}
        }),
        ok_sent("wamid.IMG1"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let bytes = png();
    let expected = bytes.clone();
    let out = blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("cat.png"), &bytes).expect("write png");
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "image", "path": "cat.png", "caption": "A tiny cat"}),
        );
        assert!(!is_error(&resp), "{resp}");
        tool_json(&resp)
    })
    .await;
    assert_eq!(out["message_id"], json!("wamid.IMG1"));
    assert_eq!(out["media_id"], json!("777"));
    assert_eq!(out["uploaded_bytes"], json!(expected.len()));
    let upload = &requests_to(&graph, "/media").await[0];
    let raw = String::from_utf8_lossy(&upload.body).into_owned();
    for needle in [
        "name=\"messaging_product\"",
        "whatsapp",
        "name=\"type\"",
        "name=\"file\"; filename=\"cat.png\"",
        "Content-Type: image/png",
    ] {
        assert!(raw.contains(needle), "{needle} missing in {raw:?}");
    }
    assert!(
        upload
            .body
            .windows(expected.len())
            .any(|w| w == expected.as_slice()),
        "the file bytes travel in the upload"
    );
    let order: Vec<String> = requests(&graph)
        .await
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        order,
        [
            format!("/v26.0/{PNID}/media"),
            format!("/v26.0/{PNID}/messages")
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_rejects_oversize_image_before_any_request() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("big.png"), vec![0u8; 5 * 1024 * 1024 + 1]).expect("write");
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "image", "path": "big.png"}),
        );
        assert!(is_error(&resp), "{resp}");
        let text = tool_text(&resp);
        assert!(text.contains("5242881") && text.contains("5 MiB"), "{text}");
        let resp = s.call_tool("whatsapp_upload_media", json!({"path": "big.png"}));
        assert!(is_error(&resp), "{resp}");
    })
    .await;
    assert!(requests(&graph).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_accepts_a_file_at_the_limit() {
    let graph = mock_graph().await;
    upload_mock("888").mount(&graph).await;
    send_mock(
        json!({"type": "image", "image": {"id": "888"}}),
        ok_sent("wamid.EDGE"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("edge.jpg"), vec![1u8; 5 * 1024 * 1024]).expect("write");
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "image", "path": "edge.jpg"}),
        ))
    })
    .await;
    assert_eq!(out["uploaded_bytes"], json!(5 * 1024 * 1024));
    let raw = String::from_utf8_lossy(&requests_to(&graph, "/media").await[0].body).into_owned();
    assert!(
        raw.contains("Content-Type: image/jpeg"),
        "jpg maps to image/jpeg"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_by_link() {
    let graph = mock_graph().await;
    send_mock(
        json!({
            "to": "16505551234", "type": "image",
            "image": {"link": "https://files.example.org/cat.png", "caption": "Look"}
        }),
        ok_sent("wamid.LNK"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "IMAGE", "url": "https://files.example.org/cat.png", "caption": "Look"}),
        ))
    })
    .await;
    assert_eq!(out["message_id"], json!("wamid.LNK"));
    assert!(out.get("media_id").is_none() && out.get("uploaded_bytes").is_none());
    let reqs = requests(&graph).await;
    assert_eq!(reqs.len(), 1, "a link needs no upload");
    assert!(body_of(&reqs[0])["image"].get("id").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_by_id_and_document_options() {
    let graph = mock_graph().await;
    send_mock(
        json!({"type": "document", "document": {"id": "4321", "caption": "Q3", "filename": "q3.pdf"}}),
        ok_sent("wamid.DOC"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "document", "media_id": "4321", "caption": "Q3", "filename": "q3.pdf"}),
        ))
    })
    .await;
    assert_eq!(out["media_id"], json!("4321"));
    assert!(out.get("uploaded_bytes").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_media_argument_errors_are_caught_locally() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("clip.mp3"), b"ID3....").expect("write");
        std::fs::write(rig.file("note.txt"), b"hello").expect("write");
        std::fs::write(rig.file("pic.png"), png()).expect("write");
        let mut s = rig.session(&uri, &[]);
        let base = json!({"to": "16505551234"});
        let cases = [
            (
                json!({"kind": "image"}),
                "exactly one of media_id, url or path",
            ),
            (
                json!({"kind": "image", "media_id": "1", "url": "https://x.example/a.png"}),
                "exactly one",
            ),
            (json!({"kind": "gif", "media_id": "1"}), "unknown kind"),
            (
                json!({"kind": "image", "url": "http://x.example/a.png"}),
                "https",
            ),
            (
                json!({"kind": "audio", "media_id": "1", "caption": "no"}),
                "cannot have a caption",
            ),
            (
                json!({"kind": "sticker", "media_id": "1", "caption": "no"}),
                "cannot have a caption",
            ),
            (
                json!({"kind": "image", "media_id": "1", "caption": "c".repeat(1025)}),
                "1024",
            ),
            (
                json!({"kind": "image", "media_id": "1", "filename": "a.png"}),
                "documents only",
            ),
            (
                json!({"kind": "video", "media_id": "1", "voice": true}),
                "audio only",
            ),
            (
                json!({"kind": "audio", "path": "clip.mp3", "voice": true}),
                "audio/ogg",
            ),
            (json!({"kind": "document", "path": "pic.png"}), "kind=image"),
            (
                json!({"kind": "image", "path": "note.txt"}),
                "kind=document",
            ),
            (
                json!({"kind": "image", "path": "missing.png"}),
                "no such file",
            ),
            (
                json!({"kind": "image", "path": "../outside.png"}),
                "outside META_MCP_FILES_DIR",
            ),
            (
                json!({"kind": "image", "path": "pic.png", "mime_type": "image/gif"}),
                "unsupported mime_type",
            ),
            (
                json!({"kind": "image", "media_id": "a/b"}),
                "not a valid id",
            ),
        ];
        for (extra, needle) in cases {
            let mut args = base.clone();
            args.as_object_mut()
                .expect("object")
                .extend(extra.as_object().expect("object").clone());
            let resp = s.call_tool("whatsapp_send_media", args.clone());
            assert!(is_error(&resp), "{args}: {resp}");
            assert!(tool_text(&resp).contains(needle), "{args}: {resp}");
        }
    })
    .await;
    assert!(requests(&graph).await.is_empty(), "nothing reached Meta");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_upload_defaults_its_filename() {
    let graph = mock_graph().await;
    upload_mock("4242").mount(&graph).await;
    send_mock(
        json!({"type": "document", "document": {"id": "4242", "filename": "Quarterly report.pdf"}}),
        ok_sent("wamid.DOC2"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("Quarterly report.pdf"), b"%PDF-1.7").expect("write");
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "document", "path": "Quarterly report.pdf"}),
        ))
    })
    .await;
    assert_eq!(out["media_id"], json!("4242"));
    let raw = String::from_utf8_lossy(&requests_to(&graph, "/media").await[0].body).into_owned();
    assert!(raw.contains("Content-Type: application/pdf"), "{raw:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_note_goes_out_as_ogg() {
    let graph = mock_graph().await;
    upload_mock("901").mount(&graph).await;
    send_mock(
        json!({"type": "audio", "audio": {"id": "901", "voice": true}}),
        ok_sent("wamid.VOICE"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("note.ogg"), b"OggS....").expect("write");
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "audio", "path": "note.ogg", "voice": true, "mime_type": "audio/ogg; codecs=opus"}),
        );
        assert!(!is_error(&resp), "{resp}");
    })
    .await;
    let raw = String::from_utf8_lossy(&requests_to(&graph, "/media").await[0].body).into_owned();
    assert!(raw.contains("audio/ogg; codecs=opus"), "{raw:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_failure_after_upload_keeps_the_media_id() {
    let graph = mock_graph().await;
    upload_mock("777").mount(&graph).await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .respond_with(ResponseTemplate::new(400).set_body_json(wa_error(
            131047,
            "(#131047) Re-engagement message",
            "Message failed to send because more than 24 hours have passed",
        )))
        .expect(1)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let text = blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("cat.png"), png()).expect("write");
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_media",
            json!({"to": "16505551234", "kind": "image", "path": "cat.png"}),
        );
        assert!(is_error(&resp), "{resp}");
        tool_text(&resp)
    })
    .await;
    assert!(
        text.contains("media_id 777") && text.contains("131047"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn path_tools_name_the_files_variable_without_a_jail() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let home = TempHome::new();
        let mut s = Session::start(
            &home,
            &uri,
            &[
                ("META_WHATSAPP_ACCESS_TOKEN", TOKEN),
                ("META_WHATSAPP_PHONE_NUMBER_ID", PNID),
            ],
        );
        for (tool, args) in [
            (
                "whatsapp_send_media",
                json!({"to": "16505551234", "kind": "image", "path": "a.png"}),
            ),
            ("whatsapp_upload_media", json!({"path": "a.png"})),
            ("whatsapp_download_media", json!({"media_id": "999"})),
        ] {
            let resp = s.call_tool(tool, args);
            assert!(is_error(&resp), "{tool}: {resp}");
            assert!(
                tool_text(&resp).contains("META_MCP_FILES_DIR"),
                "{tool}: {resp}"
            );
        }
    })
    .await;
    assert!(requests(&graph).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_template_components() {
    let graph = mock_graph().await;
    let components = json!([
        {"type": "header", "parameters": [{"type": "image", "image": {"link": "https://x.example/h.png"}}]},
        {"type": "body", "parameters": [
            {"type": "text", "text": "Ann"},
            {"type": "text", "parameter_name": "order", "text": "A-1001"}
        ]},
        {"type": "button", "sub_type": "quick_reply", "index": "0",
         "parameters": [{"type": "payload", "payload": "TRACK"}]}
    ]);
    send_mock(
        json!({"to": "16505551234", "type": "template",
               "template": {"name": "order_update", "language": {"code": "pt_BR"}}}),
        ok_sent("wamid.TPL1"),
    )
    .mount(&graph)
    .await;
    send_mock(
        json!({"type": "template", "template": {"name": "hello_world", "language": {"code": "en_US"}}}),
        ok_sent("wamid.TPL2"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let sent_components = components.clone();
    let (first, second) = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let first = tool_json(&s.call_tool(
            "whatsapp_send_template",
            json!({"to": "16505551234", "name": "order_update", "language": "pt_BR", "components": sent_components}),
        ));
        let second = tool_json(&s.call_tool(
            "whatsapp_send_template",
            json!({"to": "16505551234", "name": "hello_world"}),
        ));
        (first, second)
    })
    .await;
    assert_eq!(first["message_id"], json!("wamid.TPL1"));
    assert_eq!(second["message_id"], json!("wamid.TPL2"));
    assert!(
        first.get("window_warning").is_none(),
        "templates skip the window check"
    );
    let reqs = requests(&graph).await;
    assert_eq!(body_of(&reqs[0])["template"]["components"], components);
    assert!(body_of(&reqs[1])["template"].get("components").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_template_argument_errors_are_caught_locally() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        for (args, needle) in [
            (
                json!({"to": "16505551234", "name": "hello_world", "components": {"type": "body"}}),
                "JSON array",
            ),
            (
                json!({"to": "16505551234", "name": "hello_world", "components": "not json"}),
                "JSON array",
            ),
            (
                json!({"to": "16505551234", "name": "Hello World"}),
                "template name",
            ),
            (
                json!({"to": "16505551234", "name": "hello_world", "language": "en US"}),
                "language",
            ),
        ] {
            let resp = s.call_tool("whatsapp_send_template", args.clone());
            assert!(is_error(&resp), "{args}: {resp}");
            assert!(tool_text(&resp).contains(needle), "{args}: {resp}");
        }
    })
    .await;
    assert!(requests(&graph).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_reaction_and_removal() {
    let graph = mock_graph().await;
    send_mock(
        json!({"to": "16505551234", "type": "reaction",
               "reaction": {"message_id": "wamid.TARGET==", "emoji": "\u{1F600}"}}),
        ok_sent("wamid.R1"),
    )
    .mount(&graph)
    .await;
    send_mock(
        json!({"type": "reaction", "reaction": {"message_id": "wamid.TARGET==", "emoji": ""}}),
        ok_sent("wamid.R2"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let (added, removed) = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let added = tool_json(&s.call_tool(
            "whatsapp_send_reaction",
            json!({"to": "16505551234", "message_id": "wamid.TARGET==", "emoji": "\u{1F600}"}),
        ));
        let removed = tool_json(&s.call_tool(
            "whatsapp_send_reaction",
            json!({"to": "16505551234", "message_id": "wamid.TARGET==", "emoji": ""}),
        ));
        (added, removed)
    })
    .await;
    assert_eq!(added["message_id"], json!("wamid.R1"));
    assert_eq!(removed["message_id"], json!("wamid.R2"));
    for req in requests(&graph).await {
        assert!(
            body_of(&req).get("context").is_none(),
            "reactions take no context"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_location() {
    let graph = mock_graph().await;
    send_mock(
        json!({
            "to": "16505551234", "type": "location",
            "location": {
                "latitude": "37.440000", "longitude": "-122.160000",
                "name": "Philz Coffee", "address": "101 Forest Ave, Palo Alto, CA 94301"
            }
        }),
        ok_sent("wamid.LOC"),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let (ok, bad) = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let ok = tool_json(&s.call_tool(
            "whatsapp_send_location",
            json!({"to": "16505551234", "latitude": 37.44, "longitude": -122.16,
                   "name": "Philz Coffee", "address": "101 Forest Ave, Palo Alto, CA 94301"}),
        ));
        let bad = s.call_tool(
            "whatsapp_send_location",
            json!({"to": "16505551234", "latitude": 91.0, "longitude": 0.0}),
        );
        (ok, bad)
    })
    .await;
    assert_eq!(ok["message_id"], json!("wamid.LOC"));
    assert!(
        is_error(&bad) && tool_text(&bad).contains("latitude"),
        "{bad}"
    );
    assert_eq!(requests(&graph).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_read_with_typing_indicator() {
    let graph = mock_graph().await;
    send_mock(
        json!({"messaging_product": "whatsapp", "status": "read", "message_id": "wamid.IN1==",
               "typing_indicator": {"type": "text"}}),
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
    )
    .mount(&graph)
    .await;
    send_mock(
        json!({"status": "read", "message_id": "wamid.IN2"}),
        ResponseTemplate::new(200).set_body_json(json!({"success": true})),
    )
    .mount(&graph)
    .await;
    let uri = graph.uri();
    let (typing, plain, bad) = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let typing = tool_json(&s.call_tool(
            "whatsapp_mark_read",
            json!({"message_id": "wamid.IN1==", "typing": true}),
        ));
        let plain =
            tool_json(&s.call_tool("whatsapp_mark_read", json!({"message_id": "wamid.IN2"})));
        let bad = s.call_tool("whatsapp_mark_read", json!({"message_id": "a/b"}));
        (typing, plain, bad)
    })
    .await;
    assert_eq!(typing["success"], json!(true));
    assert_eq!(typing["typing_indicator"], json!(true));
    assert_eq!(plain["typing_indicator"], json!(false));
    assert!(is_error(&bad));
    let reqs = requests(&graph).await;
    assert_eq!(reqs.len(), 2, "the bad id never left");
    assert!(body_of(&reqs[1]).get("typing_indicator").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_media_returns_id() {
    let graph = mock_graph().await;
    upload_mock("1037543291543636").mount(&graph).await;
    let uri = graph.uri();
    let out = blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("report.pdf"), b"%PDF-1.4 test").expect("write");
        let mut s = rig.session(&uri, &[]);
        tool_json(&s.call_tool("whatsapp_upload_media", json!({"path": "report.pdf"})))
    })
    .await;
    assert_eq!(out["media_id"], json!("1037543291543636"));
    assert_eq!(out["bytes"], json!(13));
    assert_eq!(out["mime"], json!("application/pdf"));
    assert_eq!(out["kind"], json!("document"));
    assert_eq!(out["phone_number_id"], json!(PNID));
    let raw = String::from_utf8_lossy(&requests_to(&graph, "/media").await[0].body).into_owned();
    assert!(raw.contains("Content-Type: application/pdf"), "{raw:?}");
    assert!(raw.contains("filename=\"report.pdf\""), "{raw:?}");
}

async fn mount_media(graph: &MockServer, bytes: &[u8], mime: &str, sha256: Option<&str>) {
    let mut info = json!({
        "messaging_product": "whatsapp",
        "url": format!("{}/blob/999", graph.uri()),
        "mime_type": mime,
        "file_size": bytes.len().to_string(),
        "id": "999"
    });
    if let Some(hash) = sha256 {
        info["sha256"] = json!(hash);
    }
    Mock::given(method("GET"))
        .and(path("/v26.0/999"))
        .and(query_param("phone_number_id", PNID))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/blob/999"))
        .and(header("authorization", bearer().as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(graph)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_two_step_with_bearer_and_sha256() {
    let graph = mock_graph().await;
    let bytes = png();
    let digest = hex::encode(Sha256::digest(&bytes));
    mount_media(&graph, &bytes, "image/png", Some(&digest.to_uppercase())).await;
    let uri = graph.uri();
    let rig = Rig::new();
    let (rig, first, second) = blocking(move || {
        let mut s = rig.session(&uri, &[]);
        let first = s.call_tool("whatsapp_download_media", json!({"media_id": "999"}));
        let second = s.call_tool("whatsapp_download_media", json!({"media_id": "999"}));
        drop(s);
        (rig, first, second)
    })
    .await;
    assert!(!is_error(&first), "{first}");
    let out = tool_json(&first);
    let saved = rig.file("whatsapp_999.png");
    assert_eq!(out["media_id"], json!("999"));
    assert_eq!(out["bytes"], json!(bytes.len()));
    assert_eq!(out["mime"], json!("image/png"));
    assert_eq!(out["sha256"], json!(digest));
    assert_eq!(out["sha256_checked"], json!(true));
    assert_eq!(
        out["path"],
        json!(
            std::fs::canonicalize(&saved)
                .expect("saved")
                .display()
                .to_string()
        )
    );
    assert_eq!(std::fs::read(&saved).expect("file"), bytes);
    let block = &first["result"]["content"][1];
    assert_eq!(block["type"], json!("image"), "{first}");
    assert_eq!(block["mimeType"], json!("image/png"));
    assert_eq!(
        block["data"],
        json!(base64::engine::general_purpose::STANDARD.encode(&bytes))
    );
    assert!(is_error(&second), "{second}");
    assert!(tool_text(&second).contains("already exists"), "{second}");
    assert_eq!(
        requests_to(&graph, "/blob/999").await.len(),
        1,
        "an existing destination fails before the second download"
    );
    let blob = &requests_to(&graph, "/blob/999").await[0];
    assert_eq!(
        blob.headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(bearer().as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_refuses_foreign_url_host() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messaging_product": "whatsapp", "url": "https://evil.example/x",
            "mime_type": "image/png", "sha256": "0".repeat(64), "file_size": "10", "id": "999"
        })))
        .expect(1)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let rig = Rig::new();
    let (rig, resp) = blocking(move || {
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool("whatsapp_download_media", json!({"media_id": "999"}));
        drop(s);
        (rig, resp)
    })
    .await;
    assert!(is_error(&resp), "{resp}");
    let text = tool_text(&resp);
    assert!(
        text.contains("evil.example") && text.contains("refusing"),
        "{text}"
    );
    assert!(!text.contains(TOKEN));
    assert_eq!(
        requests(&graph).await.len(),
        1,
        "only the id lookup reached the mock"
    );
    assert!(!rig.file("whatsapp_999.png").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_sha_mismatch_saves_nothing() {
    let graph = mock_graph().await;
    mount_media(&graph, &png(), "image/png", Some(&"0".repeat(64))).await;
    let uri = graph.uri();
    let rig = Rig::new();
    let (rig, resp) = blocking(move || {
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool("whatsapp_download_media", json!({"media_id": "999"}));
        drop(s);
        (rig, resp)
    })
    .await;
    assert!(is_error(&resp), "{resp}");
    assert!(tool_text(&resp).contains("SHA-256"), "{resp}");
    assert!(
        std::fs::read_dir(rig.files.path())
            .expect("dir")
            .next()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_options_and_unchecked_digest() {
    let graph = mock_graph().await;
    let pdf = b"%PDF-1.4 inbound".to_vec();
    mount_media(&graph, &pdf, "application/pdf", Some("not-hex-at-all")).await;
    let uri = graph.uri();
    let rig = Rig::new();
    let (rig, first, replaced) = blocking(move || {
        let mut s = rig.session(&uri, &[]);
        let args = json!({"media_id": "999", "dest_name": "inbox/doc.pdf", "inline": false});
        let first = s.call_tool("whatsapp_download_media", args);
        let mut again = json!({"media_id": "999", "dest_name": "inbox/doc.pdf", "overwrite": true});
        let replaced = s.call_tool("whatsapp_download_media", again.take());
        drop(s);
        (rig, first, replaced)
    })
    .await;
    let out = tool_json(&first);
    assert_eq!(out["sha256_checked"], json!(false));
    assert_eq!(out["mime"], json!("application/pdf"));
    assert_eq!(
        first["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    assert_eq!(
        std::fs::read(rig.file("inbox/doc.pdf")).expect("saved"),
        pdf
    );
    assert!(!is_error(&replaced), "{replaced}");
    let default_named = rig.file("whatsapp_999.pdf");
    assert!(
        !default_named.exists(),
        "dest_name replaces the default name"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_does_not_inline_big_images() {
    let graph = mock_graph().await;
    let big = vec![9u8; 2 * 1024 * 1024 + 1];
    mount_media(&graph, &big, "image/jpeg", None).await;
    let uri = graph.uri();
    let rig = Rig::new();
    let (rig, resp) = blocking(move || {
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool("whatsapp_download_media", json!({"media_id": "999"}));
        drop(s);
        (rig, resp)
    })
    .await;
    assert!(!is_error(&resp), "{resp}");
    assert_eq!(
        resp["result"]["content"].as_array().expect("content").len(),
        1
    );
    assert_eq!(tool_json(&resp)["sha256_checked"], json!(false));
    assert_eq!(
        std::fs::metadata(rig.file("whatsapp_999.jpg"))
            .expect("saved")
            .len(),
        big.len() as u64
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_media_checks_the_destination_before_any_request() {
    let graph = mock_graph().await;
    let uri = graph.uri();
    blocking(move || {
        let rig = Rig::new();
        std::fs::write(rig.file("taken.png"), b"old").expect("write");
        let mut s = rig.session(&uri, &[]);
        for (dest, needle) in [
            ("../escape.png", "outside META_MCP_FILES_DIR"),
            (".hidden.png", "must not start with"),
            ("taken.png", "already exists"),
        ] {
            let args = json!({"media_id": "999", "dest_name": dest});
            let resp = s.call_tool("whatsapp_download_media", args);
            assert!(is_error(&resp), "{dest}: {resp}");
            assert!(tool_text(&resp).contains(needle), "{dest}: {resp}");
        }
        let resp = s.call_tool("whatsapp_download_media", json!({"media_id": "a/b"}));
        assert!(
            is_error(&resp) && tool_text(&resp).contains("not a valid id"),
            "{resp}"
        );
    })
    .await;
    assert!(requests(&graph).await.is_empty(), "nothing reached Meta");
}

#[tokio::test(flavor = "multi_thread")]
async fn hint_131047_outside_window() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .respond_with(ResponseTemplate::new(400).set_body_json(wa_error(
            131047,
            "(#131047) Re-engagement message",
            "Message failed to send because more than 24 hours have passed since the customer last replied to this number.",
        )))
        .expect(1)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let text = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let resp = s.call_tool(
            "whatsapp_send_text",
            json!({"to": "16505551234", "body": "Too late?"}),
        );
        assert!(is_error(&resp), "{resp}");
        tool_text(&resp)
    })
    .await;
    assert!(
        text.contains("131047") && text.contains("template"),
        "{text}"
    );
    assert!(text.contains("whatsapp_send_template"), "{text}");
    assert!(text.contains("[fbtrace TRACE1]"), "{text}");
    assert!(!text.contains(TOKEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn api_errors_carry_whatsapp_hints_and_sends_are_never_retried() {
    let graph = mock_graph().await;
    for (code, _) in [(132001, ""), (130429, "")] {
        Mock::given(method("POST"))
            .and(path(format!("/v26.0/{PNID}/messages")))
            .and(body_partial_json(
                json!({"type": "template", "template": {"name": format!("t{code}")}}),
            ))
            .respond_with(ResponseTemplate::new(400).set_body_json(wa_error(
                code,
                &format!("(#{code}) Something"),
                "details",
            )))
            .expect(1)
            .mount(&graph)
            .await;
    }
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PNID}/messages")))
        .and(body_partial_json(json!({"type": "text"})))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": {
            "message": "Invalid OAuth access token", "type": "OAuthException", "code": 190, "fbtrace_id": "T"}})))
        .expect(1)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let texts = blocking(move || {
        let rig = Rig::new();
        let mut s = rig.session(&uri, &[]);
        let mut call = |tool: &str, args: Value| {
            let resp = s.call_tool(tool, args);
            assert!(is_error(&resp), "{resp}");
            tool_text(&resp)
        };
        [
            call(
                "whatsapp_send_template",
                json!({"to": "16505551234", "name": "t132001"}),
            ),
            call(
                "whatsapp_send_template",
                json!({"to": "16505551234", "name": "t130429"}),
            ),
            call(
                "whatsapp_send_text",
                json!({"to": "16505551234", "body": "hi"}),
            ),
        ]
    })
    .await;
    assert!(texts[0].contains("not approved"), "{}", texts[0]);
    assert!(texts[1].contains("throughput limit"), "{}", texts[1]);
    assert!(
        texts[2].contains("META_WHATSAPP_ACCESS_TOKEN"),
        "{}",
        texts[2]
    );
    assert_eq!(
        requests(&graph).await.len(),
        3,
        "no automatic retries on sends"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn several_numbers_need_phone_number_id() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path("/v26.0/222/messages"))
        .respond_with(ok_sent("wamid.TWO"))
        .expect(1)
        .mount(&graph)
        .await;
    let uri = graph.uri();
    let (ambiguous, picked, unknown) = blocking(move || {
        let home = TempHome::new();
        home.write_credentials(&json!({
            "version": 1,
            "whatsapp": {"waba_id": "W1", "token": TOKEN, "token_kind": "system_user",
                "phone_numbers": [
                    {"id": "111", "display_phone_number": "", "verified_name": ""},
                    {"id": "222", "display_phone_number": "", "verified_name": ""}]}
        }));
        let mut s = Session::start(&home, &uri, &[]);
        let args = json!({"to": "16505551234", "body": "hi"});
        let ambiguous = s.call_tool("whatsapp_send_text", args.clone());
        let mut with_number = args.clone();
        with_number["phone_number_id"] = json!("222");
        let picked = s.call_tool("whatsapp_send_text", with_number);
        let mut wrong = args;
        wrong["phone_number_id"] = json!("333");
        let unknown = s.call_tool("whatsapp_send_text", wrong);
        (ambiguous, picked, unknown)
    })
    .await;
    assert!(
        is_error(&ambiguous) && tool_text(&ambiguous).contains("several phone numbers"),
        "{ambiguous}"
    );
    assert_eq!(tool_json(&picked)["phone_number_id"], json!("222"));
    assert!(
        is_error(&unknown) && tool_text(&unknown).contains("not configured"),
        "{unknown}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_library_api_works_without_the_mcp_layer() {
    let graph = mock_graph().await;
    send_mock(
        json!({"to": "16505551234", "type": "text"}),
        ok_sent("wamid.LIB"),
    )
    .mount(&graph)
    .await;
    let dir = tempfile::tempdir().expect("dir");
    let inbox_db = dir.path().join("inbox.db").display().to_string();
    let env = [
        ("META_WHATSAPP_ACCESS_TOKEN", TOKEN.to_string()),
        ("META_WHATSAPP_PHONE_NUMBER_ID", PNID.to_string()),
        ("META_GRAPH_BASE_URL", graph.uri()),
        ("META_MCP_INBOX_DB", inbox_db),
    ];
    let cfg = Config::from_lookup(
        |k| {
            env.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.clone())
        },
        Some(dir.path().to_path_buf()),
    )
    .expect("config");
    let client = GraphClient::new(&cfg.graph, cfg.download_hosts.clone()).expect("client");
    let inbox = Inbox::open(&cfg.inbox_db).expect("inbox");
    inbox
        .insert(&[inbound("16505551234", now_secs() - 48 * 3600)])
        .expect("insert");
    let wa = WhatsApp::new(Arc::new(cfg), Arc::new(client), inbox);

    let warning = wa.window_warning(PNID, "+1 650 555 1234").await;
    assert!(warning.expect("stale").contains("48 hours"));
    assert_eq!(wa.window_warning(PNID, "not a number").await, None);

    let out = wa
        .send_text(SendTextParams {
            to: "+1 650 555 1234".into(),
            body: "from the library".into(),
            ..SendTextParams::default()
        })
        .await
        .expect("send");
    assert_eq!(out["message_id"], json!("wamid.LIB"));
    assert!(
        out["window_warning"]
            .as_str()
            .expect("warning")
            .contains("131047")
    );
    assert_eq!(
        normalize_phone("(650) 555-1234").expect("digits"),
        "6505551234"
    );
}

fn wa_payload(contacts: Value, messages: Value, statuses: Value) -> Value {
    json!({"object": "whatsapp_business_account", "entry": [{"id": "WABA_ID", "changes": [{
        "field": "messages",
        "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": "PN1"},
            "contacts": contacts, "messages": messages, "statuses": statuses
        }
    }]}]})
}

#[test]
fn normalize_bsuid_message_without_from() {
    let payload = wa_payload(
        json!([
            {"profile": {"name": "Ana Souza", "username": "@ana.souza"}, "user_id": "US.13491208655302741918"},
            {"profile": {"name": "Bob"}, "wa_id": "16505551234", "user_id": "US.2222222222"}
        ]),
        json!([
            {"from_user_id": "US.13491208655302741918", "id": "wamid.B1", "timestamp": "1750000000",
             "type": "text", "text": {"body": "hello from a username"}},
            {"from": "16505551234", "from_user_id": "US.2222222222", "from_parent_user_id": "US.ENT.9",
             "id": "wamid.B2", "timestamp": "1750000001", "type": "text", "text": {"body": "both ids"}},
            {"id": "wamid.B3", "timestamp": "1750000002", "type": "text", "text": {"body": "nobody"}}
        ]),
        json!([]),
    );
    let events = normalize(&payload, NOW);
    assert_eq!(events.len(), 3);
    let by_id = &events[0];
    assert_eq!(by_id.id, "whatsapp:wamid.B1");
    assert_eq!(
        (by_id.platform, by_id.kind),
        (Platform::WhatsApp, EventKind::Message)
    );
    assert_eq!(by_id.sender, "US.13491208655302741918");
    assert_eq!(by_id.recipient, "PN1");
    assert_eq!(by_id.account_id, "PN1");
    assert_eq!(by_id.text.as_deref(), Some("hello from a username"));
    assert_eq!(by_id.ts, 1_750_000_000);
    assert_eq!(
        by_id.raw["_sender_user_id"],
        json!("US.13491208655302741918")
    );
    assert_eq!(by_id.raw["_sender_name"], json!("Ana Souza"));
    assert_eq!(by_id.raw["_sender_username"], json!("@ana.souza"));
    assert!(
        by_id.raw.get("from").is_none(),
        "the payload is kept as received"
    );
    let both = &events[1];
    assert_eq!(both.sender, "16505551234", "from wins when present");
    assert_eq!(both.raw["_sender_user_id"], json!("US.2222222222"));
    assert_eq!(both.raw["_sender_name"], json!("Bob"));
    assert!(both.raw.get("_sender_username").is_none());
    assert_eq!(both.raw["from_parent_user_id"], json!("US.ENT.9"));
    let nobody = &events[2];
    assert_eq!(nobody.sender, "unknown");
    assert!(
        nobody.raw.get("_sender_user_id").is_none() && nobody.raw.get("_sender_name").is_none()
    );
    assert_eq!(normalize(&payload, NOW), events, "ids are deterministic");
}

#[test]
fn normalize_unsupported_and_edit() {
    let url = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=M2";
    let payload = wa_payload(
        json!([{"profile": {"name": "Ana"}, "wa_id": "15551"}]),
        json!([
            {"from": "15551", "id": "wamid.U1", "timestamp": "1750000100", "type": "unsupported",
             "errors": [{"code": 131051, "title": "Message type is not supported"}]},
            {"from": "15551", "id": "wamid.E1", "timestamp": "1750000101", "type": "edit",
             "edit": {"original_message_id": "wamid.ORIG", "message": {"type": "text", "text": {"body": "corrected text"}}}},
            {"from": "15551", "id": "wamid.E2", "timestamp": "1750000102", "type": "edit",
             "edit": {"original_message_id": "wamid.ORIG2", "message": {"type": "image", "image": {"id": "M1", "caption": "new caption"}}}},
            {"from": "15551", "id": "wamid.E3", "timestamp": "1750000103", "type": "edit",
             "edit": {"original_message_id": "wamid.ORIG3"}},
            {"from": "15551", "id": "wamid.V1", "timestamp": "1750000104", "type": "revoke",
             "revoke": {"original_message_id": "wamid.ORIG"}},
            {"from": "15551", "id": "wamid.S1", "timestamp": "1750000105", "type": "system",
             "system": {"body": "User changed from 15551 to 15552", "type": "user_changed_number", "wa_id": "15552"}},
            {"from": "15551", "id": "wamid.O1", "timestamp": "1750000106", "type": "order",
             "order": {"catalog_id": "C1", "text": "two coffees please", "product_items": [{"product_retailer_id": "p1", "quantity": 2}]}},
            {"from": "15551", "id": "wamid.C1", "timestamp": "1750000107", "type": "contacts",
             "contacts": [{"name": {"formatted_name": "Alice Doe", "first_name": "Alice"}}, {"name": {"formatted_name": "Bob Roe"}}]},
            {"from": "15551", "id": "wamid.BT", "timestamp": "1750000108", "type": "button",
             "button": {"payload": "YES_PAYLOAD", "text": "Yes please"}, "context": {"id": "wamid.TPL"}},
            {"from": "15551", "id": "wamid.LR", "timestamp": "1750000109", "type": "interactive",
             "interactive": {"type": "list_reply", "list_reply": {"id": "row1", "title": "Row One", "description": "the first row"}}},
            {"from": "15551", "id": "wamid.IM", "timestamp": "1750000110", "type": "image",
             "image": {"id": "M2", "mime_type": "image/jpeg", "sha256": "YmFzZTY0", "caption": "cap", "url": url}},
            {"from": "15551", "id": "wamid.AU", "timestamp": "1750000111", "type": "audio",
             "audio": {"id": "M3", "mime_type": "audio/ogg; codecs=opus", "voice": true}},
            {"from": "15551", "id": "wamid.ST", "timestamp": "1750000112", "type": "sticker",
             "sticker": {"id": "M4", "mime_type": "image/webp", "animated": true}},
            {"from": "15551", "id": "wamid.GR", "timestamp": "1750000113", "type": "text",
             "text": {"body": "in a group"}, "group_id": "GRP1"}
        ]),
        json!([]),
    );
    let events = normalize(&payload, NOW);
    assert_eq!(events.len(), 14);
    let e = |i: usize| &events[i];

    let unsupported = e(0);
    assert_eq!(unsupported.kind, EventKind::Message);
    assert!(unsupported.text.is_none() && unsupported.attachments.is_empty());
    assert_eq!(unsupported.raw["errors"][0]["code"], json!(131051));
    assert_eq!(unsupported.raw["_sender_name"], json!("Ana"));

    assert_eq!(e(1).id, "whatsapp:wamid.E1");
    assert_eq!(e(1).kind, EventKind::Other);
    assert_eq!(e(1).text.as_deref(), Some("corrected text"));
    assert_eq!(e(1).reply_to.as_deref(), Some("wamid.ORIG"));
    assert_eq!(e(2).text.as_deref(), Some("new caption"));
    assert_eq!(e(2).reply_to.as_deref(), Some("wamid.ORIG2"));
    assert_eq!((e(3).kind, e(3).text.as_deref()), (EventKind::Other, None));
    assert_eq!(e(3).reply_to.as_deref(), Some("wamid.ORIG3"));

    assert_eq!((e(4).kind, e(4).text.as_deref()), (EventKind::Other, None));
    assert_eq!(e(4).reply_to.as_deref(), Some("wamid.ORIG"));
    assert_eq!(e(5).kind, EventKind::Other);
    assert_eq!(
        e(5).text.as_deref(),
        Some("User changed from 15551 to 15552")
    );
    assert_eq!(e(5).raw["system"]["type"], json!("user_changed_number"));

    assert_eq!(e(6).kind, EventKind::Message);
    assert_eq!(e(6).text.as_deref(), Some("two coffees please"));
    assert_eq!(e(6).raw["order"]["product_items"][0]["quantity"], json!(2));
    assert_eq!(e(7).kind, EventKind::Message);
    assert_eq!(e(7).text.as_deref(), Some("Alice Doe, Bob Roe"));
    assert_eq!(e(8).text.as_deref(), Some("Yes please"));
    assert_eq!(e(8).raw["button"]["payload"], json!("YES_PAYLOAD"));
    assert_eq!(e(8).reply_to.as_deref(), Some("wamid.TPL"));
    assert_eq!(e(9).text.as_deref(), Some("Row One"));
    assert_eq!(
        e(9).raw["interactive"]["list_reply"]["description"],
        json!("the first row")
    );

    let image = &e(10).attachments[0];
    assert_eq!(image.url.as_deref(), Some(url));
    assert_eq!(image.media_id.as_deref(), Some("M2"));
    assert_eq!(image.sha256.as_deref(), Some("YmFzZTY0"));
    assert_eq!(e(10).text.as_deref(), Some("cap"));
    assert_eq!(e(10).raw["image"]["url"], json!(url));
    assert_eq!(e(11).raw["_voice"], json!(true));
    assert_eq!(e(11).raw["audio"]["voice"], json!(true));
    assert_eq!(e(11).attachments[0].kind, "audio");
    assert_eq!(e(12).raw["_animated"], json!(true));
    assert!(e(10).raw.get("_voice").is_none() && e(10).raw.get("_animated").is_none());
    assert_eq!(e(13).raw["_group_id"], json!("GRP1"));
    assert!(e(0).raw.get("_group_id").is_none());
}

#[test]
fn normalize_statuses_with_business_scoped_recipients() {
    let payload = wa_payload(
        json!([]),
        json!([]),
        json!([
            {"id": "wamid.OUT", "status": "sent", "timestamp": "1750000300", "recipient_user_id": "US.1",
             "conversation": {"id": "c1"}},
            {"id": "wamid.OUT", "status": "delivered", "timestamp": "1750000301", "recipient_id": "15551",
             "recipient_user_id": "US.1"},
            {"id": "wamid.OUT", "status": "failed", "timestamp": "1750000302",
             "errors": [{"code": 131047, "title": "Re-engagement message"}]}
        ]),
    );
    let events = normalize(&payload, NOW);
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].recipient, "US.1");
    assert_eq!(events[0].text.as_deref(), Some("sent"));
    assert_eq!(events[0].raw["conversation"]["id"], json!("c1"));
    assert_eq!(events[1].recipient, "15551", "recipient_id wins");
    assert_eq!(events[1].raw["recipient_user_id"], json!("US.1"));
    assert_eq!(events[2].recipient, "");
    assert_eq!(events[2].raw["errors"][0]["code"], json!(131047));
    assert!(
        events
            .iter()
            .all(|e| e.kind == EventKind::Status && e.sender == "PN1")
    );
}

#[test]
fn normalize_survives_malformed_messages() {
    let payload = wa_payload(
        json!(["junk", 7]),
        json!([1, "text", null, {"type": 5}, {"from": 15551, "id": 9, "type": "text", "text": "plain"}]),
        json!([3, "x"]),
    );
    let events = normalize(&payload, NOW);
    assert_eq!(events.len(), 7);
    assert!(events.iter().all(|e| e.platform == Platform::WhatsApp));
    assert_eq!(events[4].sender, "15551");
    assert_eq!(events[4].id, "whatsapp:9");
}

fn list_tools(rig: &Rig, extra: &[(&str, &str)]) -> Vec<Value> {
    let mut s = rig.session("http://127.0.0.1:9", extra);
    let resp = s.request("tools/list", json!({}));
    resp["result"]["tools"].as_array().expect("tools").clone()
}

#[test]
fn tools_list_includes_whatsapp_tools() {
    let rig = Rig::new();
    let tools = list_tools(&rig, &[]);
    let by_name = |n: &str| {
        tools
            .iter()
            .find(|t| t["name"] == json!(n))
            .unwrap_or_else(|| panic!("{n} missing"))
    };
    let mut all: Vec<&str> = WRITE_TOOLS.to_vec();
    all.push("whatsapp_download_media");
    for name in &all {
        let t = by_name(name);
        let description = t["description"].as_str().expect("description");
        assert!(
            description.len() < 1000,
            "{name}: {} chars",
            description.len()
        );
        assert_eq!(t["inputSchema"]["type"], json!("object"), "{name}");
        let schema = t["inputSchema"].to_string();
        for bad in ["uint", "int32", "\"format\":\"int"] {
            assert!(!schema.contains(bad), "{name}: {schema}");
        }
        assert!(
            t["inputSchema"]["properties"]["phone_number_id"]["description"].is_string(),
            "{name} lacks phone_number_id"
        );
        let props = t["inputSchema"]["properties"]
            .as_object()
            .expect("properties");
        for (prop, def) in props {
            assert!(
                def["description"].is_string(),
                "{name}.{prop} lacks a description"
            );
        }
        assert_eq!(t["annotations"]["readOnlyHint"], json!(false), "{name}");
        assert_eq!(t["annotations"]["destructiveHint"], json!(false), "{name}");
        assert_eq!(t["annotations"]["openWorldHint"], json!(true), "{name}");
    }
    for name in [
        "whatsapp_send_text",
        "whatsapp_send_media",
        "whatsapp_send_template",
        "whatsapp_send_reaction",
        "whatsapp_send_location",
        "whatsapp_mark_read",
    ] {
        let d = by_name(name)["description"].as_str().expect("description");
        assert!(
            d.contains("real person") && d.contains("confirm intent"),
            "{name}: {d}"
        );
    }
    let download = by_name("whatsapp_download_media")["description"]
        .as_str()
        .expect("description");
    assert!(
        download.contains("untrusted third-party data"),
        "{download}"
    );
    assert_eq!(
        by_name("whatsapp_send_text")["inputSchema"]["required"],
        json!(["to", "body"])
    );
    assert_eq!(
        by_name("whatsapp_download_media")["inputSchema"]["required"],
        json!(["media_id"])
    );
    assert_eq!(
        by_name("whatsapp_send_template")["inputSchema"]["properties"]["components"]["type"],
        json!("array")
    );
    assert_eq!(
        by_name("whatsapp_send_media")["inputSchema"]["properties"]["kind"]["enum"],
        json!(["image", "video", "audio", "document", "sticker"])
    );
    assert_eq!(
        by_name("whatsapp_send_location")["inputSchema"]["properties"]["latitude"]["type"],
        json!("number")
    );
    for core in [
        "meta_status",
        "inbox_list",
        "inbox_wait",
        "inbox_mark_processed",
    ] {
        by_name(core);
    }
}

#[test]
fn read_only_hides_whatsapp_write_tools() {
    let rig = Rig::new();
    let mut s = rig.session("http://127.0.0.1:9", &[("META_MCP_READ_ONLY", "1")]);
    let names = s.tool_names();
    for hidden in WRITE_TOOLS {
        assert!(
            !names.contains(&hidden.to_string()),
            "{hidden} in {names:?}"
        );
    }
    for kept in ["whatsapp_download_media", "inbox_list", "meta_status"] {
        assert!(
            names.contains(&kept.to_string()),
            "{kept} missing in {names:?}"
        );
    }
    let resp = s.call_tool(
        "whatsapp_send_text",
        json!({"to": "16505551234", "body": "should not leave"}),
    );
    assert!(
        resp["error"].is_object(),
        "a hidden tool is unknown: {resp}"
    );
}
