//! Graph API errors with actionable hints for the agent.

use std::fmt;

use serde_json::Value;

use crate::config::ConfigError;
use crate::config::secret::redact;
use crate::files::FileError;

/// Everything a Graph call can fail with. `Display` is what the agent sees, with tokens scrubbed.
#[derive(Debug)]
pub enum GraphError {
    Api {
        code: i64,
        subcode: Option<i64>,
        message: String,
        error_type: Option<String>,
        fbtrace_id: Option<String>,
        user_msg: Option<String>,
        hint: Option<&'static str>,
        http_status: u16,
    },
    Transport(String),
    Config(String),
    Refused(String),
    Json(String),
}

/// Actionable advice for a Graph error code and subcode.
pub fn hint(code: i64, subcode: Option<i64>) -> Option<&'static str> {
    Some(match (code, subcode) {
        (190, _) => {
            "the access token is invalid or expired: run `meta-mcp setup` again or `meta-mcp token refresh`"
        }
        (_, Some(2018278 | 2534022)) => {
            "outside the 24-hour messaging window: the person must message the Page first, or use tag HUMAN_AGENT if your app has that approval"
        }
        (200, Some(2018028)) => {
            "the app is not approved yet: until pages_messaging has Advanced Access and the app is live, the Page can only message people with a role on the app (admin, developer, tester)"
        }
        (10 | 200 | 230, _) => {
            "permission denied: the Page token needs pages_messaging, pages_read_engagement and pages_manage_metadata, and the app must be allowed to message this person (Development mode only reaches app roles)"
        }
        (100, Some(2018001)) => {
            "no matching user: the recipient must be a Page-scoped ID (PSID) from this Page's conversations"
        }
        (100, _) => {
            "invalid parameter: check the recipient id and the message body; note that the CONFIRMED_EVENT_UPDATE, ACCOUNT_UPDATE and POST_PURCHASE_UPDATE tags were retired on 2026-04-27"
        }
        (551, _) => "this person is unavailable (blocked the Page, deactivated, or unreachable)",
        (4 | 17 | 32 | 613 | 80006 | 130429, _) => "rate limited: wait and retry later",
        (131047, _) => {
            "WhatsApp: more than 24 hours since the customer's last message, send an approved template instead"
        }
        (131026, _) => {
            "WhatsApp: message undeliverable (number not on WhatsApp or has not accepted the terms)"
        }
        (131030, _) => {
            "WhatsApp: recipient not in the allowed list (a test number can only message the verified recipients added in the App Dashboard)"
        }
        (368, _) => "action blocked by Meta as abusive or policy violating",
        _ => return None,
    })
}

/// Graph error codes worth retrying: transient server trouble and rate limits.
pub fn is_retryable_code(code: i64) -> bool {
    matches!(code, 1 | 2 | 4 | 17 | 32 | 613 | 80006 | 130429)
}

/// Reads an integer that Graph may send as a number or a string.
pub fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(redact)
        .filter(|s| !s.is_empty())
}

impl GraphError {
    /// Builds an `Api` error from a body holding an `error` member; `None` when it has none.
    pub fn from_body(body: &Value, http_status: u16) -> Option<Self> {
        let err = body.get("error").filter(|e| !e.is_null())?;
        if let Some(plain) = err.as_str() {
            let detail = text(body, "error_description").unwrap_or_default();
            let message = if detail.is_empty() {
                redact(plain)
            } else {
                format!("{}: {detail}", redact(plain))
            };
            return Some(Self::Api {
                code: 0,
                subcode: None,
                message,
                error_type: None,
                fbtrace_id: None,
                user_msg: None,
                hint: None,
                http_status,
            });
        }
        let code = err.get("code").and_then(as_i64).unwrap_or(0);
        let subcode = err.get("error_subcode").and_then(as_i64);
        Some(Self::Api {
            code,
            subcode,
            message: text(err, "message").unwrap_or_else(|| "unknown error".into()),
            error_type: text(err, "type"),
            fbtrace_id: text(err, "fbtrace_id"),
            user_msg: text(err, "error_user_msg")
                .or_else(|| text(err.get("error_data").unwrap_or(&Value::Null), "details")),
            hint: hint(code, subcode),
            http_status,
        })
    }

    /// The Graph error code, when this is an API error.
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Api { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn transport(message: impl AsRef<str>) -> Self {
        Self::Transport(redact(message.as_ref()))
    }

    fn render(&self) -> String {
        match self {
            Self::Api {
                code,
                subcode,
                message,
                fbtrace_id,
                user_msg,
                hint,
                ..
            } => {
                let mut s = format!("Graph API error {code}");
                if let Some(sub) = subcode {
                    s.push_str(&format!("/{sub}"));
                }
                s.push_str(": ");
                s.push_str(message);
                if let Some(u) = user_msg.as_ref().filter(|u| *u != message) {
                    s.push_str(&format!(" ({u})"));
                }
                if let Some(id) = fbtrace_id {
                    s.push_str(&format!(" [fbtrace {id}]"));
                }
                if let Some(h) = hint {
                    s.push_str(&format!(". Hint: {h}"));
                }
                s
            }
            Self::Transport(m) => format!("Graph API request failed: {m}"),
            Self::Config(m) | Self::Refused(m) => m.clone(),
            Self::Json(m) => format!("Graph API returned an unexpected response: {m}"),
        }
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&redact(&self.render()))
    }
}

impl std::error::Error for GraphError {}

impl From<ConfigError> for GraphError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e.0)
    }
}

impl From<FileError> for GraphError {
    fn from(e: FileError) -> Self {
        Self::Refused(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hint_table() {
        let cases: [(i64, Option<i64>, &str); 20] = [
            (190, None, "meta-mcp setup"),
            (190, Some(463), "meta-mcp token refresh"),
            (10, None, "pages_messaging"),
            (200, None, "pages_manage_metadata"),
            (230, None, "Development mode"),
            (10, Some(2018278), "24-hour"),
            (10, Some(2534022), "24-hour"),
            (200, Some(2018028), "Advanced Access"),
            (100, Some(2018001), "Page-scoped ID"),
            (100, Some(1), "2026-04-27"),
            (100, None, "CONFIRMED_EVENT_UPDATE"),
            (551, None, "unavailable"),
            (4, None, "rate limited"),
            (17, None, "rate limited"),
            (32, None, "rate limited"),
            (613, None, "rate limited"),
            (80006, None, "rate limited"),
            (130429, None, "rate limited"),
            (131047, None, "approved template"),
            (368, None, "abusive"),
        ];
        for (code, sub, needle) in cases {
            let h = hint(code, sub).unwrap_or_else(|| panic!("no hint for {code}/{sub:?}"));
            assert!(h.contains(needle), "{code}/{sub:?}: {h}");
        }
        assert!(hint(131026, None).expect("hint").contains("undeliverable"));
        assert!(hint(131030, None).expect("hint").contains("allowed list"));
        assert_eq!(hint(1, None), None);
        assert_eq!(hint(0, None), None);
        assert_eq!(hint(99999, Some(1)), None);
    }

    #[test]
    fn retryable_codes() {
        for c in [1, 2, 4, 17, 32, 613, 80006, 130429] {
            assert!(is_retryable_code(c), "{c}");
        }
        for c in [0, 10, 100, 190, 200, 551, 131047] {
            assert!(!is_retryable_code(c), "{c}");
        }
    }

    #[test]
    fn display_format_and_redaction() {
        let body = json!({"error": {
            "message": "Invalid token EAAB1234567890123456789012345 given",
            "type": "OAuthException", "code": 190, "error_subcode": "460",
            "fbtrace_id": "TRACE1", "error_user_msg": "Please log in again"
        }});
        let e = GraphError::from_body(&body, 400).expect("error");
        let shown = e.to_string();
        assert!(
            shown.starts_with("Graph API error 190/460: Invalid token EAA*** given"),
            "{shown}"
        );
        assert!(shown.contains("(Please log in again)"), "{shown}");
        assert!(shown.contains("[fbtrace TRACE1]"), "{shown}");
        assert!(
            shown.contains(". Hint: the access token is invalid"),
            "{shown}"
        );
        assert!(!shown.contains("1234567890"), "{shown}");
        assert_eq!(e.code(), Some(190));
    }

    #[test]
    fn whatsapp_error_data_details_are_kept() {
        let body = json!({"error": {
            "message": "(#130429) Rate limit hit", "type": "OAuthException", "code": 130429,
            "error_data": {"messaging_product": "whatsapp",
                           "details": "Cloud API message throughput has been reached."},
            "fbtrace_id": "T"
        }});
        let shown = GraphError::from_body(&body, 400)
            .expect("error")
            .to_string();
        assert!(
            shown.contains("(Cloud API message throughput has been reached.)"),
            "{shown}"
        );
    }

    #[test]
    fn display_without_optionals() {
        let e = GraphError::from_body(&json!({"error": {"message": "boom", "code": 1}}), 500)
            .expect("error");
        assert_eq!(e.to_string(), "Graph API error 1: boom");
        let plain = GraphError::from_body(
            &json!({"error": "invalid_request", "error_description": "bad code"}),
            400,
        )
        .expect("error");
        assert_eq!(
            plain.to_string(),
            "Graph API error 0: invalid_request: bad code"
        );
        assert!(GraphError::from_body(&json!({"data": []}), 200).is_none());
        assert!(GraphError::from_body(&json!({"error": null}), 200).is_none());
    }

    #[test]
    fn other_variants_scrub_tokens() {
        let tok = format!("EAA{}", "q".repeat(30));
        assert_eq!(
            GraphError::transport(format!("failed with {tok}")).to_string(),
            "Graph API request failed: failed with EAA***"
        );
        assert_eq!(
            GraphError::Refused(format!("no {tok}")).to_string(),
            "no EAA***"
        );
        assert_eq!(
            GraphError::Json("bad".into()).to_string(),
            "Graph API returned an unexpected response: bad"
        );
    }

    #[test]
    fn defensive_integer_parsing() {
        assert_eq!(as_i64(&json!(5)), Some(5));
        assert_eq!(as_i64(&json!("7")), Some(7));
        assert_eq!(as_i64(&json!(" 8 ")), Some(8));
        assert_eq!(as_i64(&json!(9.0)), Some(9));
        assert_eq!(as_i64(&json!("x")), None);
        assert_eq!(as_i64(&json!(null)), None);
    }
}
