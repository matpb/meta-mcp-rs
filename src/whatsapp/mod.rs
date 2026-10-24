//! WhatsApp Cloud API operations: sends, reactions, read receipts and media transfer.

pub mod media;
pub mod types;

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::Config;
use crate::config::secret::Secret;
use crate::files::{FileError, FileJail, clean_remote_filename, sanitize_filename};
use crate::graph::download::{TokenMode, is_allowed_host};
use crate::graph::error::as_i64;
use crate::graph::{GraphClient, GraphError};
use crate::inbox::{Event, EventKind, Inbox, ListFilter, Platform, now_secs};
use media::{MediaKind, base_mime, extension_for_mime, human_size, inline_image};
use types::{
    DownloadMediaParams, MarkReadParams, MediaSource, Recipient, SendLocationParams,
    SendMediaParams, SendReactionParams, SendTemplateParams, SendTextParams, UploadMediaParams,
    check_caption, check_language, check_link, check_template_name, check_text, components_array,
    invalid, location_content, media_content, media_id as check_media_id, message_body, opt_id,
    opt_media_id, opt_wamid, present, reaction_content, read_body, scalar, sent_summary,
    template_content, text_content, uploaded_id, wamid,
};

const WINDOW_SECS: i64 = 24 * 3600;
const DEFAULT_LANGUAGE: &str = "en_US";
const UNSURE_NOTE: &str =
    "the message may still have been delivered: check inbox status events before sending again";

/// Reduces a phone number to digits (a `(0)` after the country code goes too) and refuses one
/// that starts with 0, which is national or dialled. User ids such as `US.123` pass through.
pub fn normalize_phone(raw: &str) -> Result<String, GraphError> {
    let s = raw.trim();
    if types::is_bsuid(s) {
        return Ok(s.to_string());
    }
    let chars: Vec<char> = s.chars().collect();
    let mut digits = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c @ '0'..='9' => digits.push(c),
            '+' if i == 0 => {}
            '(' if !digits.is_empty() && chars[i..].starts_with(&['(', '0', ')']) => {
                i += 3;
                continue;
            }
            '-' | '(' | ')' | '.' => {}
            c if c.is_whitespace() => {}
            _ => return Err(not_a_recipient(s)),
        }
        i += 1;
    }
    if digits.starts_with('0') {
        return Err(invalid(format!(
            "`{s}` is not a recipient: a phone number starts with its country code, never with 0 or 00 (that is a national number or a dialling prefix)"
        )));
    }
    if (6..=15).contains(&digits.len()) {
        Ok(digits)
    } else {
        Err(not_a_recipient(s))
    }
}

fn not_a_recipient(s: &str) -> GraphError {
    invalid(format!(
        "`{s}` is not a recipient: use a phone number with its country code (6 to 15 digits) or a business-scoped user id such as US.13491208655302741918"
    ))
}

/// WhatsApp-specific advice for a Graph error code.
pub fn whatsapp_hint(code: i64) -> Option<&'static str> {
    Some(match code {
        0 | 190 => {
            "WhatsApp: the access token is invalid or expired: replace META_WHATSAPP_ACCESS_TOKEN (system user tokens do not expire), or run `meta-mcp setup` again or `meta-mcp token refresh`"
        }
        3 | 10 | 200 => {
            "WhatsApp: permission denied: the token needs whatsapp_business_messaging and whatsapp_business_management, and its system user must have access to the WhatsApp account"
        }
        4 | 80007 => {
            "WhatsApp: the app or the WhatsApp Business account hit its API rate limit: wait and retry later"
        }
        100 => {
            "WhatsApp: invalid parameter: check the recipient (digits with country code, or a business-scoped user id), the message fields and the ids"
        }
        130429 => "WhatsApp: throughput limit hit: slow down and retry in a few seconds",
        131056 => {
            "WhatsApp: too many messages to this same person too fast (about one per 6 seconds): wait and retry"
        }
        131047 => {
            "WhatsApp: more than 24 hours since this person last wrote to you, so free-form messages are blocked: send an approved template with whatsapp_send_template"
        }
        131026 => {
            "WhatsApp: the message could not be delivered: the number is not on WhatsApp, the person has not accepted the latest terms, or their app is too old"
        }
        131030 => {
            "WhatsApp: the recipient is not in the allowed list: a test number can only message the verified recipients added in the App Dashboard"
        }
        131049 => {
            "WhatsApp: Meta blocked this message to keep conversations healthy: wait about 24 hours before retrying"
        }
        131050 => {
            "WhatsApp: this person opted out of marketing messages from the business: do not send them marketing templates"
        }
        130403 => "WhatsApp: the business blocked this person: unblock them before sending",
        131053 => {
            "WhatsApp: the media upload failed, usually because the MIME type does not match the file: check the extension and mime_type"
        }
        131052 => "WhatsApp: the media this person sent is too big to download",
        131009 => {
            "WhatsApp: invalid parameter value: check that message_id is a real wamid and that a reaction target is less than 30 days old"
        }
        131021 => "WhatsApp: the recipient is the same number as the sender",
        131042 => {
            "WhatsApp: a payment problem on the WhatsApp Business account blocks sending: fix the payment method in Meta Business Manager"
        }
        132000 => {
            "WhatsApp: the template parameter count does not match: pass one parameter per placeholder in components"
        }
        132001 => {
            "WhatsApp: the template does not exist in this language or is not approved: check the name and language in WhatsApp Manager"
        }
        132012 => {
            "WhatsApp: a template parameter has the wrong format: check components against the approved template"
        }
        132015 => {
            "WhatsApp: the template is paused for low quality: fix it in WhatsApp Manager or use another one"
        }
        132016 => "WhatsApp: the template is disabled after repeated low quality: create a new one",
        133010 => {
            "WhatsApp: the phone number is not registered on the Cloud API: register it in WhatsApp Manager first"
        }
        _ => return None,
    })
}

/// Swaps in the WhatsApp-specific hint on API errors. Code 0 with no Meta error type or trace id
/// is the client's own note for a bare HTTP failure, which says nothing about the token.
fn enrich(mut e: GraphError) -> GraphError {
    if let GraphError::Api {
        code,
        error_type,
        fbtrace_id,
        hint,
        ..
    } = &mut e
    {
        let bare_http = *code == 0 && error_type.is_none() && fbtrace_id.is_none();
        if !bare_http && let Some(h) = whatsapp_hint(*code) {
            *hint = Some(h);
        }
    }
    e
}

/// Appends a parenthesised note to the error text.
fn with_note(mut e: GraphError, note: &str) -> GraphError {
    match &mut e {
        GraphError::Transport(m)
        | GraphError::Config(m)
        | GraphError::Refused(m)
        | GraphError::Json(m)
        | GraphError::Api { message: m, .. } => m.push_str(&format!(" ({note})")),
    }
    e
}

/// True when a failed send may still have reached the person: the request left, so a dropped
/// connection, a server error or an unreadable answer proves nothing about delivery.
fn outcome_unknown(e: &GraphError) -> bool {
    match e {
        GraphError::Api { http_status, .. } => *http_status >= 500,
        GraphError::Transport(_) | GraphError::Json(_) => true,
        GraphError::Config(_) | GraphError::Refused(_) => false,
    }
}

/// Adds what the agent needs after a failed send: the delivery caveat when the outcome is
/// unknown, and the media id of a file uploaded for this send.
fn explain_failed_send(e: GraphError, uploaded: Option<&str>) -> GraphError {
    let unknown = outcome_unknown(&e);
    let needs_caveat = match &e {
        GraphError::Json(m) => !m.contains("may still have been"),
        _ => unknown,
    };
    let e = if needs_caveat {
        with_note(e, UNSURE_NOTE)
    } else {
        e
    };
    match uploaded {
        Some(id) if unknown => with_note(
            e,
            &format!("the uploaded file stays available as media_id {id} for 30 days"),
        ),
        Some(id) => with_note(
            e,
            &format!(
                "the file was uploaded as media_id {id}, valid for 30 days: resend it with media_id"
            ),
        ),
        None => e,
    }
}

enum Window {
    Open,
    Stale { hours: i64 },
    NoInbound,
}

/// The ids a person's inbound messages may carry in the inbox.
struct Senders(Vec<String>);

impl From<&Recipient> for Senders {
    fn from(to: &Recipient) -> Self {
        Self(vec![to.as_str().to_string()])
    }
}

/// True when the contact Meta echoed names a different phone number or user id than `to`.
fn names_someone_else(to: &Recipient, echoed: Option<&str>) -> bool {
    let Some(echoed) = echoed.map(str::trim).filter(|e| !e.is_empty()) else {
        return false;
    };
    match to {
        Recipient::Phone(digits) => {
            let echoed_digits: String = echoed.chars().filter(char::is_ascii_digit).collect();
            !echoed_digits.is_empty() && echoed_digits != *digits
        }
        Recipient::Bsuid(id) => echoed != id,
    }
}

impl Senders {
    /// The recipient plus the `wa_id` and `user_id` Meta answered with, which can differ from the
    /// number as dialled. Ids of a contact that echoes another recipient are left out.
    fn with_answer(to: &Recipient, answer: &Value) -> Self {
        let mut ids = vec![to.as_str().to_string()];
        let echoed = answer.get("input").and_then(Value::as_str);
        if !names_someone_else(to, echoed) {
            for key in ["wa_id", "user_id"] {
                if let Some(id) = scalar(answer.get(key))
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
        }
        Self(ids)
    }

    fn has(&self, id: &str) -> bool {
        self.0.iter().any(|s| s == id)
    }
}

fn is_from(event: &Event, who: &Senders) -> bool {
    who.has(&event.sender)
        || ["_sender_user_id", "from_parent_user_id"]
            .iter()
            .any(|key| {
                event
                    .raw
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|id| who.has(id))
            })
}

fn assess_window(events: &[Event], who: impl Into<Senders>, now: i64) -> Window {
    let who = who.into();
    let newest = events
        .iter()
        .filter(|e| is_from(e, &who))
        .map(|e| e.ts)
        .max();
    match newest.map(|ts| now.saturating_sub(ts)) {
        None => Window::NoInbound,
        Some(age) if age > WINDOW_SECS => Window::Stale { hours: age / 3600 },
        Some(_) => Window::Open,
    }
}

struct Line<'a> {
    token: &'a Secret,
    id: &'a str,
}

struct Prepared {
    bytes: Vec<u8>,
    mime: String,
    kind: MediaKind,
    file_name: String,
}

struct Uploaded {
    id: String,
    bytes: usize,
    mime: String,
    kind: MediaKind,
}

#[derive(Debug)]
enum Source<'a> {
    Id(&'a str),
    Link(String),
    Path(&'a str),
}

fn pick_source(p: &SendMediaParams) -> Result<Source<'_>, GraphError> {
    let id = opt_media_id("media_id", &p.media_id)?;
    match (id, present(&p.url), present(&p.path)) {
        (Some(id), None, None) => Ok(Source::Id(id)),
        (None, Some(url), None) => Ok(Source::Link(check_link(url)?)),
        (None, None, Some(path)) => Ok(Source::Path(path)),
        _ => Err(invalid("pass exactly one of media_id, url or path")),
    }
}

fn check_media_options(kind: MediaKind, p: &SendMediaParams) -> Result<(), GraphError> {
    if let Some(caption) = present(&p.caption) {
        if !kind.allows_caption() {
            return Err(invalid(format!("{kind} messages cannot have a caption")));
        }
        check_caption(caption)?;
    }
    if present(&p.filename).is_some() && kind != MediaKind::Document {
        return Err(invalid("filename applies to documents only"));
    }
    if p.voice == Some(true) && kind != MediaKind::Audio {
        return Err(invalid("voice applies to audio only"));
    }
    Ok(())
}

fn parse_kind(raw: &str) -> Result<MediaKind, GraphError> {
    MediaKind::parse(raw).ok_or_else(|| {
        invalid(format!(
            "unknown kind `{raw}`: use image, video, audio, document or sticker"
        ))
    })
}

fn file_name_of(path: &str) -> String {
    let base = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    clean_remote_filename(&base)
}

fn read_for_upload(
    jail: &FileJail,
    path: &str,
    name: &str,
    kind: MediaKind,
    global_cap: u64,
) -> Result<Vec<u8>, GraphError> {
    let resolved = jail.resolve_read(path)?;
    let size = std::fs::metadata(&resolved)
        .map_err(|e| GraphError::Refused(format!("cannot read `{path}`: {e}")))?
        .len();
    let too_big = |limit: u64, size: u64| {
        invalid(format!(
            "`{name}` is {size} bytes; WhatsApp accepts at most {} for {kind} files",
            human_size(limit)
        ))
    };
    if size == 0 {
        return Err(invalid(format!("`{name}` is empty")));
    }
    if size > kind.max_bytes() {
        return Err(too_big(kind.max_bytes(), size));
    }
    let bytes = jail.read_capped(path, kind.max_bytes().min(global_cap))?;
    let limit = media::limit_for(kind, &bytes);
    if bytes.len() as u64 > limit {
        return Err(too_big(limit, bytes.len() as u64));
    }
    Ok(bytes)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, GraphError> + Send + 'static,
) -> Result<T, GraphError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| GraphError::Refused(format!("file task failed: {e}")))?
}

/// Fails early when the destination is outside the jail, too long or already taken. Creates
/// nothing: directories appear when the file is written.
async fn check_dest(jail: &FileJail, dest: &str, overwrite: bool) -> Result<(), GraphError> {
    let (jail, dest) = (jail.clone(), dest.to_string());
    blocking(move || Ok(jail.check_write(&dest, overwrite)?)).await
}

/// The `host:port` entries allowed to receive the access token for this media URL: Meta's
/// `lookaside.fbsbx.com`, a `*.whatsapp.net` host, or an operator host. Empty for any other host.
fn token_hosts(url: &Url, operator_hosts: &[String]) -> Vec<String> {
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Vec::new();
    };
    let meta = host == "lookaside.fbsbx.com"
        || host
            .strip_suffix(".whatsapp.net")
            .is_some_and(|label| !label.is_empty());
    if !meta && !operator_hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        return Vec::new();
    }
    match url.port_or_known_default() {
        Some(port) => vec![format!("{host}:{port}")],
        None => vec![host],
    }
}

fn media_url(info: &Value) -> Result<Url, GraphError> {
    let raw = info.get("url").and_then(Value::as_str).ok_or_else(|| {
        GraphError::Json(
            "the media answer has no url: the id may have expired (ids from webhooks last 7 days)"
                .into(),
        )
    })?;
    Url::parse(raw).map_err(|_| GraphError::Json("the media answer carries an invalid url".into()))
}

/// Compares the reported SHA-256 (hex) with the bytes; returns the computed digest and whether it was checked.
fn verify_sha256(reported: Option<&str>, bytes: &[u8]) -> Result<(String, bool), GraphError> {
    let computed = hex::encode(Sha256::digest(bytes));
    let expected = reported
        .map(str::trim)
        .filter(|r| r.len() == 64 && r.bytes().all(|b| b.is_ascii_hexdigit()));
    match expected {
        Some(want) if want.eq_ignore_ascii_case(&computed) => Ok((computed, true)),
        Some(want) => Err(GraphError::Refused(format!(
            "the download does not match the SHA-256 Meta reported ({}): nothing was saved",
            want.to_ascii_lowercase()
        ))),
        None => Ok((computed, false)),
    }
}

/// WhatsApp Cloud API operations for the configured business numbers.
pub struct WhatsApp {
    graph: Arc<GraphClient>,
    cfg: Arc<Config>,
    inbox: Inbox,
}

impl WhatsApp {
    pub fn new(cfg: Arc<Config>, graph: Arc<GraphClient>, inbox: Inbox) -> Self {
        Self { graph, cfg, inbox }
    }

    fn line(&self, phone_number_id: Option<&str>) -> Result<Line<'_>, GraphError> {
        let (wa, number) = self.cfg.phone(phone_number_id)?;
        Ok(Line {
            token: &wa.token,
            id: &number.id,
        })
    }

    fn jail(&self) -> Result<&FileJail, GraphError> {
        Ok(self.cfg.files.as_ref().ok_or(FileError::Disabled)?)
    }

    async fn post_message(&self, line: &Line<'_>, body: &Value) -> Result<Value, GraphError> {
        let path = format!("{}/messages", line.id);
        self.graph
            .post_json(&path, Some(line.token), body)
            .await
            .map_err(enrich)
    }

    /// Sends `body`; `uploaded` is the media id of a file uploaded for this very send.
    async fn deliver(
        &self,
        line: &Line<'_>,
        to: &Recipient,
        input: &str,
        body: &Value,
        check_window: bool,
        uploaded: Option<&str>,
    ) -> Result<Value, GraphError> {
        let answer = self
            .post_message(line, body)
            .await
            .map_err(|e| explain_failed_send(e, uploaded))?;
        let mut out = sent_summary(&answer, input).map_err(|e| explain_failed_send(e, uploaded))?;
        out["phone_number_id"] = json!(line.id);
        if check_window {
            let who = Senders::with_answer(to, &out);
            if let Some(warning) = self.window_warning_by(line.id, who).await {
                out["window_warning"] = json!(warning);
            }
        }
        Ok(out)
    }

    /// Like `window_warning_for`, for a phone number or business-scoped user id as text.
    pub async fn window_warning(&self, phone_number_id: &str, to: &str) -> Option<String> {
        let to = Recipient::parse(to).ok()?;
        self.window_warning_for(phone_number_id, &to).await
    }

    /// A warning when the local inbox suggests the 24-hour customer service window is closed.
    pub async fn window_warning_for(
        &self,
        phone_number_id: &str,
        to: &Recipient,
    ) -> Option<String> {
        self.window_warning_by(phone_number_id, Senders::from(to))
            .await
    }

    /// Looks up the newest message from any of the person's ids, wherever it sits in the inbox.
    async fn window_warning_by(&self, phone_number_id: &str, who: Senders) -> Option<String> {
        let filter = ListFilter {
            platform: Some(Platform::WhatsApp),
            kinds: vec![EventKind::Message],
            account_id: Some(phone_number_id.to_string()),
            limit: Some(1),
            ..ListFilter::default()
        };
        let newest = self
            .inbox
            .list_from_async(filter, who.0.clone())
            .await
            .ok()?;
        match assess_window(&newest, who, now_secs()) {
            Window::Open => None,
            Window::Stale { hours } => Some(format!(
                "last inbound message from this user was {hours} hours ago: a free-form message will fail with 131047, send a template"
            )),
            Window::NoInbound => {
                let stats = self.inbox.stats_async().await.ok()?;
                let webhook_feeds_inbox = stats
                    .by_platform
                    .iter()
                    .any(|(platform, n)| platform == "whatsapp" && *n > 0);
                webhook_feeds_inbox.then(|| "no inbound message from this user in the local inbox: outside a 24-hour window only templates deliver".into())
            }
        }
    }

    pub async fn send_text(&self, p: SendTextParams) -> Result<Value, GraphError> {
        let to = Recipient::parse(&p.to)?;
        check_text(&p.body)?;
        let reply = opt_wamid("reply_to_message_id", &p.reply_to_message_id)?;
        let account = opt_id("messaging_account_id", &p.messaging_account_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let content = text_content(&p.body, p.preview_url);
        let body = message_body(&to, "text", content, reply, account);
        self.deliver(&line, &to, p.to.trim(), &body, true, None)
            .await
    }

    pub async fn send_media(&self, p: SendMediaParams) -> Result<Value, GraphError> {
        let to = Recipient::parse(&p.to)?;
        let kind = parse_kind(&p.kind)?;
        let source = pick_source(&p)?;
        check_media_options(kind, &p)?;
        let reply = opt_wamid("reply_to_message_id", &p.reply_to_message_id)?;
        let account = opt_id("messaging_account_id", &p.messaging_account_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let mut filename = present(&p.filename).map(str::to_string);
        let mut media_id: Option<String> = None;
        let mut uploaded_bytes: Option<usize> = None;
        let media_source = match source {
            Source::Id(id) => {
                media_id = Some(id.to_string());
                MediaSource::Id(id.to_string())
            }
            Source::Link(link) => MediaSource::Link(link),
            Source::Path(path) => {
                let prepared = self
                    .prepare_upload(path, Some(kind), present(&p.mime_type))
                    .await?;
                if p.voice == Some(true) && base_mime(&prepared.mime) != "audio/ogg" {
                    return Err(invalid(
                        "voice notes must be audio/ogg with the Opus codec; send other audio without voice",
                    ));
                }
                if kind == MediaKind::Document && filename.is_none() {
                    filename = Some(prepared.file_name.clone());
                }
                let uploaded = self.upload_prepared(&line, prepared).await?;
                media_id = Some(uploaded.id.clone());
                uploaded_bytes = Some(uploaded.bytes);
                MediaSource::Id(uploaded.id)
            }
        };
        let content = media_content(
            &media_source,
            present(&p.caption),
            filename.as_deref(),
            p.voice,
        );
        let body = message_body(&to, kind.as_str(), content, reply, account);
        let uploaded = media_id.as_deref().filter(|_| uploaded_bytes.is_some());
        let mut out = self
            .deliver(&line, &to, p.to.trim(), &body, true, uploaded)
            .await?;
        if let Some(id) = media_id {
            out["media_id"] = json!(id);
        }
        if let Some(n) = uploaded_bytes {
            out["uploaded_bytes"] = json!(n);
        }
        Ok(out)
    }

    pub async fn send_template(&self, p: SendTemplateParams) -> Result<Value, GraphError> {
        let to = Recipient::parse(&p.to)?;
        let name = check_template_name(&p.name)?;
        let language = check_language(present(&p.language).unwrap_or(DEFAULT_LANGUAGE))?;
        let components = components_array(p.components.as_ref())?;
        let account = opt_id("messaging_account_id", &p.messaging_account_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let content = template_content(name, language, components);
        let body = message_body(&to, "template", content, None, account);
        self.deliver(&line, &to, p.to.trim(), &body, false, None)
            .await
    }

    pub async fn send_reaction(&self, p: SendReactionParams) -> Result<Value, GraphError> {
        let to = Recipient::parse(&p.to)?;
        let message_id = wamid("message_id", &p.message_id)?;
        let content = reaction_content(message_id, &p.emoji)?;
        let account = opt_id("messaging_account_id", &p.messaging_account_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let body = message_body(&to, "reaction", content, None, account);
        self.deliver(&line, &to, p.to.trim(), &body, false, None)
            .await
    }

    pub async fn send_location(&self, p: SendLocationParams) -> Result<Value, GraphError> {
        let to = Recipient::parse(&p.to)?;
        let content = location_content(
            p.latitude,
            p.longitude,
            present(&p.name),
            present(&p.address),
        )?;
        let account = opt_id("messaging_account_id", &p.messaging_account_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let body = message_body(&to, "location", content, None, account);
        self.deliver(&line, &to, p.to.trim(), &body, true, None)
            .await
    }

    pub async fn mark_read(&self, p: MarkReadParams) -> Result<Value, GraphError> {
        let message_id = wamid("message_id", &p.message_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let typing = p.typing.unwrap_or(false);
        let answer = self
            .post_message(&line, &read_body(message_id, typing))
            .await?;
        Ok(json!({
            "success": answer.get("success").and_then(Value::as_bool).unwrap_or(false),
            "message_id": message_id,
            "phone_number_id": line.id,
            "typing_indicator": typing,
        }))
    }

    pub async fn upload_media(&self, p: UploadMediaParams) -> Result<Value, GraphError> {
        let kind = present(&p.kind).map(parse_kind).transpose()?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let prepared = self
            .prepare_upload(p.path.trim(), kind, present(&p.mime_type))
            .await?;
        let uploaded = self.upload_prepared(&line, prepared).await?;
        Ok(json!({
            "media_id": uploaded.id,
            "bytes": uploaded.bytes,
            "mime": uploaded.mime,
            "kind": uploaded.kind.as_str(),
            "phone_number_id": line.id,
        }))
    }

    async fn prepare_upload(
        &self,
        path: &str,
        wanted: Option<MediaKind>,
        mime_override: Option<&str>,
    ) -> Result<Prepared, GraphError> {
        if path.trim().is_empty() {
            return Err(invalid("path must not be empty"));
        }
        let jail = self.jail()?.clone();
        let file_name = file_name_of(path);
        let resolved = media::resolve(&file_name, wanted, mime_override).map_err(invalid)?;
        let (path, name, kind) = (path.to_string(), file_name.clone(), resolved.kind);
        let cap = self.cfg.max_file_bytes;
        let bytes = blocking(move || read_for_upload(&jail, &path, &name, kind, cap)).await?;
        Ok(Prepared {
            bytes,
            mime: resolved.mime,
            kind,
            file_name,
        })
    }

    async fn upload_prepared(
        &self,
        line: &Line<'_>,
        prepared: Prepared,
    ) -> Result<Uploaded, GraphError> {
        let Prepared {
            bytes,
            mime,
            kind,
            file_name,
        } = prepared;
        let size = bytes.len();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(&mime)
            .map_err(|_| invalid("mime_type is not a valid MIME type"))?;
        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .text("type", mime.clone())
            .part("file", part);
        let path = format!("{}/media", line.id);
        let answer = self
            .graph
            .post_multipart(&path, Some(line.token), form)
            .await
            .map_err(enrich)?;
        Ok(Uploaded {
            id: uploaded_id(&answer)?,
            bytes: size,
            mime,
            kind,
        })
    }

    /// Two-step download: resolve the media URL, then fetch it with the Bearer token and verify it.
    pub async fn download_media(
        &self,
        p: DownloadMediaParams,
    ) -> Result<(Value, Option<(String, String)>), GraphError> {
        let media_id = check_media_id("media_id", &p.media_id)?;
        let line = self.line(p.phone_number_id.as_deref())?;
        let jail = self.jail()?.clone();
        let overwrite = p.overwrite.unwrap_or(false);
        if overwrite && self.cfg.read_only {
            return Err(GraphError::Refused(
                "overwrite=true is refused in read-only mode (META_MCP_READ_ONLY): a download may only create a new file"
                    .into(),
            ));
        }
        let named = present(&p.dest_name).map(str::to_string);
        if let Some(dest) = &named {
            check_dest(&jail, dest, overwrite).await?;
        }
        let query = [("phone_number_id", line.id.to_string())];
        let info = self
            .graph
            .get(media_id, Some(line.token), &query)
            .await
            .map_err(enrich)?;
        let url = media_url(&info)?;
        if !is_allowed_host(&url, &self.cfg.download_hosts) {
            return Err(GraphError::Refused(format!(
                "refusing to download from `{}://{}`: only https on a Meta media host or a host in META_MCP_DOWNLOAD_HOSTS is fetched (plain http for loopback only); nothing was fetched",
                url.scheme(),
                url.host_str().unwrap_or("?")
            )));
        }
        let max = self.cfg.max_file_bytes;
        if let Some(size) = info.get("file_size").and_then(as_i64)
            && size > 0
            && size as u64 > max
        {
            return Err(GraphError::Refused(format!(
                "the media is {size} bytes, above the limit of {max} (META_MCP_MAX_FILE_BYTES)"
            )));
        }
        let reported_mime = info
            .get("mime_type")
            .and_then(Value::as_str)
            .map(base_mime)
            .filter(|m| !m.is_empty());
        let dest = match named {
            Some(dest) => dest,
            None => {
                let ext = extension_for_mime(reported_mime.as_deref().unwrap_or_default());
                let dest = format!("whatsapp_{}.{ext}", sanitize_filename(media_id));
                check_dest(&jail, &dest, overwrite).await?;
                dest
            }
        };
        let hosts = token_hosts(&url, &self.cfg.download_hosts);
        let mode = if hosts.is_empty() {
            TokenMode::Never
        } else {
            TokenMode::OnHosts {
                token: line.token,
                hosts: &hosts,
            }
        };
        let got = self.graph.download(&url, mode, max).await?;
        let (sha256, checked) =
            verify_sha256(info.get("sha256").and_then(Value::as_str), &got.bytes)?;
        let mime = reported_mime
            .or(got.mime)
            .unwrap_or_else(|| "application/octet-stream".into());
        let bytes = got.bytes;
        let size = bytes.len();
        let mut inline_skipped = None;
        let inline_image = if p.inline.unwrap_or(true) {
            match inline_image(&bytes) {
                Ok(shown) => Some((STANDARD.encode(&bytes), shown.to_string())),
                Err(why) => {
                    inline_skipped = mime.starts_with("image/").then_some(why);
                    None
                }
            }
        } else {
            None
        };
        let saved = blocking(move || Ok(jail.write_new(&dest, &bytes, overwrite, max)?)).await?;
        let mut out = json!({
            "path": saved.display().to_string(),
            "bytes": size,
            "mime": mime,
            "sha256": sha256,
            "sha256_checked": checked,
            "media_id": media_id,
        });
        if let Some(why) = inline_skipped {
            out["inline_skipped"] = json!(why);
        }
        Ok((out, inline_image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sender: &str, ts: i64, raw: Value) -> Event {
        Event {
            ts,
            sender: sender.into(),
            raw,
            ..Event::new(
                format!("whatsapp:{sender}:{ts}"),
                Platform::WhatsApp,
                EventKind::Message,
            )
        }
    }

    #[test]
    fn phone_numbers_are_reduced_to_digits() {
        for (raw, want) in [
            ("+1 (650) 555-1234", "16505551234"),
            ("16505551234", "16505551234"),
            ("1.650.555.1234", "16505551234"),
            ("  +44 7911 123456 ", "447911123456"),
            ("123456", "123456"),
            ("+123456789012345", "123456789012345"),
            ("+1\u{a0}650\t555 1234", "16505551234"),
        ] {
            assert_eq!(normalize_phone(raw).expect(raw), want, "{raw}");
        }
    }

    #[test]
    fn business_scoped_ids_pass_through_unchanged() {
        for raw in [
            "US.13491208655302741918",
            "BR.1",
            "US.ENT.11815799212886844830",
        ] {
            assert_eq!(normalize_phone(raw).expect(raw), raw);
        }
    }

    #[test]
    fn bad_recipients_are_refused() {
        for raw in [
            "",
            "   ",
            "12345",
            "1234567890123456",
            "abc",
            "+1 650 555 12x4",
            "1+650555123",
            "++16505551234",
            "US.",
            "U1.123",
            "tel:+16505551234",
        ] {
            let err = normalize_phone(raw).expect_err(raw).to_string();
            assert!(err.contains("not a recipient"), "{raw}: {err}");
        }
    }

    #[test]
    fn window_assessment_by_phone_and_business_scoped_id() {
        let now = 2_000_000_000;
        let old = event("16505551234", now - 30 * 3600, json!({}));
        let fresh = event("16505551234", now - 3600, json!({}));
        let other = event("447911123456", now - 60, json!({}));
        let by_id = event("US.1", now - 25 * 3600, json!({}));
        let phone = Recipient::Phone("16505551234".into());
        assert!(matches!(
            assess_window(std::slice::from_ref(&old), &phone, now),
            Window::Stale { hours: 30 }
        ));
        assert!(matches!(
            assess_window(&[other.clone(), fresh.clone(), old.clone()], &phone, now),
            Window::Open
        ));
        assert!(matches!(
            assess_window(&[other], &phone, now),
            Window::NoInbound
        ));
        assert!(matches!(assess_window(&[], &phone, now), Window::NoInbound));
        let bsuid = Recipient::Bsuid("US.1".into());
        assert!(matches!(
            assess_window(&[by_id], &bsuid, now),
            Window::Stale { hours: 25 }
        ));
        let phone_message_with_user_id =
            event("16505551234", now - 60, json!({"_sender_user_id": "US.1"}));
        assert!(matches!(
            assess_window(&[phone_message_with_user_id], &bsuid, now),
            Window::Open
        ));
        let edge = event("16505551234", now - WINDOW_SECS, json!({}));
        assert!(matches!(assess_window(&[edge], &phone, now), Window::Open));
    }

    #[test]
    fn a_trunk_zero_in_parentheses_is_dropped_and_national_numbers_are_refused() {
        for (raw, want) in [
            ("+44 (0) 7911 123456", "447911123456"),
            ("+44(0)7911123456", "447911123456"),
            ("+33 (0)6 12 34 56 78", "33612345678"),
            ("33 (0) 6 12 34 56 78", "33612345678"),
            ("+1 (650) 555-0123", "16505550123"),
            ("(650) 555-1234", "6505551234"),
        ] {
            assert_eq!(normalize_phone(raw).expect(raw), want, "{raw}");
        }
        for raw in [
            "06 12 34 56 78",
            "0033 6 12 34 56 78",
            "00 1 650 555 1234",
            "011 44 7911 123456",
            "+0 6 12 34 56 78",
            "(0) 7911 123456",
            "(0)6 12 34 56 78",
            "0",
        ] {
            let err = normalize_phone(raw).expect_err(raw).to_string();
            assert!(err.contains("not a recipient"), "{raw}: {err}");
        }
        let err = normalize_phone("06 12 34 56 78")
            .expect_err("national")
            .to_string();
        assert!(err.contains("country code"), "{err}");
    }

    fn bare_http_error(status: u16) -> GraphError {
        GraphError::Api {
            code: 0,
            subcode: None,
            message: format!("HTTP {status}"),
            error_type: None,
            fbtrace_id: None,
            user_msg: None,
            hint: None,
            http_status: status,
        }
    }

    #[test]
    fn a_bare_http_failure_does_not_blame_the_token() {
        let shown = enrich(bare_http_error(404)).to_string();
        assert_eq!(shown, "Graph API error 0: HTTP 404");
        let plain_string_error = json!({"error": "invalid_request", "error_description": "bad"});
        let api = GraphError::from_body(&plain_string_error, 400).expect("api error");
        assert!(!enrich(api).to_string().contains("access token"));
        let real = json!({"error": {"message": "x", "type": "OAuthException", "code": 0}});
        let shown = enrich(GraphError::from_body(&real, 401).expect("api error")).to_string();
        assert!(shown.contains("access token is invalid"), "{shown}");
        let traced = json!({"error": {"message": "x", "code": 0, "fbtrace_id": "T"}});
        let shown = enrich(GraphError::from_body(&traced, 400).expect("api error")).to_string();
        assert!(shown.contains("META_WHATSAPP_ACCESS_TOKEN"), "{shown}");
        let expired = json!({"error": {"message": "x", "type": "OAuthException", "code": 190}});
        let shown = enrich(GraphError::from_body(&expired, 401).expect("api error")).to_string();
        assert!(shown.contains("access token is invalid"), "{shown}");
    }

    #[test]
    fn a_failed_send_says_what_the_agent_needs_to_know() {
        let rejected = |code: i64| {
            GraphError::from_body(&json!({"error": {"message": "no", "code": code}}), 400)
                .expect("api error")
        };
        let server = GraphError::from_body(&json!({"error": {"message": "oops", "code": 2}}), 503)
            .expect("api error");
        for e in [
            GraphError::transport("connection closed before message completed"),
            server,
            GraphError::Json("the response is not valid JSON (HTTP 200)".into()),
        ] {
            let shown = explain_failed_send(e, None).to_string();
            assert!(shown.contains(&format!("({UNSURE_NOTE})")), "{shown}");
        }
        let shown = explain_failed_send(rejected(131047), None).to_string();
        assert!(!shown.contains("may still have been delivered"), "{shown}");
        for local in [
            GraphError::Config("bad".into()),
            GraphError::Refused("no".into()),
        ] {
            assert!(
                !explain_failed_send(local, None)
                    .to_string()
                    .contains("delivered")
            );
        }
        let own_caveat = GraphError::Json(
            "the send answer has no message id, so the message may still have been accepted: check the inbox status events before sending again".into(),
        );
        let shown = explain_failed_send(own_caveat, None).to_string();
        assert_eq!(
            shown.matches("check the inbox status events").count(),
            1,
            "{shown}"
        );
        assert!(!shown.contains(UNSURE_NOTE), "{shown}");

        let definite = explain_failed_send(rejected(131047), Some("777")).to_string();
        assert!(
            definite.contains("uploaded as media_id 777")
                && definite.contains("resend it with media_id"),
            "{definite}"
        );
        let unsure =
            explain_failed_send(GraphError::transport("connection reset"), Some("777")).to_string();
        assert!(
            unsure.contains(UNSURE_NOTE) && unsure.contains("media_id 777"),
            "{unsure}"
        );
        assert!(!unsure.contains("resend"), "{unsure}");
    }

    #[test]
    fn only_a_definite_rejection_is_safe_to_resend() {
        assert!(outcome_unknown(&GraphError::transport("reset")));
        assert!(outcome_unknown(&GraphError::Json("x".into())));
        assert!(outcome_unknown(&bare_http_error(500)));
        assert!(outcome_unknown(&bare_http_error(503)));
        assert!(!outcome_unknown(&bare_http_error(499)));
        assert!(!outcome_unknown(&bare_http_error(400)));
        assert!(!outcome_unknown(&GraphError::Refused("no".into())));
        assert!(!outcome_unknown(&GraphError::Config("no".into())));
    }

    fn host_entries(url: &str, operators: &[&str]) -> Vec<String> {
        let operators: Vec<String> = operators.iter().map(|h| (*h).to_string()).collect();
        token_hosts(&Url::parse(url).expect("url"), &operators)
    }

    #[test]
    fn the_token_goes_only_to_lookaside_whatsapp_and_operator_hosts() {
        assert_eq!(
            host_entries(
                "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
                &[]
            ),
            ["lookaside.fbsbx.com:443"]
        );
        assert_eq!(
            host_entries("https://mmg.whatsapp.net/v/t62/x", &[]),
            ["mmg.whatsapp.net:443"]
        );
        assert_eq!(
            host_entries("https://LOOKASIDE.fbsbx.com:8443/x", &[]),
            ["lookaside.fbsbx.com:8443"]
        );
        assert_eq!(
            host_entries("http://127.0.0.1:41234/blob", &["127.0.0.1"]),
            ["127.0.0.1:41234"]
        );
        assert_eq!(
            host_entries("http://[::1]:8080/blob", &["[::1]"]),
            ["[::1]:8080"]
        );
        assert_eq!(
            host_entries(
                "https://files.example.org/x",
                &["other.example.org", "FILES.example.org"]
            ),
            ["files.example.org:443"],
            "only the host of the URL, never the other operator hosts"
        );
        for url in [
            "https://www.facebook.com/x",
            "https://api.messenger.com/x",
            "https://scontent.xx.fbcdn.net/x",
            "https://cdn.fbsbx.com/x",
            "https://x.lookaside.fbsbx.com/x",
            "https://evilwhatsapp.net/x",
            "https://whatsapp.net/x",
            "https://mmg.whatsapp.net.evil.example/x",
            "https://files.example.org/x",
        ] {
            assert!(host_entries(url, &[]).is_empty(), "{url}");
        }
        assert!(host_entries("https://files.example.org/x", &["a.example.org"]).is_empty());
    }

    #[test]
    fn answer_ids_count_unless_the_contact_names_someone_else() {
        let phone = Recipient::Phone("525512345678".into());
        let ids = |to: &Recipient, answer: Value| Senders::with_answer(to, &answer).0;
        assert_eq!(
            ids(
                &phone,
                json!({"input": "525512345678", "wa_id": "5215512345678"})
            ),
            ["525512345678", "5215512345678"]
        );
        assert_eq!(
            ids(
                &phone,
                json!({"input": "+52 55 1234 5678", "wa_id": "5215512345678", "user_id": "US.9"})
            ),
            ["525512345678", "5215512345678", "US.9"]
        );
        assert_eq!(
            ids(&phone, json!({"wa_id": "525512345678"})),
            ["525512345678"],
            "no repeats"
        );
        assert_eq!(
            ids(&phone, json!({"input": "x", "wa_id": "5215512345678"})),
            ["525512345678", "5215512345678"],
            "an echo without digits says nothing"
        );
        assert_eq!(
            ids(
                &phone,
                json!({"input": "+1 (650) 555-1234", "wa_id": "16505551234"})
            ),
            ["525512345678"]
        );
        let bsuid = Recipient::Bsuid("US.1".into());
        assert_eq!(
            ids(
                &bsuid,
                json!({"input": "US.1", "wa_id": "16505551234", "user_id": "US.1"})
            ),
            ["US.1", "16505551234"]
        );
        assert_eq!(
            ids(&bsuid, json!({"input": "US.2", "wa_id": "16505551234"})),
            ["US.1"]
        );
        assert_eq!(Senders::from(&bsuid).0, ["US.1"]);
    }

    #[test]
    fn windows_follow_every_id_the_person_is_known_by() {
        let now = 2_000_000_000;
        let mexico = Senders(vec!["525512345678".into(), "5215512345678".into()]);
        let fresh = event("5215512345678", now - 60, json!({}));
        assert!(matches!(assess_window(&[fresh], mexico, now), Window::Open));
        let parent = event(
            "16505551234",
            now - 60,
            json!({"from_parent_user_id": "US.ENT.9"}),
        );
        let by_parent = Recipient::Bsuid("US.ENT.9".into());
        assert!(matches!(
            assess_window(&[parent], &by_parent, now),
            Window::Open
        ));
        let by_user_id = event(
            "US.77",
            now - 26 * 3600,
            json!({"_sender_user_id": "US.77"}),
        );
        let who = Senders(vec!["16505551234".into(), "US.77".into()]);
        assert!(matches!(
            assess_window(&[by_user_id], who, now),
            Window::Stale { hours: 26 }
        ));
        let stranger = event("4479", now - 60, json!({"_sender_user_id": 7}));
        let phone = Recipient::Phone("16505551234".into());
        assert!(matches!(
            assess_window(&[stranger], &phone, now),
            Window::NoInbound
        ));
    }

    #[test]
    fn hints_cover_the_documented_codes() {
        let cases: [(i64, &str); 25] = [
            (0, "token"),
            (190, "token"),
            (3, "whatsapp_business_messaging"),
            (10, "whatsapp_business_messaging"),
            (200, "whatsapp_business_messaging"),
            (4, "rate limit"),
            (80007, "rate limit"),
            (100, "invalid parameter"),
            (130429, "throughput"),
            (131056, "6 seconds"),
            (131047, "template"),
            (131026, "not on WhatsApp"),
            (131030, "allowed list"),
            (131049, "healthy"),
            (131050, "opted out"),
            (130403, "blocked"),
            (131053, "MIME"),
            (131052, "too big"),
            (131009, "wamid"),
            (131021, "same number"),
            (131042, "payment"),
            (132000, "parameter count"),
            (132001, "not approved"),
            (132015, "paused"),
            (133010, "not registered"),
        ];
        for (code, needle) in cases {
            let h = whatsapp_hint(code).unwrap_or_else(|| panic!("no hint for {code}"));
            assert!(
                h.starts_with("WhatsApp: ") && h.contains(needle),
                "{code}: {h}"
            );
        }
        assert!(whatsapp_hint(132012).is_some() && whatsapp_hint(132016).is_some());
        assert_eq!(whatsapp_hint(1), None);
    }

    #[test]
    fn enrich_replaces_the_shared_hint_and_leaves_other_errors() {
        let body = json!({"error": {"message": "(#131047) Re-engagement message", "type": "OAuthException", "code": 131047, "fbtrace_id": "T"}});
        let err = enrich(GraphError::from_body(&body, 400).expect("api error"));
        let shown = err.to_string();
        assert!(
            shown.contains("131047") && shown.contains("whatsapp_send_template"),
            "{shown}"
        );
        let generic = json!({"error": {"message": "Invalid parameter", "code": 100}});
        let shown = enrich(GraphError::from_body(&generic, 400).expect("api error")).to_string();
        assert!(
            shown.contains("WhatsApp: invalid parameter")
                && !shown.contains("CONFIRMED_EVENT_UPDATE"),
            "{shown}"
        );
        let unknown = json!({"error": {"message": "odd", "code": 1}});
        assert_eq!(
            enrich(GraphError::from_body(&unknown, 500).expect("api error")).to_string(),
            "Graph API error 1: odd"
        );
        assert_eq!(enrich(GraphError::Refused("no".into())).to_string(), "no");
    }

    #[test]
    fn notes_are_appended_to_every_error_kind() {
        let api = GraphError::from_body(&json!({"error": {"message": "boom", "code": 1}}), 500)
            .expect("api");
        assert_eq!(
            with_note(api, "note").to_string(),
            "Graph API error 1: boom (note)"
        );
        assert_eq!(
            with_note(GraphError::Refused("no".into()), "n").to_string(),
            "no (n)"
        );
        assert_eq!(
            with_note(GraphError::Config("cfg".into()), "n").to_string(),
            "cfg (n)"
        );
        assert!(
            with_note(GraphError::transport("down"), "n")
                .to_string()
                .ends_with("down (n)")
        );
        assert!(
            with_note(GraphError::Json("j".into()), "n")
                .to_string()
                .ends_with("j (n)")
        );
    }

    #[test]
    fn sha256_is_checked_only_against_real_hex_digests() {
        let bytes = b"hello";
        let digest = hex::encode(Sha256::digest(bytes));
        assert_eq!(
            verify_sha256(Some(&digest), bytes).expect("ok"),
            (digest.clone(), true)
        );
        assert_eq!(
            verify_sha256(Some(&digest.to_uppercase()), bytes).expect("upper"),
            (digest.clone(), true)
        );
        assert_eq!(
            verify_sha256(None, bytes).expect("none"),
            (digest.clone(), false)
        );
        for skipped in ["not hex at all", "abc123", "", &"g".repeat(64)] {
            assert_eq!(
                verify_sha256(Some(skipped), bytes).expect("skip"),
                (digest.clone(), false),
                "{skipped}"
            );
        }
        let wrong = "0".repeat(64);
        let err = verify_sha256(Some(&wrong), bytes)
            .expect_err("mismatch")
            .to_string();
        assert!(
            err.contains("nothing was saved") && err.contains(&wrong),
            "{err}"
        );
    }

    #[test]
    fn media_sources_are_exclusive() {
        let base = SendMediaParams {
            to: "16505551234".into(),
            kind: "image".into(),
            ..SendMediaParams::default()
        };
        assert!(pick_source(&base).is_err());
        let id = SendMediaParams {
            media_id: Some("777".into()),
            ..SendMediaParams::default()
        };
        assert!(matches!(pick_source(&id).expect("id"), Source::Id("777")));
        let both = SendMediaParams {
            media_id: Some("777".into()),
            url: Some("https://x.example/a.png".into()),
            ..SendMediaParams::default()
        };
        let err = pick_source(&both).expect_err("both").to_string();
        assert!(err.contains("exactly one"), "{err}");
        let blank = SendMediaParams {
            media_id: Some(" ".into()),
            path: Some("a.png".into()),
            ..SendMediaParams::default()
        };
        assert!(matches!(
            pick_source(&blank).expect("blank ignored"),
            Source::Path("a.png")
        ));
        let http = SendMediaParams {
            url: Some("http://x.example/a.png".into()),
            ..SendMediaParams::default()
        };
        assert!(pick_source(&http).is_err());
    }

    #[test]
    fn media_options_depend_on_the_kind() {
        let with = |f: fn(&mut SendMediaParams)| {
            let mut p = SendMediaParams::default();
            f(&mut p);
            p
        };
        let caption = with(|p| p.caption = Some("c".into()));
        assert!(check_media_options(MediaKind::Image, &caption).is_ok());
        assert!(check_media_options(MediaKind::Audio, &caption).is_err());
        assert!(check_media_options(MediaKind::Sticker, &caption).is_err());
        let long = with(|p| p.caption = Some("c".repeat(1025)));
        assert!(check_media_options(MediaKind::Image, &long).is_err());
        let name = with(|p| p.filename = Some("a.pdf".into()));
        assert!(check_media_options(MediaKind::Document, &name).is_ok());
        assert!(check_media_options(MediaKind::Image, &name).is_err());
        let voice = with(|p| p.voice = Some(true));
        assert!(check_media_options(MediaKind::Audio, &voice).is_ok());
        assert!(check_media_options(MediaKind::Video, &voice).is_err());
        assert!(parse_kind("gif").is_err());
    }

    #[test]
    fn file_names_come_from_the_last_path_component() {
        assert_eq!(file_name_of("photos/cat.png"), "cat.png");
        assert_eq!(
            file_name_of("/abs/dir/report final.pdf"),
            "report final.pdf"
        );
        assert_eq!(file_name_of(""), "file");
    }
}
