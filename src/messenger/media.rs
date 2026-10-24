//! Media helpers: MIME by extension, attachment kinds, safe file names and the inline-image check.

use url::Url;

/// Largest image returned inline as an image block.
pub const INLINE_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Largest file Meta accepts for an upload.
pub const MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;
/// Images above this size may be refused by the Send API.
pub const IMAGE_SOFT_LIMIT_BYTES: u64 = 8 * 1024 * 1024;

const FALLBACK_NAME: &str = "attachment";
const MAX_NAME_CHARS: usize = 120;
const MAX_EXT_CHARS: usize = 16;

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
        && ext.len() <= MAX_EXT_CHARS
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

/// Reduces a remote name to one safe path component: no separators, no control or reserved characters.
pub fn safe_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
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
    shorten(trimmed)
}

/// Caps the name at `MAX_NAME_CHARS` characters and keeps a short extension.
fn shorten(name: &str) -> String {
    if name.chars().count() <= MAX_NAME_CHARS {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.chars().count() <= MAX_EXT_CHARS => {
            let keep = MAX_NAME_CHARS.saturating_sub(ext.chars().count() + 1);
            let stem: String = stem.chars().take(keep).collect();
            format!("{stem}.{ext}")
        }
        _ => name.chars().take(MAX_NAME_CHARS).collect(),
    }
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
    }
    name
}

/// The raster image type of `bytes` by magic number: png, jpeg, gif or webp.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The MIME type to show `bytes` inline with, or `None` when it is not a small raster image.
pub fn inline_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > INLINE_MAX_BYTES {
        return None;
    }
    sniff_image_mime(bytes)
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
        assert_eq!(short.chars().count(), MAX_NAME_CHARS);
        assert!(short.ends_with(".jpeg"));
        let no_ext = safe_file_name(&"y".repeat(300));
        assert_eq!(no_ext.chars().count(), MAX_NAME_CHARS);
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

    #[test]
    fn inline_decision_needs_a_small_real_raster_image() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10];
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.extend_from_slice(&[0; 8]);
        assert_eq!(inline_image_mime(png), Some("image/png"));
        assert_eq!(inline_image_mime(&jpeg), Some("image/jpeg"));
        assert_eq!(inline_image_mime(b"GIF89a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(inline_image_mime(b"GIF87a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(inline_image_mime(&webp), Some("image/webp"));
        assert_eq!(
            inline_image_mime(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
        assert_eq!(inline_image_mime(b"%PDF-1.7"), None);
        assert_eq!(inline_image_mime(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(inline_image_mime(b""), None);
        let mut big = png.to_vec();
        big.resize(INLINE_MAX_BYTES, 0);
        assert_eq!(inline_image_mime(&big), Some("image/png"));
        big.push(0);
        assert_eq!(inline_image_mime(&big), None);
    }
}
