//! Op dispatch: status, waiters, the recent ring, read proxies and attachment downloads.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use super::platform::PlatformRt;
use super::{Daemon, OpError};
use crate::config::secret::random_hex;
use crate::files;
use crate::personal::protocol::{
    CODE_FILE_REFUSED, CODE_INTERNAL, CODE_NO_FILES_DIR, CODE_UNKNOWN_OP, Reply, ReplyError,
    Request,
};
use crate::personal::{PersonalPlatform, validate};

const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const WAIT_MAX_MS: u64 = 300_000;

pub(super) fn str_param<'a>(p: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    p.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} (string) is required"))
}

pub(super) fn opt_str_param<'a>(
    p: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// Creates the download folder with mode 0700, but only when it really lies inside the files root:
/// a symlinked `inbound/...` pointing elsewhere is refused before anything is created or chmodded.
fn prepare_download_dir(root: &Path, dir: &Path) -> Result<(), OpError> {
    let refused = || {
        OpError::new(
            CODE_FILE_REFUSED,
            "the download folder resolves outside META_MCP_FILES_DIR",
        )
    };
    let failed = |_| OpError::new(CODE_INTERNAL, "cannot create the download folder");
    let existing = dir.ancestors().find(|a| a.exists()).unwrap_or(root);
    if !std::fs::canonicalize(existing)
        .map_err(failed)?
        .starts_with(root)
    {
        return Err(refused());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(failed)?;
    let real = std::fs::canonicalize(dir).map_err(failed)?;
    if !real.starts_with(root) {
        return Err(refused());
    }
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).map_err(failed)
}

fn timed_out() -> Value {
    json!({"message": null, "timed_out": true})
}

impl Daemon {
    /// Answers one client request; the reply carries the client's own id.
    pub async fn handle(self: &Arc<Self>, req: Request) -> Reply {
        let result = self.dispatch(&req.op, req.params).await;
        let op: String = req
            .op
            .chars()
            .take(32)
            .filter(|c| c.is_ascii_graphic())
            .collect();
        match &result {
            Ok(_) if req.op == "status" => tracing::debug!(id = req.id, %op, "request ok"),
            Ok(_) => tracing::info!(id = req.id, %op, "request ok"),
            Err(e) => tracing::info!(id = req.id, %op, code = %e.code, "request failed"),
        }
        match result {
            Ok(value) => Reply {
                id: req.id,
                ok: true,
                result: Some(value),
                error: None,
            },
            Err(e) => Reply {
                id: req.id,
                ok: false,
                result: None,
                error: Some(ReplyError {
                    code: e.code,
                    message: e.message,
                }),
            },
        }
    }

    async fn dispatch(self: &Arc<Self>, op: &str, params: Value) -> Result<Value, OpError> {
        match op {
            "status" => return Ok(self.status()),
            "wait_for_message" => return self.wait_for_message(&params).await,
            "recent" => return self.recent(&params),
            "login_start"
            | "login_finish"
            | "login_cancel"
            | "logout"
            | "restart"
            | "list_threads"
            | "search_users"
            | "fetch_messages"
            | "download_attachment"
            | "send_text"
            | "send_media"
            | "send_reaction"
            | "mark_read"
            | "set_typing" => {}
            _ => return Err(OpError::new(CODE_UNKNOWN_OP, "no such op")),
        }
        let (rt, p) = self.target(params)?;
        match op {
            "login_start" => self.login_start(&rt, p).await,
            "login_finish" => self.login_finish(&rt, p).await,
            "login_cancel" => self.login_cancel(&rt).await,
            "logout" => self.logout(&rt).await,
            "restart" => self.restart_platform(&rt).await,
            "list_threads" => Self::proxy_read(&rt, "list_threads", p, LIST_TIMEOUT).await,
            "search_users" => {
                if rt.platform != PersonalPlatform::Messenger {
                    return Err(OpError::bad_params(
                        "search_users is only available for messenger",
                    ));
                }
                str_param(&p, "query")
                    .and_then(validate::search_query)
                    .and_then(|()| validate::search_limit(p.get("limit")))
                    .map_err(OpError::bad_params)?;
                Self::proxy_read(&rt, "search_users", p, SEARCH_TIMEOUT).await
            }
            "fetch_messages" => {
                str_param(&p, "thread_id")
                    .and_then(validate::thread_id)
                    .map_err(OpError::bad_params)?;
                Self::proxy_read(&rt, "fetch_messages", p, FETCH_TIMEOUT).await
            }
            "download_attachment" => self.download_attachment(&rt, p).await,
            "send_text" => self.send_text(&rt, p).await,
            "send_media" => self.send_media(&rt, p).await,
            "send_reaction" => self.send_reaction(&rt, p).await,
            "mark_read" => self.mark_read(&rt, p).await,
            _ => self.set_typing(&rt, p).await,
        }
    }

    /// The platform named in `params` and the remaining params, which go to the sidecar.
    fn target(&self, params: Value) -> Result<(Arc<PlatformRt>, Map<String, Value>), OpError> {
        let Value::Object(mut p) = params else {
            return Err(OpError::bad_params(
                "params must be an object with a platform",
            ));
        };
        let platform = p
            .remove("platform")
            .as_ref()
            .and_then(Value::as_str)
            .and_then(PersonalPlatform::parse)
            .ok_or_else(|| OpError::bad_params("platform must be messenger or whatsapp"))?;
        Ok((Arc::clone(self.rt(platform)), p))
    }

    fn status(&self) -> Value {
        json!({
            "daemon": {
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "read_only": self.cfg.read_only,
                "files_dir_configured": self.cfg.files.is_some(),
            },
            "platforms": {
                "messenger": self.rt(PersonalPlatform::Messenger).status(),
                "whatsapp": self.rt(PersonalPlatform::WhatsApp).status(),
            },
        })
    }

    async fn proxy_read(
        rt: &Arc<PlatformRt>,
        op: &str,
        p: Map<String, Value>,
        timeout: Duration,
    ) -> Result<Value, OpError> {
        let sc = Self::online(rt)?;
        Ok(sc
            .link()
            .request_with_timeout(op, Value::Object(p), timeout)
            .await?)
    }

    async fn download_attachment(
        &self,
        rt: &Arc<PlatformRt>,
        mut p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let jail = self.cfg.files.as_ref().ok_or_else(|| {
            OpError::new(
                CODE_NO_FILES_DIR,
                "set META_MCP_FILES_DIR to download attachments",
            )
        })?;
        let checked = (|| {
            let thread = str_param(&p, "thread_id")?;
            validate::thread_id(thread)?;
            let message = str_param(&p, "message_id")?;
            validate::message_id(message)?;
            let attachment = str_param(&p, "attachment_id")?;
            validate::attachment_id(attachment)?;
            Ok::<_, String>((thread.to_owned(), message.to_owned(), attachment.to_owned()))
        })()
        .map_err(OpError::bad_params)?;
        let sc = Self::online(rt)?;
        let name = format!(
            "{}_{}",
            files::sanitize_filename(&format!("{}_{}", checked.1, checked.2)),
            random_hex(4)
        );
        let rel = files::inbound_dest(rt.platform.inbox_platform().as_str(), &name)
            .map_err(|e| OpError::new(CODE_INTERNAL, e.to_string()))?;
        let dest = jail.root().join(&rel);
        if let Some(parent) = dest.parent() {
            prepare_download_dir(jail.root(), parent)?;
        }
        p.clear();
        p.insert("thread_id".into(), json!(checked.0));
        p.insert("message_id".into(), json!(checked.1));
        p.insert("attachment_id".into(), json!(checked.2));
        p.insert("dest_path".into(), json!(dest.display().to_string()));
        let mut result = sc
            .link()
            .request_with_timeout("download_attachment", Value::Object(p), DOWNLOAD_TIMEOUT)
            .await?;
        if let Some(obj) = result.as_object_mut() {
            obj.insert("files_path".into(), json!(rel));
        } else {
            result = json!({"files_path": rel});
        }
        Ok(result)
    }

    /// The next live inbound message, optionally of one platform.
    async fn wait_for_message(&self, params: &Value) -> Result<Value, OpError> {
        let ms = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|ms| (1..=WAIT_MAX_MS).contains(ms))
            .ok_or_else(|| OpError::bad_params("timeout_ms must be an integer from 1 to 300000"))?;
        let platform = Self::optional_platform(params)?;
        let mut rx = self.incoming.subscribe();
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return Ok(timed_out()),
                got = tokio::time::timeout_at(deadline, rx.recv()) => match got {
                    Err(_) | Ok(Err(RecvError::Closed)) => return Ok(timed_out()),
                    Ok(Err(RecvError::Lagged(n))) => {
                        tracing::warn!(lost = n, "wait_for_message lagged, events dropped");
                        return Ok(json!({"message": null, "timed_out": false, "lagged": true}));
                    }
                    Ok(Ok(incoming)) => {
                        if platform.is_none_or(|p| p == incoming.platform) {
                            return Ok(json!({
                                "message": incoming.message,
                                "timed_out": false,
                                "lagged": false,
                            }));
                        }
                    }
                },
            }
        }
    }

    fn optional_platform(params: &Value) -> Result<Option<PersonalPlatform>, OpError> {
        match params.get("platform") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_str()
                .and_then(PersonalPlatform::parse)
                .map(Some)
                .ok_or_else(|| OpError::bad_params("platform must be messenger or whatsapp")),
        }
    }

    fn recent(&self, params: &Value) -> Result<Value, OpError> {
        let since = params
            .get("since_ts")
            .and_then(Value::as_i64)
            .ok_or_else(|| OpError::bad_params("since_ts (integer, unix seconds) is required"))?;
        let platform = Self::optional_platform(params)?;
        let mut messages = Vec::new();
        let mut truncated = false;
        for rt in &self.platforms {
            if platform.is_some_and(|p| p != rt.platform) {
                continue;
            }
            let (found, cut) = rt.inner().ring.recent(since);
            messages.extend(found);
            truncated |= cut;
        }
        messages.sort_by_key(|m| std::cmp::Reverse(m.ts));
        Ok(json!({"messages": messages, "truncated": truncated}))
    }
}
