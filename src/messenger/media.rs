//! Media helpers: MIME by extension, attachment kinds, safe file names and the inline-image check.

use sha2::{Digest, Sha256};
use url::Url;

/// Largest image returned inline as an image block.
pub const INLINE_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Largest file Meta accepts for an upload.
pub const MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;
/// Images above this size may be refused by the Send API.
pub const IMAGE_SOFT_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

const FALLBACK_NAME: &str = "attachment";
/// A derived name never exceeds what the files directory accepts for one path component.
const MAX_NAME_BYTES: usize = crate::files::MAX_NAME_BYTES;
const MAX_EXT_BYTES: usize = 16;
/// Largest width or height of a picture shown inline.
const MAX_DIMENSION: u32 = 20_000;

const MIME_BY_EXT: &[(&str, &str)] = &[
    ("3gp", "video/3gpp"),
    ("7z", "application/x-7z-compressed"),
    ("aac", "audio/aac"),
    ("amr", "audio/amr"),
    ("avi", "video/x-msvideo"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("csv", "text/csv"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("flac", "audio/flac"),
    ("gif", "image/gif"),
    ("gz", "application/gzip"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("html", "text/html"),
    ("ico", "image/x-icon"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("json", "application/json"),
    ("m4a", "audio/mp4"),
    ("m4v", "video/x-m4v"),
    ("md", "text/markdown"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("mpeg", "video/mpeg"),
    ("mpg", "video/mpeg"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("ogv", "video/ogg"),
    ("opus", "audio/opus"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("rar", "application/vnd.rar"),
    ("rtf", "application/rtf"),
    ("svg", "image/svg+xml"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("txt", "text/plain"),
    ("wav", "audio/wav"),
    ("weba", "audio/webm"),
    ("webm", "video/webm"),
    ("webp", "image/webp"),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xml", "application/xml"),
    ("zip", "application/zip"),
];

/// The extension of the last path component, lowercased, when it has a plausible one.
fn extension_of(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let (stem, ext) = last.rsplit_once('.')?;
    let plausible = !stem.is_empty()
        && !ext.is_empty()
        && ext.len() <= MAX_EXT_BYTES
        && ext.bytes().all(|b| b.is_ascii_alphanumeric());
    plausible.then(|| ext.to_ascii_lowercase())
}

/// MIME type for a file name or path by its extension, case-insensitive.
pub fn mime_for_name(name: &str) -> Option<&'static str> {
    let ext = extension_of(name)?;
    MIME_BY_EXT
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mime)| *mime)
}

/// The usual extension for a MIME type, without the dot.
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let essence = mime.split(';').next().unwrap_or_default().trim();
    Some(match essence.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/tiff" => "tiff",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/amr" => "amr",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/3gpp" => "3gp",
        "video/mpeg" => "mpg",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/json" => "json",
        "text/plain" => "txt",
        "text/csv" => "csv",
        _ => return None,
    })
}

/// The Send API attachment type that fits a MIME type.
pub fn kind_for_mime(mime: &str) -> &'static str {
    let m = mime.trim().to_ascii_lowercase();
    if m.starts_with("image/") {
        "image"
    } else if m.starts_with("audio/") {
        "audio"
    } else if m.starts_with("video/") {
        "video"
    } else {
        "file"
    }
}

/// Whether a MIME type is acceptable for a Send API attachment type; `file` takes anything.
pub fn mime_matches_kind(kind: &str, mime: &str) -> bool {
    let m = mime.trim().to_ascii_lowercase();
    match kind {
        "image" => m.starts_with("image/"),
        "audio" => m.starts_with("audio/"),
        "video" => m.starts_with("video/"),
        _ => true,
    }
}

/// True for `type/subtype` made of RFC 6838 token characters, without parameters.
pub fn valid_mime(mime: &str) -> bool {
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    mime.split_once('/')
        .is_some_and(|(t, s)| token(t) && token(s))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bidi controls, zero-width characters and line separators: invisible, so they can disguise a name.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{061C}'
            | '\u{FEFF}'
    )
}

/// Windows treats these stems as devices, with or without an extension, in any case.
fn is_windows_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "\u{b9}"
                    | "\u{b2}"
                    | "\u{b3}"
            )
        })
    })
}

/// Reduces a remote name to one safe path component: no separators, no control, invisible or
/// reserved characters, no Windows device name, at most `MAX_NAME_BYTES` bytes.
pub fn safe_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control()
                || is_invisible(c)
                || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_start_matches('.')
        .trim_end_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed.chars().all(|c| c == '_') {
        return FALLBACK_NAME.to_string();
    }
    if is_windows_device(trimmed) {
        return shorten(&format!("_{trimmed}"));
    }
    shorten(trimmed)
}

/// The longest prefix of `s` within `max` bytes that ends on a character boundary and not on a dot or space.
fn cut(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end_matches(['.', ' '])
}

/// Caps the name at `MAX_NAME_BYTES` bytes on a character boundary and keeps a short extension.
fn shorten(name: &str) -> String {
    if name.len() <= MAX_NAME_BYTES {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= MAX_EXT_BYTES => {
            format!("{}.{ext}", cut(stem, MAX_NAME_BYTES - ext.len() - 1))
        }
        _ => cut(name, MAX_NAME_BYTES).to_string(),
    }
}

/// `stem-<8 hex of the URL hash>.ext`, within `MAX_NAME_BYTES`: the name a download gets when its
/// plain name holds other content.
pub fn hashed_name(name: &str, url: &Url) -> String {
    let digest = Sha256::digest(url.as_str().as_bytes());
    let tag = format!("-{}", hex::encode(&digest[..4]));
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= MAX_EXT_BYTES => {
            (stem, format!(".{ext}"))
        }
        _ => (name, String::new()),
    };
    let room = MAX_NAME_BYTES - tag.len() - ext.len();
    format!("{}{tag}{ext}", cut(stem, room))
}

/// The last non-empty path segment of a URL, percent-decoded.
pub fn file_name_from_url(url: &Url) -> Option<String> {
    let segment = url
        .path_segments()?
        .rev()
        .find(|s| !s.is_empty())
        .map(percent_decode)?;
    (!segment.trim().is_empty()).then_some(segment)
}

/// File name for the multipart `filedata` part: keeps Unicode, drops separators, control characters and quotes.
pub fn upload_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .take(255)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Local file name for a download: dest_name, else the attachment name, else the URL segment.
/// A derived name without an extension gets one from the MIME type.
pub fn choose_file_name(
    dest_name: Option<&str>,
    attachment_name: Option<&str>,
    url: &Url,
    mime: Option<&str>,
) -> String {
    let given = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(dest) = given(dest_name) {
        return safe_file_name(&dest);
    }
    let raw = given(attachment_name).or_else(|| file_name_from_url(url));
    let mut name = raw.map_or_else(|| FALLBACK_NAME.to_string(), |r| safe_file_name(&r));
    if extension_of(&name).is_none()
        && let Some(ext) = mime.and_then(extension_for_mime)
    {
        name.push('.');
        name.push_str(ext);
        name = shorten(&name);
    }
    name
}

fn sane_size(width: u32, height: u32) -> bool {
    (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height)
}

fn be16(b: &[u8]) -> u32 {
    u32::from(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le16(b: &[u8]) -> u32 {
    u32::from(u16::from_le_bytes([b[0], b[1]]))
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Signature, then the IHDR chunk: length 13, its type and a width and height in range.
fn is_png(b: &[u8]) -> bool {
    b.len() >= 29
        && b.starts_with(b"\x89PNG\r\n\x1a\n")
        && b[8..12] == [0, 0, 0, 13]
        && &b[12..16] == b"IHDR"
        && sane_size(be32(&b[16..20]), be32(&b[20..24]))
}

/// SOI, then the segments in turn until a baseline or progressive frame header gives a size.
fn is_jpeg(b: &[u8]) -> bool {
    if !b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return false;
    }
    let mut i = 2;
    while b.get(i) == Some(&0xFF) {
        while b.get(i) == Some(&0xFF) {
            i += 1;
        }
        let Some(&marker) = b.get(i) else {
            return false;
        };
        i += 1;
        match marker {
            0xC0 | 0xC2 => {
                return b
                    .get(i..i + 7)
                    .is_some_and(|f| be16(f) >= 8 && sane_size(be16(&f[5..7]), be16(&f[3..5])));
            }
            0x00 | 0x01 | 0xD0..=0xD9 | 0xC1 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return false;
            }
            _ => {
                let Some(len) = b.get(i..i + 2).map(be16) else {
                    return false;
                };
                if marker == 0xDA || len < 2 || i + len as usize > b.len() {
                    return false;
                }
                i += len as usize;
            }
        }
    }
    false
}

/// Header and logical screen descriptor with a size in range; any data after the colour table
/// must start with a block introducer.
fn is_gif(b: &[u8]) -> bool {
    if b.len() < 13
        || !(b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"))
        || !sane_size(le16(&b[6..8]), le16(&b[8..10]))
    {
        return false;
    }
    let table = if b[10] & 0x80 != 0 {
        3usize << ((b[10] & 0x07) + 1)
    } else {
        0
    };
    b.get(13 + table)
        .is_none_or(|block| matches!(block, 0x21 | 0x2C | 0x3B))
}

/// A RIFF whose size matches the body within a byte and whose first chunk is VP8, VP8L or VP8X.
fn is_webp(b: &[u8]) -> bool {
    if b.len() < 20 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return false;
    }
    let body = b.len() as u64;
    let declared = u64::from(le32(&b[4..8])) + 8;
    let first = u64::from(le32(&b[16..20])) + 20;
    declared.abs_diff(body) <= 1
        && matches!(&b[12..16], b"VP8 " | b"VP8L" | b"VP8X")
        && first <= body + 1
}

/// The raster type of `bytes` when its structure parses as a png, jpeg, gif or webp picture.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if is_png(bytes) {
        Some("image/png")
    } else if is_jpeg(bytes) {
        Some("image/jpeg")
    } else if is_gif(bytes) {
        Some("image/gif")
    } else if is_webp(bytes) {
        Some("image/webp")
    } else {
        None
    }
}

/// The MIME type to show `bytes` inline with, or `None` unless it is a well-formed png, jpeg, gif
/// or webp picture of at most `INLINE_MAX_BYTES`. Shared by Messenger and WhatsApp downloads.
pub fn inline_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > INLINE_MAX_BYTES {
        return None;
    }
    sniff_image_mime(bytes)
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// Real 2x2 pictures written by Pillow, for the tests of this crate.
    const PNG_B64: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR42mM8ISfHwMDAxMDAwMDAAAANBAEIf62CUQAAAABJ",
        "RU5ErkJggg=="
    );
    const JPEG_B64: &str = concat!(
        "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDABALDA4MChAODQ4SERATGCgaGBYWGDEjJR0oOjM9PDkzODdASFxOQERXRTc4UG1R",
        "V19iZ2hnPk1xeXBkeFxlZ2P/2wBDARESEhgVGC8aGi9jQjhCY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2Nj",
        "Y2NjY2NjY2NjY2NjY2P/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAA",
        "AgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6",
        "Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXG",
        "x8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREA",
        "AgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5",
        "OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPE",
        "xcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwDHooorhPqD/9k="
    );
    const JPEG_PROGRESSIVE_B64: &str = concat!(
        "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDABALDA4MChAODQ4SERATGCgaGBYWGDEjJR0oOjM9PDkzODdASFxOQERXRTc4UG1R",
        "V19iZ2hnPk1xeXBkeFxlZ2P/2wBDARESEhgVGC8aGi9jQjhCY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2Nj",
        "Y2NjY2NjY2NjY2NjY2P/wgARCAACAAIDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAT/xAAVAQEBAAAAAAAAAAAA",
        "AAAAAAADBf/aAAwDAQACEAMQAAABjAVP/8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABBQJ//8QAFBEBAAAAAAAAAAAA",
        "AAAAAAAAAP/aAAgBAwEBPwF//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPwF//8QAFBABAAAAAAAAAAAAAAAAAAAA",
        "AP/aAAgBAQAGPwJ//8QAFBABAAAAAAAAAAAAAAAAAAAAAP/aAAgBAQABPyF//9oADAMBAAIAAwAAABAD/8QAFBEBAAAAAAAA",
        "AAAAAAAAAAAAAP/aAAgBAwEBPxB//8QAFBEBAAAAAAAAAAAAAAAAAAAAAP/aAAgBAgEBPxB//8QAFBABAAAAAAAAAAAAAAAA",
        "AAAAAP/aAAgBAQABPxB//9k="
    );
    const GIF_B64: &str = "R0lGODdhAgACAIEAAMgeHgAAAAAAAAAAACwAAAAAAgACAAAIBgABCAQQEAA7";
    const WEBP_LOSSY_B64: &str =
        "UklGRjYAAABXRUJQVlA4ICoAAACQAQCdASoCAAIAAsBMJaACdLoAA5gA/u2QP4hd7G2//PTP62/j/xkAAAA=";
    const WEBP_LOSSLESS_B64: &str = "UklGRh4AAABXRUJQVlA4TBEAAAAvAUAAAAdQjyLXo/+BiOh/AAA=";
    const WEBP_ALPHA_B64: &str = concat!(
        "UklGRlYAAABXRUJQVlA4WAoAAAAQAAAAAQAAAQAAQUxQSAUAAAAAgICAgABWUDggKgAAAJABAJ0BKgIAAgACwEwloAJ0ugAD",
        "mAD+7ZA/iF3sbb/89M/rb+P/GQAAAA=="
    );

    fn decode(b64: &str) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("fixture")
    }

    pub fn png() -> Vec<u8> {
        decode(PNG_B64)
    }

    pub fn jpeg() -> Vec<u8> {
        decode(JPEG_B64)
    }

    pub fn jpeg_progressive() -> Vec<u8> {
        decode(JPEG_PROGRESSIVE_B64)
    }

    pub fn gif() -> Vec<u8> {
        decode(GIF_B64)
    }

    pub fn webp_lossy() -> Vec<u8> {
        decode(WEBP_LOSSY_B64)
    }

    pub fn webp_lossless() -> Vec<u8> {
        decode(WEBP_LOSSLESS_B64)
    }

    pub fn webp_alpha() -> Vec<u8> {
        decode(WEBP_ALPHA_B64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("url")
    }

    #[test]
    fn mime_by_extension_map() {
        for (name, mime) in [
            ("photo.JPG", "image/jpeg"),
            ("photo.jpeg", "image/jpeg"),
            ("a/b/c.png", "image/png"),
            ("x.gif", "image/gif"),
            ("x.webp", "image/webp"),
            ("voice.mp3", "audio/mpeg"),
            ("voice.m4a", "audio/mp4"),
            ("voice.OGG", "audio/ogg"),
            ("clip.mp4", "video/mp4"),
            ("clip.mov", "video/quicktime"),
            ("doc.pdf", "application/pdf"),
            ("notes.txt", "text/plain"),
            ("archive.tar.gz", "application/gzip"),
            (
                "C:\\files\\sheet.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
        ] {
            assert_eq!(mime_for_name(name), Some(mime), "{name}");
        }
        for none in [
            "noext",
            ".png",
            "photo.",
            "photo.unknownext",
            "dir.png/file",
            "a.b c",
        ] {
            assert_eq!(mime_for_name(none), None, "{none}");
        }
        let mut seen = Vec::new();
        for (ext, _) in MIME_BY_EXT {
            assert!(!seen.contains(ext), "duplicate extension {ext}");
            assert_eq!(*ext, ext.to_ascii_lowercase());
            seen.push(ext);
        }
    }

    #[test]
    fn extensions_for_mime_types() {
        assert_eq!(extension_for_mime("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for_mime("IMAGE/PNG; charset=binary"), Some("png"));
        assert_eq!(extension_for_mime("audio/mpeg"), Some("mp3"));
        assert_eq!(extension_for_mime("application/octet-stream"), None);
        for (ext, mime) in MIME_BY_EXT {
            if let Some(back) = extension_for_mime(mime) {
                assert_eq!(mime_for_name(&format!("f.{back}")), Some(*mime), "{ext}");
            }
        }
    }

    #[test]
    fn kinds_and_mime_compatibility() {
        assert_eq!(kind_for_mime("image/png"), "image");
        assert_eq!(kind_for_mime("Audio/MPEG"), "audio");
        assert_eq!(kind_for_mime("video/mp4"), "video");
        assert_eq!(kind_for_mime("application/pdf"), "file");
        assert!(mime_matches_kind("image", "image/webp"));
        assert!(!mime_matches_kind("image", "application/pdf"));
        assert!(mime_matches_kind("audio", "audio/ogg"));
        assert!(!mime_matches_kind("audio", "video/mp4"));
        assert!(mime_matches_kind("file", "image/png"));
        assert!(valid_mime("image/png") && valid_mime("application/vnd.rar"));
        for bad in [
            "",
            "image",
            "image/",
            "/png",
            "a b/c",
            "image/png; q=1",
            "a/b/c",
        ] {
            assert!(!valid_mime(bad), "{bad}");
        }
    }

    #[test]
    fn file_names_are_reduced_to_one_safe_component() {
        assert_eq!(safe_file_name("photo.jpg"), "photo.jpg");
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\me\\cv.pdf"), "cv.pdf");
        assert_eq!(safe_file_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name("...."), FALLBACK_NAME);
        assert_eq!(safe_file_name(""), FALLBACK_NAME);
        assert_eq!(safe_file_name("   "), FALLBACK_NAME);
        assert_eq!(safe_file_name("???"), FALLBACK_NAME);
        assert_eq!(safe_file_name("a/b/"), FALLBACK_NAME);
        assert_eq!(
            safe_file_name("rapport \u{e9}t\u{e9}.pdf"),
            "rapport \u{e9}t\u{e9}.pdf"
        );
        assert_eq!(safe_file_name("tab\there\nname.txt"), "tab_here_name.txt");
        assert_eq!(safe_file_name("trailing. . "), "trailing");
        let long = format!("{}.jpeg", "x".repeat(300));
        let short = safe_file_name(&long);
        assert_eq!(short.len(), MAX_NAME_BYTES);
        assert!(short.ends_with(".jpeg"));
        let no_ext = safe_file_name(&"y".repeat(300));
        assert_eq!(no_ext.len(), MAX_NAME_BYTES);
    }

    #[test]
    fn invisible_characters_and_windows_devices_are_neutralised() {
        for (raw, want) in [
            ("invoice\u{202e}gpj.exe", "invoice_gpj.exe"),
            (
                "a\u{202a}b\u{202b}c\u{202c}d\u{202d}e\u{202e}f",
                "a_b_c_d_e_f",
            ),
            ("a\u{2066}b\u{2067}c\u{2068}d\u{2069}e", "a_b_c_d_e"),
            (
                "a\u{200b}b\u{200c}c\u{200d}d\u{200e}e\u{200f}f",
                "a_b_c_d_e_f",
            ),
            ("a\u{2028}b\u{2029}c\u{feff}d", "a_b_c_d"),
            ("a\u{2060}b\u{061c}c", "a_b_c"),
            ("\u{202e}\u{200b}", FALLBACK_NAME),
            ("CON", "_CON"),
            ("con.txt", "_con.txt"),
            ("Nul.tar.gz", "_Nul.tar.gz"),
            ("PRN", "_PRN"),
            ("aux.png", "_aux.png"),
            ("COM1", "_COM1"),
            ("com9.log", "_com9.log"),
            ("LPT1.txt", "_LPT1.txt"),
            ("lpt9", "_lpt9"),
            ("CON .txt", "_CON .txt"),
            ("CONIN$", "_CONIN$"),
            ("com\u{b9}.txt", "_com\u{b9}.txt"),
            ("CON.", "_CON"),
            ("COM0", "COM0"),
            ("COM10", "COM10"),
            ("CONSOLE.txt", "CONSOLE.txt"),
            ("my.con", "my.con"),
            ("LPT", "LPT"),
            ("communique.pdf", "communique.pdf"),
        ] {
            assert_eq!(safe_file_name(raw), want, "{raw:?}");
        }
        let u = url("https://cdn.fbsbx.com/files/stream");
        assert_eq!(
            choose_file_name(Some("NUL"), None, &u, None),
            "_NUL",
            "an explicit name is sanitised too"
        );
        assert_eq!(
            choose_file_name(None, Some("aux.png"), &u, Some("image/png")),
            "_aux.png"
        );
    }

    #[test]
    fn names_are_capped_by_bytes_on_a_character_boundary() {
        let wide = "\u{3042}".repeat(100);
        let capped = safe_file_name(&wide);
        assert!(capped.len() <= MAX_NAME_BYTES, "{}", capped.len());
        assert_eq!(
            capped,
            "\u{3042}".repeat(66),
            "66 characters of 3 bytes fit"
        );
        let with_ext = safe_file_name(&format!("{wide}.pdf"));
        assert_eq!(with_ext, format!("{}.pdf", "\u{3042}".repeat(65)));
        let emoji = format!("{}.png", "\u{1F600}".repeat(70));
        let out = safe_file_name(&emoji);
        assert_eq!(out, format!("{}.png", "\u{1F600}".repeat(49)));
        assert_eq!(out.len(), MAX_NAME_BYTES);
        let edge = format!("{}.png", "n".repeat(196));
        assert_eq!(safe_file_name(&edge), edge, "exactly 200 bytes is kept");
        let over = format!("{}.png", "n".repeat(197));
        assert_eq!(safe_file_name(&over).len(), MAX_NAME_BYTES);
        let odd = format!("{} {}.txt", "a".repeat(195), "b".repeat(30));
        assert_eq!(
            safe_file_name(&odd),
            format!("{}.txt", "a".repeat(195)),
            "a cut that ends on a space drops it"
        );
        let long_ext = format!("{}.{}", "s".repeat(100), "e".repeat(300));
        assert_eq!(safe_file_name(&long_ext).len(), MAX_NAME_BYTES);
    }

    #[test]
    fn an_extension_added_from_the_mime_type_stays_within_the_cap() {
        let u = url("https://cdn.fbsbx.com/files/stream");
        let bare = "n".repeat(250);
        let name = choose_file_name(None, Some(&bare), &u, Some("image/png"));
        assert_eq!(name, format!("{}.png", "n".repeat(196)));
        let wide = "\u{3042}".repeat(100);
        let name = choose_file_name(None, Some(&wide), &u, Some("application/pdf"));
        assert_eq!(name, format!("{}.pdf", "\u{3042}".repeat(65)));
        assert!(name.len() <= MAX_NAME_BYTES);
        let exact = "n".repeat(196);
        assert_eq!(
            choose_file_name(None, Some(&exact), &u, Some("image/png")),
            format!("{exact}.png"),
            "a name that fits with its extension is untouched"
        );
    }

    #[test]
    fn a_taken_name_gets_a_hash_of_the_url() {
        let a = url("https://cdn.fbsbx.com/a/doc.pdf");
        let b = url("https://cdn.fbsbx.com/b/doc.pdf");
        assert_eq!(hashed_name("doc.pdf", &a), "doc-ac964be3.pdf");
        assert_eq!(hashed_name("doc.pdf", &b), "doc-358b1744.pdf");
        assert_eq!(hashed_name("doc.pdf", &a), hashed_name("doc.pdf", &a));
        assert_eq!(hashed_name("archive", &a), "archive-ac964be3");
        assert_eq!(hashed_name("a.tar.gz", &a), "a.tar-ac964be3.gz");
        let long = format!("{}.jpeg", "x".repeat(200));
        let name = hashed_name(&safe_file_name(&long), &a);
        assert!(name.len() <= MAX_NAME_BYTES && name.ends_with("-ac964be3.jpeg"));
        let wide = hashed_name(&safe_file_name(&"\u{3042}".repeat(100)), &a);
        assert!(wide.len() <= MAX_NAME_BYTES && wide.ends_with("-ac964be3"));
        assert!(wide.is_char_boundary(wide.len() - 9));
    }

    #[test]
    fn upload_names_keep_unicode_and_lose_header_breakers() {
        assert_eq!(
            upload_file_name("rapport \u{e9}t\u{e9}.pdf"),
            "rapport \u{e9}t\u{e9}.pdf"
        );
        assert_eq!(upload_file_name("dir/sub\\a\"b.png"), "ab.png");
        assert_eq!(
            upload_file_name("evil\r\nX-Header: 1.txt"),
            "evilX-Header: 1.txt"
        );
        assert_eq!(upload_file_name("  \n "), "file");
        assert_eq!(upload_file_name("\"\""), "file");
        assert_eq!(upload_file_name(&"z".repeat(400)).chars().count(), 255);
    }

    #[test]
    fn url_names_are_decoded_and_pick_the_last_segment() {
        let u = url("https://scontent.xx.fbcdn.net/v/t1.15752-9/12345_n.jpg?oh=abc&oe=1");
        assert_eq!(file_name_from_url(&u).as_deref(), Some("12345_n.jpg"));
        let u = url("https://cdn.fbsbx.com/a/my%20file%C3%A9.pdf");
        assert_eq!(file_name_from_url(&u).as_deref(), Some("my file\u{e9}.pdf"));
        let u = url("https://cdn.fbsbx.com/a/b/");
        assert_eq!(file_name_from_url(&u).as_deref(), Some("b"));
        let u = url("https://cdn.fbsbx.com/");
        assert_eq!(file_name_from_url(&u), None);
        let u = url("https://cdn.fbsbx.com/%2e%2e%2f%2e%2e%2fpasswd");
        let name = file_name_from_url(&u).expect("segment");
        assert_eq!(safe_file_name(&name), "passwd");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }

    #[test]
    fn download_name_precedence_and_extension() {
        let u = url("https://cdn.fbsbx.com/files/stream");
        assert_eq!(
            choose_file_name(Some(" mine.bin "), Some("att.png"), &u, Some("image/png")),
            "mine.bin"
        );
        assert_eq!(
            choose_file_name(Some("../x"), None, &u, Some("image/png")),
            "x"
        );
        assert_eq!(
            choose_file_name(None, Some("att.png"), &u, Some("image/jpeg")),
            "att.png"
        );
        assert_eq!(choose_file_name(Some(" "), None, &u, None), "stream");
        assert_eq!(
            choose_file_name(None, None, &u, Some("image/jpeg")),
            "stream.jpg"
        );
        let bare = url("https://cdn.fbsbx.com/");
        assert_eq!(choose_file_name(None, None, &bare, None), FALLBACK_NAME);
        assert_eq!(
            choose_file_name(None, None, &bare, Some("audio/mpeg")),
            "attachment.mp3"
        );
    }

    fn patched(base: &[u8], at: usize, patch: &[u8]) -> Vec<u8> {
        let mut v = base.to_vec();
        v[at..at + patch.len()].copy_from_slice(patch);
        v
    }

    #[test]
    fn inline_decision_needs_a_small_real_raster_image() {
        let png = fixtures::png();
        for (bytes, want) in [
            (png.clone(), "image/png"),
            (fixtures::jpeg(), "image/jpeg"),
            (fixtures::jpeg_progressive(), "image/jpeg"),
            (fixtures::gif(), "image/gif"),
            (fixtures::webp_lossy(), "image/webp"),
            (fixtures::webp_lossless(), "image/webp"),
            (fixtures::webp_alpha(), "image/webp"),
        ] {
            assert_eq!(inline_image_mime(&bytes), Some(want));
            assert_eq!(sniff_image_mime(&bytes), Some(want));
        }
        assert_eq!(
            inline_image_mime(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
        assert_eq!(inline_image_mime(b"%PDF-1.7"), None);
        assert_eq!(inline_image_mime(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(inline_image_mime(b""), None);
        let mut big = png;
        big.resize(INLINE_MAX_BYTES, 0);
        assert_eq!(inline_image_mime(&big), Some("image/png"));
        big.push(0);
        assert_eq!(inline_image_mime(&big), None);
        assert_eq!(
            sniff_image_mime(&big),
            Some("image/png"),
            "only the size stops a big picture"
        );
    }

    #[test]
    fn polyglot_bodies_that_only_start_like_an_image_are_not_images() {
        for (label, body) in [
            (
                "png magic then html",
                [
                    b"\x89PNG\r\n\x1a\n".as_slice(),
                    b"<html><script>alert(1)</script>",
                ]
                .concat(),
            ),
            (
                "jpeg magic then text",
                [b"\xFF\xD8\xFF".as_slice(), b"not really a jpeg, just text"].concat(),
            ),
            (
                "gif magic then php",
                [b"GIF89a".as_slice(), b"<?php system($_GET[0]); ?>"].concat(),
            ),
            (
                "gif magic then html with small dimensions",
                [
                    b"GIF89a".as_slice(),
                    b" <!-- <html><script>alert(1)</script>",
                ]
                .concat(),
            ),
            (
                "webp magic then text",
                [b"RIFF\0\0\0\0WEBP".as_slice(), b"garbage garbage"].concat(),
            ),
            (
                "webp with the right size but no vp8 chunk",
                [b"RIFF\x1a\0\0\0WEBP".as_slice(), b"<html>1234567890</html>"].concat(),
            ),
            ("magic numbers only", b"\x89PNG\r\n\x1a\n".to_vec()),
            ("jpeg magic only", vec![0xFF, 0xD8, 0xFF, 0xE0]),
            ("gif magic only", b"GIF89a".to_vec()),
        ] {
            assert_eq!(sniff_image_mime(&body), None, "{label}");
            assert_eq!(inline_image_mime(&body), None, "{label}");
        }
    }

    #[test]
    fn png_structure_is_the_signature_and_an_ihdr_with_sane_sizes() {
        let png = fixtures::png();
        let sizes = |w: u32, h: u32| {
            let mut v = patched(&png, 16, &w.to_be_bytes());
            v[20..24].copy_from_slice(&h.to_be_bytes());
            v
        };
        for (label, bytes, ok) in [
            ("the real file", png.clone(), true),
            ("1 by 1", sizes(1, 1), true),
            ("20000 by 20000", sizes(20_000, 20_000), true),
            ("zero width", sizes(0, 2), false),
            ("zero height", sizes(2, 0), false),
            ("width 20001", sizes(20_001, 2), false),
            ("height 20001", sizes(2, 20_001), false),
            ("huge", sizes(u32::MAX, u32::MAX), false),
            ("wrong IHDR length", patched(&png, 8, &[0, 0, 0, 12]), false),
            ("another chunk first", patched(&png, 12, b"IDAT"), false),
            ("damaged signature", patched(&png, 1, b"png"), false),
            ("cut inside the dimensions", png[..23].to_vec(), false),
            ("cut right after the IHDR data", png[..29].to_vec(), true),
        ] {
            assert_eq!(sniff_image_mime(&bytes).is_some(), ok, "{label}");
        }
    }

    #[test]
    fn jpeg_segments_are_walked_to_a_baseline_or_progressive_frame() {
        let sof = |marker: u8, h: u16, w: u16| {
            let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, marker, 0, 8, 8];
            v.extend(h.to_be_bytes());
            v.extend(w.to_be_bytes());
            v
        };
        for (label, bytes, ok) in [
            ("real baseline", fixtures::jpeg(), true),
            ("real progressive", fixtures::jpeg_progressive(), true),
            ("sof0 after an app segment", sof(0xC0, 16, 16), true),
            ("sof2", sof(0xC2, 16, 16), true),
            ("20000 by 20000", sof(0xC0, 20_000, 20_000), true),
            ("zero height", sof(0xC0, 0, 16), false),
            ("zero width", sof(0xC0, 16, 0), false),
            ("width 20001", sof(0xC0, 16, 20_001), false),
            ("height 65535", sof(0xC0, 65_535, 16), false),
            ("extended sequential", sof(0xC1, 16, 16), false),
            ("lossless", sof(0xC3, 16, 16), false),
            ("arithmetic coded", sof(0xC9, 16, 16), false),
            (
                "sof straight after soi",
                {
                    let mut v = vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1];
                    v.push(0);
                    v
                },
                true,
            ),
            (
                "fill bytes before a marker",
                vec![0xFF, 0xD8, 0xFF, 0xFF, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1],
                true,
            ),
            (
                "a table segment is skipped",
                vec![
                    0xFF, 0xD8, 0xFF, 0xC4, 0, 4, 0, 0, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1,
                ],
                true,
            ),
            (
                "start of scan before any frame",
                vec![
                    0xFF, 0xD8, 0xFF, 0xDA, 0, 4, 0, 0, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1,
                ],
                false,
            ),
            (
                "end of image before any frame",
                vec![0xFF, 0xD8, 0xFF, 0xD9, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1],
                false,
            ),
            (
                "a segment longer than the file",
                vec![0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0, 0],
                false,
            ),
            (
                "a segment length of one",
                vec![
                    0xFF, 0xD8, 0xFF, 0xE0, 0, 1, 0, 0, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1,
                ],
                false,
            ),
            (
                "junk between segments",
                vec![
                    0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0x41, 0x41, 0xFF, 0xC0, 0, 8, 8, 0, 1, 0, 1,
                ],
                false,
            ),
            (
                "a stuffed zero is not a marker",
                vec![0xFF, 0xD8, 0xFF, 0x00, 0, 4, 0, 0],
                false,
            ),
            (
                "frame header too short",
                vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 7, 8, 0, 1, 0, 1],
                false,
            ),
            (
                "cut inside the frame header",
                vec![0xFF, 0xD8, 0xFF, 0xC0, 0, 8, 8, 0, 1],
                false,
            ),
            ("soi alone", vec![0xFF, 0xD8, 0xFF], false),
            (
                "trailing garbage after a frame is fine",
                {
                    let mut v = sof(0xC0, 16, 16);
                    v.extend(b"<html>");
                    v
                },
                true,
            ),
        ] {
            assert_eq!(sniff_image_mime(&bytes).is_some(), ok, "{label}");
        }
    }

    #[test]
    fn gif_structure_is_a_header_a_screen_descriptor_and_a_block() {
        let real = fixtures::gif();
        for (label, bytes, ok) in [
            ("the real file", real.clone(), true),
            (
                "87a header and screen only",
                b"GIF87a\x01\0\x01\0\x80\0\0".to_vec(),
                true,
            ),
            (
                "89a header and screen only",
                b"GIF89a\x01\0\x01\0\0\0\0".to_vec(),
                true,
            ),
            (
                "with a trailer",
                b"GIF89a\x01\0\x01\0\0\0\0;".to_vec(),
                true,
            ),
            (
                "with an image block",
                b"GIF89a\x01\0\x01\0\0\0\0,junk".to_vec(),
                true,
            ),
            (
                "with an extension block",
                b"GIF89a\x01\0\x01\0\0\0\0!\xf9".to_vec(),
                true,
            ),
            (
                "width and height only",
                b"GIF89a\x01\0\x01\0".to_vec(),
                false,
            ),
            ("zero width", b"GIF89a\0\0\x01\0\0\0\0".to_vec(), false),
            ("zero height", b"GIF89a\x01\0\0\0\0\0\0".to_vec(), false),
            ("width 20001", b"GIF89a\x21\x4e\x01\0\0\0\0".to_vec(), false),
            (
                "65535 by 65535",
                b"GIF89a\xff\xff\xff\xff\0\0\0".to_vec(),
                false,
            ),
            (
                "a made-up version",
                b"GIF88a\x01\0\x01\0\0\0\0".to_vec(),
                false,
            ),
            ("lower case", b"gif89a\x01\0\x01\0\0\0\0".to_vec(), false),
            (
                "text after the screen",
                b"GIF89a\x01\0\x01\0\0\0\0<html>".to_vec(),
                false,
            ),
        ] {
            assert_eq!(sniff_image_mime(&bytes).is_some(), ok, "{label}");
        }
    }

    #[test]
    fn webp_structure_is_a_riff_of_the_right_size_with_a_vp8_chunk() {
        let lossy = fixtures::webp_lossy();
        let declared = u32::from_le_bytes([lossy[4], lossy[5], lossy[6], lossy[7]]);
        assert_eq!(declared as usize, lossy.len() - 8);
        let resized = |delta: i64| {
            patched(
                &lossy,
                4,
                &u32::try_from(i64::from(declared) + delta)
                    .expect("size")
                    .to_le_bytes(),
            )
        };
        let mut longer = lossy.clone();
        longer.push(0);
        let mut much_longer = lossy.clone();
        much_longer.extend([0, 0]);
        for (label, bytes, ok) in [
            ("lossy", lossy.clone(), true),
            ("lossless", fixtures::webp_lossless(), true),
            ("extended", fixtures::webp_alpha(), true),
            ("size one too small", resized(-1), true),
            ("size one too large", resized(1), true),
            ("a pad byte after the declared size", longer, true),
            ("size two too small", resized(-2), false),
            ("size two too large", resized(2), false),
            (
                "two stray bytes after the declared size",
                much_longer,
                false,
            ),
            ("size zero", patched(&lossy, 4, &[0, 0, 0, 0]), false),
            ("another chunk first", patched(&lossy, 12, b"ALPH"), false),
            ("a wave file", patched(&lossy, 8, b"WAVE"), false),
            ("not riff", patched(&lossy, 0, b"RIFX"), false),
            (
                "first chunk larger than the file",
                patched(&lossy, 16, &[0xFF, 0, 0, 0]),
                false,
            ),
            ("cut inside the chunk header", lossy[..18].to_vec(), false),
        ] {
            assert_eq!(sniff_image_mime(&bytes).is_some(), ok, "{label}");
        }
    }
}
