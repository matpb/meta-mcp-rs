//! Messenger tools against wiremock: the stdio server for the tool layer, the library for the rest.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use common::{Session, TempHome, blocking, mock_graph, tool_json, tool_text};
use meta_mcp::config::{Config, PageConfig, Secret};
use meta_mcp::graph::GraphClient;
use meta_mcp::messenger::{ListConversationsParams, Messenger};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{any, body_json, header, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "EAAtest0123456789ABCDEFGHIJKLMNOP";
const PAGE: &str = "1234567890";
const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
const HEART: &str = "\u{2764}\u{fe0f}";

fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG_B64)
        .expect("png fixture")
}

fn jpeg() -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    bytes.extend_from_slice(b"JFIF fixture");
    bytes
}

fn graph_error(code: i64, subcode: Option<i64>, message: &str) -> Value {
    json!({"error": {"message": message, "type": "OAuthException", "code": code,
        "error_subcode": subcode, "fbtrace_id": "TRACE1"}})
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"] == json!(true)
}

async fn requests(graph: &MockServer) -> Vec<Request> {
    graph.received_requests().await.expect("recording")
}

fn header_of(req: &Request, name: &str) -> Option<String> {
    req.headers
        .get(name)
        .map(|v| v.to_str().expect("ascii header").to_string())
}

/// Runs the calls in order on one stdio server wired to the mock Graph API.
async fn call_many(
    graph: &MockServer,
    files: Option<&Path>,
    calls: Vec<(&'static str, Value)>,
) -> Vec<Value> {
    let uri = graph.uri();
    let files = files.map(|p| p.display().to_string());
    blocking(move || {
        let home = TempHome::new();
        let mut envs = vec![
            ("META_PAGE_ACCESS_TOKEN", TOKEN),
            ("META_PAGE_ID", PAGE),
            ("META_APP_SECRET", "secret"),
            ("META_MCP_DOWNLOAD_HOSTS", "127.0.0.1"),
        ];
        if let Some(dir) = files.as_deref() {
            envs.push(("META_MCP_FILES_DIR", dir));
        }
        let mut session = Session::start(&home, &uri, &envs);
        calls
            .into_iter()
            .map(|(tool, args)| session.call_tool(tool, args))
            .collect()
    })
    .await
}

async fn call(graph: &MockServer, files: &Path, tool: &'static str, args: Value) -> Value {
    call_many(graph, Some(files), vec![(tool, args)])
        .await
        .remove(0)
}

fn files_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("files dir")
}

fn library(graph: &MockServer, files: &Path, page_id: &str) -> Messenger {
    let home = tempfile::tempdir().expect("home");
    let vars: HashMap<&str, String> = HashMap::from([
        ("META_GRAPH_BASE_URL", graph.uri()),
        ("META_PAGE_ACCESS_TOKEN", TOKEN.to_string()),
        ("META_PAGE_ID", page_id.to_string()),
        ("META_APP_SECRET", "secret".to_string()),
        ("META_MCP_FILES_DIR", files.display().to_string()),
        ("META_MCP_DOWNLOAD_HOSTS", "127.0.0.1".to_string()),
        (
            "META_MCP_INBOX_DB",
            home.path().join("inbox.db").display().to_string(),
        ),
    ]);
    let cfg = Config::from_lookup(|k| vars.get(k).cloned(), Some(home.path().to_path_buf()))
        .expect("config");
    let client = GraphClient::new(&cfg.graph, cfg.download_hosts.clone()).expect("client");
    Messenger::new(Arc::new(cfg), Arc::new(client))
}

struct FieldsContain(&'static str);

impl Match for FieldsContain {
    fn matches(&self, req: &Request) -> bool {
        req.url
            .query_pairs()
            .any(|(k, v)| k == "fields" && v.contains(self.0))
    }
}

/// A multipart upload with the file part and the JSON `message` part Meta expects.
struct Upload {
    file_name: &'static str,
    mime: &'static str,
    kind: &'static str,
}

impl Match for Upload {
    fn matches(&self, req: &Request) -> bool {
        let content_type = header_of(req, "content-type").unwrap_or_default();
        let body = String::from_utf8_lossy(&req.body);
        content_type.starts_with("multipart/form-data")
            && body.contains("name=\"filedata\"")
            && body.contains(&format!("filename=\"{}\"", self.file_name))
            && body
                .to_ascii_lowercase()
                .contains(&format!("content-type: {}", self.mime))
            && body.contains("name=\"message\"")
            && body.contains("\"is_reusable\":true")
            && body.contains(&format!("\"type\":\"{}\"", self.kind))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_conversations_maps_counterpart() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path(format!("/v26.0/{PAGE}/conversations")))
        .and(query_param("platform", "messenger"))
        .and(query_param("limit", "5"))
        .and(query_param("after", "CUR1"))
        .and(query_param("user_id", "PSID1"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .and(FieldsContain("snippet"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "t_1", "updated_time": "2026-10-24T12:00:00+0000",
                 "participants": {"data": [
                    {"id": "PSID1", "name": "Alice Martin", "email": "PSID1@facebook.com"},
                    {"id": PAGE, "name": "My Shop", "email": format!("{PAGE}@facebook.com")}]},
                 "snippet": "Hello there", "unread_count": 2, "message_count": 14,
                 "can_reply": true, "link": "/myshop/inbox/1", "is_owner": false},
                {"id": "t_2", "updated_time": "2026-10-23T08:30:00+0000",
                 "participants": {"data": [
                    {"id": 1234567890_i64, "name": "My Shop"},
                    {"id": "PSID2", "name": "Bob"}]}}
            ],
            "paging": {"cursors": {"before": "B1", "after": "CUR2"},
                       "next": "https://graph.facebook.com/v26.0/1234567890/conversations?after=CUR2"}
        })))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let resp = call(
        &graph,
        files.path(),
        "messenger_list_conversations",
        json!({"limit": 5, "after": "CUR1", "user_id": "PSID1"}),
    )
    .await;
    assert!(!is_error(&resp), "{resp}");
    let v = tool_json(&resp);
    assert_eq!(v["page_id"], json!(PAGE));
    assert_eq!(v["next_cursor"], json!("CUR2"));
    assert!(v.get("fields_reduced").is_none());
    let first = &v["conversations"][0];
    assert_eq!(first["id"], json!("t_1"));
    assert_eq!(first["updated_time"], json!("2026-10-24T12:00:00+0000"));
    assert_eq!(
        first["counterpart"],
        json!({"psid": "PSID1", "name": "Alice Martin"})
    );
    assert_eq!(
        first["participants"],
        json!([{"id": "PSID1", "name": "Alice Martin"}, {"id": PAGE, "name": "My Shop"}])
    );
    assert_eq!(first["snippet"], json!("Hello there"));
    assert_eq!(first["unread_count"], json!(2));
    assert_eq!(first["message_count"], json!(14));
    assert_eq!(first["can_reply"], json!(true));
    assert_eq!(first["link"], json!("/myshop/inbox/1"));
    assert_eq!(first["is_owner"], json!(false));
    let second = &v["conversations"][1];
    assert_eq!(
        second["counterpart"],
        json!({"psid": "PSID2", "name": "Bob"})
    );
    assert!(second.get("snippet").is_none() && second.get("unread_count").is_none());

    let sent = requests(&graph).await;
    assert_eq!(sent.len(), 1);
    let query: HashMap<String, String> = sent[0]
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert!(query["fields"].contains("participants{id,name,email}"));
    assert!(!query.contains_key("access_token"));
    assert!(query.contains_key("appsecret_proof"));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_conversations_falls_back_on_unknown_field() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path(format!("/v26.0/{PAGE}/conversations")))
        .and(FieldsContain("snippet"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            None,
            "(#100) Tried accessing nonexisting field (snippet) on node type (Conversation)",
        )))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v26.0/{PAGE}/conversations")))
        .and(query_param("fields", "id,updated_time,participants{id,name,email}"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "t_9", "updated_time": "2026-10-24T12:00:00+0000",
                "participants": {"data": [{"id": PAGE, "name": "Shop"}, {"id": "PSID9", "name": "Zed"}]}}]
        })))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let resp = call(
        &graph,
        files.path(),
        "messenger_list_conversations",
        json!({}),
    )
    .await;
    assert!(!is_error(&resp), "{resp}");
    let v = tool_json(&resp);
    assert_eq!(v["fields_reduced"], json!(true));
    assert_eq!(v["conversations"][0]["counterpart"]["psid"], json!("PSID9"));
    assert!(v["conversations"][0].get("snippet").is_none());
    assert!(v.get("next_cursor").is_none());
    assert_eq!(requests(&graph).await.len(), 2);

    let other = mock_graph().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            None,
            "(#100) Invalid parameter",
        )))
        .expect(1)
        .mount(&other)
        .await;
    let resp = call(
        &other,
        files.path(),
        "messenger_list_conversations",
        json!({}),
    )
    .await;
    assert!(is_error(&resp), "{resp}");
    assert!(tool_text(&resp).contains("Invalid parameter"));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_conversation_parses_attachments_and_shares() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_100/messages"))
        .and(query_param("limit", "3"))
        .and(FieldsContain("attachments"))
        .and(FieldsContain("shares{link,name,description,id}"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
              {"id": "m_3", "created_time": "2022-07-12T19:11:07+0000",
               "from": {"id": "PSID1", "name": "Alice Martin", "email": "PSID1@facebook.com"},
               "to": {"data": [{"id": PAGE, "name": "Shop"}]},
               "message": "Voici les photos",
               "attachments": {"data": [
                 {"id": "a_img", "mime_type": "image/jpeg", "name": "img.jpg", "size": 2048,
                  "image_data": {"url": "https://scontent.xx.fbcdn.net/v/img.jpg",
                    "preview_url": "https://scontent.xx.fbcdn.net/v/img_p.jpg", "width": 640,
                    "height": 480, "max_width": 960, "max_height": 720, "render_as_sticker": false}},
                 {"id": "a_vid", "mime_type": "video/mp4", "name": "clip.mp4", "size": 99999,
                  "video_data": {"url": "https://video.xx.fbcdn.net/v/clip.mp4",
                    "preview_url": "https://video.xx.fbcdn.net/v/clip.jpg", "width": 320,
                    "height": 240, "length": 12, "rotation": 0}},
                 {"id": "a_aud", "mime_type": "audio/mpeg", "name": "voice.mp3", "size": 5000,
                  "file_url": "https://cdn.fbsbx.com/v/voice.mp3"},
                 {"id": "a_pdf", "mime_type": "application/pdf", "name": "invoice.pdf", "size": 12345,
                  "file_url": "https://cdn.fbsbx.com/v/invoice.pdf"},
                 {"id": "a_stk", "image_data": {"url": "https://scontent.xx.fbcdn.net/v/sticker.png",
                    "width": 128, "height": 128, "render_as_sticker": true}}]},
               "shares": {"data": [{"id": "sh1", "link": "https://example.org/post",
                  "name": "A post", "description": "About things"}]}},
              {"id": "m_2", "created_time": "2022-07-12T19:10:00+0000",
               "from": {"id": PAGE, "name": "Shop"},
               "to": {"data": [{"id": "PSID1", "name": "Alice Martin"}]},
               "message": "Merci !", "reply_to": {"mid": "m_1", "is_self_reply": false},
               "tags": {"data": [{"name": "source:chat_plugin"}]}},
              {"id": 7001, "created_time": "2022-07-12T19:09:00+0000",
               "from": {"id": "PSID1", "name": "Alice Martin"}, "message": "Bonjour"}
            ],
            "paging": {"cursors": {"before": "B", "after": "AFTER9"},
                       "next": "https://graph.facebook.com/v26.0/t_100/messages?after=AFTER9"}
        })))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let resp = call(
        &graph,
        files.path(),
        "messenger_read_conversation",
        json!({"conversation_id": "t_100", "limit": 3}),
    )
    .await;
    assert!(!is_error(&resp), "{resp}");
    let v = tool_json(&resp);
    assert_eq!(v["conversation_id"], json!("t_100"));
    assert_eq!(v["next_cursor"], json!("AFTER9"));
    assert!(v.get("note").is_none(), "{v}");
    let m = &v["messages"];
    assert_eq!(m.as_array().expect("messages").len(), 3);

    let first = &m[0];
    assert_eq!(first["id"], json!("m_3"));
    assert_eq!(first["created_time"], json!("2022-07-12T19:11:07+0000"));
    assert_eq!(first["created_at"], json!(1_657_653_067));
    assert_eq!(
        first["from"],
        json!({"id": "PSID1", "name": "Alice Martin"})
    );
    assert_eq!(first["to"], json!([{"id": PAGE, "name": "Shop"}]));
    assert_eq!(first["text"], json!("Voici les photos"));
    assert_eq!(first["is_from_page"], json!(false));
    let atts = &first["attachments"];
    assert_eq!(atts.as_array().expect("attachments").len(), 5);
    assert_eq!(
        atts[0],
        json!({"id": "a_img", "kind": "image", "mime_type": "image/jpeg", "name": "img.jpg",
            "size": 2048, "url": "https://scontent.xx.fbcdn.net/v/img.jpg",
            "preview_url": "https://scontent.xx.fbcdn.net/v/img_p.jpg", "width": 640, "height": 480})
    );
    assert_eq!(
        atts[1],
        json!({"id": "a_vid", "kind": "video", "mime_type": "video/mp4", "name": "clip.mp4",
            "size": 99999, "url": "https://video.xx.fbcdn.net/v/clip.mp4",
            "preview_url": "https://video.xx.fbcdn.net/v/clip.jpg", "width": 320, "height": 240,
            "length": 12})
    );
    assert_eq!(
        atts[2],
        json!({"id": "a_aud", "kind": "audio", "mime_type": "audio/mpeg", "name": "voice.mp3",
            "size": 5000, "url": "https://cdn.fbsbx.com/v/voice.mp3"})
    );
    assert_eq!(
        atts[3],
        json!({"id": "a_pdf", "kind": "file", "mime_type": "application/pdf", "name": "invoice.pdf",
            "size": 12345, "url": "https://cdn.fbsbx.com/v/invoice.pdf"})
    );
    assert_eq!(atts[4]["kind"], json!("sticker"));
    assert_eq!(
        first["shares"],
        json!([{"kind": "share", "id": "sh1", "link": "https://example.org/post",
            "name": "A post", "description": "About things"}])
    );

    let own = &m[1];
    assert_eq!(own["is_from_page"], json!(true));
    assert_eq!(own["reply_to_mid"], json!("m_1"));
    assert_eq!(own["tags"], json!(["source:chat_plugin"]));
    assert_eq!(own["attachments"], json!([]));
    assert_eq!(own["shares"], json!([]));
    assert_eq!(m[2]["id"], json!("7001"));
    assert!(m[2].get("reply_to_mid").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn read_conversation_notes_the_twenty_message_window() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_200/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
              {"id": "m_new", "created_time": "2026-10-24T12:00:00+0000", "message": "recent",
               "from": {"id": "PSID1"}},
              {"id": "m_old", "created_time": "2026-09-01T12:00:00+0000", "from": {"id": "PSID1"}}
            ]
        })))
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_201/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "m_ok", "message": "full", "from": {"id": "PSID1"}},
                     {"id": "m_share", "from": {"id": "PSID1"},
                      "shares": {"data": [{"link": "https://example.org/x"}]}}]
        })))
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_read_conversation",
                json!({"conversation_id": "t_200"}),
            ),
            (
                "messenger_read_conversation",
                json!({"conversation_id": "t_201"}),
            ),
            (
                "messenger_read_conversation",
                json!({"conversation_id": "t_201", "limit": 50}),
            ),
            (
                "messenger_read_conversation",
                json!({"conversation_id": "bad/id"}),
            ),
        ],
    )
    .await;
    let note = "Meta exposes details for the 20 most recent messages of a conversation only";
    assert_eq!(
        tool_json(&out[0])["note"],
        json!(note),
        "message without content"
    );
    assert!(
        tool_json(&out[1]).get("note").is_none(),
        "shares are content"
    );
    assert_eq!(
        tool_json(&out[2])["note"],
        json!(note),
        "asked for more than 20"
    );
    assert!(is_error(&out[3]), "{}", out[3]);
    let sent = requests(&graph).await;
    assert_eq!(sent.len(), 3);
    assert_eq!(
        sent[2]
            .url
            .query_pairs()
            .find(|(k, _)| k == "limit")
            .map(|(_, v)| v.into_owned()),
        Some("50".to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn get_message_returns_one_normalised_message() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/m_single"))
        .and(FieldsContain("is_unsupported"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m_single", "created_time": "2022-07-12T19:11:07+0000",
            "from": {"id": "PSID1", "name": "Alice"}, "message": "just one",
            "attachments": {"data": [{"id": "a1", "mime_type": "image/png",
                "image_data": {"url": "https://scontent.xx.fbcdn.net/v/a.png", "width": 1, "height": 1}}]}
        })))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/m_gone"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            9_000_001,
            None,
            "This Message has been deleted by the user or the business.",
        )))
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            ("messenger_get_message", json!({"message_id": "m_single"})),
            ("messenger_get_message", json!({"message_id": "m_gone"})),
            ("messenger_get_message", json!({"message_id": "a b"})),
        ],
    )
    .await;
    let v = tool_json(&out[0]);
    assert_eq!(v["page_id"], json!(PAGE));
    assert_eq!(v["message"]["id"], json!("m_single"));
    assert_eq!(v["message"]["text"], json!("just one"));
    assert_eq!(v["message"]["attachments"][0]["kind"], json!("image"));
    assert_eq!(v["message"]["created_at"], json!(1_657_653_067));
    assert!(is_error(&out[1]));
    let text = tool_text(&out[1]);
    assert!(
        text.contains("deleted") && text.contains("20 most recent"),
        "{text}"
    );
    assert!(is_error(&out[2]));
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_rejects_2000_chars_before_any_request() {
    let graph = mock_graph().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&graph)
        .await;
    let files = files_dir();
    let resp = call(
        &graph,
        files.path(),
        "messenger_send_text",
        json!({"text": "a".repeat(2000), "recipient_psid": "PSID1"}),
    )
    .await;
    assert!(is_error(&resp), "{resp}");
    let text = tool_text(&resp);
    assert!(
        text.contains("2000") && text.contains("fewer than"),
        "{text}"
    );
    assert!(requests(&graph).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_counts_characters_not_bytes() {
    let graph = mock_graph().await;
    let text = "\u{e9}".repeat(1999);
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE", "message": {"text": text}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_long"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_text",
                json!({"text": text, "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_text",
                json!({"text": "   ", "recipient_psid": "PSID1"}),
            ),
            ("messenger_send_text", json!({"text": "hi"})),
        ],
    )
    .await;
    assert_eq!(tool_json(&out[0])["message_id"], json!("m_long"));
    assert!(is_error(&out[1]) && tool_text(&out[1]).contains("empty"));
    assert!(is_error(&out[2]) && tool_text(&out[2]).contains("recipient_psid or conversation_id"));
    assert_eq!(requests(&graph).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_body_and_reply_to() {
    let graph = mock_graph().await;
    let send_path = format!("/v26.0/{PAGE}/messages");
    Mock::given(method("POST"))
        .and(path(&send_path))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE",
            "message": {"text": "Bonjour \u{e9}t\u{e9}", "metadata": "crm-42"},
            "reply_to": {"mid": "m_abc"}, "notification_type": "SILENT_PUSH"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_new1"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(&send_path))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "MESSAGE_TAG",
            "message": {"text": "late answer"}, "tag": "HUMAN_AGENT"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_new2"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_text",
                json!({"text": "Bonjour \u{e9}t\u{e9}", "recipient_psid": "PSID1",
                    "reply_to_mid": "m_abc", "notification_type": "silent_push", "metadata": "crm-42"}),
            ),
            (
                "messenger_send_text",
                json!({"text": "late answer", "recipient_psid": "PSID1", "tag": "HUMAN_AGENT"}),
            ),
            (
                "messenger_send_text",
                json!({"text": "x", "recipient_psid": "PSID1", "messaging_type": "MESSAGE_TAG"}),
            ),
            (
                "messenger_send_text",
                json!({"text": "x", "recipient_psid": "PSID1", "tag": "ACCOUNT_UPDATE"}),
            ),
            (
                "messenger_send_text",
                json!({"text": "x", "recipient_psid": "a/b"}),
            ),
        ],
    )
    .await;
    let first = tool_json(&out[0]);
    assert_eq!(
        first,
        json!({"recipient_id": "PSID1", "message_id": "m_new1", "page_id": PAGE})
    );
    assert_eq!(tool_json(&out[1])["message_id"], json!("m_new2"));
    for rejected in &out[2..] {
        assert!(is_error(rejected), "{rejected}");
    }
    let sent = requests(&graph).await;
    assert_eq!(sent.len(), 2, "rejected calls must not reach Meta");
    for req in &sent {
        assert!(!req.url.as_str().contains("access_token"));
        assert!(!String::from_utf8_lossy(&req.body).contains(TOKEN));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn send_text_resolves_conversation_id() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_resolve_1"))
        .and(query_param("fields", "participants{id}"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "t_resolve_1",
            "participants": {"data": [{"id": PAGE}, {"id": "PSID_RESOLVED"}]}
        })))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .and(body_json(json!({
            "recipient": {"id": "PSID_RESOLVED"}, "messaging_type": "RESPONSE",
            "message": {"text": "hello again"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID_RESOLVED", "message_id": "m_r"})))
        .expect(2)
        .mount(&graph)
        .await;
    let files = files_dir();
    let args = json!({"text": "hello again", "conversation_id": "t_resolve_1"});
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            ("messenger_send_text", args.clone()),
            ("messenger_send_text", args),
        ],
    )
    .await;
    for resp in &out {
        assert_eq!(
            tool_json(resp)["recipient_id"],
            json!("PSID_RESOLVED"),
            "{resp}"
        );
    }
    let sent = requests(&graph).await;
    assert_eq!(sent.len(), 3, "the lookup happens once, then it is cached");
    graph.verify().await;
}

#[tokio::test]
async fn resolve_recipient_caches_and_validates() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_cache_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "participants": {"data": [{"id": "PAGE_CACHE"}, {"id": "PSIDC"}]}})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_cache_alone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "participants": {"data": [{"id": "PAGE_CACHE"}]}})))
        .mount(&graph)
        .await;
    let files = files_dir();
    let m = library(&graph, files.path(), "PAGE_CACHE");
    let page = PageConfig {
        id: "PAGE_CACHE".into(),
        name: String::new(),
        token: Secret::new(TOKEN),
    };
    let by =
        |psid: Option<&str>, conv: Option<&str>| (psid.map(String::from), conv.map(String::from));
    let (psid, conv) = by(None, Some("t_cache_1"));
    assert_eq!(
        m.resolve_recipient(&page, psid, conv).await.expect("first"),
        "PSIDC"
    );
    let (psid, conv) = by(None, Some("t_cache_1"));
    assert_eq!(
        m.resolve_recipient(&page, psid, conv)
            .await
            .expect("cached"),
        "PSIDC"
    );
    let (psid, conv) = by(Some("EXPLICIT"), Some("t_cache_1"));
    assert_eq!(
        m.resolve_recipient(&page, psid, conv)
            .await
            .expect("explicit"),
        "EXPLICIT"
    );
    let (psid, conv) = by(None, None);
    let err = m
        .resolve_recipient(&page, psid, conv)
        .await
        .expect_err("neither");
    assert!(
        err.to_string()
            .contains("recipient_psid or conversation_id")
    );
    let (psid, conv) = by(None, Some("t_cache_alone"));
    let err = m
        .resolve_recipient(&page, psid, conv)
        .await
        .expect_err("alone");
    assert!(
        err.to_string()
            .contains("no participant other than the Page"),
        "{err}"
    );
    let (psid, conv) = by(Some("a b"), None);
    assert!(m.resolve_recipient(&page, psid, conv).await.is_err());
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_attachment_by_url() {
    let graph = mock_graph().await;
    let send_path = format!("/v26.0/{PAGE}/messages");
    Mock::given(method("POST"))
        .and(path(&send_path))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE",
            "message": {"attachment": {"type": "image", "payload": {
                "url": "https://example.org/pic.jpg", "is_reusable": true}}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_a1", "attachment_id": "ATT_URL"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(&send_path))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE",
            "message": {"attachments": [
                {"type": "image", "payload": {"url": "https://example.org/1.png"}},
                {"type": "image", "payload": {"url": "https://example.org/2.png"}}]}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_a2"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(&send_path))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "MESSAGE_TAG", "tag": "HUMAN_AGENT",
            "message": {"attachment": {"type": "file", "payload": {
                "url": "https://example.org/doc.pdf", "is_reusable": false}}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_a3"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let many: Vec<String> = (0..31)
        .map(|i| format!("https://example.org/{i}.png"))
        .collect();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_attachment",
                json!({"kind": "image", "url": "https://example.org/pic.jpg",
                    "recipient_psid": "PSID1", "is_reusable": true}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "recipient_psid": "PSID1",
                    "urls": ["https://example.org/1.png", "https://example.org/2.png"]}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "file", "url": "https://example.org/doc.pdf",
                    "recipient_psid": "PSID1", "tag": "HUMAN_AGENT"}),
            ),
            ("messenger_send_attachment", json!({"kind": "image", "recipient_psid": "PSID1"})),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "recipient_psid": "PSID1", "url": "https://example.org/a.png",
                    "urls": ["https://example.org/b.png"]}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "recipient_psid": "PSID1", "url": "ftp://example.org/a.png"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "video", "recipient_psid": "PSID1", "urls": ["https://example.org/a.mp4"]}),
            ),
            ("messenger_send_attachment", json!({"kind": "image", "recipient_psid": "PSID1", "urls": many})),
            (
                "messenger_send_attachment",
                json!({"kind": "gif", "recipient_psid": "PSID1", "url": "https://example.org/a.gif"}),
            ),
        ],
    )
    .await;
    let first = tool_json(&out[0]);
    assert_eq!(first["attachment_id"], json!("ATT_URL"));
    assert_eq!(first["message_id"], json!("m_a1"));
    assert_eq!(first["recipient_id"], json!("PSID1"));
    assert!(first.get("uploaded_bytes").is_none());
    assert_eq!(tool_json(&out[1])["message_id"], json!("m_a2"));
    assert_eq!(tool_json(&out[2])["message_id"], json!("m_a3"));
    for rejected in &out[3..] {
        assert!(is_error(rejected), "{rejected}");
    }
    assert!(tool_text(&out[3]).contains("exactly one of url, path or urls"));
    assert!(tool_text(&out[7]).contains("at most 30"));
    assert_eq!(
        requests(&graph).await.len(),
        3,
        "rejected calls must not reach Meta"
    );
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_attachment_by_path_uploads_multipart() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/message_attachments")))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .and(Upload {
            file_name: "photo.png",
            mime: "image/png",
            kind: "image",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attachment_id": "ATT_UP1"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE",
            "message": {"attachment": {"type": "image", "payload": {"attachment_id": "ATT_UP1"}}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_up"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let bytes = png();
    std::fs::write(files.path().join("photo.png"), &bytes).expect("fixture");
    std::fs::write(files.path().join("data.xyz"), b"opaque").expect("fixture");
    std::fs::write(files.path().join("voice.mp3"), b"ID3").expect("fixture");
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "photo.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "../photo.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "missing.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "data.xyz", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "audio", "path": "photo.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "voice.mp3", "recipient_psid": "PSID1"}),
            ),
        ],
    )
    .await;
    let sent = tool_json(&out[0]);
    assert_eq!(sent["attachment_id"], json!("ATT_UP1"), "{}", out[0]);
    assert_eq!(sent["message_id"], json!("m_up"));
    assert_eq!(sent["uploaded_bytes"], json!(bytes.len()));
    assert!(sent.get("warning").is_none());
    assert!(
        tool_text(&out[1]).contains("outside META_MCP_FILES_DIR"),
        "{}",
        out[1]
    );
    assert!(tool_text(&out[2]).contains("no such file"), "{}", out[2]);
    assert!(tool_text(&out[3]).contains("mime_type"), "{}", out[3]);
    assert!(
        tool_text(&out[4]).contains("does not fit kind"),
        "{}",
        out[4]
    );
    assert!(tool_text(&out[5]).contains("kind audio"), "{}", out[5]);
    for rejected in &out[1..] {
        assert!(is_error(rejected), "{rejected}");
    }
    assert_eq!(
        requests(&graph).await.len(),
        2,
        "only the good call reaches Meta"
    );
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_with_explicit_mime_type_and_files_dir_required() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/message_attachments")))
        .and(Upload {
            file_name: "data.xyz",
            mime: "image/webp",
            kind: "image",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attachment_id": "ATT_UP2"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_up2"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    std::fs::write(files.path().join("data.xyz"), b"webp-ish").expect("fixture");
    let resp = call(
        &graph,
        files.path(),
        "messenger_send_attachment",
        json!({"kind": "image", "path": "data.xyz", "mime_type": "image/webp", "recipient_psid": "PSID1"}),
    )
    .await;
    assert_eq!(
        tool_json(&resp)["attachment_id"],
        json!("ATT_UP2"),
        "{resp}"
    );

    let out = call_many(
        &graph,
        None,
        vec![
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "photo.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": "http://127.0.0.1:9/x.png"}),
            ),
        ],
    )
    .await;
    for resp in &out {
        assert!(is_error(resp), "{resp}");
        assert!(tool_text(resp).contains("META_MCP_FILES_DIR"), "{resp}");
    }
    assert_eq!(requests(&graph).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_keeps_readable_file_names() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/message_attachments")))
        .and(Upload {
            file_name: "rapport \u{e9}t\u{e9} 2026.pdf",
            mime: "application/pdf",
            kind: "file",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attachment_id": "ATT_PDF"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .and(body_json(json!({
            "recipient": {"id": "PSID1"}, "messaging_type": "RESPONSE",
            "message": {"attachment": {"type": "file", "payload": {"attachment_id": "ATT_PDF"}}}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_pdf"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    std::fs::write(
        files.path().join("rapport \u{e9}t\u{e9} 2026.pdf"),
        b"%PDF-1.7",
    )
    .expect("fixture");
    let resp = call(
        &graph,
        files.path(),
        "messenger_send_attachment",
        json!({"kind": "file", "path": "rapport \u{e9}t\u{e9} 2026.pdf", "recipient_psid": "PSID1"}),
    )
    .await;
    assert_eq!(
        tool_json(&resp)["attachment_id"],
        json!("ATT_PDF"),
        "{resp}"
    );
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_size_limits() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/message_attachments")))
        .and(|req: &Request| {
            header_of(req, "content-type").is_some_and(|c| c.starts_with("multipart/form-data"))
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attachment_id": "ATT_BIG"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "recipient_id": "PSID1", "message_id": "m_big"})))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    let big_image = std::fs::File::create(files.path().join("big.png")).expect("fixture");
    big_image.set_len(8 * 1024 * 1024 + 1).expect("sparse file");
    let too_big = std::fs::File::create(files.path().join("huge.pdf")).expect("fixture");
    too_big.set_len(25 * 1024 * 1024 + 1).expect("sparse file");
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "big.png", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "file", "path": "huge.pdf", "recipient_psid": "PSID1"}),
            ),
        ],
    )
    .await;
    let sent = tool_json(&out[0]);
    assert_eq!(sent["uploaded_bytes"], json!(8 * 1024 * 1024 + 1));
    assert!(
        sent["warning"].as_str().expect("warning").contains("8 MB"),
        "{sent}"
    );
    assert!(is_error(&out[1]));
    assert!(tool_text(&out[1]).contains("25 MB"), "{}", out[1]);
    assert_eq!(
        requests(&graph).await.len(),
        2,
        "the oversized file never leaves the machine"
    );
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn page_sizes_are_clamped() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path(format!("/v26.0/{PAGE}/conversations")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/t_clamp/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&graph)
        .await;
    let files = files_dir();
    let list = |args: Value| ("messenger_list_conversations", args);
    let read = |args: Value| ("messenger_read_conversation", args);
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            list(json!({})),
            list(json!({"limit": 500})),
            list(json!({"limit": 0})),
            list(json!({"limit": -3})),
            read(json!({"conversation_id": "t_clamp"})),
            read(json!({"conversation_id": "t_clamp", "limit": 1000})),
            read(json!({"conversation_id": "t_clamp", "limit": 0, "after": "CUR"})),
        ],
    )
    .await;
    for resp in &out {
        assert!(!is_error(resp), "{resp}");
    }
    let query_of = |req: &Request, key: &str| -> Option<String> {
        req.url
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    let sent = requests(&graph).await;
    let limits: Vec<Option<String>> = sent.iter().map(|r| query_of(r, "limit")).collect();
    let expected = ["20", "50", "1", "1", "20", "100", "1"].map(|l| Some(l.to_string()));
    assert_eq!(limits, expected);
    assert!(
        sent[..4]
            .iter()
            .all(|r| query_of(r, "platform").as_deref() == Some("messenger"))
    );
    assert!(
        sent[..4]
            .iter()
            .all(|r| query_of(r, "user_id").is_none() && query_of(r, "after").is_none())
    );
    assert_eq!(query_of(&sent[6], "after").as_deref(), Some("CUR"));
}

#[tokio::test(flavor = "multi_thread")]
async fn download_attachment_saves_and_inlines_image() {
    let graph = mock_graph().await;
    let bytes = png();
    let cdn = format!("{}/cdn/t1/12345_n.png?oh=sig&oe=1", graph.uri());
    Mock::given(method("GET"))
        .and(path("/v26.0/m_img"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m_img",
            "attachments": {"data": [
                {"id": "a_other", "mime_type": "application/pdf", "file_url": "https://cdn.fbsbx.com/v/x.pdf"},
                {"id": "a1", "mime_type": "image/png", "name": "cat photo.png",
                 "size": bytes.len(), "image_data": {"url": cdn, "width": 1, "height": 1}}]}
        })))
        .expect(3)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/t1/12345_n.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(bytes.clone())
                .insert_header("content-type", "image/png"),
        )
        .expect(2)
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_download_attachment",
                json!({"message_id": "m_img", "index": 1}),
            ),
            (
                "messenger_download_attachment",
                json!({"message_id": "m_img", "index": 1, "dest_name": "../../evil dir/shot.png",
                    "inline": false}),
            ),
            (
                "messenger_download_attachment",
                json!({"message_id": "m_img", "index": 5}),
            ),
            ("messenger_download_attachment", json!({})),
        ],
    )
    .await;
    let first = &out[0];
    assert!(!is_error(first), "{first}");
    let content = first["result"]["content"].as_array().expect("content");
    assert_eq!(content.len(), 2, "text block and image block");
    assert_eq!(content[1]["type"], json!("image"));
    assert_eq!(content[1]["mimeType"], json!("image/png"));
    assert_eq!(content[1]["data"], json!(PNG_B64));
    let v = tool_json(first);
    let saved = files.path().join("cat photo.png");
    assert_eq!(
        std::fs::canonicalize(v["path"].as_str().expect("path")).expect("saved path"),
        std::fs::canonicalize(&saved).expect("file in the jail")
    );
    assert_eq!(std::fs::read(&saved).expect("saved"), bytes);
    assert_eq!(v["bytes"], json!(bytes.len()));
    assert_eq!(v["mime"], json!("image/png"));
    assert_eq!(v["sha256"], json!(hex::encode(Sha256::digest(&bytes))));
    assert_eq!(v["source_url_host"], json!("127.0.0.1"));

    let second = &out[1];
    assert!(!is_error(second), "{second}");
    assert_eq!(
        second["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    let renamed = files.path().join("shot.png");
    assert_eq!(std::fs::read(renamed).expect("sanitised name"), bytes);
    assert!(
        !files
            .path()
            .parent()
            .expect("parent")
            .join("evil dir")
            .exists()
    );

    assert!(tool_text(&out[2]).contains("out of range"), "{}", out[2]);
    assert!(
        tool_text(&out[3]).contains("exactly one of url or message_id"),
        "{}",
        out[3]
    );

    let sent = requests(&graph).await;
    let cdn_requests: Vec<&Request> = sent
        .iter()
        .filter(|r| r.url.path().starts_with("/cdn/"))
        .collect();
    assert_eq!(cdn_requests.len(), 2);
    for req in cdn_requests {
        assert!(
            header_of(req, "authorization").is_none(),
            "no token for a first attempt"
        );
        assert!(!req.url.as_str().contains("appsecret_proof"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn download_does_not_inline_non_images_or_big_images_and_never_overwrites_silently() {
    let graph = mock_graph().await;
    let mut big = png();
    big.resize(2 * 1024 * 1024 + 1, 0);
    Mock::given(method("GET"))
        .and(path("/cdn/big.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(big.clone())
                .insert_header("content-type", "image/png"),
        )
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/doc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"%PDF-1.7 data".to_vec())
                .insert_header("content-type", "application/pdf"),
        )
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/svg"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(b"<svg/>".to_vec())
                .insert_header("content-type", "image/svg+xml"),
        )
        .mount(&graph)
        .await;
    let base = graph.uri();
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/big.png")}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/doc")}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/doc")}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/doc"), "overwrite": true}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/svg")}),
            ),
        ],
    )
    .await;
    assert_eq!(
        out[0]["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    assert!(
        tool_json(&out[0])["note"]
            .as_str()
            .expect("note")
            .contains("2 MiB")
    );
    assert_eq!(
        std::fs::metadata(files.path().join("big.png"))
            .expect("big")
            .len(),
        big.len() as u64
    );
    let doc = tool_json(&out[1]);
    assert_eq!(doc["mime"], json!("application/pdf"));
    assert_eq!(
        doc["path"].as_str().map(|p| p.ends_with("/doc.pdf")),
        Some(true),
        "{doc}"
    );
    assert_eq!(
        out[1]["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    assert!(
        is_error(&out[2]) && tool_text(&out[2]).contains("already exists"),
        "{}",
        out[2]
    );
    assert!(!is_error(&out[3]), "{}", out[3]);
    assert_eq!(
        out[4]["result"]["content"]
            .as_array()
            .expect("content")
            .len(),
        1
    );
    assert!(
        tool_json(&out[4])["note"]
            .as_str()
            .expect("note")
            .contains("png, jpeg, gif and webp")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_retries_with_token_on_403_only_for_meta_hosts() {
    let graph = mock_graph().await;
    let jpeg = jpeg();
    Mock::given(method("GET"))
        .and(path("/cdn/secret.jpg"))
        .and(|req: &Request| !req.headers.contains_key("authorization"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/secret.jpg"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(jpeg.clone())
                .insert_header("content-type", "image/jpeg"),
        )
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/missing.jpg"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/cdn/forbidden.jpg"))
        .respond_with(ResponseTemplate::new(403))
        .expect(2)
        .mount(&graph)
        .await;
    let base = graph.uri();
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/secret.jpg")}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/missing.jpg")}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": format!("{base}/cdn/forbidden.jpg")}),
            ),
        ],
    )
    .await;
    assert!(!is_error(&out[0]), "{}", out[0]);
    let v = tool_json(&out[0]);
    assert_eq!(v["mime"], json!("image/jpeg"));
    assert_eq!(
        std::fs::read(files.path().join("secret.jpg")).expect("saved"),
        jpeg
    );
    assert!(
        is_error(&out[1]) && tool_text(&out[1]).contains("404"),
        "{}",
        out[1]
    );
    assert!(
        is_error(&out[2]) && tool_text(&out[2]).contains("403"),
        "{}",
        out[2]
    );
    let sent = requests(&graph).await;
    let on = |p: &str| -> Vec<&Request> { sent.iter().filter(|r| r.url.path() == p).collect() };
    let secret = on("/cdn/secret.jpg");
    assert_eq!(secret.len(), 2);
    assert!(header_of(secret[0], "authorization").is_none());
    assert_eq!(
        header_of(secret[1], "authorization"),
        Some(format!("Bearer {TOKEN}"))
    );
    assert_eq!(
        on("/cdn/missing.jpg").len(),
        1,
        "a 404 is not retried with the token"
    );
    let forbidden = on("/cdn/forbidden.jpg");
    assert_eq!(forbidden.len(), 2, "one retry only");
    assert!(header_of(forbidden[0], "authorization").is_none());
    assert!(header_of(forbidden[1], "authorization").is_some());
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn download_refuses_foreign_host() {
    let graph = mock_graph().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&graph)
        .await;
    let files = files_dir();
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_download_attachment",
                json!({"url": "https://evil.example/x.png"}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": "https://scontent.fbcdn.net.evil.example/x.png"}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": "http://scontent.xx.fbcdn.net/x.png"}),
            ),
            (
                "messenger_download_attachment",
                json!({"url": "file:///etc/passwd"}),
            ),
            ("messenger_download_attachment", json!({"url": "not a url"})),
        ],
    )
    .await;
    for resp in &out {
        assert!(is_error(resp), "{resp}");
    }
    let first = tool_text(&out[0]);
    assert!(
        first.contains("evil.example") && first.contains("refusing"),
        "{first}"
    );
    assert!(requests(&graph).await.is_empty());
    assert_eq!(std::fs::read_dir(files.path()).expect("dir").count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn sender_action_react_payload() {
    let graph = mock_graph().await;
    let send_path = format!("/v26.0/{PAGE}/messages");
    for (body, id) in [
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "react",
                "payload": {"message_id": "m_abc", "reaction": HEART}}),
            "react_love",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "react",
                "payload": {"message_id": "m_abc", "reaction": "\u{1f44d}"}}),
            "react_like",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "react",
                "payload": {"message_id": "m_abc", "reaction": "\u{1f389}"}}),
            "react_emoji",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "unreact",
                "payload": {"message_id": "m_abc"}}),
            "unreact",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "typing_on"}),
            "typing_on",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "typing_off"}),
            "typing_off",
        ),
        (
            json!({"recipient": {"id": "PSID1"}, "sender_action": "mark_seen"}),
            "mark_seen",
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(&send_path))
            .and(body_json(body))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"recipient_id": "PSID1"})),
            )
            .named(id)
            .expect(1)
            .mount(&graph)
            .await;
    }
    let files = files_dir();
    let act = |args: Value| ("messenger_sender_action", args);
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            act(json!({"action": "react", "recipient_psid": "PSID1", "message_id": "m_abc"})),
            act(json!({"action": "REACT", "recipient_psid": "PSID1", "message_id": "m_abc", "reaction": "like"})),
            act(json!({"action": "react", "recipient_psid": "PSID1", "message_id": "m_abc", "reaction": "\u{1f389}"})),
            act(json!({"action": "unreact", "recipient_psid": "PSID1", "message_id": "m_abc", "reaction": "love"})),
            act(json!({"action": "typing_on", "recipient_psid": "PSID1"})),
            act(json!({"action": "typing_off", "recipient_psid": "PSID1"})),
            act(json!({"action": "mark_seen", "recipient_psid": "PSID1", "message_id": "ignored"})),
            act(json!({"action": "react", "recipient_psid": "PSID1"})),
            act(json!({"action": "react", "recipient_psid": "PSID1", "message_id": "m_abc", "reaction": "lovee"})),
            act(json!({"action": "wave", "recipient_psid": "PSID1"})),
            act(json!({"action": "typing_on"})),
        ],
    )
    .await;
    let actions = [
        "react",
        "react",
        "react",
        "unreact",
        "typing_on",
        "typing_off",
        "mark_seen",
    ];
    for (resp, action) in out.iter().zip(actions) {
        assert_eq!(
            tool_json(resp),
            json!({"recipient_id": "PSID1", "action": action}),
            "{resp}"
        );
    }
    for rejected in &out[7..] {
        assert!(is_error(rejected), "{rejected}");
    }
    assert!(tool_text(&out[7]).contains("needs message_id"));
    assert!(tool_text(&out[9]).contains("typing_on, typing_off, mark_seen, react or unreact"));
    assert_eq!(
        requests(&graph).await.len(),
        7,
        "rejected calls must not reach Meta"
    );
    graph.verify().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn get_profile_fields() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PSID1"))
        .and(query_param("fields", "first_name,last_name,profile_pic"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "PSID1", "first_name": "Alice", "last_name": "Martin",
            "profile_pic": "https://platform-lookaside.fbsbx.com/p.jpg"})))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PSID2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "PSID2"})))
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PSID3"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            Some(2_018_247),
            "(#100) Not authorized to access the user profile",
        )))
        .mount(&graph)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PSID4"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            Some(2_018_001),
            "(#100) No matching user found",
        )))
        .mount(&graph)
        .await;
    let files = files_dir();
    let get = |psid: &str| ("messenger_get_profile", json!({"psid": psid}));
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            get("PSID1"),
            get("PSID2"),
            get("PSID3"),
            get("PSID4"),
            get("bad id"),
        ],
    )
    .await;
    assert_eq!(
        tool_json(&out[0]),
        json!({"profile": {"first_name": "Alice", "last_name": "Martin",
            "profile_pic": "https://platform-lookaside.fbsbx.com/p.jpg"}})
    );
    let note = "profile fields need the Business Asset User Profile Access feature; participant names from messenger_list_conversations do not";
    for empty in [&out[1], &out[2]] {
        assert_eq!(
            tool_json(empty),
            json!({"profile": {}, "note": note}),
            "{empty}"
        );
    }
    assert!(
        is_error(&out[3]) && tool_text(&out[3]).contains("No matching user"),
        "{}",
        out[3]
    );
    assert!(
        tool_text(&out[3]).contains("Page-scoped ID"),
        "the Phase A hint is kept"
    );
    assert!(is_error(&out[4]));
}

#[tokio::test(flavor = "multi_thread")]
async fn send_errors_carry_hints_and_never_the_token() {
    let graph = mock_graph().await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/messages")))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            Some(2_018_278),
            &format!("(#100) Outside of allowed window, token {TOKEN} rejected"),
        )))
        .expect(1)
        .mount(&graph)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v26.0/{PAGE}/message_attachments")))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            Some(2_018_109),
            "(#100) Attachment size exceeds allowable limit",
        )))
        .expect(1)
        .mount(&graph)
        .await;
    let files = files_dir();
    std::fs::write(files.path().join("a.png"), png()).expect("fixture");
    let out = call_many(
        &graph,
        Some(files.path()),
        vec![
            (
                "messenger_send_text",
                json!({"text": "hi", "recipient_psid": "PSID1"}),
            ),
            (
                "messenger_send_attachment",
                json!({"kind": "image", "path": "a.png", "recipient_psid": "PSID1"}),
            ),
        ],
    )
    .await;
    assert!(is_error(&out[0]));
    let text = tool_text(&out[0]);
    assert!(
        text.contains("24-hour") && text.contains("EAA***"),
        "{text}"
    );
    assert!(!text.contains(TOKEN));
    let text = tool_text(&out[1]);
    assert!(is_error(&out[1]) && text.contains("too large"), "{text}");
    assert_eq!(requests(&graph).await.len(), 2, "sends are never retried");
}

#[tokio::test]
async fn conversation_reads_are_spaced_per_page() {
    let graph = mock_graph().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/PAGE_SPACED/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&graph)
        .await;
    let files = files_dir();
    let m = library(&graph, files.path(), "PAGE_SPACED");
    let started = Instant::now();
    for _ in 0..3 {
        let out = m
            .list_conversations(ListConversationsParams::default())
            .await
            .expect("list");
        assert_eq!(out["conversations"], json!([]));
    }
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "three reads on one page must be spaced by 500 ms: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_list_includes_messenger_tools() {
    let uri = "http://127.0.0.1:9".to_string();
    let (names, tools) = blocking(move || {
        let home = TempHome::new();
        let mut s = Session::start(
            &home,
            &uri,
            &[("META_PAGE_ACCESS_TOKEN", TOKEN), ("META_PAGE_ID", PAGE)],
        );
        let listed = s.request("tools/list", json!({}));
        (s.tool_names(), listed["result"]["tools"].clone())
    })
    .await;
    let messenger = [
        "messenger_download_attachment",
        "messenger_get_message",
        "messenger_get_profile",
        "messenger_list_conversations",
        "messenger_read_conversation",
        "messenger_send_attachment",
        "messenger_send_text",
        "messenger_sender_action",
    ];
    for name in messenger {
        assert!(
            names.iter().any(|n| n == name),
            "{name} missing in {names:?}"
        );
    }
    for core in [
        "inbox_list",
        "inbox_mark_processed",
        "inbox_wait",
        "meta_status",
    ] {
        assert!(names.iter().any(|n| n == core), "{core}");
    }
    let tools = tools.as_array().expect("tools");
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name}"))
    };
    let props = |name: &str| -> Vec<String> {
        let mut keys: Vec<String> = tool(name)["inputSchema"]["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    };
    let expected: [(&str, &[&str], &[&str]); 8] = [
        (
            "messenger_list_conversations",
            &["after", "limit", "page_id", "user_id"],
            &[],
        ),
        (
            "messenger_read_conversation",
            &["after", "conversation_id", "limit", "page_id"],
            &["conversation_id"],
        ),
        (
            "messenger_get_message",
            &["message_id", "page_id"],
            &["message_id"],
        ),
        (
            "messenger_send_text",
            &[
                "conversation_id",
                "messaging_type",
                "metadata",
                "notification_type",
                "page_id",
                "recipient_psid",
                "reply_to_mid",
                "tag",
                "text",
            ],
            &["text"],
        ),
        (
            "messenger_send_attachment",
            &[
                "conversation_id",
                "is_reusable",
                "kind",
                "messaging_type",
                "mime_type",
                "page_id",
                "path",
                "recipient_psid",
                "tag",
                "url",
                "urls",
            ],
            &["kind"],
        ),
        (
            "messenger_download_attachment",
            &[
                "dest_name",
                "index",
                "inline",
                "message_id",
                "overwrite",
                "page_id",
                "url",
            ],
            &[],
        ),
        (
            "messenger_sender_action",
            &[
                "action",
                "conversation_id",
                "message_id",
                "page_id",
                "reaction",
                "recipient_psid",
            ],
            &["action"],
        ),
        ("messenger_get_profile", &["page_id", "psid"], &["psid"]),
    ];
    for (name, want_props, want_required) in expected {
        let t = tool(name);
        assert_eq!(props(name), want_props, "{name}");
        let mut required: Vec<String> = t["inputSchema"]["required"]
            .as_array()
            .map(|r| {
                r.iter()
                    .map(|v| v.as_str().expect("name").to_string())
                    .collect()
            })
            .unwrap_or_default();
        required.sort();
        assert_eq!(required, want_required, "{name}");
        for (prop, schema) in t["inputSchema"]["properties"]
            .as_object()
            .expect("properties")
        {
            assert!(
                schema["description"].is_string(),
                "{name}.{prop} lacks a description"
            );
        }
        let schema = t["inputSchema"].to_string();
        for bad in ["uint", "\"format\":\"int32\""] {
            assert!(!schema.contains(bad), "{name}: {schema}");
        }
        let d = t["description"].as_str().expect("description");
        assert!(d.len() < 1000, "{name}: {} chars", d.len());
    }
    let list = tool("messenger_list_conversations")["inputSchema"].to_string();
    assert!(list.contains("\"format\":\"int64\""), "{list}");
    assert!(
        tool("messenger_download_attachment")["inputSchema"]
            .to_string()
            .contains("\"format\":\"int64\"")
    );

    for name in [
        "messenger_list_conversations",
        "messenger_read_conversation",
        "messenger_get_message",
        "messenger_get_profile",
    ] {
        assert_eq!(
            tool(name)["annotations"]["readOnlyHint"],
            json!(true),
            "{name}"
        );
        assert!(
            tool(name)["description"]
                .as_str()
                .expect("description")
                .contains("untrusted third-party content"),
            "{name}"
        );
    }
    assert_eq!(
        tool("messenger_list_conversations")["description"]
            .as_str()
            .expect("description")
            .split(". ")
            .next(),
        Some("List Messenger conversations of a Facebook Page (newest activity first)")
    );
    for name in [
        "messenger_send_text",
        "messenger_send_attachment",
        "messenger_sender_action",
    ] {
        let a = &tool(name)["annotations"];
        assert_eq!(a["readOnlyHint"], json!(false), "{name}");
        assert_eq!(a["destructiveHint"], json!(false), "{name}");
        assert_eq!(a["openWorldHint"], json!(true), "{name}");
        let d = tool(name)["description"].as_str().expect("description");
        assert!(
            d.contains("real person") && d.contains("confirm"),
            "{name}: {d}"
        );
    }
    let text = tool("messenger_send_text")["description"]
        .as_str()
        .expect("description");
    assert!(
        text.contains("24 hours") && text.contains("HUMAN_AGENT"),
        "{text}"
    );
    let dl = &tool("messenger_download_attachment");
    assert_eq!(dl["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(dl["annotations"]["destructiveHint"], json!(false));
    let d = dl["description"].as_str().expect("description");
    assert!(
        d.contains("expire") && d.contains("untrusted third-party content"),
        "{d}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_hides_messenger_write_tools() {
    let names = blocking(|| {
        let home = TempHome::new();
        let mut s = Session::start(
            &home,
            "http://127.0.0.1:9",
            &[
                ("META_PAGE_ACCESS_TOKEN", TOKEN),
                ("META_PAGE_ID", PAGE),
                ("META_MCP_READ_ONLY", "1"),
            ],
        );
        s.tool_names()
    })
    .await;
    for hidden in [
        "messenger_send_text",
        "messenger_send_attachment",
        "messenger_sender_action",
    ] {
        assert!(
            !names.iter().any(|n| n == hidden),
            "{hidden} must be hidden: {names:?}"
        );
    }
    for shown in [
        "messenger_list_conversations",
        "messenger_read_conversation",
        "messenger_get_message",
        "messenger_download_attachment",
        "messenger_get_profile",
        "inbox_list",
        "meta_status",
    ] {
        assert!(
            names.iter().any(|n| n == shown),
            "{shown} missing in {names:?}"
        );
    }
}
