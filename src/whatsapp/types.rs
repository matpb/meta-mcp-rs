//! Tool arguments, recipients, request bodies and answer parsing for the WhatsApp Cloud API.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::normalize_phone;
use crate::graph::GraphError;
use crate::mcp::valid_id;

pub const MAX_TEXT_CHARS: usize = 4096;
pub const MAX_CAPTION_CHARS: usize = 1024;
const MAX_TEMPLATE_NAME: usize = 512;
const MAX_EMOJI_CHARS: usize = 64;
const MAX_WAMID_BYTES: usize = 200;
const MAX_MEDIA_ID_DIGITS: usize = 32;
/// Prefix of the inbox event ids built from WhatsApp message ids.
const INBOX_ID_PREFIX: &str = "whatsapp:";

/// Arguments of `whatsapp_send_text`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendTextParams {
    /// Recipient: a phone number with country code (spaces, dashes and a leading + are fine) or a business-scoped user id such as US.13491208655302741918.
    pub to: String,
    /// Message text, at most 4096 characters.
    pub body: String,
    /// Show a link preview for the first URL in the text (default false).
    pub preview_url: Option<bool>,
    /// WhatsApp message id (wamid...) to quote as a reply.
    pub reply_to_message_id: Option<String>,
    /// Only for tokens that reach several messaging accounts on the number; normally omit.
    pub messaging_account_id: Option<String>,
    /// WhatsApp phone number ID to send from; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_send_media`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendMediaParams {
    /// Recipient: a phone number with country code or a business-scoped user id.
    pub to: String,
    /// Media type to send: image, video, audio, document or sticker.
    #[schemars(extend("enum" = ["image", "video", "audio", "document", "sticker"]))]
    pub kind: String,
    /// ID of media already uploaded to this number (see whatsapp_upload_media). Give exactly one of media_id, url or path.
    pub media_id: Option<String>,
    /// Public https URL that Meta fetches itself (cached for 10 minutes); uploads are more reliable.
    pub url: Option<String>,
    /// File inside META_MCP_FILES_DIR: it is uploaded first, then sent by the returned id.
    pub path: Option<String>,
    /// Caption for image, video or document (at most 1024 characters).
    pub caption: Option<String>,
    /// File name shown to the recipient; documents only (defaults to the file name of path).
    pub filename: Option<String>,
    /// Send audio as a voice note; needs audio/ogg with the Opus codec.
    pub voice: Option<bool>,
    /// MIME type to send with a path upload: it must match the file extension and only adds parameters (for example audio/ogg; codecs=opus).
    pub mime_type: Option<String>,
    /// WhatsApp message id (wamid...) to quote as a reply.
    pub reply_to_message_id: Option<String>,
    /// Only for tokens that reach several messaging accounts on the number; normally omit.
    pub messaging_account_id: Option<String>,
    /// WhatsApp phone number ID to send from; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_send_template`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendTemplateParams {
    /// Recipient: a phone number with country code or a business-scoped user id.
    pub to: String,
    /// Name of an approved template, for example hello_world.
    pub name: String,
    /// Template language code (default en_US).
    pub language: Option<String>,
    /// Template components exactly as in Meta's API: a JSON array of header, body and button objects with their parameters.
    #[schemars(extend("type" = "array", "items" = {"type": "object"}))]
    pub components: Option<Value>,
    /// Only for tokens that reach several messaging accounts on the number; normally omit.
    pub messaging_account_id: Option<String>,
    /// WhatsApp phone number ID to send from; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_send_reaction`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendReactionParams {
    /// Recipient: the person who wrote the message you react to.
    pub to: String,
    /// WhatsApp message id (wamid...) to react to; it must be less than 30 days old.
    pub message_id: String,
    /// The emoji; an empty string removes your reaction.
    pub emoji: String,
    /// Only for tokens that reach several messaging accounts on the number; normally omit.
    pub messaging_account_id: Option<String>,
    /// WhatsApp phone number ID to send from; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_send_location`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendLocationParams {
    /// Recipient: a phone number with country code or a business-scoped user id.
    pub to: String,
    /// Latitude in degrees, -90 to 90.
    pub latitude: f64,
    /// Longitude in degrees, -180 to 180.
    pub longitude: f64,
    /// Name of the place.
    pub name: Option<String>,
    /// Street address shown under the name.
    pub address: Option<String>,
    /// Only for tokens that reach several messaging accounts on the number; normally omit.
    pub messaging_account_id: Option<String>,
    /// WhatsApp phone number ID to send from; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_mark_read`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct MarkReadParams {
    /// WhatsApp message id (wamid...) of an inbound message; it and earlier ones are marked read.
    pub message_id: String,
    /// Also show a typing indicator for up to 25 seconds (default false); only use it when a reply follows.
    pub typing: Option<bool>,
    /// WhatsApp phone number ID that received the message; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_upload_media`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UploadMediaParams {
    /// File inside META_MCP_FILES_DIR to upload.
    pub path: String,
    /// Media type the file will be sent as; inferred from the file extension when omitted.
    #[schemars(extend("enum" = ["image", "video", "audio", "document", "sticker", null]))]
    pub kind: Option<String>,
    /// MIME type to send with the upload: it must match the file extension and only adds parameters (for example codecs=opus).
    pub mime_type: Option<String>,
    /// WhatsApp phone number ID to upload to; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

/// Arguments of `whatsapp_download_media`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DownloadMediaParams {
    /// Media ID of an inbound message (from the inbox attachments); it expires 7 days after receipt.
    pub media_id: String,
    /// File name inside META_MCP_FILES_DIR to save to (default: whatsapp_ plus the media id and the file extension).
    pub dest_name: Option<String>,
    /// Also return real PNG, JPEG, GIF or WEBP pictures up to 2 MiB inline so you can look at them (default true); inline_skipped says why one is not shown.
    pub inline: Option<bool>,
    /// Replace the destination file when it already exists (default false).
    pub overwrite: Option<bool>,
    /// WhatsApp phone number ID that received the media; optional when only one number is configured.
    pub phone_number_id: Option<String>,
}

pub fn invalid(msg: impl Into<String>) -> GraphError {
    GraphError::Config(msg.into())
}

/// An optional text argument: blank counts as absent.
pub fn present(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// An optional Graph id argument checked for safe characters.
pub fn opt_id<'a>(label: &str, v: &'a Option<String>) -> Result<Option<&'a str>, GraphError> {
    match present(v) {
        None => Ok(None),
        Some(id) if valid_id(id) => Ok(Some(id)),
        Some(_) => Err(invalid(format!("{label} is not a valid id"))),
    }
}

/// A required Graph id argument checked for safe characters.
pub fn required_id<'a>(label: &str, v: &'a str) -> Result<&'a str, GraphError> {
    let id = v.trim();
    if valid_id(id) {
        Ok(id)
    } else {
        Err(invalid(format!("{label} is empty or not a valid id")))
    }
}

/// A WhatsApp message id for a request body: `wamid.` then printable ASCII without spaces.
/// An inbox event id is accepted too; its `whatsapp:` prefix is dropped.
pub fn wamid<'a>(label: &str, raw: &'a str) -> Result<&'a str, GraphError> {
    let trimmed = raw.trim();
    let id = trimmed.strip_prefix(INBOX_ID_PREFIX).unwrap_or(trimmed);
    let ok = id.len() > "wamid.".len()
        && id.len() <= MAX_WAMID_BYTES
        && id.starts_with("wamid.")
        && id.bytes().all(|b| b.is_ascii_graphic());
    if ok {
        Ok(id)
    } else {
        Err(invalid(format!(
            "{label} is not a valid WhatsApp message id: it starts with wamid. and has no spaces (the whatsapp: prefix of an inbox message id is fine)"
        )))
    }
}

/// An optional message id argument: blank counts as absent.
pub fn opt_wamid<'a>(label: &str, v: &'a Option<String>) -> Result<Option<&'a str>, GraphError> {
    present(v).map(|id| wamid(label, id)).transpose()
}

/// A WhatsApp media id: 1 to 32 digits.
pub fn media_id<'a>(label: &str, raw: &'a str) -> Result<&'a str, GraphError> {
    let id = raw.trim();
    if (1..=MAX_MEDIA_ID_DIGITS).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit()) {
        Ok(id)
    } else {
        Err(invalid(format!(
            "{label} is not a valid id: a WhatsApp media id is 1 to {MAX_MEDIA_ID_DIGITS} digits"
        )))
    }
}

/// An optional media id argument: blank counts as absent.
pub fn opt_media_id<'a>(label: &str, v: &'a Option<String>) -> Result<Option<&'a str>, GraphError> {
    present(v).map(|id| media_id(label, id)).transpose()
}

/// True for business-scoped user ids: two letters, a dot, then letters, digits and dots.
pub fn is_bsuid(s: &str) -> bool {
    let b = s.as_bytes();
    (4..=131).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b[1].is_ascii_alphabetic()
        && b[2] == b'.'
        && b[3..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'.')
}

/// Who a message goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// Digits only, country code first.
    Phone(String),
    /// A business-scoped user id such as US.13491208655302741918.
    Bsuid(String),
}

impl Recipient {
    pub fn parse(raw: &str) -> Result<Self, GraphError> {
        let normalized = normalize_phone(raw)?;
        Ok(if is_bsuid(&normalized) {
            Self::Bsuid(normalized)
        } else {
            Self::Phone(normalized)
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Phone(s) | Self::Bsuid(s) => s,
        }
    }
}

/// The common envelope of a message send with `content` under the `kind` key.
pub fn message_body(
    to: &Recipient,
    kind: &str,
    content: Value,
    reply_to: Option<&str>,
    account: Option<&str>,
) -> Value {
    let mut body = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "type": kind,
    });
    match to {
        Recipient::Phone(digits) => body["to"] = json!(digits),
        Recipient::Bsuid(id) => body["recipient"] = json!(id),
    }
    body[kind] = content;
    if let Some(id) = reply_to {
        body["context"] = json!({"message_id": id});
    }
    if let Some(id) = account {
        body["messaging_account_id"] = json!(id);
    }
    body
}

pub fn check_text(body: &str) -> Result<(), GraphError> {
    if body.trim().is_empty() {
        return Err(invalid("body must not be empty"));
    }
    let n = body.chars().count();
    if n > MAX_TEXT_CHARS {
        return Err(invalid(format!(
            "body is {n} characters; a WhatsApp text message holds at most {MAX_TEXT_CHARS}"
        )));
    }
    Ok(())
}

pub fn check_caption(caption: &str) -> Result<(), GraphError> {
    let n = caption.chars().count();
    if n > MAX_CAPTION_CHARS {
        return Err(invalid(format!(
            "caption is {n} characters; WhatsApp captions hold at most {MAX_CAPTION_CHARS}"
        )));
    }
    Ok(())
}

pub fn text_content(body: &str, preview_url: Option<bool>) -> Value {
    let mut text = json!({"body": body});
    if let Some(p) = preview_url {
        text["preview_url"] = json!(p);
    }
    text
}

/// Where a media message gets its file from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    Id(String),
    Link(String),
}

pub fn media_content(
    source: &MediaSource,
    caption: Option<&str>,
    filename: Option<&str>,
    voice: Option<bool>,
) -> Value {
    let mut media = match source {
        MediaSource::Id(id) => json!({"id": id}),
        MediaSource::Link(link) => json!({"link": link}),
    };
    if let Some(c) = caption {
        media["caption"] = json!(c);
    }
    if let Some(f) = filename {
        media["filename"] = json!(f);
    }
    if voice == Some(true) {
        media["voice"] = json!(true);
    }
    media
}

/// A public https link without credentials.
pub fn check_link(raw: &str) -> Result<String, GraphError> {
    let link = raw.trim();
    let parsed = url::Url::parse(link).map_err(|_| invalid("url is not a valid URL"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(invalid(
            "url must be a public https URL without credentials: Meta fetches it itself",
        ));
    }
    Ok(link.to_string())
}

pub fn check_template_name(name: &str) -> Result<&str, GraphError> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= MAX_TEMPLATE_NAME
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(name)
    } else {
        Err(invalid(
            "name must be a template name: letters, digits and underscores, for example hello_world",
        ))
    }
}

pub fn check_language(code: &str) -> Result<&str, GraphError> {
    let code = code.trim();
    let ok = !code.is_empty()
        && code.len() <= 16
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(code)
    } else {
        Err(invalid(
            "language must be a template language code such as en_US or pt_BR",
        ))
    }
}

/// Components as a JSON array; a string holding a JSON array is accepted too.
pub fn components_array(raw: Option<&Value>) -> Result<Option<Value>, GraphError> {
    let parsed = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(None),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .map_err(|_| invalid("components must be a JSON array of component objects"))?,
        Some(other) => other.clone(),
    };
    if parsed.is_array() {
        Ok(Some(parsed))
    } else {
        Err(invalid(
            "components must be a JSON array of component objects",
        ))
    }
}

pub fn template_content(name: &str, language: &str, components: Option<Value>) -> Value {
    let mut template = json!({"name": name, "language": {"code": language}});
    if let Some(c) = components {
        template["components"] = c;
    }
    template
}

/// The scalars of the emoji blocks; generous on purpose, so a real emoji is never turned away.
fn is_pictograph(c: char) -> bool {
    matches!(u32::from(c),
        0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x24C2 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x25A0..=0x25FF | 0x2600..=0x27BF
        | 0x2900..=0x297F | 0x2B00..=0x2BFF
        | 0x1F000..=0x1F0FF | 0x1F170..=0x1F171 | 0x1F17E..=0x1F17F | 0x1F18E | 0x1F191..=0x1F19A
        | 0x1F1E6..=0x1F1FF | 0x1F201..=0x1F202 | 0x1F21A | 0x1F22F | 0x1F232..=0x1F23A
        | 0x1F250..=0x1F251 | 0x1F300..=0x1FAFF)
}

/// True when every char is an emoji scalar, a joiner, a variation selector, a skin tone, a tag or
/// part of a keycap sequence (digit, `#` or `*`, optional variation selector, then U+20E3).
fn is_emoji_text(s: &str) -> bool {
    let mut chars = s.chars().peekable();
    let mut has_base = false;
    while let Some(c) = chars.next() {
        match c {
            '0'..='9' | '#' | '*' => {
                if chars.peek() == Some(&'\u{FE0F}') {
                    chars.next();
                }
                if chars.next() != Some('\u{20E3}') {
                    return false;
                }
            }
            '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{E0020}'..='\u{E007F}' => {
                if !has_base {
                    return false;
                }
            }
            c if is_pictograph(c) => {}
            _ => return false,
        }
        has_base = true;
    }
    has_base
}

/// A reaction body: an emoji, or the empty string that removes the reaction. Text and
/// whitespace-only values are refused rather than sent as a removal.
pub fn reaction_content(message_id: &str, emoji: &str) -> Result<Value, GraphError> {
    if emoji.is_empty() {
        return Ok(json!({"message_id": message_id, "emoji": ""}));
    }
    let shown = emoji.trim();
    if shown.chars().count() > MAX_EMOJI_CHARS || !is_emoji_text(shown) {
        return Err(invalid(
            "emoji must be a single emoji (no words or spaces), or an empty string to remove your reaction",
        ));
    }
    Ok(json!({"message_id": message_id, "emoji": shown}))
}

pub fn location_content(
    latitude: f64,
    longitude: f64,
    name: Option<&str>,
    address: Option<&str>,
) -> Result<Value, GraphError> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(invalid("latitude must be between -90 and 90"));
    }
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(invalid("longitude must be between -180 and 180"));
    }
    let mut location = json!({
        "latitude": format!("{latitude:.6}"),
        "longitude": format!("{longitude:.6}"),
    });
    if let Some(n) = name {
        location["name"] = json!(n);
    }
    if let Some(a) = address {
        location["address"] = json!(a);
    }
    Ok(location)
}

/// The status update that marks a message read, optionally with the typing indicator.
pub fn read_body(message_id: &str, typing: bool) -> Value {
    let mut body = json!({
        "messaging_product": "whatsapp",
        "status": "read",
        "message_id": message_id,
    });
    if typing {
        body["typing_indicator"] = json!({"type": "text"});
    }
    body
}

/// A string or number as text; empty strings count as absent.
pub fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The tool-facing summary of a send answer.
pub fn sent_summary(answer: &Value, input: &str) -> Result<Value, GraphError> {
    let message = answer.pointer("/messages/0");
    let id = scalar(message.and_then(|m| m.get("id"))).ok_or_else(|| {
        GraphError::Json(
            "the send answer has no message id, so the message may still have been accepted: check the inbox status events before sending again".into(),
        )
    })?;
    let contact = answer.pointer("/contacts/0");
    let field = |obj: Option<&Value>, key: &str| scalar(obj.and_then(|o| o.get(key)));
    let mut out = json!({
        "message_id": id,
        "input": field(contact, "input").unwrap_or_else(|| input.to_string()),
    });
    for (key, value) in [
        ("wa_id", field(contact, "wa_id")),
        ("user_id", field(contact, "user_id")),
        ("message_status", field(message, "message_status")),
    ] {
        if let Some(v) = value {
            out[key] = json!(v);
        }
    }
    Ok(out)
}

/// The media id in an upload answer.
pub fn uploaded_id(answer: &Value) -> Result<String, GraphError> {
    scalar(answer.get("id"))
        .ok_or_else(|| GraphError::Json("the upload answer has no media id".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(digits: &str) -> Recipient {
        Recipient::Phone(digits.into())
    }

    #[test]
    fn envelope_for_phone_and_business_scoped_recipients() {
        let body = message_body(
            &phone("16505551234"),
            "text",
            text_content("hi", Some(true)),
            Some("wamid.Q"),
            Some("MA1"),
        );
        assert_eq!(
            body,
            json!({
                "messaging_product": "whatsapp", "recipient_type": "individual",
                "type": "text", "to": "16505551234",
                "text": {"body": "hi", "preview_url": true},
                "context": {"message_id": "wamid.Q"}, "messaging_account_id": "MA1"
            })
        );
        let bsuid = Recipient::parse("US.13491208655302741918").expect("bsuid");
        let body = message_body(&bsuid, "text", text_content("hi", None), None, None);
        assert_eq!(body["recipient"], json!("US.13491208655302741918"));
        assert!(body.get("to").is_none() && body.get("context").is_none());
        assert!(body["text"].get("preview_url").is_none());
    }

    #[test]
    fn recipients_are_told_apart() {
        assert_eq!(
            Recipient::parse("+1 (650) 555-1234").expect("phone"),
            phone("16505551234")
        );
        assert_eq!(
            Recipient::parse("US.ENT.11815799212886844830").expect("parent bsuid"),
            Recipient::Bsuid("US.ENT.11815799212886844830".into())
        );
        for bad in [
            "",
            "abc",
            "12345",
            "1234567890123456",
            "+1+650",
            "US.",
            "U1.123",
        ] {
            assert!(Recipient::parse(bad).is_err(), "{bad}");
        }
        assert!(is_bsuid("BR.1") && !is_bsuid("B.1") && !is_bsuid("US-1"));
        assert_eq!(phone("1").as_str(), "1");
    }

    #[test]
    fn text_and_caption_limits_count_characters() {
        assert!(check_text(&"x".repeat(4096)).is_ok());
        let err = check_text(&"x".repeat(4097))
            .expect_err("too long")
            .to_string();
        assert!(err.contains("4097") && err.contains("4096"), "{err}");
        assert!(
            check_text(&"\u{1F600}".repeat(4096)).is_ok(),
            "emoji count once each"
        );
        assert!(check_text("  \n ").is_err());
        assert!(check_caption(&"c".repeat(1024)).is_ok());
        assert!(check_caption(&"c".repeat(1025)).is_err());
    }

    #[test]
    fn media_objects_carry_one_source() {
        let id = media_content(
            &MediaSource::Id("777".into()),
            Some("cap"),
            Some("a.pdf"),
            Some(true),
        );
        assert_eq!(
            id,
            json!({"id": "777", "caption": "cap", "filename": "a.pdf", "voice": true})
        );
        let link = media_content(
            &MediaSource::Link("https://x.example/a.png".into()),
            None,
            None,
            Some(false),
        );
        assert_eq!(link, json!({"link": "https://x.example/a.png"}));
    }

    #[test]
    fn links_must_be_public_https() {
        assert_eq!(
            check_link(" https://x.example/a.png ").expect("ok"),
            "https://x.example/a.png"
        );
        for bad in [
            "http://x.example/a.png",
            "ftp://x.example/a",
            "https://user:pw@x.example/a",
            "x.example/a.png",
            "",
        ] {
            assert!(check_link(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn template_arguments_are_checked() {
        assert!(check_template_name("hello_world").is_ok());
        assert!(check_template_name("Hello World").is_err());
        assert!(check_template_name("").is_err());
        assert!(check_language("en_US").is_ok() && check_language("fil").is_ok());
        assert!(check_language("en US").is_err());
        let comps = json!([{"type": "body", "parameters": [{"type": "text", "text": "Ann"}]}]);
        assert_eq!(
            components_array(Some(&comps)).expect("array"),
            Some(comps.clone())
        );
        let as_text = json!(comps.to_string());
        assert_eq!(
            components_array(Some(&as_text)).expect("text"),
            Some(comps.clone())
        );
        assert_eq!(components_array(None).expect("none"), None);
        assert_eq!(components_array(Some(&Value::Null)).expect("null"), None);
        assert!(components_array(Some(&json!({"type": "body"}))).is_err());
        assert!(components_array(Some(&json!("not json"))).is_err());
        assert_eq!(
            template_content("hello_world", "en_US", Some(comps.clone())),
            json!({"name": "hello_world", "language": {"code": "en_US"}, "components": comps})
        );
        assert!(
            template_content("hello_world", "en_US", None)
                .get("components")
                .is_none()
        );
    }

    #[test]
    fn locations_are_strings_with_six_decimals() {
        let loc = location_content(37.44, -122.16, Some("Cafe"), None).expect("ok");
        assert_eq!(
            loc,
            json!({"latitude": "37.440000", "longitude": "-122.160000", "name": "Cafe"})
        );
        assert!(location_content(91.0, 0.0, None, None).is_err());
        assert!(location_content(0.0, -180.5, None, None).is_err());
        assert!(location_content(f64::NAN, 0.0, None, None).is_err());
        assert!(location_content(0.0, f64::INFINITY, None, None).is_err());
    }

    #[test]
    fn reaction_and_read_bodies() {
        assert_eq!(
            reaction_content("wamid.X", "").expect("removal"),
            json!({"message_id": "wamid.X", "emoji": ""})
        );
        assert!(reaction_content("wamid.X", &"e".repeat(65)).is_err());
        assert_eq!(
            read_body("wamid.X", false),
            json!({"messaging_product": "whatsapp", "status": "read", "message_id": "wamid.X"})
        );
        assert_eq!(
            read_body("wamid.X", true)["typing_indicator"],
            json!({"type": "text"})
        );
    }

    #[test]
    fn message_ids_are_body_safe_wamids() {
        let longest = format!("wamid.{}", "A".repeat(194));
        let too_long = format!("wamid.{}", "A".repeat(195));
        assert_eq!((longest.len(), too_long.len()), (200, 201));
        for ok in [
            "wamid.HBgLMTY1MDM4Nzk0MzkVAgASGBQzQUZC",
            "wamid.HBg/Lab+c=",
            "wamid.A==",
            longest.as_str(),
        ] {
            assert_eq!(wamid("m", ok).expect(ok), ok);
        }
        assert_eq!(
            wamid("m", " whatsapp:wamid.HBgLabc= ").expect("inbox id"),
            "wamid.HBgLabc="
        );
        for bad in [
            "",
            " ",
            "a/b",
            "wamid.",
            "wamid.a b",
            "wamid.a\nb",
            "wamid.\u{e9}",
            "whatsapp:",
            "whatsapp:whatsapp:wamid.A",
            "whatsapp:status:wamid.X:sent",
            "status:wamid.X:sent",
            "WAMID.A",
            too_long.as_str(),
        ] {
            let err = wamid("message_id", bad).expect_err(bad).to_string();
            assert!(
                err.contains("message_id is not a valid WhatsApp message id"),
                "{bad:?}: {err}"
            );
        }
        assert_eq!(opt_wamid("m", &None).expect("none"), None);
        assert_eq!(opt_wamid("m", &Some("  ".into())).expect("blank"), None);
        assert_eq!(
            opt_wamid("m", &Some("whatsapp:wamid.Q".into())).expect("id"),
            Some("wamid.Q")
        );
        assert!(opt_wamid("m", &Some("a/b".into())).is_err());
    }

    #[test]
    fn media_ids_are_one_to_32_digits() {
        let longest = "9".repeat(32);
        for ok in ["1", "999", " 1037543291543636 ", longest.as_str()] {
            assert_eq!(media_id("media_id", ok).expect(ok), ok.trim());
        }
        for bad in [
            "",
            " ",
            "me",
            "..",
            "v26.0",
            "a/b",
            "1 2",
            "12a",
            "-1",
            "\u{661}\u{662}",
            "9".repeat(33).as_str(),
        ] {
            let err = media_id("media_id", bad).expect_err(bad).to_string();
            assert!(err.contains("media_id is not a valid id"), "{bad:?}: {err}");
        }
        assert_eq!(opt_media_id("m", &None).expect("none"), None);
        assert_eq!(opt_media_id("m", &Some(" ".into())).expect("blank"), None);
        assert!(opt_media_id("m", &Some("a/b".into())).is_err());
    }

    #[test]
    fn reactions_are_one_emoji_or_empty() {
        let emoji_of = |e: &str| reaction_content("wamid.X", e).map(|v| v["emoji"].clone());
        for ok in [
            "\u{1F44D}",
            "\u{2764}\u{FE0F}",
            "\u{1F600}",
            "\u{1F64F}",
            "\u{2B50}",
            "\u{A9}\u{FE0F}",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
            "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}",
            "\u{1F44D}\u{1F3FD}",
            "\u{1F1E8}\u{1F1E6}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            "1\u{FE0F}\u{20E3}",
            "#\u{20E3}",
            "*\u{FE0F}\u{20E3}",
        ] {
            assert_eq!(emoji_of(ok).expect(ok), json!(ok));
        }
        assert_eq!(
            emoji_of(" \u{1F44D}\n").expect("trimmed"),
            json!("\u{1F44D}")
        );
        assert_eq!(emoji_of("").expect("removal"), json!(""));
        for bad in [
            "thumbs up",
            "ok",
            "ab",
            "123",
            "1",
            "#",
            "<script>",
            "\u{1F44D} ok",
            "ok \u{1F44D}",
            " ",
            "\t",
            "\n",
            "\u{200D}",
            "\u{FE0F}",
            "\u{200D}\u{1F44D}",
            "\u{20E3}",
            "1\u{20E3}x",
            "\u{1F44D}\u{200B}",
            "\u{FF11}",
        ] {
            let err = emoji_of(bad).expect_err(bad).to_string();
            assert!(err.contains("single emoji"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn send_answers_are_summarised() {
        let answer = json!({
            "messaging_product": "whatsapp",
            "contacts": [{"input": "+1650", "wa_id": "1650", "user_id": "US.1"}],
            "messages": [{"id": "wamid.OUT", "message_status": "accepted"}]
        });
        assert_eq!(
            sent_summary(&answer, "fallback").expect("ok"),
            json!({"message_id": "wamid.OUT", "input": "+1650", "wa_id": "1650",
                   "user_id": "US.1", "message_status": "accepted"})
        );
        let bare = json!({"messages": [{"id": 42}]});
        assert_eq!(
            sent_summary(&bare, "fallback").expect("ok"),
            json!({"message_id": "42", "input": "fallback"})
        );
        for broken in [json!({}), json!({"messages": []}), Value::Null] {
            let err = sent_summary(&broken, "x").expect_err("no id").to_string();
            assert!(err.contains("check the inbox status events"), "{err}");
        }
        assert_eq!(uploaded_id(&json!({"id": "1037"})).expect("id"), "1037");
        assert!(uploaded_id(&json!({})).is_err());
    }

    #[test]
    fn blank_arguments_count_as_absent() {
        assert_eq!(present(&Some("  ".into())), None);
        assert_eq!(present(&Some(" x ".into())), Some("x"));
        assert_eq!(opt_id("a", &None).expect("none"), None);
        assert_eq!(
            opt_id("a", &Some("wamid.A=".into())).expect("id"),
            Some("wamid.A=")
        );
        assert!(opt_id("a", &Some("a/b".into())).is_err());
        assert!(required_id("a", " ").is_err());
        assert_eq!(required_id("a", " 99 ").expect("id"), "99");
    }
}
