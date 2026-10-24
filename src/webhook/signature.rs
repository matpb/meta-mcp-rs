//! `X-Hub-Signature-256` verification, constant time.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Lowercase header name carrying the signature.
pub const HEADER: &str = "x-hub-signature-256";

const PREFIX: &str = "sha256=";

fn mac(app_secret: &str, body: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(app_secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body);
    mac.finalize().into_bytes().to_vec()
}

/// `sha256=<lowercase hex HMAC-SHA256 of body keyed by the app secret>`.
pub fn sign(app_secret: &str, body: &[u8]) -> String {
    format!("{PREFIX}{}", hex::encode(mac(app_secret, body)))
}

/// Constant-time equality of two strings (the length is not secret).
pub fn ct_str_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// True when `header` holds the valid signature of `body`; any parse failure is false.
pub fn verify(app_secret: &str, body: &[u8], header: Option<&str>) -> bool {
    if app_secret.is_empty() {
        return false;
    }
    let Some(hex_part) = header.and_then(|h| h.trim().strip_prefix(PREFIX)) else {
        return false;
    };
    let Ok(provided) = hex::decode(hex_part) else {
        return false;
    };
    let expected = mac(app_secret, body);
    expected.len() == provided.len() && bool::from(expected.as_slice().ct_eq(provided.as_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "app-secret";

    #[test]
    fn valid_signature_verifies() {
        let body = br#"{"object":"page"}"#;
        let header = sign(SECRET, body);
        assert!(header.starts_with("sha256="));
        assert!(verify(SECRET, body, Some(&header)));
        assert!(verify(
            SECRET,
            body,
            Some(&header.to_uppercase().replace("SHA256=", "sha256="))
        ));
    }

    #[test]
    fn known_vector() {
        let header = sign("key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            header,
            "sha256=f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn rejects_every_malformed_or_wrong_signature() {
        let body = b"payload";
        let good = sign(SECRET, body);
        assert!(!verify(SECRET, body, None));
        assert!(!verify(SECRET, body, Some("")));
        assert!(!verify(SECRET, body, Some("sha256=")));
        assert!(!verify(SECRET, body, Some("sha256=zz")));
        assert!(!verify(
            SECRET,
            body,
            Some(&good.replace("sha256=", "sha1="))
        ));
        assert!(!verify(
            SECRET,
            body,
            Some(good.trim_start_matches("sha256="))
        ));
        assert!(!verify(SECRET, body, Some(&good[..good.len() - 2])));
        assert!(!verify(SECRET, body, Some(&format!("{good}00"))));
        assert!(!verify(SECRET, b"other payload", Some(&good)));
        assert!(!verify("other-secret", body, Some(&good)));
        assert!(!verify("", body, Some(&sign("", body))));
        let mut flipped = good.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
        assert!(!verify(
            SECRET,
            body,
            Some(std::str::from_utf8(&flipped).expect("ascii"))
        ));
    }

    #[test]
    fn unicode_bodies_are_signed_over_bytes() {
        let body = "{\"text\":\"héllo 🙂\"}".as_bytes();
        let header = sign(SECRET, body);
        assert!(verify(SECRET, body, Some(&header)));
        assert!(!verify(
            SECRET,
            "{\"text\":\"hello\"}".as_bytes(),
            Some(&header)
        ));
    }

    #[test]
    fn constant_time_helper() {
        assert!(ct_str_eq("same", "same"));
        assert!(ct_str_eq("", ""));
        assert!(!ct_str_eq("same", "samf"));
        assert!(!ct_str_eq("same", "same-longer"));
        assert!(!ct_str_eq("longer-one", "short"));
    }
}
