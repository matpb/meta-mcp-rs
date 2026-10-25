//! Input checks for write ops. Errors are `bad_params` messages and never echo the input.

pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_CAPTION_CHARS: usize = 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_EMOJI_CHARS: usize = 10;

/// `\r\n` becomes `\n`, the result is trimmed.
fn normalise(raw: &str) -> String {
    raw.replace("\r\n", "\n").trim().to_owned()
}

/// Bidirectional overrides and isolates, which can make text read differently than it is.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Tag characters, invisible and used to smuggle hidden text.
fn is_tag_char(c: char) -> bool {
    matches!(c, '\u{E0000}'..='\u{E007F}')
}

fn check_body(text: &str, what: &str, max: usize) -> Result<(), String> {
    if text.chars().count() > max {
        return Err(format!("{what} must be at most {max} characters"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(format!(
            "{what} must not contain control characters other than newline and tab"
        ));
    }
    if text.chars().any(|c| is_bidi_control(c) || is_tag_char(c)) {
        return Err(format!(
            "{what} must not contain bidirectional controls or tag characters"
        ));
    }
    Ok(())
}

/// The cleaned message text: 1 to 4000 scalar values, no control character but newline and tab.
pub fn text(raw: &str) -> Result<String, String> {
    let cleaned = normalise(raw);
    if cleaned.is_empty() {
        return Err(format!("text must be 1 to {MAX_TEXT_CHARS} characters"));
    }
    check_body(&cleaned, "text", MAX_TEXT_CHARS)?;
    Ok(cleaned)
}

/// The cleaned caption, `None` when empty.
pub fn caption(raw: &str) -> Result<Option<String>, String> {
    let cleaned = normalise(raw);
    check_body(&cleaned, "caption", MAX_CAPTION_CHARS)?;
    Ok((!cleaned.is_empty()).then_some(cleaned))
}

fn id(raw: &str, what: &str) -> Result<(), String> {
    let ok = !raw.is_empty()
        && raw.len() <= MAX_ID_BYTES
        && raw.bytes().all(|b| (0x21..=0x7E).contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{what} must be 1 to {MAX_ID_BYTES} printable ASCII characters without spaces"
        ))
    }
}

pub fn thread_id(raw: &str) -> Result<(), String> {
    id(raw, "thread_id")
}

pub fn message_id(raw: &str) -> Result<(), String> {
    id(raw, "message_id")
}

pub fn attachment_id(raw: &str) -> Result<(), String> {
    id(raw, "attachment_id")
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

/// Empty removes a reaction; otherwise 1 to 10 scalars that form an emoji.
pub fn emoji(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Ok(());
    }
    if raw.chars().count() <= MAX_EMOJI_CHARS && is_emoji_text(raw) {
        Ok(())
    } else {
        Err(format!(
            "emoji must be empty or one emoji of at most {MAX_EMOJI_CHARS} characters"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_normalised_trimmed_and_bounded() {
        assert_eq!(text("  hi\r\nthere \n").expect("ok"), "hi\nthere");
        assert!(text("").is_err());
        assert!(text("  \n ").is_err());
        assert!(text(&"a".repeat(MAX_TEXT_CHARS)).is_ok());
        assert!(text(&"a".repeat(MAX_TEXT_CHARS + 1)).is_err());
        assert!(text(&"\u{1F600}".repeat(MAX_TEXT_CHARS)).is_ok());
        assert!(text(&"\u{1F600}".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }

    #[test]
    fn text_refuses_control_characters_but_keeps_newlines() {
        assert!(text("a\nb").is_ok());
        for bad in ["a\u{0}b", "a\rb", "a\u{1b}[0m", "a\u{85}b"] {
            assert!(text(bad).is_err(), "{bad:?}");
        }
        let msg = text("secret\u{7}").expect_err("control");
        assert!(!msg.contains("secret"));
    }

    #[test]
    fn tabs_are_allowed_but_bidi_controls_and_tag_characters_are_not() {
        assert_eq!(text("col1\tcol2").expect("tab"), "col1\tcol2");
        for c in ('\u{202A}'..='\u{202E}').chain('\u{2066}'..='\u{2069}') {
            let msg = text(&format!("pay {c}gpj.exe")).expect_err("bidi");
            assert!(msg.contains("bidirectional"), "{msg}");
        }
        for c in ['\u{E0000}', '\u{E0041}', '\u{E007F}'] {
            assert!(text(&format!("a{c}b")).is_err(), "{c:?}");
        }
        for fine in [
            "a\u{200b}b",
            "caf\u{e9}",
            "\u{4f60}\u{597d}",
            "a\u{2028}b",
            "\u{1F600}",
        ] {
            assert!(text(fine).is_ok(), "{fine:?}");
        }
    }

    #[test]
    fn caption_may_be_empty_and_is_shorter() {
        assert_eq!(caption("  ").expect("empty"), None);
        assert_eq!(caption(" hi ").expect("ok").as_deref(), Some("hi"));
        assert!(caption(&"a".repeat(MAX_CAPTION_CHARS)).is_ok());
        assert!(caption(&"a".repeat(MAX_CAPTION_CHARS + 1)).is_err());
        assert!(caption("a\tb").is_ok());
        assert!(caption("a\u{202e}b").is_err());
    }

    #[test]
    fn ids_are_printable_ascii_without_spaces() {
        assert!(thread_id("123456789@s.whatsapp.net").is_ok());
        assert!(thread_id("t_1").is_ok());
        assert!(thread_id(&"x".repeat(128)).is_ok());
        assert!(thread_id(&"x".repeat(129)).is_err());
        for bad in ["", "a b", "a\n", "caf\u{e9}", "a\u{7f}", "\u{20}"] {
            assert!(thread_id(bad).is_err(), "{bad:?}");
        }
        assert!(message_id("3EB0ABC").is_ok());
        assert!(message_id("").is_err());
        assert!(attachment_id("att 1").is_err());
    }

    #[test]
    fn emoji_rules() {
        assert!(emoji("").is_ok());
        assert!(emoji("\u{1F44D}").is_ok());
        assert!(emoji("\u{2764}\u{FE0F}").is_ok());
        assert!(emoji("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}").is_ok());
        assert!(emoji("\u{A9}").is_ok());
        assert!(emoji("1\u{FE0F}\u{20E3}").is_ok());
        assert!(emoji("\u{1F44D}\u{1F3FD}").is_ok());
        assert!(emoji("\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}").is_ok());
        for bad in [
            "\u{e9}\u{e9}",
            "\u{4f60}\u{597d}",
            "\u{200D}",
            "\u{FE0F}",
            "\u{202e}\u{1F44D}",
            "\u{1F44D}\u{200b}",
        ] {
            assert!(emoji(bad).is_err(), "{bad:?}");
        }
        for bad in [
            "a",
            "\u{1F44D}a",
            "\u{1F44D} ",
            "\n",
            "1",
            "#",
            "\u{1F44D}\t",
        ] {
            assert!(emoji(bad).is_err(), "{bad:?}");
        }
        assert!(emoji(&"\u{1F44D}".repeat(10)).is_ok());
        assert!(emoji(&"\u{1F44D}".repeat(11)).is_err());
    }
}
