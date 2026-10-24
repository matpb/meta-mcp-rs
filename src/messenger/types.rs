//! Parameters of the Messenger tools and the shapes they return, parsed defensively from Graph JSON.

use chrono::DateTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::error::as_i64;

/// Arguments of `messenger_list_conversations`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListConversationsParams {
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
    /// Maximum conversations to return (default 20, maximum 50).
    pub limit: Option<i64>,
    /// Pagination cursor: the next_cursor of the previous answer.
    pub after: Option<String>,
    /// Only the conversation with this customer, given as a Page-scoped ID (PSID).
    pub user_id: Option<String>,
}

/// Arguments of `messenger_read_conversation`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ReadConversationParams {
    /// Conversation ID from messenger_list_conversations (looks like t_123456789).
    pub conversation_id: String,
    /// Maximum messages to return, newest first (default 20, maximum 100).
    pub limit: Option<i64>,
    /// Pagination cursor: the next_cursor of the previous answer, for older messages.
    pub after: Option<String>,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
}

/// Arguments of `messenger_get_message`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetMessageParams {
    /// Message ID (the id of a message from messenger_read_conversation or the inbox).
    pub message_id: String,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
}

/// Arguments of `messenger_send_text`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendTextParams {
    /// The message text: UTF-8, fewer than 2000 characters.
    pub text: String,
    /// Recipient's Page-scoped ID (PSID), as counterpart.psid in messenger_list_conversations.
    pub recipient_psid: Option<String>,
    /// Conversation ID to reply in, used when recipient_psid is not given; recipient_psid wins if both are set.
    pub conversation_id: Option<String>,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
    /// RESPONSE (default, within 24 hours of the person's last message), UPDATE, or MESSAGE_TAG (needs tag).
    #[schemars(extend("enum" = ["RESPONSE", "UPDATE", "MESSAGE_TAG", null]))]
    pub messaging_type: Option<String>,
    /// HUMAN_AGENT lets a person reply up to 7 days after the last message and needs Meta's Human Agent approval; it implies MESSAGE_TAG.
    #[schemars(extend("enum" = ["HUMAN_AGENT", null]))]
    pub tag: Option<String>,
    /// Message ID to reply to as a quoted reply.
    pub reply_to_mid: Option<String>,
    /// Push behaviour: REGULAR (default), SILENT_PUSH or NO_PUSH.
    #[schemars(extend("enum" = ["REGULAR", "SILENT_PUSH", "NO_PUSH", null]))]
    pub notification_type: Option<String>,
    /// Opaque string (fewer than 1000 characters) returned with the message echo webhook.
    pub metadata: Option<String>,
}

/// Arguments of `messenger_send_attachment`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendAttachmentParams {
    /// Attachment type: image, audio, video or file.
    #[schemars(extend("enum" = ["image", "audio", "video", "file"]))]
    pub kind: String,
    /// Public https URL of the file; Meta downloads it (images up to 8 MB, others up to 25 MB).
    pub url: Option<String>,
    /// Local file inside the files directory (META_MCP_FILES_DIR) to upload (up to 25 MB).
    pub path: Option<String>,
    /// Several image URLs sent as one message (kind image only, at most 30).
    pub urls: Option<Vec<String>>,
    /// Recipient's Page-scoped ID (PSID), as counterpart.psid in messenger_list_conversations.
    pub recipient_psid: Option<String>,
    /// Conversation ID to reply in, used when recipient_psid is not given; recipient_psid wins if both are set.
    pub conversation_id: Option<String>,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
    /// With url: ask Meta to keep the file so the returned attachment_id can be sent again (default false).
    pub is_reusable: Option<bool>,
    /// With path: MIME type of the file when its extension is unknown, for example image/png.
    pub mime_type: Option<String>,
    /// RESPONSE (default), UPDATE, or MESSAGE_TAG (needs tag).
    #[schemars(extend("enum" = ["RESPONSE", "UPDATE", "MESSAGE_TAG", null]))]
    pub messaging_type: Option<String>,
    /// HUMAN_AGENT lets a person reply up to 7 days after the last message and needs Meta's Human Agent approval; it implies MESSAGE_TAG.
    #[schemars(extend("enum" = ["HUMAN_AGENT", null]))]
    pub tag: Option<String>,
}

/// Arguments of `messenger_download_attachment`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DownloadAttachmentParams {
    /// Attachment URL on a Meta media host (the url of an attachment from messenger_read_conversation).
    pub url: Option<String>,
    /// Message ID to take the attachment from, used when url is not given.
    pub message_id: Option<String>,
    /// With message_id: which attachment of the message, starting at 0 (default 0).
    pub index: Option<i64>,
    /// File name to save as, inside the files directory; path separators are removed.
    pub dest_name: Option<String>,
    /// For small images (up to 2 MiB), also return the picture itself (default true).
    pub inline: Option<bool>,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
    /// Replace a file of the same name (default false: the call fails if it exists).
    pub overwrite: Option<bool>,
}

/// Arguments of `messenger_sender_action`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SenderActionParams {
    /// typing_on, typing_off, mark_seen, react or unreact.
    #[schemars(extend("enum" = ["typing_on", "typing_off", "mark_seen", "react", "unreact"]))]
    pub action: String,
    /// Recipient's Page-scoped ID (PSID), as counterpart.psid in messenger_list_conversations.
    pub recipient_psid: Option<String>,
    /// Conversation ID, used when recipient_psid is not given; recipient_psid wins if both are set.
    pub conversation_id: Option<String>,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
    /// Message to react to or to remove the reaction from (react and unreact).
    pub message_id: Option<String>,
    /// For react: an emoji, or one of love, like, haha, wow, sad, angry, smile, dislike (default love).
    pub reaction: Option<String>,
}

/// Arguments of `messenger_get_profile`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetProfileParams {
    /// Page-scoped ID (PSID) of the person.
    pub psid: String,
    /// Facebook Page ID; optional when only one Page is configured.
    pub page_id: Option<String>,
}

/// A person on a conversation: the customer or the Page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Person {
    pub id: String,
    pub name: Option<String>,
}

/// The customer side of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Counterpart {
    pub psid: String,
    pub name: Option<String>,
}

/// One conversation of the Page inbox.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conversation {
    pub id: String,
    pub updated_time: Option<String>,
    pub counterpart: Option<Counterpart>,
    pub participants: Vec<Person>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_reply: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_owner: Option<bool>,
}

/// A file attached to a message, with the kind and URL normalised.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attachment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<i64>,
}

/// A link, post or reel shared in a message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Share {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub id: String,
    pub created_time: Option<String>,
    pub created_at: Option<i64>,
    pub from: Option<Person>,
    pub to: Vec<Person>,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub shares: Vec<Share>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_mid: Option<String>,
    pub is_from_page: bool,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub is_unsupported: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A Graph id or counter that may arrive as a string or a number.
pub fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(as_i64)
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    match v.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn items<'a>(v: &'a Value, pointer: &str) -> impl Iterator<Item = &'a Value> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// Unix seconds of a Graph timestamp: `2022-07-12T19:11:07+0000`, RFC 3339 or a number.
pub fn parse_graph_time(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => parse_graph_datetime(s),
        Value::Number(n) => n.as_i64().map(epoch_seconds),
        _ => None,
    }
}

/// Unix seconds of a Graph timestamp string; a numeric string is read as epoch seconds.
pub fn parse_graph_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    for format in ["%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S%.f%z"] {
        if let Ok(dt) = DateTime::parse_from_str(s, format) {
            return Some(dt.timestamp());
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    s.parse::<i64>().ok().map(epoch_seconds)
}

/// Epoch values above 10^11 are milliseconds.
fn epoch_seconds(n: i64) -> i64 {
    if n.abs() > 100_000_000_000 {
        n / 1000
    } else {
        n
    }
}

fn person(v: &Value) -> Option<Person> {
    Some(Person {
        id: id_string(v.get("id")?)?,
        name: text(v, "name"),
    })
}

/// `participants.data` of a conversation, or `to.data` of a message.
pub fn people(v: &Value, pointer: &str) -> Vec<Person> {
    items(v, pointer).filter_map(person).collect()
}

/// The participants of a conversation answer.
pub fn participants(v: &Value) -> Vec<Person> {
    people(v, "/participants/data")
}

/// The participant who is not the Page.
pub fn counterpart(participants: &[Person], page_id: &str) -> Option<Counterpart> {
    participants
        .iter()
        .find(|p| p.id != page_id)
        .map(|p| Counterpart {
            psid: p.id.clone(),
            name: p.name.clone(),
        })
}

/// A conversation from `GET {page}/conversations`; `None` when it has no id.
pub fn conversation(v: &Value, page_id: &str) -> Option<Conversation> {
    let participants = participants(v);
    Some(Conversation {
        id: id_string(v.get("id")?)?,
        updated_time: v.get("updated_time").and_then(id_string),
        counterpart: counterpart(&participants, page_id),
        participants,
        snippet: text(v, "snippet"),
        unread_count: int(v, "unread_count"),
        message_count: int(v, "message_count"),
        can_reply: flag(v, "can_reply"),
        link: text(v, "link"),
        is_owner: flag(v, "is_owner"),
    })
}

/// An attachment from `attachments.data[]`: kind is image, sticker, video, audio or file.
pub fn attachment(v: &Value) -> Attachment {
    let image = v.get("image_data").filter(|d| d.is_object());
    let video = v.get("video_data").filter(|d| d.is_object());
    let mime_type = text(v, "mime_type");
    let kind = if let Some(img) = image {
        if flag(img, "render_as_sticker") == Some(true) {
            "sticker"
        } else {
            "image"
        }
    } else if video.is_some() {
        "video"
    } else if mime_type
        .as_deref()
        .is_some_and(|m| m.to_ascii_lowercase().starts_with("audio/"))
    {
        "audio"
    } else {
        "file"
    };
    let media = image.or(video);
    Attachment {
        id: v.get("id").and_then(id_string),
        kind,
        mime_type,
        name: text(v, "name"),
        size: int(v, "size"),
        url: media
            .and_then(|m| text(m, "url"))
            .or_else(|| text(v, "file_url")),
        preview_url: media.and_then(|m| text(m, "preview_url")),
        width: media.and_then(|m| int(m, "width")),
        height: media.and_then(|m| int(m, "height")),
        length: video.and_then(|m| int(m, "length")),
    }
}

/// A share from `shares.data[]`.
pub fn share(v: &Value) -> Share {
    Share {
        kind: "share",
        id: v.get("id").and_then(id_string),
        link: text(v, "link"),
        name: text(v, "name"),
        description: text(v, "description"),
    }
}

/// A message from `GET {conversation}/messages` or `GET {message}`; `None` when it has no id.
pub fn message(v: &Value, page_id: &str) -> Option<Message> {
    let from = v.get("from").and_then(person);
    let created = v.get("created_time");
    Some(Message {
        id: id_string(v.get("id")?)?,
        created_time: created.and_then(id_string),
        created_at: created.and_then(parse_graph_time),
        is_from_page: from.as_ref().is_some_and(|p| p.id == page_id),
        from,
        to: people(v, "/to/data"),
        text: v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        attachments: items(v, "/attachments/data").map(attachment).collect(),
        shares: items(v, "/shares/data").map(share).collect(),
        reply_to_mid: v.pointer("/reply_to/mid").and_then(id_string),
        tags: items(v, "/tags/data")
            .filter_map(|t| text(t, "name"))
            .collect(),
        is_unsupported: flag(v, "is_unsupported").unwrap_or(false),
    })
}

/// The cursor for the next page, only when Graph says there is one.
pub fn next_cursor(answer: &Value) -> Option<String> {
    let next = answer.pointer("/paging/next").and_then(Value::as_str)?;
    answer
        .pointer("/paging/cursors/after")
        .and_then(id_string)
        .or_else(|| {
            url::Url::parse(next)
                .ok()?
                .query_pairs()
                .find(|(k, _)| k == "after")
                .map(|(_, v)| v.into_owned())
        })
        .filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graph_datetime_parser() {
        let want = Some(1_657_653_067);
        for ok in [
            "2022-07-12T19:11:07+0000",
            "2022-07-12T19:11:07+00:00",
            "2022-07-12T19:11:07Z",
            "2022-07-12T19:11:07.250+0000",
            "  2022-07-12T19:11:07+0000 ",
            "1657653067",
        ] {
            assert_eq!(parse_graph_datetime(ok), want, "{ok}");
        }
        assert_eq!(
            parse_graph_datetime("2022-07-12T19:11:07+0530"),
            Some(1_657_633_267)
        );
        assert_eq!(
            parse_graph_datetime("2026-10-24T12:00:00+0000"),
            Some(1_792_843_200)
        );
        assert_eq!(parse_graph_time(&json!(1_657_653_067)), want);
        assert_eq!(parse_graph_time(&json!(1_657_653_067_000_i64)), want);
        assert_eq!(parse_graph_time(&json!("2022-07-12T19:11:07+0000")), want);
        for bad in ["", "yesterday", "2022-13-45T00:00:00+0000", "2022-07-12"] {
            assert_eq!(parse_graph_datetime(bad), None, "{bad}");
        }
        for bad in [json!(null), json!(true), json!([1]), json!(1.5)] {
            assert_eq!(parse_graph_time(&bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_may_be_numbers_or_strings() {
        assert_eq!(id_string(&json!("t_1")), Some("t_1".into()));
        assert_eq!(id_string(&json!(12345)), Some("12345".into()));
        assert_eq!(id_string(&json!("")), None);
        assert_eq!(id_string(&json!(null)), None);
        assert_eq!(id_string(&json!({"a": 1})), None);
    }

    #[test]
    fn counterpart_is_the_participant_who_is_not_the_page() {
        let v = json!({"id": "t_1", "updated_time": "2026-10-24T12:00:00+0000",
            "participants": {"data": [
                {"id": 4242, "name": "My Page", "email": "4242@facebook.com"},
                {"id": "PSID9", "name": "Alice", "email": "PSID9@facebook.com"}]},
            "snippet": "hi there", "unread_count": "3", "message_count": 12,
            "can_reply": true, "link": "/p/inbox/1", "is_owner": "false"});
        let c = conversation(&v, "4242").expect("conversation");
        assert_eq!(c.id, "t_1");
        assert_eq!(
            c.counterpart,
            Some(Counterpart {
                psid: "PSID9".into(),
                name: Some("Alice".into())
            })
        );
        assert_eq!(c.participants.len(), 2);
        assert_eq!(c.unread_count, Some(3));
        assert_eq!(c.message_count, Some(12));
        assert_eq!(c.can_reply, Some(true));
        assert_eq!(c.is_owner, Some(false));
        let out = serde_json::to_value(&c).expect("json");
        assert_eq!(out["counterpart"]["psid"], json!("PSID9"));
        assert_eq!(
            out["participants"][0],
            json!({"id": "4242", "name": "My Page"})
        );

        let reduced = conversation(&json!({"id": "t_2", "participants": {"data": []}}), "4242")
            .expect("conversation");
        assert_eq!(reduced.counterpart, None);
        let out = serde_json::to_value(&reduced).expect("json");
        assert!(out.get("snippet").is_none() && out.get("unread_count").is_none());
        assert!(conversation(&json!({"participants": {}}), "4242").is_none());
    }

    #[test]
    fn attachment_kinds_and_urls() {
        let image = attachment(
            &json!({"id": "a1", "mime_type": "image/jpeg", "name": "p.jpg",
            "size": "2048", "image_data": {"url": "https://x/i.jpg", "preview_url": "https://x/p.jpg",
            "width": 640, "height": "480", "render_as_sticker": false}}),
        );
        assert_eq!(image.kind, "image");
        assert_eq!(image.url.as_deref(), Some("https://x/i.jpg"));
        assert_eq!(image.preview_url.as_deref(), Some("https://x/p.jpg"));
        assert_eq!(
            (image.width, image.height, image.size),
            (Some(640), Some(480), Some(2048))
        );

        let sticker = attachment(
            &json!({"image_data": {"url": "https://x/s.png", "render_as_sticker": true}}),
        );
        assert_eq!(sticker.kind, "sticker");

        let video = attachment(&json!({"video_data": {"url": "https://x/v.mp4",
            "preview_url": "https://x/v.jpg", "width": 320, "height": 240, "length": 12}}));
        assert_eq!(video.kind, "video");
        assert_eq!(video.length, Some(12));
        assert_eq!(video.url.as_deref(), Some("https://x/v.mp4"));

        let audio = attachment(&json!({"mime_type": "Audio/MPEG", "file_url": "https://x/a.mp3"}));
        assert_eq!(audio.kind, "audio");
        assert_eq!(audio.url.as_deref(), Some("https://x/a.mp3"));

        let file = attachment(&json!({"mime_type": "application/pdf", "name": "r.pdf",
            "file_url": "https://x/r.pdf", "size": 10}));
        assert_eq!(file.kind, "file");
        assert_eq!(file.url.as_deref(), Some("https://x/r.pdf"));

        let bare = attachment(&json!({"image_data": {"width": 1}, "file_url": "https://x/f"}));
        assert_eq!(bare.url.as_deref(), Some("https://x/f"));
        let none = attachment(&json!({}));
        assert_eq!((none.kind, none.url), ("file", None));
    }

    #[test]
    fn message_fields_and_origin() {
        let v = json!({"id": "m_1", "created_time": "2022-07-12T19:11:07+0000",
            "from": {"id": "PSID9", "name": "Alice", "email": "e"},
            "to": {"data": [{"id": "4242", "name": "My Page"}]},
            "message": "hello", "reply_to": {"mid": "m_0", "is_self_reply": false},
            "tags": {"data": [{"name": "inbox"}, {"name": "source:chat_plugin"}]},
            "shares": {"data": [{"id": "s1", "link": "https://x/post", "name": "Post"}]},
            "attachments": {"data": [{"id": "a1", "file_url": "https://x/f.txt", "mime_type": "text/plain"}]},
            "is_unsupported": false});
        let m = message(&v, "4242").expect("message");
        assert_eq!(m.created_at, Some(1_657_653_067));
        assert_eq!(m.created_time.as_deref(), Some("2022-07-12T19:11:07+0000"));
        assert!(!m.is_from_page);
        assert_eq!(
            m.to,
            vec![Person {
                id: "4242".into(),
                name: Some("My Page".into())
            }]
        );
        assert_eq!(m.reply_to_mid.as_deref(), Some("m_0"));
        assert_eq!(m.tags, ["inbox", "source:chat_plugin"]);
        assert_eq!(m.shares[0].kind, "share");
        assert_eq!(m.attachments[0].kind, "file");
        let out = serde_json::to_value(&m).expect("json");
        assert!(out.get("is_unsupported").is_none());
        assert_eq!(out["shares"][0]["link"], json!("https://x/post"));

        let own = message(
            &json!({"id": 77, "from": {"id": 4242}, "message": null}),
            "4242",
        )
        .expect("message");
        assert!(own.is_from_page);
        assert_eq!(own.id, "77");
        assert_eq!(own.text, "");
        assert_eq!(own.created_at, None);
        assert!(message(&json!({"message": "no id"}), "4242").is_none());
        let odd = message(&json!({"id": "m_9", "is_unsupported": true}), "4242").expect("message");
        assert_eq!(
            serde_json::to_value(&odd).expect("json")["is_unsupported"],
            json!(true)
        );
    }

    #[test]
    fn cursor_needs_a_next_page() {
        let both = json!({"paging": {"cursors": {"after": "AFTER1"},
            "next": "https://graph.facebook.com/v26.0/1/conversations?after=AFTER1&limit=2"}});
        assert_eq!(next_cursor(&both), Some("AFTER1".into()));
        let only_next = json!({"paging": {"next": "https://graph.facebook.com/v26.0/1/conversations?limit=2&after=QQ%3D%3D"}});
        assert_eq!(next_cursor(&only_next), Some("QQ==".into()));
        let last_page = json!({"paging": {"cursors": {"before": "B", "after": "A"}}});
        assert_eq!(next_cursor(&last_page), None);
        assert_eq!(next_cursor(&json!({"data": []})), None);
        let junk = json!({"paging": {"next": "not a url"}});
        assert_eq!(next_cursor(&junk), None);
    }
}
