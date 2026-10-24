//! Facebook Messenger (Page inbox) operations on top of the Graph client.

pub mod media;
pub mod types;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use url::Url;

use crate::config::{Config, PageConfig};
use crate::files::{FileError, FileJail, inbound_dest};
use crate::graph::download::TokenMode;
use crate::graph::{GraphClient, GraphError};
use crate::mcp::{plain_segment, valid_id};
pub use types::{
    DownloadAttachmentParams, GetMessageParams, GetProfileParams, ListConversationsParams,
    ReadConversationParams, SendAttachmentParams, SendTextParams, SenderActionParams,
};
use types::{id_string, participants};

const CONVERSATION_FIELDS: &str = "id,updated_time,participants{id,name,email},snippet,unread_count,message_count,can_reply,link,is_owner";
const CONVERSATION_FIELDS_REDUCED: &str = "id,updated_time,participants{id,name,email}";
const MESSAGE_FIELDS: &str = "id,created_time,from,to,message,attachments,shares{link,name,description,id},reply_to,tags,is_unsupported";
const PROFILE_FIELDS: &str = "first_name,last_name,profile_pic";
const DETAIL_NOTE: &str =
    "Meta exposes details for the 20 most recent messages of a conversation only";
const PROFILE_NOTE: &str = "profile fields need the Business Asset User Profile Access feature; participant names from messenger_list_conversations do not";

/// Conversations API limit: 2 calls per second per Page.
const READ_SPACING: Duration = Duration::from_millis(500);
const PSID_CACHE_MAX: usize = 1000;
const DETAIL_WINDOW: i64 = 20;
const TEXT_LIMIT_CHARS: usize = 2000;
const METADATA_LIMIT_CHARS: usize = 1000;
const MAX_IMAGES: usize = 30;
/// Meta CDN hosts that may receive the Page token; the Graph host and `*.facebook.com` never do.
const TOKEN_HOST_SUFFIXES: &[&str] = &[".fbcdn.net", ".fbsbx.com"];
/// Graph codes a conversation read repeats once.
const RETRYABLE_READ_CODES: [i64; 7] = [1, 2, 4, 17, 32, 613, 80006];
const INBOUND_PLATFORM: &str = "messenger";
const MAYBE_DELIVERED: &str =
    "the message may still have been delivered: read the conversation before sending again";

type Gate = Arc<tokio::sync::Mutex<Option<Instant>>>;
type GateMap = Mutex<HashMap<String, Gate>>;
type PsidCache = Mutex<HashMap<String, String>>;

static PSID_CACHE: OnceLock<PsidCache> = OnceLock::new();
static READ_GATE: OnceLock<GateMap> = OnceLock::new();

/// Waits until `spacing` has passed since the previous call for the same page.
async fn throttle_in(map: &GateMap, page_id: &str, spacing: Duration) {
    let gate = {
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        guard.entry(page_id.to_string()).or_default().clone()
    };
    let mut last = gate.lock().await;
    if let Some(previous) = *last {
        let ready = previous + spacing;
        if ready > Instant::now() {
            tokio::time::sleep_until(ready).await;
        }
    }
    *last = Some(Instant::now());
}

fn cache_get(map: &PsidCache, key: &str) -> Option<String> {
    map.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned()
}

fn cache_put(map: &PsidCache, key: String, psid: String) {
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= PSID_CACHE_MAX
        && !guard.contains_key(&key)
        && let Some(oldest) = guard.keys().next().cloned()
    {
        guard.remove(&oldest);
    }
    guard.insert(key, psid);
}

fn invalid(msg: impl Into<String>) -> GraphError {
    GraphError::Config(msg.into())
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn check_id(label: &str, value: &str) -> Result<(), GraphError> {
    if valid_id(value) {
        Ok(())
    } else {
        Err(invalid(format!("`{label}` is not a valid id: `{value}`")))
    }
}

/// Message ids may carry `$` in the legacy `mid.$...` form.
fn check_mid(label: &str, value: &str) -> Result<(), GraphError> {
    let ok = !value.is_empty()
        && value.len() <= 512
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'=' | b'+' | b'$')
        })
        && plain_segment(value);
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "`{label}` is not a valid message id: `{value}`"
        )))
    }
}

fn extra_hint(code: i64, subcode: Option<i64>) -> Option<&'static str> {
    let is = |n: i64| code == n || subcode == Some(n);
    if is(9_000_001) {
        Some(
            "message details are only exposed for the 20 most recent messages of a conversation; older ones are deleted or unavailable through the API",
        )
    } else if is(2_018_047) {
        Some(
            "the attachment type does not match the file: use image, audio or video only for files of that kind, otherwise file",
        )
    } else if is(2_018_109) {
        Some("the attachment is too large: images up to 8 MB, other files up to 25 MB")
    } else if is(2_018_008) {
        Some(
            "Meta could not fetch the URL: it must be public, https with a valid certificate, and answer within 10 seconds (75 for video)",
        )
    } else if is(2_018_278) || is(2_534_022) {
        Some(
            "outside the 24-hour messaging window: the person must write to the Page first. HUMAN_AGENT is only for a person replying by hand within 7 days, never for generated replies",
        )
    } else {
        None
    }
}

/// Adds this phase's hints on top of the ones the Graph client already attaches.
fn explain(mut e: GraphError) -> GraphError {
    if let GraphError::Api {
        code,
        subcode,
        hint,
        ..
    } = &mut e
        && let Some(extra) = extra_hint(*code, *subcode)
    {
        *hint = Some(extra);
    }
    e
}

/// True for the code 100 answer Graph gives when a requested field does not exist.
fn unknown_field_error(e: &GraphError) -> bool {
    matches!(e, GraphError::Api { code: 100, message, .. }
        if message.to_ascii_lowercase().contains("nonexisting field"))
}

/// Errors a conversation read repeats once: transient Graph codes, server errors, timeouts and
/// connections that did not complete.
fn retryable_read(e: &GraphError) -> bool {
    match e {
        GraphError::Api {
            code, http_status, ..
        } => RETRYABLE_READ_CODES.contains(code) || *http_status >= 500,
        GraphError::Transport(m) => {
            m.contains("timed out")
                || m.contains("could not connect")
                || m.contains("answered HTTP 5")
        }
        _ => false,
    }
}

/// A failed send whose outcome is unknown gets a note: the request may have reached the person.
fn with_delivery_caveat(mut e: GraphError) -> GraphError {
    let unsure = match &e {
        GraphError::Transport(_) | GraphError::Json(_) => true,
        GraphError::Api { http_status, .. } => *http_status >= 500,
        GraphError::Config(_) | GraphError::Refused(_) => false,
    };
    if unsure {
        match &mut e {
            GraphError::Transport(m)
            | GraphError::Json(m)
            | GraphError::Config(m)
            | GraphError::Refused(m)
            | GraphError::Api { message: m, .. } => m.push_str(&format!(" ({MAYBE_DELIVERED})")),
        }
    }
    e
}

/// `GraphClient::download` reports the HTTP status only inside its error message.
fn is_auth_failure(e: &GraphError) -> bool {
    matches!(e, GraphError::Transport(m)
        if m.starts_with("download answered HTTP 401") || m.starts_with("download answered HTTP 403"))
}

/// Whether the Page token may be sent to this host: Meta CDN hosts or hosts listed by the operator.
fn token_host_allowed(host: &str, extra: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
        || TOKEN_HOST_SUFFIXES
            .iter()
            .any(|s| host.len() > s.len() && host.ends_with(s))
}

/// The `host:port` entry for the URL, so that the token follows no redirect to another port.
fn token_entry(url: &Url, host: String) -> String {
    match url.port_or_known_default() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// The `messaging_type` and `tag` to send; a tag alone implies MESSAGE_TAG.
fn messaging_fields(
    messaging_type: Option<&str>,
    tag: Option<&str>,
) -> Result<(&'static str, Option<&'static str>), GraphError> {
    let tag = match tag.map(str::trim).filter(|t| !t.is_empty()) {
        None => None,
        Some(t) if t.eq_ignore_ascii_case("HUMAN_AGENT") => Some("HUMAN_AGENT"),
        Some(t) => {
            return Err(invalid(format!(
                "tag `{t}` is not supported: HUMAN_AGENT is the only message tag that can still be sent"
            )));
        }
    };
    let kind = match messaging_type.map(str::trim).filter(|t| !t.is_empty()) {
        None if tag.is_some() => "MESSAGE_TAG",
        None => "RESPONSE",
        Some(t) => match t.to_ascii_uppercase().as_str() {
            "RESPONSE" => "RESPONSE",
            "UPDATE" => "UPDATE",
            "MESSAGE_TAG" => "MESSAGE_TAG",
            _ => {
                return Err(invalid(format!(
                    "messaging_type `{t}` is not valid: use RESPONSE, UPDATE or MESSAGE_TAG"
                )));
            }
        },
    };
    match (kind, tag) {
        ("MESSAGE_TAG", None) => Err(invalid("messaging_type MESSAGE_TAG needs tag HUMAN_AGENT")),
        (k, Some(_)) if k != "MESSAGE_TAG" => {
            Err(invalid("tag HUMAN_AGENT needs messaging_type MESSAGE_TAG"))
        }
        _ => Ok((kind, tag)),
    }
}

fn notification_type(v: Option<&str>) -> Result<Option<&'static str>, GraphError> {
    match v.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) => match t.to_ascii_uppercase().as_str() {
            "REGULAR" => Ok(Some("REGULAR")),
            "SILENT_PUSH" => Ok(Some("SILENT_PUSH")),
            "NO_PUSH" => Ok(Some("NO_PUSH")),
            _ => Err(invalid(format!(
                "notification_type `{t}` is not valid: use REGULAR, SILENT_PUSH or NO_PUSH"
            ))),
        },
    }
}

fn attachment_kind(raw: &str) -> Result<&'static str, GraphError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "image" => Ok("image"),
        "audio" => Ok("audio"),
        "video" => Ok("video"),
        "file" => Ok("file"),
        _ => Err(invalid(format!(
            "kind `{}` is not valid: use image, audio, video or file",
            raw.trim()
        ))),
    }
}

/// The emoji sent on the wire; Meta's react action takes an emoji, so the usual names map to one.
fn reaction_emoji(reaction: Option<&str>) -> Result<String, GraphError> {
    let raw = reaction
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("love");
    let named = match raw.to_ascii_lowercase().as_str() {
        "love" => Some("\u{2764}\u{fe0f}"),
        "like" => Some("\u{1f44d}"),
        "dislike" => Some("\u{1f44e}"),
        "haha" => Some("\u{1f606}"),
        "wow" => Some("\u{1f62e}"),
        "sad" => Some("\u{1f622}"),
        "angry" => Some("\u{1f620}"),
        "smile" => Some("\u{1f60a}"),
        _ => None,
    };
    if let Some(emoji) = named {
        return Ok(emoji.to_string());
    }
    let wordlike = raw
        .chars()
        .all(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '-' | ' '));
    if wordlike || raw.chars().count() > 16 {
        return Err(invalid(format!(
            "reaction `{raw}` is not an emoji or one of love, like, dislike, haha, wow, sad, angry, smile"
        )));
    }
    Ok(raw.to_string())
}

/// An http(s) URL with a host, as Meta needs to fetch it.
fn check_media_url(raw: &str) -> Result<(), GraphError> {
    let url = Url::parse(raw.trim())
        .map_err(|_| invalid(format!("`{}` is not a valid URL", raw.trim())))?;
    let plain = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none();
    if plain {
        Ok(())
    } else {
        Err(invalid(format!(
            "`{}` must be a public http(s) URL without credentials",
            raw.trim()
        )))
    }
}

fn send_output(answer: &Value, page_id: &str, psid: &str) -> Value {
    let mut out = json!({
        "recipient_id": answer.get("recipient_id").and_then(id_string).unwrap_or_else(|| psid.to_string()),
        "message_id": answer.get("message_id").and_then(id_string),
        "page_id": page_id,
    });
    if let Some(attachment_id) = answer.get("attachment_id").and_then(id_string) {
        out["attachment_id"] = json!(attachment_id);
    }
    out
}

struct Upload {
    bytes: Vec<u8>,
    name: String,
    mime: String,
}

/// Messenger operations for one configuration; cheap to build per call.
pub struct Messenger {
    graph: Arc<GraphClient>,
    cfg: Arc<Config>,
    files: Option<FileJail>,
    psid_cache: &'static PsidCache,
    read_gate: &'static GateMap,
}

impl Messenger {
    pub fn new(cfg: Arc<Config>, graph: Arc<GraphClient>) -> Self {
        Self {
            files: cfg.files.clone(),
            psid_cache: PSID_CACHE.get_or_init(Default::default),
            read_gate: READ_GATE.get_or_init(Default::default),
            cfg,
            graph,
        }
    }

    fn page(&self, page_id: Option<&str>) -> Result<&PageConfig, GraphError> {
        Ok(self.cfg.page(page_id)?)
    }

    fn jail(&self) -> Result<&FileJail, GraphError> {
        self.files
            .as_ref()
            .ok_or_else(|| FileError::Disabled.into())
    }

    /// A Conversations API read, spaced per Page. A transient failure is repeated once, through
    /// the same gate, so a retry never lands inside the spacing.
    async fn read(
        &self,
        page: &PageConfig,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, GraphError> {
        let once = || async {
            throttle_in(self.read_gate, &page.id, READ_SPACING).await;
            self.graph
                .get_with_retry(path, Some(&page.token), query, false)
                .await
        };
        match once().await {
            Err(e) if retryable_read(&e) => once().await,
            other => other,
        }
    }

    async fn send(&self, page: &PageConfig, body: &Value) -> Result<Value, GraphError> {
        let path = format!("{}/messages", page.id);
        self.graph.post_json(&path, Some(&page.token), body).await
    }

    /// `send` for a message: a failure that leaves the outcome open says so.
    async fn send_message(&self, page: &PageConfig, body: &Value) -> Result<Value, GraphError> {
        self.send(page, body).await.map_err(with_delivery_caveat)
    }

    pub async fn list_conversations(
        &self,
        p: ListConversationsParams,
    ) -> Result<Value, GraphError> {
        self.list_conversations_inner(p).await.map_err(explain)
    }

    async fn list_conversations_inner(
        &self,
        p: ListConversationsParams,
    ) -> Result<Value, GraphError> {
        let page = self.page(p.page_id.as_deref())?;
        let limit = p.limit.unwrap_or(20).clamp(1, 50);
        let mut query: Vec<(&str, String)> = vec![
            ("platform", "messenger".to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(after) = clean(p.after) {
            query.push(("after", after));
        }
        if let Some(user_id) = clean(p.user_id) {
            check_id("user_id", &user_id)?;
            query.push(("user_id", user_id));
        }
        query.push(("fields", CONVERSATION_FIELDS.to_string()));
        let path = format!("{}/conversations", page.id);
        let mut reduced = false;
        let answer = match self.read(page, &path, &query).await {
            Err(e) if unknown_field_error(&e) => {
                reduced = true;
                if let Some(fields) = query.last_mut() {
                    fields.1 = CONVERSATION_FIELDS_REDUCED.to_string();
                }
                self.read(page, &path, &query).await?
            }
            other => other?,
        };
        let conversations: Vec<types::Conversation> = answer
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| types::conversation(c, &page.id))
            .collect();
        let mut out = json!({"page_id": page.id, "conversations": conversations});
        if let Some(cursor) = types::next_cursor(&answer) {
            out["next_cursor"] = json!(cursor);
        }
        if reduced {
            out["fields_reduced"] = json!(true);
        }
        Ok(out)
    }

    pub async fn read_conversation(&self, p: ReadConversationParams) -> Result<Value, GraphError> {
        self.read_conversation_inner(p).await.map_err(explain)
    }

    async fn read_conversation_inner(
        &self,
        p: ReadConversationParams,
    ) -> Result<Value, GraphError> {
        let page = self.page(p.page_id.as_deref())?;
        let conversation_id = p.conversation_id.trim().to_string();
        check_id("conversation_id", &conversation_id)?;
        let requested = p.limit.unwrap_or(DETAIL_WINDOW);
        let mut query = vec![
            ("fields", MESSAGE_FIELDS.to_string()),
            ("limit", requested.clamp(1, 100).to_string()),
        ];
        if let Some(after) = clean(p.after) {
            query.push(("after", after));
        }
        let path = format!("{conversation_id}/messages");
        let answer = self.read(page, &path, &query).await?;
        let messages: Vec<types::Message> = answer
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| types::message(m, &page.id))
            .collect();
        let thin = messages.iter().any(|m| {
            m.text.is_empty()
                && m.attachments.is_empty()
                && m.shares.is_empty()
                && !m.is_unsupported
        });
        let mut out = json!({"conversation_id": conversation_id, "messages": messages});
        if let Some(cursor) = types::next_cursor(&answer) {
            out["next_cursor"] = json!(cursor);
        }
        if requested > DETAIL_WINDOW || thin {
            out["note"] = json!(DETAIL_NOTE);
        }
        Ok(out)
    }

    pub async fn get_message(&self, p: GetMessageParams) -> Result<Value, GraphError> {
        self.get_message_inner(p).await.map_err(explain)
    }

    async fn get_message_inner(&self, p: GetMessageParams) -> Result<Value, GraphError> {
        let page = self.page(p.page_id.as_deref())?;
        let message_id = p.message_id.trim().to_string();
        check_mid("message_id", &message_id)?;
        let raw = self
            .read(page, &message_id, &[("fields", MESSAGE_FIELDS.to_string())])
            .await?;
        let message = types::message(&raw, &page.id)
            .ok_or_else(|| GraphError::Json("the message answer has no id".into()))?;
        Ok(json!({"page_id": page.id, "message": message}))
    }

    /// The PSID to send to: the given one, or the non-Page participant of the conversation.
    pub async fn resolve_recipient(
        &self,
        page: &PageConfig,
        recipient_psid: Option<String>,
        conversation_id: Option<String>,
    ) -> Result<String, GraphError> {
        if let Some(psid) = clean(recipient_psid) {
            check_id("recipient_psid", &psid)?;
            return Ok(psid);
        }
        let Some(conversation_id) = clean(conversation_id) else {
            return Err(invalid("pass recipient_psid or conversation_id"));
        };
        check_id("conversation_id", &conversation_id)?;
        let key = format!("{}/{conversation_id}", page.id);
        if let Some(psid) = cache_get(self.psid_cache, &key) {
            return Ok(psid);
        }
        let query = [("fields", "participants{id}".to_string())];
        let answer = self.read(page, &conversation_id, &query).await?;
        let psid = participants(&answer)
            .into_iter()
            .map(|p| p.id)
            .find(|id| *id != page.id)
            .ok_or_else(|| {
                invalid(format!(
                    "conversation `{conversation_id}` has no participant other than the Page"
                ))
            })?;
        cache_put(self.psid_cache, key, psid.clone());
        Ok(psid)
    }

    pub async fn send_text(&self, p: SendTextParams) -> Result<Value, GraphError> {
        self.send_text_inner(p).await.map_err(explain)
    }

    async fn send_text_inner(&self, p: SendTextParams) -> Result<Value, GraphError> {
        if p.text.trim().is_empty() {
            return Err(invalid("text must not be empty"));
        }
        let chars = p.text.chars().count();
        if chars >= TEXT_LIMIT_CHARS {
            return Err(invalid(format!(
                "text has {chars} characters: Messenger needs fewer than {TEXT_LIMIT_CHARS}, so split it into several messages"
            )));
        }
        let (messaging_type, tag) =
            messaging_fields(p.messaging_type.as_deref(), p.tag.as_deref())?;
        let notification = notification_type(p.notification_type.as_deref())?;
        let metadata = clean(p.metadata);
        if let Some(m) = &metadata
            && m.chars().count() >= METADATA_LIMIT_CHARS
        {
            return Err(invalid(format!(
                "metadata must be fewer than {METADATA_LIMIT_CHARS} characters"
            )));
        }
        let reply_to = clean(p.reply_to_mid);
        if let Some(mid) = &reply_to {
            check_mid("reply_to_mid", mid)?;
        }
        let page = self.page(p.page_id.as_deref())?;
        let psid = self
            .resolve_recipient(page, p.recipient_psid, p.conversation_id)
            .await?;
        let mut body = json!({
            "recipient": {"id": psid},
            "messaging_type": messaging_type,
            "message": {"text": p.text},
        });
        if let Some(tag) = tag {
            body["tag"] = json!(tag);
        }
        if let Some(mid) = reply_to {
            body["reply_to"] = json!({"mid": mid});
        }
        if let Some(n) = notification {
            body["notification_type"] = json!(n);
        }
        if let Some(m) = metadata {
            body["message"]["metadata"] = json!(m);
        }
        let answer = self.send_message(page, &body).await?;
        Ok(send_output(&answer, &page.id, &psid))
    }

    pub async fn send_attachment(&self, p: SendAttachmentParams) -> Result<Value, GraphError> {
        self.send_attachment_inner(p).await.map_err(explain)
    }

    async fn send_attachment_inner(&self, p: SendAttachmentParams) -> Result<Value, GraphError> {
        let kind = attachment_kind(&p.kind)?;
        let url = clean(p.url);
        let path = clean(p.path);
        let urls = p.urls.filter(|u| !u.is_empty());
        let given = [url.is_some(), path.is_some(), urls.is_some()]
            .iter()
            .filter(|set| **set)
            .count();
        if given != 1 {
            return Err(invalid("pass exactly one of url, path or urls"));
        }
        let (messaging_type, tag) =
            messaging_fields(p.messaging_type.as_deref(), p.tag.as_deref())?;
        if let Some(u) = &url {
            check_media_url(u)?;
        }
        if let Some(list) = &urls {
            if kind != "image" {
                return Err(invalid(
                    "urls sends several pictures at once, so kind must be image",
                ));
            }
            if list.len() > MAX_IMAGES {
                return Err(invalid(format!("urls takes at most {MAX_IMAGES} images")));
            }
            list.iter().try_for_each(|u| check_media_url(u))?;
        }
        let page = self.page(p.page_id.as_deref())?;
        let allow_inbound = p.allow_inbound.unwrap_or(false);
        let upload = match &path {
            Some(path) => Some(
                self.load_upload(path, kind, clean(p.mime_type), allow_inbound)
                    .await?,
            ),
            None => None,
        };
        let psid = self
            .resolve_recipient(page, p.recipient_psid, p.conversation_id)
            .await?;

        let mut warning = None;
        let mut uploaded = None;
        let message = if let Some(upload) = upload {
            let size = upload.bytes.len();
            if kind == "image" && size as u64 > media::IMAGE_SOFT_LIMIT_BYTES {
                warning = Some(
                    "the image is larger than 8 MB, which Meta may refuse; use kind file if it fails",
                );
            }
            let attachment_id = self.upload(page, kind, upload).await?;
            let message =
                json!({"attachment": {"type": kind, "payload": {"attachment_id": attachment_id}}});
            uploaded = Some((attachment_id, size));
            message
        } else if let Some(urls) = urls {
            let items: Vec<Value> = urls
                .iter()
                .map(|u| json!({"type": "image", "payload": {"url": u.trim()}}))
                .collect();
            json!({"attachments": items})
        } else {
            let reusable = p.is_reusable.unwrap_or(false);
            let url = url.unwrap_or_default();
            json!({"attachment": {"type": kind, "payload": {"url": url, "is_reusable": reusable}}})
        };
        let mut body = json!({
            "recipient": {"id": psid},
            "messaging_type": messaging_type,
            "message": message,
        });
        if let Some(tag) = tag {
            body["tag"] = json!(tag);
        }
        let answer = self.send_message(page, &body).await?;
        let mut out = send_output(&answer, &page.id, &psid);
        if let Some((attachment_id, size)) = uploaded {
            out["attachment_id"] = json!(attachment_id);
            out["uploaded_bytes"] = json!(size);
        }
        if let Some(w) = warning {
            out["warning"] = json!(w);
        }
        Ok(out)
    }

    async fn load_upload(
        &self,
        path: &str,
        kind: &'static str,
        mime_override: Option<String>,
        allow_inbound: bool,
    ) -> Result<Upload, GraphError> {
        let jail = self.jail()?.clone();
        let path = path.to_string();
        let cap = self.cfg.max_file_bytes.min(media::MAX_UPLOAD_BYTES);
        let (bytes, name) = tokio::task::spawn_blocking(move || {
            let bytes = match jail.read_upload(&path, cap, allow_inbound) {
                Err(FileError::TooLarge { size, .. }) if cap == media::MAX_UPLOAD_BYTES => {
                    return Err(GraphError::Refused(format!(
                        "`{path}` is {size} bytes: Meta accepts attachments up to 25 MB"
                    )));
                }
                other => other?,
            };
            let base = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok::<_, GraphError>((bytes, media::upload_file_name(&base)))
        })
        .await
        .map_err(|e| GraphError::transport(format!("file read task failed: {e}")))??;
        let mime = match mime_override {
            Some(m) => {
                let m = m.to_ascii_lowercase();
                if !media::valid_mime(&m) {
                    return Err(invalid(format!("mime_type `{m}` is not a valid MIME type")));
                }
                m
            }
            None => match media::mime_for_name(&name) {
                Some(m) => m.to_string(),
                None if kind == "file" => "application/octet-stream".to_string(),
                None => {
                    return Err(invalid(format!(
                        "cannot tell the MIME type of `{name}`: pass mime_type, for example image/png"
                    )));
                }
            },
        };
        if !media::mime_matches_kind(kind, &mime) {
            return Err(invalid(format!(
                "`{name}` is {mime}, which does not fit kind {kind}: use kind {}",
                media::kind_for_mime(&mime)
            )));
        }
        Ok(Upload { bytes, name, mime })
    }

    /// Uploads a file as a reusable attachment and returns its attachment_id.
    async fn upload(
        &self,
        page: &PageConfig,
        kind: &'static str,
        upload: Upload,
    ) -> Result<String, GraphError> {
        let part = reqwest::multipart::Part::bytes(upload.bytes)
            .file_name(upload.name)
            .mime_str(&upload.mime)
            .map_err(|_| invalid(format!("`{}` is not a valid MIME type", upload.mime)))?;
        let message = json!({"attachment": {"type": kind, "payload": {"is_reusable": true}}});
        let form = reqwest::multipart::Form::new()
            .text("message", message.to_string())
            .part("filedata", part);
        let path = format!("{}/message_attachments", page.id);
        let answer = self
            .graph
            .post_multipart(&path, Some(&page.token), form)
            .await
            .map_err(with_delivery_caveat)?;
        answer
            .get("attachment_id")
            .and_then(id_string)
            .ok_or_else(|| GraphError::Json("the upload answer has no attachment_id".into()))
    }

    pub async fn download_attachment(
        &self,
        p: DownloadAttachmentParams,
    ) -> Result<(Value, Option<(String, String)>), GraphError> {
        self.download_attachment_inner(p).await.map_err(explain)
    }

    async fn download_attachment_inner(
        &self,
        p: DownloadAttachmentParams,
    ) -> Result<(Value, Option<(String, String)>), GraphError> {
        let jail = self.jail()?.clone();
        let page = self.page(p.page_id.as_deref())?;
        if p.overwrite.unwrap_or(false) && self.cfg.read_only {
            return Err(GraphError::Refused(
                "overwrite=true is refused in read-only mode (META_MCP_READ_ONLY): a download may only create a new file"
                    .into(),
            ));
        }
        let (raw_url, source_name, source_mime) = match (clean(p.url), clean(p.message_id)) {
            (Some(u), None) => (u, None, None),
            (None, Some(mid)) => {
                self.attachment_source(page, &mid, p.index.unwrap_or(0))
                    .await?
            }
            _ => return Err(invalid("pass exactly one of url or message_id")),
        };
        let url =
            Url::parse(&raw_url).map_err(|_| invalid("the attachment URL is not a valid URL"))?;
        let max = self.cfg.max_file_bytes;
        let downloaded = match self.graph.download(&url, TokenMode::Never, max).await {
            Err(e) if is_auth_failure(&e) => {
                let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
                if !token_host_allowed(&host, &self.cfg.download_hosts) {
                    return Err(e);
                }
                let hosts = [token_entry(&url, host)];
                let mode = TokenMode::OnHosts {
                    token: &page.token,
                    hosts: &hosts,
                };
                self.graph.download(&url, mode, max).await?
            }
            other => other?,
        };
        let sniffed = media::sniff_image_mime(&downloaded.bytes);
        let mime = sniffed
            .map(str::to_string)
            .or_else(|| {
                downloaded
                    .mime
                    .clone()
                    .filter(|m| !m.ends_with("octet-stream"))
            })
            .or(source_mime)
            .or_else(|| media::mime_for_name(downloaded.final_url.path()).map(str::to_string))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let name = media::choose_file_name(
            p.dest_name.as_deref(),
            source_name.as_deref(),
            &downloaded.final_url,
            Some(&mime),
        );
        let overwrite = p.overwrite.unwrap_or(false);
        let derived = clean(p.dest_name).is_none();
        let dest = inbound_dest(INBOUND_PLATFORM, &name)?;
        let alt = if derived {
            Some(inbound_dest(
                INBOUND_PLATFORM,
                &media::hashed_name(&name, &url),
            )?)
        } else {
            None
        };
        let data = downloaded.bytes.clone();
        let (saved, sha256, renamed): (PathBuf, String, bool) =
            tokio::task::spawn_blocking(move || {
                let sha256 = hex::encode(Sha256::digest(&data));
                match jail.write_new(&dest, &data, overwrite, max) {
                    Ok(saved) => Ok((saved, sha256, false)),
                    Err(FileError::Exists(taken)) => match alt {
                        Some(alt) if !jail.holds(&dest, &data) => {
                            let saved = jail.write_new(&alt, &data, false, max)?;
                            Ok((saved, sha256, true))
                        }
                        _ => Err(FileError::Exists(taken)),
                    },
                    Err(e) => Err(e),
                }
            })
            .await
            .map_err(|e| GraphError::transport(format!("file write task failed: {e}")))??;
        let mut out = json!({
            "path": saved.to_string_lossy(),
            "bytes": downloaded.bytes.len(),
            "mime": mime,
            "sha256": sha256,
            "source_url_host": url.host_str(),
        });
        if renamed {
            out["name_note"] = json!(
                "a different file already had this name, so the URL hash was added to this one's name"
            );
        }
        let mut image = None;
        if p.inline.unwrap_or(true) && mime.starts_with("image/") {
            match media::inline_image_mime(&downloaded.bytes) {
                Some(shown) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(&downloaded.bytes);
                    image = Some((data, shown.to_string()));
                }
                None if downloaded.bytes.len() > media::INLINE_MAX_BYTES => {
                    out["note"] = json!(
                        "the image is larger than 2 MiB, so it is saved but not shown inline"
                    );
                }
                None => {
                    out["note"] = json!(
                        "the image is saved but not shown inline: only png, jpeg, gif and webp pictures can be shown, and these bytes are not a well-formed one"
                    );
                }
            }
        }
        Ok((out, image))
    }

    /// The URL, name and MIME type of attachment `index` of a message.
    async fn attachment_source(
        &self,
        page: &PageConfig,
        message_id: &str,
        index: i64,
    ) -> Result<(String, Option<String>, Option<String>), GraphError> {
        check_mid("message_id", message_id)?;
        let raw = self
            .read(
                page,
                message_id,
                &[("fields", "id,attachments".to_string())],
            )
            .await?;
        let message = types::message(&raw, &page.id)
            .ok_or_else(|| GraphError::Json("the message answer has no id".into()))?;
        let count = message.attachments.len();
        if count == 0 {
            return Err(invalid(format!(
                "message `{message_id}` has no attachments to download (shared links are not files)"
            )));
        }
        let picked = usize::try_from(index)
            .ok()
            .and_then(|i| message.attachments.into_iter().nth(i))
            .ok_or_else(|| {
                invalid(format!(
                    "index {index} is out of range: the message has {count} attachment(s), numbered from 0"
                ))
            })?;
        let url = picked.url.ok_or_else(|| {
            invalid(format!(
                "attachment {index} of `{message_id}` has no download URL"
            ))
        })?;
        Ok((url, picked.name, picked.mime_type))
    }

    pub async fn sender_action(&self, p: SenderActionParams) -> Result<Value, GraphError> {
        self.sender_action_inner(p).await.map_err(explain)
    }

    async fn sender_action_inner(&self, p: SenderActionParams) -> Result<Value, GraphError> {
        let action = match p.action.trim().to_ascii_lowercase().as_str() {
            "typing_on" => "typing_on",
            "typing_off" => "typing_off",
            "mark_seen" => "mark_seen",
            "react" => "react",
            "unreact" => "unreact",
            _ => {
                return Err(invalid(format!(
                    "action `{}` is not valid: use typing_on, typing_off, mark_seen, react or unreact",
                    p.action.trim()
                )));
            }
        };
        let payload = if matches!(action, "react" | "unreact") {
            let Some(message_id) = clean(p.message_id) else {
                return Err(invalid(format!("{action} needs message_id")));
            };
            check_mid("message_id", &message_id)?;
            let mut payload = json!({"message_id": message_id});
            if action == "react" {
                payload["reaction"] = json!(reaction_emoji(p.reaction.as_deref())?);
            }
            Some(payload)
        } else {
            None
        };
        let page = self.page(p.page_id.as_deref())?;
        let psid = self
            .resolve_recipient(page, p.recipient_psid, p.conversation_id)
            .await?;
        let mut body = json!({"recipient": {"id": psid}, "sender_action": action});
        if let Some(payload) = payload {
            body["payload"] = payload;
        }
        let answer = self.send(page, &body).await?;
        let recipient = answer
            .get("recipient_id")
            .and_then(id_string)
            .unwrap_or(psid);
        Ok(json!({"recipient_id": recipient, "action": action}))
    }

    pub async fn get_profile(&self, p: GetProfileParams) -> Result<Value, GraphError> {
        self.get_profile_inner(p).await.map_err(explain)
    }

    async fn get_profile_inner(&self, p: GetProfileParams) -> Result<Value, GraphError> {
        let psid = p.psid.trim().to_string();
        check_id("psid", &psid)?;
        let page = self.page(p.page_id.as_deref())?;
        let fields = [("fields", PROFILE_FIELDS.to_string())];
        let answer = match self.graph.get(&psid, Some(&page.token), &fields).await {
            Ok(v) => v,
            Err(e) if profile_unavailable(&e) => Value::Null,
            Err(e) => return Err(e),
        };
        let mut profile = serde_json::Map::new();
        for key in ["first_name", "last_name", "profile_pic"] {
            if let Some(s) = answer
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                profile.insert(key.to_string(), json!(s));
            }
        }
        let empty = profile.is_empty();
        let mut out = json!({"profile": profile});
        if empty {
            out["note"] = json!(PROFILE_NOTE);
        }
        Ok(out)
    }
}

/// The user profile feature is not granted: Graph answers with these codes or an empty object.
fn profile_unavailable(e: &GraphError) -> bool {
    matches!(e, GraphError::Api { code, subcode, .. }
        if [2_018_247, 2_018_218].iter().any(|n| *code == *n || *subcode == Some(*n)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(code: i64, subcode: Option<i64>, message: &str) -> GraphError {
        GraphError::from_body(
            &json!({"error": {"message": message, "type": "OAuthException", "code": code,
                "error_subcode": subcode}}),
            400,
        )
        .expect("api error")
    }

    #[tokio::test]
    async fn read_throttle_spaces_calls_per_page() {
        let map: GateMap = Mutex::new(HashMap::new());
        let spacing = Duration::from_millis(400);
        let start = std::time::Instant::now();
        throttle_in(&map, "page-a", spacing).await;
        assert!(start.elapsed() < spacing, "first call must not wait");
        throttle_in(&map, "page-b", spacing).await;
        assert!(start.elapsed() < spacing, "another page has its own gate");
        throttle_in(&map, "page-a", spacing).await;
        assert!(start.elapsed() >= spacing, "{:?}", start.elapsed());
        let second = std::time::Instant::now();
        throttle_in(&map, "page-a", spacing).await;
        assert!(second.elapsed() >= spacing - Duration::from_millis(5));
    }

    #[tokio::test]
    async fn concurrent_reads_on_one_page_are_serialised() {
        let map: Arc<GateMap> = Arc::new(Mutex::new(HashMap::new()));
        let spacing = Duration::from_millis(80);
        let start = std::time::Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let map = map.clone();
            tasks.push(tokio::spawn(async move {
                throttle_in(&map, "page-c", spacing).await;
            }));
        }
        for t in tasks {
            t.await.expect("join");
        }
        assert!(
            start.elapsed() >= spacing * 2 - Duration::from_millis(10),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn psid_cache_is_bounded() {
        let map: PsidCache = Mutex::new(HashMap::new());
        for i in 0..PSID_CACHE_MAX {
            cache_put(&map, format!("p/{i}"), format!("psid{i}"));
        }
        assert_eq!(map.lock().expect("lock").len(), PSID_CACHE_MAX);
        cache_put(&map, "p/new".into(), "psid-new".into());
        assert_eq!(map.lock().expect("lock").len(), PSID_CACHE_MAX);
        assert_eq!(cache_get(&map, "p/new").as_deref(), Some("psid-new"));
        cache_put(&map, "p/new".into(), "psid-newer".into());
        assert_eq!(map.lock().expect("lock").len(), PSID_CACHE_MAX);
        assert_eq!(cache_get(&map, "p/new").as_deref(), Some("psid-newer"));
        assert_eq!(cache_get(&map, "p/missing"), None);
    }

    #[test]
    fn messaging_type_and_tag_rules() {
        assert_eq!(
            messaging_fields(None, None).expect("ok"),
            ("RESPONSE", None)
        );
        assert_eq!(
            messaging_fields(Some(" update "), None).expect("ok"),
            ("UPDATE", None)
        );
        assert_eq!(
            messaging_fields(None, Some("human_agent")).expect("ok"),
            ("MESSAGE_TAG", Some("HUMAN_AGENT"))
        );
        assert_eq!(
            messaging_fields(Some("message_tag"), Some("HUMAN_AGENT")).expect("ok"),
            ("MESSAGE_TAG", Some("HUMAN_AGENT"))
        );
        for (mt, tag, needle) in [
            (Some("MESSAGE_TAG"), None, "needs tag HUMAN_AGENT"),
            (
                Some("RESPONSE"),
                Some("HUMAN_AGENT"),
                "needs messaging_type MESSAGE_TAG",
            ),
            (None, Some("ACCOUNT_UPDATE"), "only message tag"),
            (Some("PROMO"), None, "RESPONSE, UPDATE or MESSAGE_TAG"),
        ] {
            let err = messaging_fields(mt, tag).expect_err("rejected").to_string();
            assert!(err.contains(needle), "{err}");
        }
        assert_eq!(
            notification_type(Some("silent_push")).expect("ok"),
            Some("SILENT_PUSH")
        );
        assert_eq!(notification_type(Some("")).expect("ok"), None);
        assert!(notification_type(Some("LOUD")).is_err());
        assert_eq!(attachment_kind(" Video ").expect("ok"), "video");
        assert!(attachment_kind("gif").is_err());
    }

    #[test]
    fn reactions_become_emoji() {
        assert_eq!(reaction_emoji(None).expect("ok"), "\u{2764}\u{fe0f}");
        assert_eq!(reaction_emoji(Some("LIKE")).expect("ok"), "\u{1f44d}");
        assert_eq!(reaction_emoji(Some("\u{1f389}")).expect("ok"), "\u{1f389}");
        assert_eq!(
            reaction_emoji(Some("1\u{fe0f}\u{20e3}")).expect("ok"),
            "1\u{fe0f}\u{20e3}"
        );
        for bad in ["lovee", "thumbs up", &"x".repeat(40)] {
            assert!(reaction_emoji(Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn token_goes_to_meta_hosts_and_listed_hosts_only() {
        let extra = vec!["127.0.0.1".to_string(), "files.example.org".to_string()];
        for host in [
            "scontent.xx.fbcdn.net",
            "lookaside.fbsbx.com",
            "cdn.fbsbx.com",
            "SCONTENT.XX.FBCDN.NET",
            "127.0.0.1",
            "files.example.org",
        ] {
            assert!(token_host_allowed(host, &extra), "{host}");
        }
        for host in [
            "graph.facebook.com",
            "www.facebook.com",
            "facebook.com",
            "api.messenger.com",
            "mmg.whatsapp.net",
            "evil.example",
            "fbcdn.net",
            "evilfbcdn.net",
            "scontent.fbcdn.net.evil.com",
            "a.files.example.org",
            "localhost",
        ] {
            assert!(!token_host_allowed(host, &extra), "{host}");
        }
    }

    #[test]
    fn the_token_entry_names_the_host_and_the_port() {
        let url = |s: &str| Url::parse(s).expect("url");
        for (target, want) in [
            (
                "https://scontent.xx.fbcdn.net/v/a.jpg",
                "scontent.xx.fbcdn.net:443",
            ),
            (
                "https://SCONTENT.xx.fbcdn.net:8443/a",
                "scontent.xx.fbcdn.net:8443",
            ),
            ("https://cdn.fbsbx.com/a", "cdn.fbsbx.com:443"),
            ("http://127.0.0.1:9000/a", "127.0.0.1:9000"),
            ("http://[::1]:9001/a", "[::1]:9001"),
        ] {
            let u = url(target);
            let host = u.host_str().expect("host").to_ascii_lowercase();
            assert_eq!(token_entry(&u, host), want, "{target}");
        }
    }

    #[test]
    fn only_transient_conversation_read_failures_are_repeated() {
        for code in [1, 2, 4, 17, 32, 613, 80006] {
            assert!(retryable_read(&api(code, None, "busy")), "{code}");
        }
        for code in [0, 10, 100, 190, 200, 368, 130429] {
            assert!(!retryable_read(&api(code, None, "no")), "{code}");
        }
        let server = |status: u16| GraphError::Api {
            code: 100,
            subcode: None,
            message: "boom".into(),
            error_type: None,
            fbtrace_id: None,
            user_msg: None,
            hint: None,
            http_status: status,
        };
        assert!(retryable_read(&server(500)) && retryable_read(&server(503)));
        assert!(!retryable_read(&server(400)) && !retryable_read(&server(404)));
        for message in [
            "request timed out: operation timed out",
            "request could not connect: connection refused",
            "Graph API answered HTTP 502 Bad Gateway without JSON",
        ] {
            assert!(retryable_read(&GraphError::transport(message)), "{message}");
        }
        assert!(!retryable_read(&GraphError::transport(
            "request failed: bad header"
        )));
        assert!(!retryable_read(&GraphError::Refused("no".into())));
        assert!(!retryable_read(&GraphError::Config("no".into())));
        assert!(!retryable_read(&GraphError::Json("odd".into())));
    }

    #[test]
    fn a_send_with_an_open_outcome_says_it_may_have_gone_through() {
        let server =
            GraphError::from_body(&json!({"error": {"message": "unknown", "code": 2}}), 503)
                .expect("api error");
        for unsure in [
            GraphError::transport("request timed out: deadline has elapsed"),
            GraphError::Json("the response is not valid JSON (HTTP 200)".into()),
            server,
        ] {
            let shown = with_delivery_caveat(unsure).to_string();
            assert!(
                shown.contains(
                    "(the message may still have been delivered: read the conversation before sending again)"
                ),
                "{shown}"
            );
            assert_eq!(shown.matches("may still have been delivered").count(), 1);
        }
        for sure in [
            api(100, Some(2_018_278), "outside the window"),
            api(200, None, "permission"),
            GraphError::Refused("text is too long".into()),
            GraphError::Config("bad id".into()),
        ] {
            let before = sure.to_string();
            assert_eq!(with_delivery_caveat(sure).to_string(), before);
        }
    }

    #[test]
    fn download_errors_are_classified() {
        let denied = GraphError::transport("download answered HTTP 403 Forbidden");
        let unauthorized = GraphError::transport("download answered HTTP 401 Unauthorized");
        let missing = GraphError::transport("download answered HTTP 404 Not Found");
        assert!(is_auth_failure(&denied) && is_auth_failure(&unauthorized));
        assert!(!is_auth_failure(&missing));
        assert!(!is_auth_failure(&GraphError::Refused(
            "download answered HTTP 403".into()
        )));

        assert!(unknown_field_error(&api(
            100,
            None,
            "(#100) Tried accessing nonexisting field (snippet) on node type (Conversation)"
        )));
        assert!(unknown_field_error(&api(
            100,
            None,
            "(#100) TRIED ACCESSING NONEXISTING FIELD (snippet) on node type (Conversation)"
        )));
        assert!(
            !unknown_field_error(&api(100, None, "Object does not exist")),
            "a missing object is not a missing field"
        );
        assert!(!unknown_field_error(&api(
            100,
            Some(33),
            "Unsupported get request. Object with ID '1234567890' does not exist, cannot be loaded due to missing permissions, or does not support this operation"
        )));
        assert!(!unknown_field_error(&api(100, None, "Invalid parameter")));
        assert!(!unknown_field_error(&api(190, None, "does not exist")));
        assert!(!unknown_field_error(&missing));
    }

    #[test]
    fn extra_hints_are_attached_to_api_errors() {
        for (code, sub, needle) in [
            (9_000_001, None, "20 most recent"),
            (100, Some(9_000_001), "20 most recent"),
            (100, Some(2_018_047), "does not match the file"),
            (100, Some(2_018_109), "too large"),
            (100, Some(2_018_008), "could not fetch the URL"),
        ] {
            let shown = explain(api(code, sub, "boom")).to_string();
            assert!(
                shown.contains("Hint: ") && shown.contains(needle),
                "{shown}"
            );
        }
        let window = explain(api(100, Some(2_018_278), "boom")).to_string();
        assert!(window.contains("24-hour"), "{window}");
        assert!(
            window.contains("never for generated replies") && window.contains("by hand"),
            "the window hint must not offer HUMAN_AGENT for generated replies: {window}"
        );
        let other = explain(api(10, Some(2_534_022), "boom")).to_string();
        assert!(
            other.contains("24-hour") && other.contains("by hand"),
            "{other}"
        );
        let plain = explain(api(1, None, "boom")).to_string();
        assert!(!plain.contains("Hint"), "{plain}");
        let refused = explain(GraphError::Refused("nope".into())).to_string();
        assert_eq!(refused, "nope");
    }

    #[test]
    fn media_urls_and_ids_are_checked() {
        assert!(check_media_url("https://example.org/a.png").is_ok());
        assert!(check_media_url("http://example.org/a.png").is_ok());
        for bad in [
            "",
            "ftp://example.org/a",
            "file:///etc/passwd",
            "https://u:p@example.org/a",
            "data:text/plain,hi",
            "nonsense",
        ] {
            assert!(check_media_url(bad).is_err(), "{bad}");
        }
        assert!(check_mid("m", "m_AbC-9").is_ok());
        assert!(check_mid("m", "mid.$cAAAAA:1234").is_ok());
        assert!(check_mid("m", "v26").is_ok() && check_mid("m", "video.1").is_ok());
        for bad in [
            "",
            "a/b",
            "a b",
            "a?b",
            &"x".repeat(600),
            ".",
            "..",
            ".hidden",
            "$mid",
            "v1.0",
            "v26.0",
            "v9.zz",
        ] {
            assert!(check_mid("m", bad).is_err(), "{bad}");
        }
        for bad in [".", "..", "v1.0", "v26.0", "-x", "+1"] {
            assert!(check_id("i", bad).is_err(), "{bad}");
        }
        assert!(check_id("i", "t_123").is_ok() && check_id("i", "US.123").is_ok());
    }
}
