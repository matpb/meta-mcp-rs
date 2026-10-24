//! Input checks for write ops. Errors are `bad_params` messages and never echo the input.

pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_CAPTION_CHARS: usize = 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_EMOJI_CHARS: usize = 10;

/// `\r\n` becomes `\n`, the result is trimmed.
fn normalise(raw: &str) -> String {
    raw.replace("\r\n", "\n").trim().to_owned()
}

fn check_body(text: &str, what: &str, max: usize) -> Result<(), String> {
    if text.chars().count() > max {
        return Err(format!("{what} must be at most {max} characters"));
    }
    if text.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!(
            "{what} must not contain control characters other than newline"
        ));
    }
    Ok(())
}

/// The cleaned message text: 1 to 4000 scalar values, no control character but newline.
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

/// Empty removes a reaction; otherwise 1 to 10 scalars with at least one non-ASCII symbol.
pub fn emoji(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        return Ok(());
    }
    let count = raw.chars().count();
    let clean = raw
        .chars()
        .all(|c| !c.is_whitespace() && !c.is_control() && !c.is_ascii_alphabetic());
    let symbol = raw.chars().any(|c| c >= '\u{A9}');
    if count <= MAX_EMOJI_CHARS && clean && symbol {
        Ok(())
    } else {
        Err(format!(
            "emoji must be empty or 1 to {MAX_EMOJI_CHARS} symbols without letters or spaces"
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
        for bad in ["a\tb", "a\u{0}b", "a\rb", "a\u{1b}[0m", "a\u{85}b"] {
            assert!(text(bad).is_err(), "{bad:?}");
        }
        let msg = text("secret\u{7}").expect_err("control");
        assert!(!msg.contains("secret"));
    }

    #[test]
    fn caption_may_be_empty_and_is_shorter() {
        assert_eq!(caption("  ").expect("empty"), None);
        assert_eq!(caption(" hi ").expect("ok").as_deref(), Some("hi"));
        assert!(caption(&"a".repeat(MAX_CAPTION_CHARS)).is_ok());
        assert!(caption(&"a".repeat(MAX_CAPTION_CHARS + 1)).is_err());
        assert!(caption("a\tb").is_err());
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
