//! Secret strings that never print, plus a scrubber for Graph access tokens in free text.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A secret value. `Debug` and `Display` print `***`; read it with `expose`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value, for headers, signatures and storage only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// `bytes` random bytes from the OS-seeded generator, as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    use rand::Rng;
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

const TOKEN_PREFIX: &str = "EAA";
const TOKEN_MIN_TAIL: usize = 20;

/// Replaces every run matching `EAA[A-Za-z0-9]{20,}` with `EAA***`.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(TOKEN_PREFIX) {
        let tail = &rest[pos + TOKEN_PREFIX.len()..];
        let run = tail.bytes().take_while(u8::is_ascii_alphanumeric).count();
        if run >= TOKEN_MIN_TAIL {
            out.push_str(&rest[..pos]);
            out.push_str("EAA***");
            rest = &tail[run..];
        } else {
            out.push_str(&rest[..=pos]);
            rest = &rest[pos + 1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "***");
        assert_eq!(format!("{s}"), "***");
        assert_eq!(s.expose(), "hunter2");
        assert_eq!(serde_json::to_string(&s).expect("json"), "\"hunter2\"");
        let back: Secret = serde_json::from_str("\"abc\"").expect("parse");
        assert_eq!(back.expose(), "abc");
    }

    #[test]
    fn random_hex_has_the_requested_size_and_varies() {
        let a = random_hex(32);
        let b = random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, b);
        assert_eq!(random_hex(0), "");
    }

    #[test]
    fn redacts_long_tokens_only() {
        let tok = format!("EAAB{}", "x".repeat(40));
        assert_eq!(
            redact(&format!("bad token {tok} here")),
            "bad token EAA*** here"
        );
        assert_eq!(redact("EAA short"), "EAA short");
        assert_eq!(redact("EAAshort and EAAshorter"), "EAAshort and EAAshorter");
        let exactly = format!("EAA{}", "a".repeat(20));
        assert_eq!(redact(&exactly), "EAA***");
        let almost = format!("EAA{}", "a".repeat(19));
        assert_eq!(redact(&almost), almost);
    }

    #[test]
    fn redacts_inside_words_and_repeats() {
        let a = format!("EAA{}", "9".repeat(25));
        let text = format!("x{a}y, {a};{a}");
        assert_eq!(redact(&text), "xEAA***, EAA***;EAA***");
    }

    #[test]
    fn keeps_unicode_intact() {
        assert_eq!(redact("héllo EAA wörld 🙂"), "héllo EAA wörld 🙂");
        let tok = format!("EAA{}", "z".repeat(30));
        assert_eq!(redact(&format!("é{tok}é")), "éEAA***é");
    }
}
