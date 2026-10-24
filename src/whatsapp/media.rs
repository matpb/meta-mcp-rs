//! Media types WhatsApp accepts: extensions, MIME types, kinds and size limits.

use std::fmt;
use std::path::Path;

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;
const STATIC_STICKER_MAX: u64 = 100 * KIB;

/// The five media message types of the Cloud API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Document,
    Sticker,
}

impl MediaKind {
    pub const ALL: [MediaKind; 5] = [
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Document,
        Self::Sticker,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Document => "document",
            Self::Sticker => "sticker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let wanted = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|k| k.as_str() == wanted)
    }

    /// Largest file WhatsApp takes for this kind (the animated limit for stickers).
    pub fn max_bytes(self) -> u64 {
        match self {
            Self::Image => 5 * MIB,
            Self::Video | Self::Audio => 16 * MIB,
            Self::Document => 100 * MIB,
            Self::Sticker => 500 * KIB,
        }
    }

    /// Images, videos and documents may carry a caption.
    pub fn allows_caption(self) -> bool {
        matches!(self, Self::Image | Self::Video | Self::Document)
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One accepted file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaType {
    pub ext: &'static str,
    pub mime: &'static str,
    pub kind: MediaKind,
}

const fn entry(ext: &'static str, mime: &'static str, kind: MediaKind) -> MediaType {
    MediaType { ext, mime, kind }
}

const DOC_XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const DOC_DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const DOC_PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// Every extension the tools recognise; the first entry of a MIME type names its default extension.
pub const TYPES: &[MediaType] = &[
    entry("aac", "audio/aac", MediaKind::Audio),
    entry("amr", "audio/amr", MediaKind::Audio),
    entry("mp3", "audio/mpeg", MediaKind::Audio),
    entry("m4a", "audio/mp4", MediaKind::Audio),
    entry("ogg", "audio/ogg", MediaKind::Audio),
    entry("txt", "text/plain", MediaKind::Document),
    entry("xls", "application/vnd.ms-excel", MediaKind::Document),
    entry("xlsx", DOC_XLSX, MediaKind::Document),
    entry("doc", "application/msword", MediaKind::Document),
    entry("docx", DOC_DOCX, MediaKind::Document),
    entry("ppt", "application/vnd.ms-powerpoint", MediaKind::Document),
    entry("pptx", DOC_PPTX, MediaKind::Document),
    entry("pdf", "application/pdf", MediaKind::Document),
    entry("jpg", "image/jpeg", MediaKind::Image),
    entry("jpeg", "image/jpeg", MediaKind::Image),
    entry("png", "image/png", MediaKind::Image),
    entry("webp", "image/webp", MediaKind::Sticker),
    entry("3gp", "video/3gpp", MediaKind::Video),
    entry("mp4", "video/mp4", MediaKind::Video),
];

/// The media type for a file name, by its extension (case-insensitive).
pub fn by_extension(file_name: &str) -> Option<&'static MediaType> {
    let ext = Path::new(file_name)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    TYPES.iter().find(|t| t.ext == ext)
}

/// The base MIME type of a header-style value: parameters dropped, lowercased.
pub fn base_mime(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// The media type for a MIME type, parameters ignored.
pub fn by_mime(mime: &str) -> Option<&'static MediaType> {
    let base = base_mime(mime);
    TYPES.iter().find(|t| t.mime == base)
}

/// Default file extension for a MIME type; `bin` when unknown.
pub fn extension_for_mime(mime: &str) -> &'static str {
    by_mime(mime).map_or("bin", |t| t.ext)
}

/// A file's kind and the exact MIME type to send with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub kind: MediaKind,
    pub mime: String,
}

fn accepted_mimes() -> String {
    let mut parts = Vec::new();
    for kind in MediaKind::ALL {
        let mut mimes: Vec<&str> = TYPES
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.mime)
            .collect();
        mimes.dedup();
        parts.push(format!("{kind}: {}", mimes.join(", ")));
    }
    parts.join("; ")
}

fn accepted_extensions() -> String {
    TYPES.iter().map(|t| t.ext).collect::<Vec<_>>().join(", ")
}

/// Works out kind and MIME from the file name or an explicit `mime_type`, checking `wanted`.
pub fn resolve(
    file_name: &str,
    wanted: Option<MediaKind>,
    mime_override: Option<&str>,
) -> Result<Resolved, String> {
    let (found, mime) = match mime_override.map(str::trim).filter(|m| !m.is_empty()) {
        Some(raw) => {
            if !raw.is_ascii() || raw.len() > 100 || raw.chars().any(char::is_control) {
                return Err("mime_type must be a plain ASCII MIME type such as image/png".into());
            }
            let found = by_mime(raw).ok_or_else(|| {
                format!(
                    "unsupported mime_type `{raw}`: WhatsApp accepts {}",
                    accepted_mimes()
                )
            })?;
            let mime = match raw.split_once(';') {
                Some((base, params)) => format!("{};{params}", base.trim().to_ascii_lowercase()),
                None => raw.to_ascii_lowercase(),
            };
            (found, mime)
        }
        None => {
            let found = by_extension(file_name).ok_or_else(|| {
                format!(
                    "cannot tell the media type of `{file_name}`: use one of the extensions {} or pass mime_type",
                    accepted_extensions()
                )
            })?;
            (found, found.mime.to_string())
        }
    };
    if let Some(kind) = wanted
        && kind != found.kind
    {
        return Err(format!(
            "`{file_name}` is {}, which WhatsApp only accepts as kind={}, not kind={kind}",
            found.mime, found.kind
        ));
    }
    Ok(Resolved {
        kind: found.kind,
        mime,
    })
}

/// True when the bytes start a WebP file whose VP8X header sets the animation flag.
pub fn is_animated_webp(head: &[u8]) -> bool {
    head.len() > 20
        && &head[0..4] == b"RIFF"
        && &head[8..12] == b"WEBP"
        && &head[12..16] == b"VP8X"
        && head[20] & 0x02 != 0
}

/// Size limit for these bytes: static stickers are held to 100 KiB, the rest to the kind's limit.
pub fn limit_for(kind: MediaKind, head: &[u8]) -> u64 {
    if kind == MediaKind::Sticker && !is_animated_webp(head) {
        STATIC_STICKER_MAX
    } else {
        kind.max_bytes()
    }
}

/// `5 MiB`, `100 KiB` or `1234 bytes`.
pub fn human_size(bytes: u64) -> String {
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_table_matches_the_documented_types() {
        let cases = [
            ("a.aac", "audio/aac", MediaKind::Audio),
            ("a.amr", "audio/amr", MediaKind::Audio),
            ("a.mp3", "audio/mpeg", MediaKind::Audio),
            ("a.m4a", "audio/mp4", MediaKind::Audio),
            ("a.ogg", "audio/ogg", MediaKind::Audio),
            ("a.txt", "text/plain", MediaKind::Document),
            ("a.xls", "application/vnd.ms-excel", MediaKind::Document),
            ("a.xlsx", DOC_XLSX, MediaKind::Document),
            ("a.doc", "application/msword", MediaKind::Document),
            ("a.docx", DOC_DOCX, MediaKind::Document),
            (
                "a.ppt",
                "application/vnd.ms-powerpoint",
                MediaKind::Document,
            ),
            ("a.pptx", DOC_PPTX, MediaKind::Document),
            ("a.pdf", "application/pdf", MediaKind::Document),
            ("a.jpg", "image/jpeg", MediaKind::Image),
            ("a.jpeg", "image/jpeg", MediaKind::Image),
            ("a.png", "image/png", MediaKind::Image),
            ("a.webp", "image/webp", MediaKind::Sticker),
            ("a.3gp", "video/3gpp", MediaKind::Video),
            ("a.mp4", "video/mp4", MediaKind::Video),
        ];
        for (name, mime, kind) in cases {
            let t = by_extension(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((t.mime, t.kind), (mime, kind), "{name}");
        }
        assert_eq!(
            by_extension("PHOTO.JPG").map(|t| t.mime),
            Some("image/jpeg")
        );
        assert!(by_extension("noextension").is_none());
        assert!(by_extension("archive.zip").is_none());
        assert!(
            by_extension(".png").is_none(),
            "a bare dotfile has no extension"
        );
    }

    #[test]
    fn size_limits_follow_the_table() {
        assert_eq!(MediaKind::Image.max_bytes(), 5 * 1024 * 1024);
        assert_eq!(MediaKind::Video.max_bytes(), 16 * 1024 * 1024);
        assert_eq!(MediaKind::Audio.max_bytes(), 16 * 1024 * 1024);
        assert_eq!(MediaKind::Document.max_bytes(), 100 * 1024 * 1024);
        assert_eq!(MediaKind::Sticker.max_bytes(), 500 * 1024);
        assert!(MediaKind::Image.allows_caption() && MediaKind::Document.allows_caption());
        assert!(!MediaKind::Audio.allows_caption() && !MediaKind::Sticker.allows_caption());
    }

    #[test]
    fn kinds_parse_case_insensitively() {
        assert_eq!(MediaKind::parse(" Image "), Some(MediaKind::Image));
        assert_eq!(MediaKind::parse("STICKER"), Some(MediaKind::Sticker));
        assert_eq!(MediaKind::parse("gif"), None);
        for k in MediaKind::ALL {
            assert_eq!(MediaKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn resolve_by_extension_and_override() {
        let r = resolve("cat.PNG", Some(MediaKind::Image), None).expect("png");
        assert_eq!((r.kind, r.mime.as_str()), (MediaKind::Image, "image/png"));
        let r = resolve("blob.bin", None, Some("Application/PDF")).expect("override");
        assert_eq!(
            (r.kind, r.mime.as_str()),
            (MediaKind::Document, "application/pdf")
        );
        let r = resolve(
            "v.ogg",
            Some(MediaKind::Audio),
            Some("audio/ogg; codecs=opus"),
        )
        .expect("params");
        assert_eq!(r.mime, "audio/ogg; codecs=opus");
        assert_eq!(r.kind, MediaKind::Audio);
    }

    #[test]
    fn resolve_rejects_unknown_and_mismatched_types() {
        let err = resolve("x.zip", None, None).expect_err("zip");
        assert!(err.contains("x.zip") && err.contains("mime_type"), "{err}");
        let err = resolve("x.png", None, Some("image/gif")).expect_err("gif");
        assert!(
            err.contains("image/gif") && err.contains("image/png"),
            "{err}"
        );
        let err = resolve("x.png", Some(MediaKind::Document), None).expect_err("png as doc");
        assert!(
            err.contains("kind=image") && err.contains("kind=document"),
            "{err}"
        );
        assert!(resolve("x.png", None, Some("image/png\r\nX: y")).is_err());
        assert!(resolve("x.png", None, Some("imagé/png")).is_err());
    }

    #[test]
    fn mime_helpers() {
        assert_eq!(base_mime(" Audio/OGG ; codecs=opus"), "audio/ogg");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("audio/mp4"), "m4a");
        assert_eq!(extension_for_mime("application/pdf; x=1"), "pdf");
        assert_eq!(extension_for_mime("application/x-unknown"), "bin");
        assert!(by_mime("video/mp4").is_some() && by_mime("video/webm").is_none());
    }

    #[test]
    fn webp_animation_flag_sets_the_sticker_limit() {
        let mut animated = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00".to_vec();
        animated.extend_from_slice(&[0x02, 0, 0, 0]);
        let mut still = animated.clone();
        still[20] = 0x00;
        assert!(is_animated_webp(&animated));
        assert!(!is_animated_webp(&still));
        assert!(!is_animated_webp(b"RIFF"));
        assert!(!is_animated_webp(
            b"\x89PNG\r\n\x1a\n and some more bytes here"
        ));
        assert_eq!(limit_for(MediaKind::Sticker, &animated), 500 * 1024);
        assert_eq!(limit_for(MediaKind::Sticker, &still), 100 * 1024);
        assert_eq!(limit_for(MediaKind::Sticker, b"junk"), 100 * 1024);
        assert_eq!(limit_for(MediaKind::Image, b"junk"), 5 * 1024 * 1024);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(5 * MIB), "5 MiB");
        assert_eq!(human_size(100 * KIB), "100 KiB");
        assert_eq!(human_size(5 * MIB + 1), "5242881 bytes");
        assert_eq!(human_size(0), "0 bytes");
    }
}
