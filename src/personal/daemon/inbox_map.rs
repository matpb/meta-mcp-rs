//! Sidecar events to inbox rows. Defensive on the JSON: ids and timestamps may be strings or
//! numbers, fields may be missing, and an event without an id is skipped.

use serde_json::Value;

use crate::inbox::{Attachment, Event as InboxEvent, EventKind, now_secs};
use crate::personal::PersonalPlatform;
use crate::personal::protocol::Event;

/// A non-empty string, or a number rendered as text.
pub(super) fn id_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| {
            // Truncating a fractional timestamp is intended.
            n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn string_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

fn attachments_of(message: &Value) -> Vec<Attachment> {
    message
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.is_object())
        .map(|a| Attachment {
            kind: string_of(a.get("kind")).unwrap_or_else(|| "other".to_owned()),
            url: string_of(a.get("url")),
            media_id: id_of(a.get("id")),
            mime_type: string_of(a.get("mime")),
            filename: string_of(a.get("name")),
            ..Attachment::default()
        })
        .collect()
}

/// One inbox row for a protocol message; `echo` marks a message sent from this account.
fn message_row(
    platform: PersonalPlatform,
    account_id: &str,
    message: &Value,
    fallback_thread: Option<&str>,
    echo: bool,
    raw: Value,
    now: i64,
) -> Option<InboxEvent> {
    let inbox_platform = platform.inbox_platform();
    let message_id = id_of(message.get("id"))?;
    let thread_id = id_of(message.get("thread_id"))
        .or_else(|| fallback_thread.map(str::to_owned))
        .unwrap_or_default();
    // WhatsApp message ids are unique per chat only, so the thread is part of the row id.
    let row_id = format!("{inbox_platform}:{thread_id}:{message_id}");
    let kind = if echo {
        EventKind::Echo
    } else {
        EventKind::Message
    };
    Some(InboxEvent {
        ts: int_of(message.get("ts")).unwrap_or(0),
        account_id: account_id.to_owned(),
        sender: id_of(message.get("sender").and_then(|s| s.get("id"))).unwrap_or_default(),
        recipient: thread_id,
        text: string_of(message.get("text")),
        attachments: attachments_of(message),
        reply_to: id_of(message.get("reply_to")),
        raw,
        received_at: now,
        ..InboxEvent::new(row_id, inbox_platform, kind)
    })
}

/// The inbox rows one sidecar event produces; empty for events the inbox does not keep.
pub fn inbox_events(
    platform: PersonalPlatform,
    account_id: &str,
    event: &Event,
) -> Vec<InboxEvent> {
    let now = now_secs();
    let data = &event.data;
    let inbox_platform = platform.inbox_platform();
    match event.event.as_str() {
        "message" => {
            let echo = data.get("direction").and_then(Value::as_str) == Some("out");
            data.get("message")
                .and_then(|m| message_row(platform, account_id, m, None, echo, data.clone(), now))
                .into_iter()
                .collect()
        }
        "history" => {
            let thread = id_of(data.get("thread_id"));
            data.get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|m| {
                    let echo = m
                        .get("sender")
                        .and_then(|s| s.get("is_me"))
                        .and_then(Value::as_bool)
                        == Some(true);
                    let raw = serde_json::json!({"message": m, "history": true});
                    message_row(platform, account_id, m, thread.as_deref(), echo, raw, now)
                })
                .collect()
        }
        "reaction" => {
            let (Some(thread), Some(message_id)) =
                (id_of(data.get("thread_id")), id_of(data.get("message_id")))
            else {
                return Vec::new();
            };
            let sender = id_of(data.get("sender_id")).unwrap_or_default();
            let ts = int_of(data.get("ts")).unwrap_or(0);
            let emoji = string_of(data.get("emoji")).filter(|e| !e.is_empty());
            vec![InboxEvent {
                ts,
                account_id: account_id.to_owned(),
                sender: sender.clone(),
                recipient: thread.clone(),
                text: emoji,
                reply_to: Some(message_id.clone()),
                raw: data.clone(),
                received_at: now,
                ..InboxEvent::new(
                    format!("{inbox_platform}:reaction:{thread}:{message_id}:{sender}:{ts}"),
                    inbox_platform,
                    EventKind::Reaction,
                )
            }]
        }
        "receipt" => {
            let Some(thread) = id_of(data.get("thread_id")) else {
                return Vec::new();
            };
            let (label, kind) = match data.get("kind").and_then(Value::as_str) {
                Some("read") => ("read", EventKind::Read),
                Some("delivered") => ("delivered", EventKind::Delivery),
                _ => return Vec::new(),
            };
            let sender = id_of(data.get("sender_id")).unwrap_or_default();
            let up_to = int_of(data.get("up_to_ts")).unwrap_or(0);
            vec![InboxEvent {
                ts: up_to,
                account_id: account_id.to_owned(),
                sender: sender.clone(),
                recipient: thread.clone(),
                raw: data.clone(),
                received_at: now,
                ..InboxEvent::new(
                    format!("{inbox_platform}:{label}:{thread}:{sender}:{up_to}"),
                    inbox_platform,
                    kind,
                )
            }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::Platform;
    use serde_json::json;

    fn ev(name: &str, data: Value) -> Event {
        Event {
            event: name.into(),
            data,
        }
    }

    fn msg() -> Value {
        json!({
            "id": "mid.1", "thread_id": "t1", "ts": 1_700_000_000,
            "sender": {"id": "u2", "name": "Bob", "is_me": false},
            "text": "hello", "reply_to": "mid.0",
            "attachments": [{"id": "a1", "kind": "image", "mime": "image/jpeg",
                "name": "x.jpg", "url": "https://cdn/x"}]
        })
    }

    #[test]
    fn inbound_message_maps_every_field() {
        let data = json!({"message": msg(), "direction": "in"});
        let rows = inbox_events(
            PersonalPlatform::Messenger,
            "100001",
            &ev("message", data.clone()),
        );
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.id, "messenger_personal:t1:mid.1");
        assert_eq!(r.platform, Platform::MessengerPersonal);
        assert_eq!(r.kind, EventKind::Message);
        assert_eq!(r.ts, 1_700_000_000);
        assert_eq!(r.account_id, "100001");
        assert_eq!(r.sender, "u2");
        assert_eq!(r.recipient, "t1");
        assert_eq!(r.text.as_deref(), Some("hello"));
        assert_eq!(r.reply_to.as_deref(), Some("mid.0"));
        assert_eq!(r.raw, data);
        assert!(r.received_at > 0);
        let a = &r.attachments[0];
        assert_eq!(a.kind, "image");
        assert_eq!(a.media_id.as_deref(), Some("a1"));
        assert_eq!(a.mime_type.as_deref(), Some("image/jpeg"));
        assert_eq!(a.filename.as_deref(), Some("x.jpg"));
        assert_eq!(a.url.as_deref(), Some("https://cdn/x"));
    }

    #[test]
    fn outbound_message_is_an_echo() {
        let data = json!({"message": msg(), "direction": "out"});
        let rows = inbox_events(PersonalPlatform::WhatsApp, "a", &ev("message", data));
        assert_eq!(rows[0].kind, EventKind::Echo);
        assert_eq!(rows[0].id, "whatsapp_personal:t1:mid.1");
        assert_eq!(rows[0].platform, Platform::WhatsAppPersonal);
    }

    #[test]
    fn history_yields_one_row_per_message_with_echo_from_is_me() {
        let mut mine = msg();
        mine["id"] = json!("mid.2");
        mine["sender"] = json!({"id": "me", "is_me": true});
        mine.as_object_mut().expect("obj").remove("thread_id");
        let data = json!({"thread_id": "t9", "messages": [msg(), mine, {"text": "no id"}], "complete": true});
        let rows = inbox_events(PersonalPlatform::WhatsApp, "a", &ev("history", data));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, EventKind::Message);
        assert_eq!(rows[1].kind, EventKind::Echo);
        assert_eq!(rows[0].id, "whatsapp_personal:t1:mid.1");
        assert_eq!(rows[1].id, "whatsapp_personal:t9:mid.2");
        assert_eq!(rows[1].recipient, "t9");
        assert_eq!(rows[1].raw["history"], true);
    }

    #[test]
    fn the_same_message_id_in_two_chats_gets_two_rows() {
        let row = |thread: &str| {
            let data = json!({"message": {"id": "3EB0A", "thread_id": thread}, "direction": "in"});
            inbox_events(PersonalPlatform::WhatsApp, "a", &ev("message", data)).remove(0)
        };
        assert_eq!(row("a@g.us").id, "whatsapp_personal:a@g.us:3EB0A");
        assert_ne!(row("a@g.us").id, row("b@g.us").id);
    }

    #[test]
    fn reaction_maps_and_removal_has_no_text() {
        let data = json!({"thread_id": "t1", "message_id": "mid.1", "sender_id": "u2",
            "emoji": "\u{1F44D}", "ts": 77});
        let rows = inbox_events(PersonalPlatform::Messenger, "a", &ev("reaction", data));
        assert_eq!(rows[0].id, "messenger_personal:reaction:t1:mid.1:u2:77");
        assert_eq!(rows[0].kind, EventKind::Reaction);
        assert_eq!(rows[0].text.as_deref(), Some("\u{1F44D}"));
        assert_eq!(rows[0].reply_to.as_deref(), Some("mid.1"));
        assert_eq!(rows[0].recipient, "t1");
        assert_eq!(rows[0].sender, "u2");
        assert_eq!(rows[0].ts, 77);
        let gone = json!({"thread_id": "t1", "message_id": "mid.1", "sender_id": "u2",
            "emoji": null, "ts": 78});
        let rows = inbox_events(PersonalPlatform::Messenger, "a", &ev("reaction", gone));
        assert_eq!(rows[0].text, None);
    }

    #[test]
    fn receipts_map_read_and_delivered() {
        let read = json!({"thread_id": "t1", "kind": "read", "sender_id": "u2", "up_to_ts": 90});
        let rows = inbox_events(PersonalPlatform::WhatsApp, "a", &ev("receipt", read));
        assert_eq!(rows[0].id, "whatsapp_personal:read:t1:u2:90");
        assert_eq!(rows[0].kind, EventKind::Read);
        assert_eq!(rows[0].ts, 90);
        assert_eq!(rows[0].recipient, "t1");
        let delivered =
            json!({"thread_id": "t1", "kind": "delivered", "sender_id": "u2", "up_to_ts": 91});
        let rows = inbox_events(PersonalPlatform::WhatsApp, "a", &ev("receipt", delivered));
        assert_eq!(rows[0].id, "whatsapp_personal:delivered:t1:u2:91");
        assert_eq!(rows[0].kind, EventKind::Delivery);
        let other = json!({"thread_id": "t1", "kind": "typing", "sender_id": "u2"});
        assert!(inbox_events(PersonalPlatform::WhatsApp, "a", &ev("receipt", other)).is_empty());
    }

    #[test]
    fn numeric_and_string_ids_and_timestamps_are_accepted() {
        let data = json!({"message": {"id": 12345, "thread_id": 99, "ts": "1700000001",
            "sender": {"id": 7}}, "direction": "in"});
        let rows = inbox_events(PersonalPlatform::Messenger, "a", &ev("message", data));
        assert_eq!(rows[0].id, "messenger_personal:99:12345");
        assert_eq!(rows[0].recipient, "99");
        assert_eq!(rows[0].sender, "7");
        assert_eq!(rows[0].ts, 1_700_000_001);
        let float = json!({"message": {"id": "m", "ts": 5.9}});
        let rows = inbox_events(PersonalPlatform::Messenger, "a", &ev("message", float));
        assert_eq!(rows[0].ts, 5);
        assert_eq!(rows[0].recipient, "");
        assert!(rows[0].attachments.is_empty());
    }

    #[test]
    fn events_without_an_id_or_of_other_kinds_are_skipped() {
        let no_id = json!({"message": {"text": "x", "thread_id": "t"}, "direction": "in"});
        assert!(inbox_events(PersonalPlatform::Messenger, "a", &ev("message", no_id)).is_empty());
        assert!(
            inbox_events(PersonalPlatform::Messenger, "a", &ev("message", json!({}))).is_empty()
        );
        assert!(
            inbox_events(
                PersonalPlatform::Messenger,
                "a",
                &ev("reaction", json!({"emoji": "x"}))
            )
            .is_empty()
        );
        for name in [
            "thread",
            "error",
            "qr",
            "connection",
            "logged_in",
            "nonsense",
        ] {
            assert!(
                inbox_events(
                    PersonalPlatform::Messenger,
                    "a",
                    &ev(name, json!({"id": "x"}))
                )
                .is_empty(),
                "{name}"
            );
        }
    }
}
