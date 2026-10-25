//! Typed frames of the sidecar line protocol (version 1). Replies carry an `id`; events do not.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

pub const CODE_BAD_PARAMS: &str = "bad_params";
pub const CODE_UNKNOWN_OP: &str = "unknown_op";
pub const CODE_READ_ONLY: &str = "read_only";
pub const CODE_SENDING_BLOCKED: &str = "sending_blocked";
pub const CODE_NOT_LOGGED_IN: &str = "not_logged_in";
pub const CODE_SIDECAR_UNAVAILABLE: &str = "sidecar_unavailable";
pub const CODE_NO_FILES_DIR: &str = "no_files_dir";
pub const CODE_FILE_REFUSED: &str = "file_refused";
pub const CODE_AUDIT_FAILED: &str = "audit_failed";
pub const CODE_TIMEOUT: &str = "timeout";
pub const CODE_TOO_MANY_REQUESTS: &str = "too_many_requests";
pub const CODE_TOO_MANY_WAITERS: &str = "too_many_waiters";
pub const CODE_SEND_UNKNOWN: &str = "send_unknown";
pub const CODE_RATE_LIMITED: &str = "rate_limited";
pub const CODE_SHUTTING_DOWN: &str = "shutting_down";
pub const CODE_INTERNAL: &str = "internal";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ReplyError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Reply(Reply),
    Event(Event),
}

impl Frame {
    /// Parses one line. A top-level `id` key makes it a reply, otherwise an event.
    pub fn parse(line: &[u8]) -> Result<Frame, serde_json::Error> {
        let value: Value = serde_json::from_slice(line)?;
        if value.get("id").is_some() {
            serde_json::from_value(value).map(Frame::Reply)
        } else {
            serde_json::from_value(value).map(Frame::Event)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol: u32,
    pub platform: String,
    pub data_dir: String,
    pub state: Value,
}

/// The signed-in account a sidecar reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Parsed leniently: only `protocol` is required.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: u32,
    #[serde(default)]
    pub sidecar_version: String,
    #[serde(default)]
    pub library: Library,
    #[serde(default)]
    pub go: String,
    #[serde(default)]
    pub logged_in: bool,
    #[serde(default)]
    pub account: Option<Account>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip() {
        let req = Request {
            id: 7,
            op: "ping".into(),
            params: Value::Null,
        };
        let line = serde_json::to_string(&req).expect("serialize");
        assert_eq!(line, r#"{"id":7,"op":"ping","params":null}"#);
        let back: Request = serde_json::from_str(&line).expect("parse");
        assert_eq!(back, req);
    }

    #[test]
    fn hello_params_shape() {
        let params = serde_json::to_value(HelloParams {
            protocol: PROTOCOL_VERSION,
            platform: "whatsapp".into(),
            data_dir: "/d".into(),
            state: Value::Null,
        })
        .expect("params");
        assert_eq!(
            params,
            json!({"protocol": 1, "platform": "whatsapp", "data_dir": "/d", "state": null})
        );
    }

    #[test]
    fn reply_and_event_frames_parse() {
        let reply = Frame::parse(br#"{"id": 7, "ok": true, "result": {"pong": true}}"#).expect("r");
        assert!(matches!(reply, Frame::Reply(r) if r.id == 7 && r.ok));
        let err =
            Frame::parse(br#"{"id": 8, "ok": false, "error": {"code": "x", "message": "m"}}"#)
                .expect("e");
        assert!(matches!(&err, Frame::Reply(r) if r.error.as_ref().is_some_and(|e| e.code == "x")));
        let ev = Frame::parse(br#"{"event": "ready", "data": {"pid": 1}, "extra": 1}"#).expect("v");
        assert!(matches!(ev, Frame::Event(e) if e.event == "ready"));
        let bare = Frame::parse(br#"{"event": "logged_out"}"#).expect("bare");
        assert!(matches!(bare, Frame::Event(e) if e.data.is_null()));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(Frame::parse(b"not json").is_err());
        assert!(Frame::parse(br#"{"id": "x"}"#).is_err());
    }

    #[test]
    fn hello_result_is_lenient() {
        let min: HelloResult = serde_json::from_value(json!({"protocol": 1})).expect("min");
        assert!(!min.logged_in && min.account.is_none() && min.library.name.is_empty());
        let full: HelloResult = serde_json::from_value(json!({
            "protocol": 1, "sidecar_version": "0.1.0",
            "library": {"name": "whatsmeow", "version": "v0"}, "go": "go1.26",
            "logged_in": true, "account": {"id": "1", "name": "N"}, "future": 1
        }))
        .expect("full");
        assert_eq!(full.account.expect("account").id, "1");
        assert!(serde_json::from_value::<HelloResult>(json!({"logged_in": true})).is_err());
    }
}
