//! Write ops: send_text, send_media, send_reaction, mark_read, set_typing. Every audited send
//! runs the same pipeline: read-only, blocked, validation, write-ahead attempt, sidecar, completion.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::ops::{opt_str_param, str_param};
use super::platform::{PlatformRt, private_dir};
use super::{Daemon, OpError, reply_code, sanitize_code};
use crate::config::secret::random_hex;
use crate::files::{FileJail, clean_remote_filename};
use crate::messenger::media::{kind_for_mime, mime_for_name};
use crate::personal::audit::{AuditEntry, Kind, NewEntry, Outcome};
use crate::personal::link::LinkError;
use crate::personal::protocol::{
    CODE_AUDIT_FAILED, CODE_FILE_REFUSED, CODE_NO_FILES_DIR, CODE_READ_ONLY, CODE_SEND_UNKNOWN,
};
use crate::personal::sidecar::Sidecar;
use crate::personal::validate;

const SEND_TIMEOUT: Duration = Duration::from_secs(300);
const REACTION_TIMEOUT: Duration = Duration::from_secs(60);
const MARK_TIMEOUT: Duration = Duration::from_secs(30);
const MEDIA_KINDS: [&str; 4] = ["image", "video", "audio", "file"];
const UNKNOWN_MESSAGE: &str = "the message may have been sent: read the thread before retrying";
const AUDIT_WARNING: &str = "the operation completed but its audit line could not be written";

/// What a finished call left in the audit log.
struct Completion {
    audit_id: Option<u64>,
}

impl Completion {
    fn merge_into(&self, out: &mut Value) {
        out["audit_id"] = json!(self.audit_id);
        if self.audit_id.is_none() {
            out["audit_warning"] = json!(AUDIT_WARNING);
        }
    }
}

struct Pipeline<'a> {
    d: &'a Arc<Daemon>,
    rt: &'a Arc<PlatformRt>,
    /// `None` for ops that are not audited.
    kind: Option<Kind>,
    thread: Option<String>,
    body: String,
    file_sha: Option<String>,
}

impl<'a> Pipeline<'a> {
    fn new(
        d: &'a Arc<Daemon>,
        rt: &'a Arc<PlatformRt>,
        kind: Option<Kind>,
        p: &Map<String, Value>,
        body_key: &str,
    ) -> Self {
        let thread = p
            .get("thread_id")
            .and_then(Value::as_str)
            .filter(|t| validate::thread_id(t).is_ok())
            .map(str::to_owned);
        let body = p
            .get(body_key)
            .and_then(Value::as_str)
            .map(|s| s.replace("\r\n", "\n").trim().to_owned())
            .unwrap_or_default();
        Self {
            d,
            rt,
            kind,
            thread,
            body,
            file_sha: None,
        }
    }

    fn entry(&self, outcome: Outcome) -> Option<NewEntry> {
        self.kind.map(|kind| {
            NewEntry::new(self.rt.platform, kind, outcome)
                .thread(self.thread.as_deref())
                .body(&self.body)
                .file(self.file_sha.clone())
        })
    }

    async fn append(&self, entry: NewEntry) -> std::io::Result<AuditEntry> {
        let log = Arc::clone(&self.d.audit);
        tokio::task::spawn_blocking(move || log.append(entry))
            .await
            .map_err(std::io::Error::other)?
    }

    /// Audits a line that must not fail the op; a write error is logged.
    async fn record(&self, outcome: Outcome, code: &str) {
        if let Some(entry) = self.entry(outcome)
            && let Err(e) = self.append(entry.error(code)).await
        {
            tracing::error!(error = %e, "audit write failed");
        }
    }

    async fn refused(&self, e: OpError) -> OpError {
        self.record(Outcome::Refused, &e.code).await;
        e
    }

    async fn invalid(&self, message: String) -> OpError {
        self.record(Outcome::Invalid, crate::personal::protocol::CODE_BAD_PARAMS)
            .await;
        OpError::bad_params(message)
    }

    /// Steps 1 and 2: read-only mode, then a blocked account.
    async fn gate(&self) -> Result<(), OpError> {
        let refusal = if self.d.cfg.read_only {
            Some(OpError::new(
                CODE_READ_ONLY,
                "the daemon runs read-only (META_MCP_READ_ONLY)",
            ))
        } else {
            self.rt.blocked_error()
        };
        match refusal {
            Some(e) => Err(self.refused(e).await),
            None => Ok(()),
        }
    }

    async fn online(&self) -> Result<Arc<Sidecar>, OpError> {
        match Daemon::online(self.rt) {
            Ok(sc) => Ok(sc),
            Err(e) => Err(self.refused(e).await),
        }
    }

    /// Step 4: the write-ahead line, fsynced before the sidecar is asked.
    async fn attempt(&self) -> Result<u64, OpError> {
        let Some(entry) = self.entry(Outcome::Attempt) else {
            return Ok(0);
        };
        match self.append(entry).await {
            Ok(stored) => Ok(stored.id),
            Err(e) => {
                tracing::error!(error = %e, "audit write failed, refusing the send");
                Err(OpError::new(
                    CODE_AUDIT_FAILED,
                    "the audit log could not be written, so nothing was sent",
                ))
            }
        }
    }

    async fn complete(
        &self,
        outcome: Outcome,
        attempt: u64,
        message: Option<String>,
        code: Option<&str>,
    ) -> Completion {
        let Some(mut entry) = self.entry(outcome) else {
            return Completion { audit_id: None };
        };
        entry = entry.attempt(attempt).message(message);
        if let Some(code) = code {
            entry = entry.error(code);
        }
        match self.append(entry).await {
            Ok(stored) => Completion {
                audit_id: Some(stored.id),
            },
            Err(e) => {
                tracing::error!(error = %e, "audit completion write failed");
                Completion { audit_id: None }
            }
        }
    }

    /// Steps 5 and 6: the sidecar call and its completion line.
    async fn call(
        &self,
        attempt: u64,
        sc: &Sidecar,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<(Value, Completion), OpError> {
        match sc.link().request_with_timeout(op, params, timeout).await {
            Ok(result) => {
                let message = result
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let done = self.complete(Outcome::Sent, attempt, message, None).await;
                Ok((result, done))
            }
            Err(LinkError::Remote { code, message, .. }) if code != CODE_SEND_UNKNOWN => {
                self.complete(Outcome::Failed, attempt, None, Some(&sanitize_code(&code)))
                    .await;
                Err(OpError {
                    code: reply_code(&code),
                    message,
                })
            }
            Err(e) => {
                let code = match &e {
                    LinkError::Remote { code, .. } => code.clone(),
                    LinkError::Timeout(_) => "timeout".to_owned(),
                    _ => "link".to_owned(),
                };
                self.complete(Outcome::Unknown, attempt, None, Some(&code))
                    .await;
                Err(OpError::new(CODE_SEND_UNKNOWN, UNKNOWN_MESSAGE))
            }
        }
    }
}

/// Removes the outbound media copy when the send is over, whatever the outcome.
struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn defuse(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct PreparedMedia {
    copy: PathBuf,
    sha256: String,
    mime: String,
    kind: String,
    filename: String,
}

fn refused_file(message: impl Into<String>) -> OpError {
    OpError::new(CODE_FILE_REFUSED, message)
}

/// Resolves, checks and copies the file into the platform's private outbox, hashing on the way.
fn prepare_media(
    jail: &FileJail,
    max_bytes: u64,
    data_dir: &Path,
    path: &str,
    allow_inbound: bool,
    kind: Option<&str>,
    filename: Option<&str>,
) -> Result<PreparedMedia, OpError> {
    let resolved = jail
        .resolve_read(path)
        .map_err(|e| refused_file(e.to_string()))?;
    if jail.is_inbound(&resolved) && !allow_inbound {
        return Err(refused_file(
            "files under inbound/ are downloads: set allow_inbound to send one",
        ));
    }
    let original = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut source =
        File::open(&resolved).map_err(|e| refused_file(format!("cannot read the file: {e}")))?;
    let size = source
        .metadata()
        .map_err(|e| refused_file(format!("cannot read the file: {e}")))?
        .len();
    if size > max_bytes {
        return Err(refused_file(format!(
            "the file is {size} bytes, over the {max_bytes} byte limit (META_MCP_MAX_FILE_BYTES)"
        )));
    }
    let outbox = data_dir.join("outbox");
    private_dir(&outbox)
        .map_err(|_| OpError::new(CODE_AUDIT_FAILED, "cannot create the outbox"))?;
    let ext = Path::new(&original)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map_or_else(|| "bin".to_owned(), str::to_ascii_lowercase);
    let copy = outbox.join(format!("{}.{ext}", random_hex(8)));
    let mut dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&copy)
        .map_err(|_| OpError::new(CODE_AUDIT_FAILED, "cannot create the outbound copy"))?;
    let guard = RemoveOnDrop::new(copy.clone());
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = source
            .read(&mut buf)
            .map_err(|e| refused_file(format!("cannot read the file: {e}")))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            return Err(refused_file(
                "the file grew past the size limit while it was copied",
            ));
        }
        hasher.update(&buf[..n]);
        dest.write_all(&buf[..n])
            .map_err(|_| OpError::new(CODE_AUDIT_FAILED, "cannot write the outbound copy"))?;
    }
    dest.sync_all()
        .map_err(|_| OpError::new(CODE_AUDIT_FAILED, "cannot write the outbound copy"))?;
    guard.defuse();
    let mime = mime_for_name(&original).unwrap_or("application/octet-stream");
    let filename = filename
        .map(clean_remote_filename)
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| clean_remote_filename(&original));
    Ok(PreparedMedia {
        copy,
        sha256: hex::encode(hasher.finalize()),
        mime: mime.to_owned(),
        kind: kind.map_or_else(|| kind_for_mime(mime).to_owned(), str::to_owned),
        filename,
    })
}

impl Daemon {
    pub(super) async fn send_text(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let mut pl = Pipeline::new(self, rt, Some(Kind::Text), &p, "text");
        pl.gate().await?;
        let checked = (|| {
            let thread = str_param(&p, "thread_id")?;
            validate::thread_id(thread)?;
            let text = validate::text(str_param(&p, "text")?)?;
            let reply_to = opt_str_param(&p, "reply_to")?;
            if let Some(r) = reply_to {
                validate::message_id(r)?;
            }
            Ok::<_, String>((thread.to_owned(), text, reply_to.map(str::to_owned)))
        })();
        let (thread, text, reply_to) = match checked {
            Ok(v) => v,
            Err(m) => return Err(pl.invalid(m).await),
        };
        pl.thread = Some(thread.clone());
        pl.body = text.clone();
        let sc = pl.online().await?;
        let attempt = pl.attempt().await?;
        let mut params = json!({"thread_id": thread, "text": text});
        if let Some(r) = reply_to {
            params["reply_to"] = json!(r);
        }
        let (result, done) = pl
            .call(attempt, &sc, "send_text", params, SEND_TIMEOUT)
            .await?;
        let mut out = json!({"message_id": result.get("message_id"), "ts": result.get("ts")});
        done.merge_into(&mut out);
        Ok(out)
    }

    pub(super) async fn send_reaction(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let mut pl = Pipeline::new(self, rt, Some(Kind::Reaction), &p, "emoji");
        pl.gate().await?;
        let checked = (|| {
            let thread = str_param(&p, "thread_id")?;
            validate::thread_id(thread)?;
            let message = str_param(&p, "message_id")?;
            validate::message_id(message)?;
            let emoji = str_param(&p, "emoji")?;
            validate::emoji(emoji)?;
            Ok::<_, String>((thread.to_owned(), message.to_owned(), emoji.to_owned()))
        })();
        let (thread, message, emoji) = match checked {
            Ok(v) => v,
            Err(m) => return Err(pl.invalid(m).await),
        };
        pl.thread = Some(thread.clone());
        pl.body = emoji.clone();
        let sc = pl.online().await?;
        let attempt = pl.attempt().await?;
        let params = json!({"thread_id": thread, "message_id": message, "emoji": emoji});
        let (_, done) = pl
            .call(attempt, &sc, "send_reaction", params, REACTION_TIMEOUT)
            .await?;
        let mut out = json!({});
        done.merge_into(&mut out);
        Ok(out)
    }

    pub(super) async fn send_media(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let mut pl = Pipeline::new(self, rt, Some(Kind::Media), &p, "caption");
        pl.gate().await?;
        let checked = (|| {
            let thread = str_param(&p, "thread_id")?;
            validate::thread_id(thread)?;
            let path = str_param(&p, "path")?;
            let caption = validate::caption(opt_str_param(&p, "caption")?.unwrap_or(""))?;
            let kind = opt_str_param(&p, "kind")?;
            if kind.is_some_and(|k| !MEDIA_KINDS.contains(&k)) {
                return Err("kind must be image, video, audio or file".to_owned());
            }
            let filename = opt_str_param(&p, "filename")?;
            let allow_inbound = match p.get("allow_inbound") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err("allow_inbound must be a boolean".to_owned()),
            };
            Ok((
                thread.to_owned(),
                path.to_owned(),
                caption,
                kind.map(str::to_owned),
                filename.map(str::to_owned),
                allow_inbound,
            ))
        })();
        let (thread, path, caption, kind, filename, allow_inbound) = match checked {
            Ok(v) => v,
            Err(m) => return Err(pl.invalid(m).await),
        };
        pl.thread = Some(thread.clone());
        pl.body = caption.clone().unwrap_or_default();
        let sc = pl.online().await?;
        let Some(jail) = self.cfg.files.clone() else {
            let e = OpError::new(CODE_NO_FILES_DIR, "set META_MCP_FILES_DIR to send files");
            return Err(pl.refused(e).await);
        };
        let max = self.cfg.max_file_bytes;
        let data_dir = std::path::absolute(rt.store.data_dir())
            .map_err(|_| OpError::new(CODE_AUDIT_FAILED, "cannot resolve the data folder"))?;
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_media(
                &jail,
                max,
                &data_dir,
                &path,
                allow_inbound,
                kind.as_deref(),
                filename.as_deref(),
            )
        })
        .await
        .map_err(|_| {
            OpError::new(
                crate::personal::protocol::CODE_INTERNAL,
                "media task failed",
            )
        })?;
        let prepared = match prepared {
            Ok(m) => m,
            Err(e) => return Err(pl.refused(e).await),
        };
        let _cleanup = RemoveOnDrop::new(prepared.copy.clone());
        pl.file_sha = Some(prepared.sha256.clone());
        let attempt = pl.attempt().await?;
        let mut params = json!({
            "thread_id": thread,
            "path": prepared.copy.display().to_string(),
            "mime": prepared.mime,
            "kind": prepared.kind,
            "filename": prepared.filename,
        });
        if let Some(c) = caption {
            params["caption"] = json!(c);
        }
        let (result, done) = pl
            .call(attempt, &sc, "send_media", params, SEND_TIMEOUT)
            .await?;
        let mut out = json!({"message_id": result.get("message_id"), "ts": result.get("ts")});
        done.merge_into(&mut out);
        Ok(out)
    }

    pub(super) async fn mark_read(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let pl = Pipeline::new(self, rt, None, &p, "");
        pl.gate().await?;
        let checked = (|| {
            let thread = str_param(&p, "thread_id")?;
            validate::thread_id(thread)?;
            let up_to = match p.get("up_to_ts_ms") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_i64()
                        .ok_or_else(|| "up_to_ts_ms must be an integer".to_owned())?,
                ),
            };
            Ok::<_, String>((thread.to_owned(), up_to))
        })()
        .map_err(OpError::bad_params)?;
        let sc = pl.online().await?;
        let mut params = json!({"thread_id": checked.0});
        if let Some(ts) = checked.1 {
            params["up_to_ts_ms"] = json!(ts);
        }
        Ok(sc
            .link()
            .request_with_timeout("mark_read", params, MARK_TIMEOUT)
            .await?)
    }

    pub(super) async fn set_typing(
        self: &Arc<Self>,
        rt: &Arc<PlatformRt>,
        p: Map<String, Value>,
    ) -> Result<Value, OpError> {
        let pl = Pipeline::new(self, rt, None, &p, "");
        pl.gate().await?;
        let thread = str_param(&p, "thread_id")
            .and_then(|t| validate::thread_id(t).map(|()| t.to_owned()))
            .map_err(OpError::bad_params)?;
        let on = p
            .get("on")
            .and_then(Value::as_bool)
            .ok_or_else(|| OpError::bad_params("on (boolean) is required"))?;
        let sc = pl.online().await?;
        Ok(sc
            .link()
            .request_with_timeout(
                "set_typing",
                json!({"thread_id": thread, "on": on}),
                MARK_TIMEOUT,
            )
            .await?)
    }
}
