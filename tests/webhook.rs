//! Webhook receiver in-process (verify handshake, signatures, storage) and payload normalisation.

use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use meta_mcp::config::Secret;
use meta_mcp::inbox::{EventKind, Inbox, ListFilter, Platform};
use meta_mcp::webhook::normalize::normalize;
use meta_mcp::webhook::{WebhookState, router};
use serde_json::{Value, json};
use sha2::Sha256;

const APP_SECRET: &str = "the-app-secret";
const VERIFY_TOKEN: &str = "the-verify-token";
const NOW: i64 = 1_800_000_000;

struct Hook {
    base: String,
    inbox: Inbox,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Hook {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let inbox = Inbox::open(&dir.path().join("inbox.db")).expect("inbox");
        let state = Arc::new(WebhookState::new(
            Secret::new(APP_SECRET),
            Secret::new(VERIFY_TOKEN),
            inbox.clone(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.ok();
        });
        Self {
            base,
            inbox,
            http: reqwest::Client::new(),
            _dir: dir,
        }
    }

    fn verify_url(&self, mode: &str, token: &str, challenge: &str) -> String {
        format!(
            "{}/webhook?hub.mode={mode}&hub.verify_token={token}&hub.challenge={challenge}",
            self.base
        )
    }

    async fn post(&self, body: &[u8], signature: Option<&str>) -> reqwest::Response {
        let mut req = self
            .http
            .post(format!("{}/webhook", self.base))
            .header("content-type", "application/json")
            .body(body.to_vec());
        if let Some(sig) = signature {
            req = req.header("X-Hub-Signature-256", sig);
        }
        req.send().await.expect("send")
    }

    fn events(&self) -> Vec<meta_mcp::inbox::Event> {
        self.inbox.list(&ListFilter::default()).expect("list")
    }
}

fn sign(body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(APP_SECRET.as_bytes()).expect("key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn messenger_text_payload() -> Value {
    json!({"object": "page", "entry": [{"id": "PAGE_ID", "time": 1458692752478_i64, "messaging": [{
        "sender": {"id": "USER_ID"}, "recipient": {"id": "PAGE_ID"}, "timestamp": 1458692752478_i64,
        "message": {"mid": "m_AbC123", "text": "hello, world!", "reply_to": {"mid": "m_prev"}}
    }]}]})
}

#[tokio::test]
async fn verify_get_ok() {
    let hook = Hook::start().await;
    let resp = hook
        .http
        .get(hook.verify_url("subscribe", VERIFY_TOKEN, "1158201444"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .expect("ascii")
            .starts_with("text/plain")
    );
    assert_eq!(resp.text().await.expect("body"), "1158201444");
}

#[tokio::test]
async fn verify_get_bad_token() {
    let hook = Hook::start().await;
    for url in [
        hook.verify_url("subscribe", "wrong-token", "42"),
        hook.verify_url("subscribe", "", "42"),
        hook.verify_url("unsubscribe", VERIFY_TOKEN, "42"),
        format!(
            "{}/webhook?hub.mode=subscribe&hub.verify_token={VERIFY_TOKEN}",
            hook.base
        ),
        format!("{}/webhook?hub.challenge=42", hook.base),
        format!("{}/webhook", hook.base),
    ] {
        let resp = hook.http.get(&url).send().await.expect("send");
        assert_eq!(resp.status(), 403, "{url}");
        assert_eq!(resp.text().await.expect("body"), "verification failed");
    }
}

#[tokio::test]
async fn post_valid_signature_stores_and_dedupes() {
    let hook = Hook::start().await;
    let body = serde_json::to_vec(&messenger_text_payload()).expect("json");
    let sig = sign(&body);
    let first = hook.post(&body, Some(&sig)).await;
    assert_eq!(first.status(), 200);
    assert_eq!(first.text().await.expect("body"), "EVENT_RECEIVED");
    assert_eq!(hook.events().len(), 1);
    let replay = hook.post(&body, Some(&sig)).await;
    assert_eq!(replay.status(), 200);
    let events = hook.events();
    assert_eq!(events.len(), 1, "a replay must not create a second event");
    assert_eq!(events[0].id, "messenger:m_AbC123");
    assert_eq!(events[0].text.as_deref(), Some("hello, world!"));
    assert!(events[0].received_at > 1_700_000_000);

    let health: Value = hook
        .http
        .get(format!("{}/health", hook.base))
        .send()
        .await
        .expect("health")
        .json()
        .await
        .expect("json");
    assert_eq!(health["ok"], json!(true));
    assert_eq!(health["events"], json!(1));
    assert_eq!(health["unprocessed"], json!(1));
    assert!(health["last_received_at"].is_i64());
}

#[tokio::test]
async fn post_bad_signature_401() {
    let hook = Hook::start().await;
    let body = serde_json::to_vec(&messenger_text_payload()).expect("json");
    let wrong_secret = {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(b"other-secret").expect("key");
        mac.update(&body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    };
    for sig in [
        wrong_secret,
        "sha256=deadbeef".to_string(),
        "sha1=abc".to_string(),
        sign(b"other body"),
    ] {
        let resp = hook.post(&body, Some(&sig)).await;
        assert_eq!(resp.status(), 401, "{sig}");
    }
    assert!(
        hook.events().is_empty(),
        "nothing may be stored without a valid signature"
    );
}

#[tokio::test]
async fn post_missing_signature_401() {
    let hook = Hook::start().await;
    let body = serde_json::to_vec(&messenger_text_payload()).expect("json");
    assert_eq!(hook.post(&body, None).await.status(), 401);
    assert!(hook.events().is_empty());
}

#[tokio::test]
async fn unicode_body_signature() {
    let hook = Hook::start().await;
    let raw = r#"{"object":"page","entry":[{"id":"PAGE_ID","messaging":[{"sender":{"id":"U"},"recipient":{"id":"PAGE_ID"},"timestamp":1700000000000,"message":{"mid":"m_uni","text":"héllo 🙂"}}]}]}"#;
    let resp = hook.post(raw.as_bytes(), Some(&sign(raw.as_bytes()))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(hook.events()[0].text.as_deref(), Some("héllo 🙂"));

    let reencoded =
        serde_json::to_vec(&serde_json::from_str::<Value>(raw).expect("json")).expect("bytes");
    let escaped = raw.replace("héllo 🙂", "h\\u00e9llo \\ud83d\\ude42");
    assert_ne!(escaped.as_bytes(), raw.as_bytes());
    let mismatch = hook
        .post(escaped.as_bytes(), Some(&sign(raw.as_bytes())))
        .await;
    assert_eq!(
        mismatch.status(),
        401,
        "the signature covers the exact bytes sent"
    );
    let ok_escaped = hook
        .post(escaped.as_bytes(), Some(&sign(escaped.as_bytes())))
        .await;
    assert_eq!(ok_escaped.status(), 200);
    assert_eq!(hook.events().len(), 1, "same mid dedupes across encodings");
    assert!(!reencoded.is_empty());
}

#[tokio::test]
async fn invalid_json_oversize_and_unknown_objects() {
    let hook = Hook::start().await;
    let junk = b"not json at all";
    assert_eq!(hook.post(junk, Some(&sign(junk))).await.status(), 400);

    let big = vec![b' '; 1024 * 1024 + 1];
    assert_eq!(hook.post(&big, Some(&sign(&big))).await.status(), 413);

    let unknown = serde_json::to_vec(
        &json!({"object": "permissions", "entry": [{"id": "9", "changed_fields": ["email"]}]}),
    )
    .expect("json");
    assert_eq!(
        hook.post(&unknown, Some(&sign(&unknown))).await.status(),
        200
    );
    let events = hook.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Other);
    assert_eq!(events[0].raw["changed_fields"][0], json!("email"));

    let empty = serde_json::to_vec(&json!({"object": "page", "entry": []})).expect("json");
    assert_eq!(hook.post(&empty, Some(&sign(&empty))).await.status(), 200);
}

#[tokio::test]
async fn whatsapp_text_message_normalized() {
    let payload = json!({"object": "whatsapp_business_account", "entry": [{"id": "WABA_ID", "changes": [{
        "field": "messages",
        "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
            "contacts": [{"profile": {"name": "Sheena Nelson"}, "wa_id": "16505551234"}],
            "messages": [{
                "from": "16505551234",
                "id": "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQUZC",
                "timestamp": "1749416383",
                "type": "text",
                "text": {"body": "Does it come in another color?"},
                "context": {"from": "15550783881", "id": "wamid.PREVIOUS"}
            }]
        }
    }]}]});
    let events = normalize(&payload, NOW);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.id, "whatsapp:wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQUZC");
    assert_eq!(
        (e.platform, e.kind),
        (Platform::WhatsApp, EventKind::Message)
    );
    assert_eq!(e.sender, "16505551234");
    assert_eq!(e.recipient, "106540352242922");
    assert_eq!(e.account_id, "106540352242922");
    assert_eq!(e.text.as_deref(), Some("Does it come in another color?"));
    assert_eq!(e.ts, 1_749_416_383);
    assert_eq!(e.reply_to.as_deref(), Some("wamid.PREVIOUS"));
    assert_eq!(e.received_at, NOW);
    assert!(!e.processed && e.attachments.is_empty());
    assert_eq!(e.raw["_sender_name"], json!("Sheena Nelson"));
    assert_eq!(e.raw["type"], json!("text"));
}

#[tokio::test]
async fn whatsapp_image_and_status_normalized() {
    let image = json!({"object": "whatsapp_business_account", "entry": [{"id": "WABA_ID", "changes": [{
        "field": "messages",
        "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": "PN1"},
            "contacts": [{"profile": {"name": "Ana"}, "wa_id": "5511"}],
            "messages": [{
                "from": "5511", "id": "wamid.IMG", "timestamp": "1750000000", "type": "image",
                "image": {"caption": "Look at this", "mime_type": "image/jpeg", "sha256": "abc123hash", "id": "MEDIA_ID_1"}
            }, {
                "from": "5511", "id": "wamid.DOC", "timestamp": "1750000001", "type": "document",
                "document": {"filename": "invoice.pdf", "mime_type": "application/pdf", "sha256": "dochash", "id": "MEDIA_ID_2"}
            }]
        }
    }]}]});
    let events = normalize(&image, NOW);
    assert_eq!(events.len(), 2);
    let img = &events[0];
    assert_eq!(img.kind, EventKind::Message);
    assert_eq!(img.text.as_deref(), Some("Look at this"));
    assert_eq!(img.attachments.len(), 1);
    let a = &img.attachments[0];
    assert_eq!(a.kind, "image");
    assert_eq!(a.media_id.as_deref(), Some("MEDIA_ID_1"));
    assert_eq!(a.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(a.sha256.as_deref(), Some("abc123hash"));
    assert_eq!(a.caption.as_deref(), Some("Look at this"));
    let doc = &events[1].attachments[0];
    assert_eq!(
        (doc.kind.as_str(), doc.filename.as_deref()),
        ("document", Some("invoice.pdf"))
    );
    assert_eq!(events[1].text, None);

    let status = json!({"object": "whatsapp_business_account", "entry": [{"id": "WABA_ID", "changes": [{
        "field": "messages",
        "value": {
            "messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": "PN1"},
            "statuses": [
                {"id": "wamid.OUT", "status": "delivered", "timestamp": "1750000100", "recipient_id": "5511",
                 "conversation": {"id": "c1", "origin": {"type": "service"}}, "pricing": {"billable": false}},
                {"id": "wamid.OUT", "status": "failed", "timestamp": "1750000200", "recipient_id": "5511",
                 "errors": [{"code": 131047, "title": "Re-engagement message", "message": "Re-engagement message",
                             "error_data": {"details": "Message failed to send because more than 24 hours have passed"}}]}
            ]
        }
    }]}]});
    let events = normalize(&status, NOW);
    assert_eq!(events.len(), 2);
    let delivered = &events[0];
    assert_eq!(delivered.id, "whatsapp:status:wamid.OUT:delivered");
    assert_eq!(
        (delivered.kind, delivered.platform),
        (EventKind::Status, Platform::WhatsApp)
    );
    assert_eq!(delivered.sender, "PN1");
    assert_eq!(delivered.recipient, "5511");
    assert_eq!(delivered.text.as_deref(), Some("delivered"));
    assert_eq!(delivered.ts, 1_750_000_100);
    assert_eq!(delivered.raw["conversation"]["id"], json!("c1"));
    let failed = &events[1];
    assert_eq!(failed.id, "whatsapp:status:wamid.OUT:failed");
    assert_eq!(failed.raw["errors"][0]["code"], json!(131047));
}

#[tokio::test]
async fn messenger_text_and_image_normalized() {
    let events = normalize(&messenger_text_payload(), NOW);
    assert_eq!(events.len(), 1);
    let t = &events[0];
    assert_eq!(t.id, "messenger:m_AbC123");
    assert_eq!(
        (t.platform, t.kind),
        (Platform::Messenger, EventKind::Message)
    );
    assert_eq!(t.sender, "USER_ID");
    assert_eq!(t.recipient, "PAGE_ID");
    assert_eq!(t.account_id, "PAGE_ID");
    assert_eq!(t.text.as_deref(), Some("hello, world!"));
    assert_eq!(t.ts, 1_458_692_752);
    assert_eq!(t.reply_to.as_deref(), Some("m_prev"));
    assert_eq!(t.received_at, NOW);

    let image = json!({"object": "page", "entry": [{"id": "PAGE_ID", "time": 1458692752478_i64, "messaging": [{
        "sender": {"id": "USER_ID"}, "recipient": {"id": "PAGE_ID"}, "timestamp": 1458692752999_i64,
        "message": {"mid": "m_img", "attachments": [
            {"type": "image", "payload": {"url": "https://scontent.xx.fbcdn.net/v/t1/abc.jpg?oh=1&oe=2"}},
            {"type": "audio", "payload": {"url": "https://cdn.fbsbx.com/voice.mp4"}}
        ]}
    }]}]});
    let events = normalize(&image, NOW);
    assert_eq!(events.len(), 1);
    let m = &events[0];
    assert_eq!(m.kind, EventKind::Message);
    assert_eq!(m.text, None);
    assert_eq!(m.ts, 1_458_692_752);
    assert_eq!(m.attachments.len(), 2);
    assert_eq!(m.attachments[0].kind, "image");
    assert_eq!(
        m.attachments[0].url.as_deref(),
        Some("https://scontent.xx.fbcdn.net/v/t1/abc.jpg?oh=1&oe=2")
    );
    assert_eq!(m.attachments[1].kind, "audio");
}

#[tokio::test]
async fn messenger_echo_and_read_normalized() {
    let echo = json!({"object": "page", "entry": [{"id": "PAGE_ID", "messaging": [{
        "sender": {"id": "PAGE_ID"}, "recipient": {"id": "USER_ID"}, "timestamp": 1700000000000_i64,
        "message": {"is_echo": true, "app_id": 123456, "mid": "m_echo", "text": "We reply from the app",
                    "metadata": "custom"}
    }]}]});
    let events = normalize(&echo, NOW);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.kind, EventKind::Echo);
    assert_eq!(e.id, "messenger:m_echo");
    assert_eq!(e.sender, "PAGE_ID");
    assert_eq!(e.recipient, "USER_ID");
    assert_eq!(e.account_id, "PAGE_ID");
    assert_eq!(e.text.as_deref(), Some("We reply from the app"));
    assert_eq!(e.ts, 1_700_000_000);

    let read_and_delivery = json!({"object": "page", "entry": [{"id": "PAGE_ID", "messaging": [
        {"sender": {"id": "USER_ID"}, "recipient": {"id": "PAGE_ID"}, "timestamp": 1700000005000_i64,
         "read": {"watermark": 1700000004000_i64}},
        {"sender": {"id": "USER_ID"}, "recipient": {"id": "PAGE_ID"}, "timestamp": 1700000006000_i64,
         "delivery": {"mids": ["m_a", "m_b"], "watermark": 1700000005500_i64}}
    ]}]});
    let events = normalize(&read_and_delivery, NOW);
    assert_eq!(events.len(), 2);
    let read = &events[0];
    assert_eq!(read.kind, EventKind::Read);
    assert_eq!(read.id, "messenger:read:PAGE_ID:USER_ID:1700000004000");
    assert_eq!(read.sender, "USER_ID");
    assert_eq!(read.account_id, "PAGE_ID");
    assert_eq!(read.ts, 1_700_000_005);
    let delivery = &events[1];
    assert_eq!(delivery.kind, EventKind::Delivery);
    assert_eq!(
        delivery.id,
        "messenger:delivery:PAGE_ID:USER_ID:1700000005500"
    );
    assert_eq!(delivery.text.as_deref(), Some("m_a,m_b"));
}
