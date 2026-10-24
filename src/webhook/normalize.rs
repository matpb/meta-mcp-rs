//! Turns Messenger and WhatsApp webhook payloads into inbox events.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::graph::error::as_i64;
use crate::inbox::{Attachment, Event, EventKind, Platform};

const WA_MEDIA: [&str; 5] = ["image", "video", "audio", "document", "sticker"];

fn text_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn id_of(v: Option<&Value>) -> String {
    text_of(v).unwrap_or_default()
}

fn digest8(v: &Value) -> String {
    let hash = Sha256::digest(v.to_string().as_bytes());
    hex::encode(&hash[..8])
}

fn entries(body: &Value) -> &[Value] {
    body.get("entry")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Normalises one webhook body; every entry yields at least one event.
pub fn normalize(body: &Value, received_at: i64) -> Vec<Event> {
    match body
        .get("object")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "page" => messenger(body, received_at),
        "whatsapp_business_account" => whatsapp(body, received_at),
        other => unknown_object(other, body, received_at),
    }
}

fn other_event(
    id: String,
    platform: Platform,
    account_id: &str,
    raw: &Value,
    ts: i64,
    received_at: i64,
) -> Event {
    Event {
        ts,
        account_id: account_id.to_string(),
        raw: raw.clone(),
        received_at,
        ..Event::new(id, platform, EventKind::Other)
    }
}

fn unknown_object(object: &str, body: &Value, received_at: i64) -> Vec<Event> {
    let platform = if object == "instagram" {
        Platform::Instagram
    } else {
        Platform::Messenger
    };
    let make = |entry_id: &str, raw: &Value| {
        let id = format!("other:{object}:{entry_id}:{received_at}");
        other_event(id, platform, entry_id, raw, received_at, received_at)
    };
    let list = entries(body);
    if list.is_empty() {
        return vec![make("", body)];
    }
    list.iter().map(|e| make(&id_of(e.get("id")), e)).collect()
}

struct Page<'a> {
    id: &'a str,
    entry_time: Option<i64>,
    received_at: i64,
}

fn messenger(body: &Value, received_at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    for entry in entries(body) {
        let page_id = id_of(entry.get("id"));
        let page = Page {
            id: &page_id,
            entry_time: entry.get("time").and_then(as_i64).map(|ms| ms / 1000),
            received_at,
        };
        let before = out.len();
        for (key, standby) in [("messaging", false), ("standby", true)] {
            let items = entry.get(key).and_then(Value::as_array);
            out.extend(
                items
                    .into_iter()
                    .flatten()
                    .map(|i| messenger_item(i, &page, standby)),
            );
        }
        if out.len() == before {
            let id = format!("messenger:other:{page_id}:{}", digest8(entry));
            let ts = page.entry_time.unwrap_or(received_at);
            out.push(other_event(
                id,
                Platform::Messenger,
                &page_id,
                entry,
                ts,
                received_at,
            ));
        }
    }
    out
}

fn messenger_attachment(a: &Value) -> Attachment {
    let field =
        |key: &str| text_of(a.pointer(&format!("/payload/{key}"))).or_else(|| text_of(a.get(key)));
    Attachment {
        kind: id_of(a.get("type")),
        url: field("url"),
        sticker_id: field("sticker_id"),
        title: field("title"),
        ..Attachment::default()
    }
}

fn messenger_item(item: &Value, page: &Page<'_>, standby: bool) -> Event {
    let sender = id_of(item.pointer("/sender/id"));
    let recipient = id_of(item.pointer("/recipient/id"));
    let stamp = item.get("timestamp").and_then(as_i64);
    let ts = stamp
        .map(|ms| ms / 1000)
        .or(page.entry_time)
        .unwrap_or(page.received_at);
    let stamp_s = stamp.map(|s| s.to_string()).unwrap_or_default();
    let who = format!("{}:{sender}", page.id);
    let tag = if standby { "standby:" } else { "" };
    let make = |id: String, kind: EventKind, raw: Value| Event {
        ts,
        account_id: page.id.to_string(),
        sender: sender.clone(),
        recipient: recipient.clone(),
        raw,
        received_at: page.received_at,
        ..Event::new(id, Platform::Messenger, kind)
    };
    let kind_of = |k: EventKind| if standby { EventKind::Standby } else { k };

    if let Some(msg) = item.get("message").filter(|m| m.is_object()) {
        let mid = id_of(msg.get("mid"));
        let id = if mid.is_empty() {
            format!("messenger:{who}:{stamp_s}")
        } else {
            format!("messenger:{mid}")
        };
        let echo = msg.get("is_echo").and_then(Value::as_bool).unwrap_or(false);
        let mut raw = item.clone();
        if standby {
            raw["standby"] = json!(true);
        }
        let mut ev = make(
            id,
            if echo {
                EventKind::Echo
            } else {
                EventKind::Message
            },
            raw,
        );
        ev.text = text_of(msg.get("text")).or_else(|| text_of(msg.pointer("/quick_reply/payload")));
        ev.attachments = msg
            .get("attachments")
            .and_then(Value::as_array)
            .map(|list| list.iter().map(messenger_attachment).collect())
            .unwrap_or_default();
        ev.reply_to = text_of(msg.pointer("/reply_to/mid"));
        return ev;
    }
    if let Some(d) = item.get("delivery") {
        let mark = d
            .get("watermark")
            .and_then(as_i64)
            .or(stamp)
            .map(|w| w.to_string());
        let id = format!("messenger:{tag}delivery:{who}:{}", mark.unwrap_or_default());
        let mut ev = make(id, kind_of(EventKind::Delivery), item.clone());
        let mids: Vec<&str> = d
            .get("mids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        ev.text = (!mids.is_empty()).then(|| mids.join(","));
        return ev;
    }
    if let Some(r) = item.get("read") {
        let mark = r
            .get("watermark")
            .and_then(as_i64)
            .or(stamp)
            .map(|w| w.to_string());
        let id = format!("messenger:{tag}read:{who}:{}", mark.unwrap_or_default());
        return make(id, kind_of(EventKind::Read), item.clone());
    }
    if let Some(r) = item.get("reaction") {
        let action = id_of(r.get("action"));
        let mid = id_of(r.get("mid"));
        let id = format!("messenger:{tag}reaction:{mid}:{sender}:{action}");
        let mut ev = make(id, kind_of(EventKind::Reaction), item.clone());
        ev.text = text_of(r.get("emoji")).or_else(|| text_of(r.get("reaction")));
        ev.reply_to = text_of(r.get("mid"));
        return ev;
    }
    if let Some(p) = item.get("postback") {
        let id = match text_of(p.get("mid")) {
            Some(mid) => format!("messenger:{tag}postback:{mid}"),
            None => format!("messenger:{tag}postback:{who}:{stamp_s}"),
        };
        let mut ev = make(id, kind_of(EventKind::Postback), item.clone());
        ev.text = text_of(p.get("payload"));
        return ev;
    }
    if let Some(r) = item.get("referral") {
        let mut ev = make(
            format!("messenger:{tag}referral:{who}:{stamp_s}"),
            kind_of(EventKind::Referral),
            item.clone(),
        );
        ev.text = text_of(r.get("ref"));
        return ev;
    }
    if let Some(o) = item.get("optin") {
        let mut ev = make(
            format!("messenger:{tag}optin:{who}:{stamp_s}"),
            kind_of(EventKind::Optin),
            item.clone(),
        );
        ev.text = text_of(o.get("payload")).or_else(|| text_of(o.get("ref")));
        return ev;
    }
    make(
        format!("messenger:{tag}other:{}:{}", page.id, digest8(item)),
        EventKind::Other,
        item.clone(),
    )
}

fn whatsapp(body: &Value, received_at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    for entry in entries(body) {
        let waba = id_of(entry.get("id"));
        let before = out.len();
        let changes = entry.get("changes").and_then(Value::as_array);
        for change in changes.into_iter().flatten() {
            let field = id_of(change.get("field"));
            let value = change.get("value").unwrap_or(&Value::Null);
            let produced = if field == "messages" {
                whatsapp_value(value, &waba, received_at)
            } else {
                Vec::new()
            };
            if produced.is_empty() {
                let id = format!("whatsapp:other:{field}:{waba}:{}", digest8(change));
                out.push(other_event(
                    id,
                    Platform::WhatsApp,
                    &waba,
                    change,
                    received_at,
                    received_at,
                ));
            } else {
                out.extend(produced);
            }
        }
        if out.len() == before {
            let id = format!("whatsapp:other:entry:{waba}:{}", digest8(entry));
            out.push(other_event(
                id,
                Platform::WhatsApp,
                &waba,
                entry,
                received_at,
                received_at,
            ));
        }
    }
    out
}

struct WaContact {
    wa_id: String,
    user_id: String,
    name: Option<String>,
    username: Option<String>,
}

fn whatsapp_contacts(value: &Value) -> Vec<WaContact> {
    value
        .get("contacts")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| WaContact {
                    wa_id: id_of(c.get("wa_id")),
                    user_id: id_of(c.get("user_id")),
                    name: text_of(c.pointer("/profile/name")),
                    username: text_of(c.pointer("/profile/username")),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn whatsapp_value(value: &Value, waba: &str, received_at: i64) -> Vec<Event> {
    let phone_id = id_of(value.pointer("/metadata/phone_number_id"));
    let contacts = whatsapp_contacts(value);
    let mut out = Vec::new();
    for m in value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push(whatsapp_message(m, &phone_id, &contacts, received_at));
    }
    for s in value
        .get("statuses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push(whatsapp_status(s, &phone_id, received_at));
    }
    if let Some(errors) = value.get("errors").filter(|e| !e.is_null()) {
        let id = format!("whatsapp:error:{phone_id}:{}", digest8(errors));
        let raw = json!({"errors": errors, "waba_id": waba});
        out.push(other_event(
            id,
            Platform::WhatsApp,
            &phone_id,
            &raw,
            received_at,
            received_at,
        ));
    }
    out
}

fn set_raw(raw: &mut Value, key: &str, value: Value) {
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(key.to_string(), value);
    }
}

fn whatsapp_message(m: &Value, phone_id: &str, contacts: &[WaContact], received_at: i64) -> Event {
    let from = id_of(m.get("from"));
    let from_user_id = id_of(m.get("from_user_id"));
    let sender = [&from, &from_user_id]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let kind_name = id_of(m.get("type"));
    let mut raw = m.clone();
    if !from_user_id.is_empty() {
        set_raw(&mut raw, "_sender_user_id", json!(from_user_id));
    }
    let contact = contacts.iter().find(|c| {
        (!from.is_empty() && c.wa_id == from)
            || (!from_user_id.is_empty() && c.user_id == from_user_id)
    });
    if let Some(c) = contact {
        if let Some(name) = &c.name {
            set_raw(&mut raw, "_sender_name", json!(name));
        }
        if let Some(username) = &c.username {
            set_raw(&mut raw, "_sender_username", json!(username));
        }
    }
    if let Some(group) = text_of(m.get("group_id")) {
        set_raw(&mut raw, "_group_id", json!(group));
    }
    let media = WA_MEDIA
        .contains(&kind_name.as_str())
        .then(|| m.get(kind_name.as_str()))
        .flatten();
    for (media_kind, flag) in [("audio", "voice"), ("sticker", "animated")] {
        let set = media.and_then(|x| x.get(flag)).and_then(Value::as_bool);
        if kind_name == media_kind && set == Some(true) {
            set_raw(&mut raw, &format!("_{flag}"), json!(true));
        }
    }
    let context_id = text_of(m.pointer("/context/id"));
    let (kind, text, reply_to) = match kind_name.as_str() {
        "reaction" => (
            EventKind::Reaction,
            text_of(m.pointer("/reaction/emoji")),
            text_of(m.pointer("/reaction/message_id")),
        ),
        "edit" => (
            EventKind::Other,
            edited_text(m),
            text_of(m.pointer("/edit/original_message_id")),
        ),
        "revoke" => (
            EventKind::Other,
            None,
            text_of(m.pointer("/revoke/original_message_id")),
        ),
        "system" => (
            EventKind::Other,
            text_of(m.pointer("/system/body")),
            context_id,
        ),
        other => (
            EventKind::Message,
            whatsapp_text(other, m, media),
            context_id,
        ),
    };
    Event {
        ts: m.get("timestamp").and_then(as_i64).unwrap_or(received_at),
        account_id: phone_id.to_string(),
        sender,
        recipient: phone_id.to_string(),
        text,
        attachments: media
            .map(|x| vec![whatsapp_attachment(&kind_name, x)])
            .unwrap_or_default(),
        reply_to,
        raw,
        received_at,
        ..Event::new(
            format!("whatsapp:{}", id_of(m.get("id"))),
            Platform::WhatsApp,
            kind,
        )
    }
}

fn whatsapp_text(kind: &str, m: &Value, media: Option<&Value>) -> Option<String> {
    match kind {
        "text" => text_of(m.pointer("/text/body")),
        "interactive" => text_of(m.pointer("/interactive/button_reply/title"))
            .or_else(|| text_of(m.pointer("/interactive/list_reply/title"))),
        "button" => text_of(m.pointer("/button/text")),
        "location" => location_text(m.get("location")),
        "contacts" => contact_names(m.get("contacts")),
        "order" => text_of(m.pointer("/order/text")),
        "unsupported" => None,
        _ => media.and_then(|x| text_of(x.get("caption"))),
    }
}

fn edited_text(m: &Value) -> Option<String> {
    let edited = m.pointer("/edit/message")?;
    text_of(edited.pointer("/text/body")).or_else(|| {
        WA_MEDIA
            .iter()
            .find_map(|k| text_of(edited.pointer(&format!("/{k}/caption"))))
    })
}

fn contact_names(contacts: Option<&Value>) -> Option<String> {
    let names: Vec<String> = contacts?
        .as_array()?
        .iter()
        .filter_map(|c| {
            text_of(c.pointer("/name/formatted_name"))
                .or_else(|| text_of(c.pointer("/name/first_name")))
        })
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

fn whatsapp_attachment(kind: &str, media: &Value) -> Attachment {
    Attachment {
        kind: kind.to_string(),
        url: text_of(media.get("url")),
        media_id: text_of(media.get("id")),
        mime_type: text_of(media.get("mime_type")),
        caption: text_of(media.get("caption")),
        filename: text_of(media.get("filename")),
        sha256: text_of(media.get("sha256")),
        ..Attachment::default()
    }
}

fn location_text(loc: Option<&Value>) -> Option<String> {
    let loc = loc?;
    let coords = format!(
        "{},{}",
        text_of(loc.get("latitude"))?,
        text_of(loc.get("longitude"))?
    );
    Some(match text_of(loc.get("name")) {
        Some(name) => format!("{coords} {name}"),
        None => coords,
    })
}

fn whatsapp_status(s: &Value, phone_id: &str, received_at: i64) -> Event {
    let status = id_of(s.get("status"));
    let recipient = text_of(s.get("recipient_id"))
        .or_else(|| text_of(s.get("recipient_user_id")))
        .unwrap_or_default();
    Event {
        ts: s.get("timestamp").and_then(as_i64).unwrap_or(received_at),
        account_id: phone_id.to_string(),
        sender: phone_id.to_string(),
        recipient,
        text: text_of(s.get("status")),
        raw: s.clone(),
        received_at,
        ..Event::new(
            format!("whatsapp:status:{}:{status}", id_of(s.get("id"))),
            Platform::WhatsApp,
            EventKind::Status,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_objects_become_other_events() {
        let body = json!({"object": "permissions", "entry": [{"id": "7", "x": 1}, {"id": 8}]});
        let evs = normalize(&body, 1234);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].id, "other:permissions:7:1234");
        assert_eq!(evs[1].id, "other:permissions:8:1234");
        assert!(
            evs.iter()
                .all(|e| e.kind == EventKind::Other && e.received_at == 1234)
        );
        assert_eq!(evs[0].raw["x"], json!(1));

        let bare = normalize(&json!({"object": "instagram"}), 5);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].id, "other:instagram::5");
        assert_eq!(bare[0].platform, Platform::Instagram);

        let junk = normalize(&json!([1, 2]), 9);
        assert_eq!(junk.len(), 1);
        assert_eq!(junk[0].kind, EventKind::Other);
    }

    #[test]
    fn page_entries_without_messaging_are_kept_as_other() {
        let body = json!({"object": "page", "entry": [{"id": "1", "time": 5000,
            "changes": [{"field": "feed", "value": {"item": "post"}}]}]});
        let evs = normalize(&body, 77);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].kind, EventKind::Other);
        assert_eq!(evs[0].ts, 5);
        assert_eq!(evs[0].account_id, "1");
        assert_eq!(evs[0], normalize(&body, 77)[0], "ids are deterministic");
    }

    #[test]
    fn messenger_postback_reaction_referral_optin() {
        let body = json!({"object": "page", "entry": [{"id": "P", "time": 1700000000999_i64, "messaging": [
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1700000001000_i64,
             "postback": {"title": "Start", "payload": "GET_STARTED", "mid": "m_pb"}},
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1700000002000_i64,
             "reaction": {"mid": "m_x", "action": "react", "emoji": "\u{2764}", "reaction": "love"}},
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1700000003000_i64,
             "referral": {"ref": "promo", "source": "SHORTLINK"}},
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1700000004000_i64,
             "optin": {"payload": "NEWSLETTER"}},
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1700000005000_i64,
             "message_edit": {"mid": "m_e", "text": "edited"}}
        ]}]});
        let evs = normalize(&body, 1);
        let kinds: Vec<_> = evs.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                EventKind::Postback,
                EventKind::Reaction,
                EventKind::Referral,
                EventKind::Optin,
                EventKind::Other
            ]
        );
        assert_eq!(evs[0].id, "messenger:postback:m_pb");
        assert_eq!(evs[0].text.as_deref(), Some("GET_STARTED"));
        assert_eq!(evs[0].raw["postback"]["title"], json!("Start"));
        assert_eq!(evs[1].id, "messenger:reaction:m_x:U:react");
        assert_eq!(evs[1].text.as_deref(), Some("\u{2764}"));
        assert_eq!(evs[1].reply_to.as_deref(), Some("m_x"));
        assert_eq!(evs[2].text.as_deref(), Some("promo"));
        assert_eq!(evs[3].text.as_deref(), Some("NEWSLETTER"));
        assert!(evs.iter().all(|e| e.account_id == "P" && e.sender == "U"));
        assert_eq!(evs[0].ts, 1_700_000_001);
    }

    #[test]
    fn messenger_standby_events() {
        let body = json!({"object": "page", "entry": [{"id": "P", "standby": [
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1000,
             "message": {"mid": "m_s", "text": "hello while standby"}},
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 2000,
             "read": {"watermark": 2000}}
        ]}]});
        let evs = normalize(&body, 1);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].kind, EventKind::Message);
        assert_eq!(evs[0].raw["standby"], json!(true));
        assert_eq!(evs[0].text.as_deref(), Some("hello while standby"));
        assert_eq!(evs[1].kind, EventKind::Standby);
        assert_eq!(evs[1].id, "messenger:standby:read:P:U:2000");
    }

    #[test]
    fn messenger_attachment_shapes() {
        let body = json!({"object": "page", "entry": [{"id": "P", "messaging": [
            {"sender": {"id": "U"}, "recipient": {"id": "P"}, "timestamp": 1000,
             "message": {"mid": "m_a", "attachments": [
                {"type": "sticker", "payload": {"url": "https://cdn.example/s.png", "sticker_id": 369239263222822_i64}},
                {"type": "fallback", "title": "A link", "url": "https://example.org/x", "payload": null},
                {"type": "location", "title": "Pin", "url": "https://maps.example/?q=1", "payload": {"coordinates": {"lat": 1.5, "long": 2.5}}}
             ]}}
        ]}]});
        let evs = normalize(&body, 1);
        let a = &evs[0].attachments;
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].kind, "sticker");
        assert_eq!(a[0].sticker_id.as_deref(), Some("369239263222822"));
        assert_eq!(a[1].url.as_deref(), Some("https://example.org/x"));
        assert_eq!(a[1].title.as_deref(), Some("A link"));
        assert_eq!(a[2].kind, "location");
        assert_eq!(evs[0].text, None);
    }

    #[test]
    fn whatsapp_other_fields_statuses_and_errors() {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "W", "changes": [
            {"field": "message_template_status_update", "value": {"event": "APPROVED"}},
            {"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "1", "phone_number_id": "PN"},
                "errors": [{"code": 131000, "title": "Something went wrong"}]}},
            {"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"phone_number_id": "PN"},
                "messages": [
                    {"from": "15550001", "id": "wamid.R", "timestamp": "1700000000", "type": "reaction",
                     "reaction": {"message_id": "wamid.T", "emoji": "\u{1F44D}"}},
                    {"from": "15550001", "id": "wamid.L", "timestamp": "1700000001", "type": "location",
                     "location": {"latitude": 45.5, "longitude": -73.5, "name": "Cafe"}},
                    {"from": "15550001", "id": "wamid.I", "timestamp": "1700000002", "type": "interactive",
                     "interactive": {"type": "button_reply", "button_reply": {"id": "b1", "title": "Yes"}},
                     "context": {"from": "PN", "id": "wamid.Q"}},
                    {"from": "15550001", "id": "wamid.B", "timestamp": "1700000003", "type": "button",
                     "button": {"payload": "p", "text": "Later"}}
                ]}}
        ]}]});
        let evs = normalize(&body, 42);
        assert_eq!(evs.len(), 6);
        assert_eq!(evs[0].kind, EventKind::Other);
        assert!(
            evs[0]
                .id
                .starts_with("whatsapp:other:message_template_status_update:W:")
        );
        assert_eq!(evs[1].kind, EventKind::Other);
        assert!(evs[1].id.starts_with("whatsapp:error:PN:"));
        assert_eq!(evs[1].raw["errors"][0]["code"], json!(131000));
        assert_eq!(
            (evs[2].kind, evs[2].text.as_deref()),
            (EventKind::Reaction, Some("\u{1F44D}"))
        );
        assert_eq!(evs[2].reply_to.as_deref(), Some("wamid.T"));
        assert_eq!(evs[3].text.as_deref(), Some("45.5,-73.5 Cafe"));
        assert_eq!(evs[4].text.as_deref(), Some("Yes"));
        assert_eq!(evs[4].reply_to.as_deref(), Some("wamid.Q"));
        assert_eq!(evs[5].text.as_deref(), Some("Later"));
        assert!(evs.iter().all(|e| e.platform == Platform::WhatsApp));
    }
}
