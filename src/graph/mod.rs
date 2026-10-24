//! Graph API client: Bearer auth, `appsecret_proof`, bounded retries and typed errors.

pub mod download;
pub mod error;
pub mod token;

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use reqwest::Method;
use serde_json::Value;
use sha2::Sha256;
use url::Url;

pub use crate::config::GraphSettings;
use crate::config::is_loopback_host;
use crate::config::secret::{Secret, redact};
pub use download::{Downloaded, TokenMode};
pub use error::{GraphError, hint};

const MAX_RETRIES: usize = 2;
const RETRY_DELAYS_MS: [u64; MAX_RETRIES] = [400, 1200];
const MAX_JITTER_MS: u64 = 200;
const TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;
const MAX_PAGES: usize = 200;

/// Lowercase hex HMAC-SHA256 of `token` keyed by the app secret.
pub fn appsecret_proof(app_secret: &Secret, token: &Secret) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(app_secret.expose().as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(token.expose().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// True when the path already starts with a version segment such as `v26.0`.
fn starts_with_version(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    let Some(rest) = first.strip_prefix('v') else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('.')
}

/// Sends are not idempotent, so they are never retried.
fn is_send_path(path: &str) -> bool {
    let p = path
        .split('?')
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    ["/messages", "/media", "/message_attachments"]
        .iter()
        .any(|suffix| p.ends_with(suffix))
}

enum Payload<'a> {
    None,
    Json(&'a Value),
    Form(&'a [(&'a str, String)]),
}

struct Failure {
    error: GraphError,
    retryable: bool,
}

fn failure(error: GraphError, retryable: bool) -> Failure {
    Failure { error, retryable }
}

pub(crate) fn transport_error(e: reqwest::Error) -> GraphError {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else {
        "failed"
    };
    let e = e.without_url();
    let mut msg = format!("request {kind}: {e}");
    let mut cause = std::error::Error::source(&e);
    while let Some(c) = cause {
        msg.push_str(&format!(": {c}"));
        cause = c.source();
    }
    GraphError::transport(msg)
}

/// Reads a response body up to `cap` bytes.
pub(crate) async fn read_body(
    mut resp: reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, GraphError> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
        if out.len() + chunk.len() > cap {
            return Err(GraphError::Refused(format!(
                "response is larger than {cap} bytes"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Graph API client shared by every tool call.
pub struct GraphClient {
    http: reqwest::Client,
    pub(crate) dl_http: reqwest::Client,
    base: Url,
    settings: GraphSettings,
    pub(crate) download_hosts: Vec<String>,
}

impl GraphClient {
    /// `download_hosts` are extra exact hosts that `download` may fetch from.
    pub fn new(settings: &GraphSettings, download_hosts: Vec<String>) -> Result<Self, GraphError> {
        let base = Url::parse(settings.base_url.trim_end_matches('/'))
            .map_err(|_| GraphError::Config("the Graph base URL is not a valid URL".into()))?;
        let secure = base.scheme() == "https"
            || (base.scheme() == "http" && base.host_str().is_some_and(is_loopback_host));
        if !secure || !base.username().is_empty() {
            return Err(GraphError::Config(
                "the Graph base URL must use https (http only for loopback) and carry no credentials"
                    .into(),
            ));
        }
        let ua = concat!("meta-mcp/", env!("CARGO_PKG_VERSION"));
        let builder = || {
            reqwest::Client::builder()
                .user_agent(ua)
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
        };
        let build = |b: reqwest::ClientBuilder| {
            b.build()
                .map_err(|e| GraphError::transport(format!("cannot build HTTP client: {e}")))
        };
        Ok(Self {
            http: build(builder().timeout(TIMEOUT))?,
            dl_http: build(builder().timeout(Duration::from_secs(300)))?,
            base,
            settings: settings.clone(),
            download_hosts: download_hosts
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
        })
    }

    pub fn settings(&self) -> &GraphSettings {
        &self.settings
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// `{base}/{version}/{path}`; versioned paths (`v26.0/...`) are used as given.
    pub fn url_for(&self, path: &str) -> Result<Url, GraphError> {
        let (path_part, query_part) = path.split_once('?').unwrap_or((path, ""));
        let trimmed = path_part.trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(GraphError::Config("empty Graph API path".into()));
        }
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| GraphError::Config("the Graph base URL cannot be a base".into()))?;
            segments.pop_if_empty();
            if !starts_with_version(trimmed) {
                segments.push(&self.settings.version);
            }
            for seg in trimmed.split('/').filter(|s| !s.is_empty()) {
                segments.push(seg);
            }
        }
        if !query_part.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in url::form_urlencoded::parse(query_part.as_bytes()) {
                pairs.append_pair(&k, &v);
            }
        }
        Ok(url)
    }

    fn finish_url(&self, mut url: Url, query: &[(&str, String)], token: Option<&Secret>) -> Url {
        let proof = self
            .settings
            .app_secret
            .as_ref()
            .zip(token)
            .map(|(secret, tok)| appsecret_proof(secret, tok));
        if !query.is_empty() || proof.is_some() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
            if let Some(p) = &proof {
                pairs.append_pair("appsecret_proof", p);
            }
        }
        url
    }

    fn request_url(
        &self,
        path: &str,
        query: &[(&str, String)],
        token: Option<&Secret>,
    ) -> Result<Url, GraphError> {
        Ok(self.finish_url(self.url_for(path)?, query, token))
    }

    fn builder(&self, method: Method, url: Url, token: Option<&Secret>) -> reqwest::RequestBuilder {
        let req = self.http.request(method, url);
        match token {
            Some(t) => req.bearer_auth(t.expose()),
            None => req,
        }
    }

    async fn read_response(&self, resp: reqwest::Response) -> Result<Value, Failure> {
        let status = resp.status();
        let server_side = status.is_server_error();
        let bytes = read_body(resp, MAX_JSON_BYTES)
            .await
            .map_err(|e| failure(e, false))?;
        let parsed: Result<Value, _> = if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes)
        };
        match parsed {
            Ok(body) => {
                if let Some(err) = GraphError::from_body(&body, status.as_u16()) {
                    let retry = server_side || err.code().is_some_and(error::is_retryable_code);
                    return Err(failure(err, retry));
                }
                if status.is_success() {
                    return Ok(body);
                }
                Err(failure(
                    GraphError::Api {
                        code: 0,
                        subcode: None,
                        message: format!("HTTP {status}"),
                        error_type: None,
                        fbtrace_id: None,
                        user_msg: None,
                        hint: None,
                        http_status: status.as_u16(),
                    },
                    server_side,
                ))
            }
            Err(_) if status.is_success() => Err(failure(
                GraphError::Json(format!("the response is not valid JSON (HTTP {status})")),
                false,
            )),
            Err(_) => Err(failure(
                GraphError::transport(format!("Graph API answered HTTP {status} without JSON")),
                server_side,
            )),
        }
    }

    async fn attempt(&self, req: reqwest::RequestBuilder) -> Result<Value, Failure> {
        match req.send().await {
            Ok(resp) => self.read_response(resp).await,
            Err(e) => {
                let transient = e.is_connect() || e.is_timeout();
                Err(failure(transport_error(e), transient))
            }
        }
    }

    async fn execute(
        &self,
        method: Method,
        url: Url,
        token: Option<&Secret>,
        payload: Payload<'_>,
        retry: bool,
    ) -> Result<Value, GraphError> {
        let mut retries = 0;
        loop {
            let req = self.builder(method.clone(), url.clone(), token);
            let req = match &payload {
                Payload::None => req,
                Payload::Json(body) => req.json(*body),
                Payload::Form(pairs) => req.form(pairs),
            };
            match self.attempt(req).await {
                Ok(v) => return Ok(v),
                Err(f) if retry && f.retryable && retries < MAX_RETRIES => {
                    let jitter = rand::random_range(0..=MAX_JITTER_MS);
                    tracing::debug!(
                        "retrying Graph call after: {}",
                        redact(&f.error.to_string())
                    );
                    tokio::time::sleep(Duration::from_millis(RETRY_DELAYS_MS[retries] + jitter))
                        .await;
                    retries += 1;
                }
                Err(f) => return Err(f.error),
            }
        }
    }

    pub async fn get(
        &self,
        path: &str,
        token: Option<&Secret>,
        query: &[(&str, String)],
    ) -> Result<Value, GraphError> {
        self.get_with_retry(path, token, query, true).await
    }

    /// `get` with the client's own retries on or off; a caller that paces its requests turns them off.
    pub async fn get_with_retry(
        &self,
        path: &str,
        token: Option<&Secret>,
        query: &[(&str, String)],
        retry: bool,
    ) -> Result<Value, GraphError> {
        let url = self.request_url(path, query, token)?;
        self.execute(Method::GET, url, token, Payload::None, retry)
            .await
    }

    pub async fn post_json(
        &self,
        path: &str,
        token: Option<&Secret>,
        body: &Value,
    ) -> Result<Value, GraphError> {
        let url = self.request_url(path, &[], token)?;
        let retry = !is_send_path(path);
        self.execute(Method::POST, url, token, Payload::Json(body), retry)
            .await
    }

    pub async fn post_form(
        &self,
        path: &str,
        token: Option<&Secret>,
        form: &[(&str, String)],
    ) -> Result<Value, GraphError> {
        let url = self.request_url(path, &[], token)?;
        let retry = !is_send_path(path);
        self.execute(Method::POST, url, token, Payload::Form(form), retry)
            .await
    }

    /// One attempt only: the form is consumed by the send.
    pub async fn post_multipart(
        &self,
        path: &str,
        token: Option<&Secret>,
        form: reqwest::multipart::Form,
    ) -> Result<Value, GraphError> {
        let url = self.request_url(path, &[], token)?;
        let req = self
            .builder(Method::POST, url, token)
            .timeout(UPLOAD_TIMEOUT)
            .multipart(form);
        self.attempt(req).await.map_err(|f| f.error)
    }

    pub async fn delete(
        &self,
        path: &str,
        token: Option<&Secret>,
        query: &[(&str, String)],
    ) -> Result<Value, GraphError> {
        let url = self.request_url(path, query, token)?;
        self.execute(Method::DELETE, url, token, Payload::None, true)
            .await
    }

    /// A `paging.next` URL: same origin as the base, token parameters stripped.
    fn next_url(&self, next: &str, token: Option<&Secret>) -> Result<Url, GraphError> {
        let parsed = Url::parse(next)
            .map_err(|_| GraphError::Json("paging.next is not a valid URL".into()))?;
        if parsed.origin() != self.base.origin() {
            return Err(GraphError::Refused(format!(
                "refusing paging URL on host `{}`: it does not match the Graph base host",
                parsed.host_str().unwrap_or("?")
            )));
        }
        let kept: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(k, _)| k != "access_token" && k != "appsecret_proof")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut bare = parsed;
        bare.set_query(None);
        let query: Vec<(&str, String)> =
            kept.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        Ok(self.finish_url(bare, &query, token))
    }

    /// Follows `paging.next` until `max_items` items are collected or the pages run out.
    pub async fn get_all(
        &self,
        path: &str,
        token: Option<&Secret>,
        query: &[(&str, String)],
        max_items: usize,
    ) -> Result<Vec<Value>, GraphError> {
        let mut items: Vec<Value> = Vec::new();
        if max_items == 0 {
            return Ok(items);
        }
        let mut page = self.get(path, token, query).await?;
        for _ in 0..MAX_PAGES {
            if let Some(data) = page.get("data").and_then(Value::as_array) {
                items.extend(data.iter().cloned());
            }
            if items.len() >= max_items {
                items.truncate(max_items);
                break;
            }
            let Some(next) = page.pointer("/paging/next").and_then(Value::as_str) else {
                break;
            };
            let url = self.next_url(next, token)?;
            page = self
                .execute(Method::GET, url, token, Payload::None, true)
                .await?;
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(base: &str) -> GraphClient {
        let settings = GraphSettings {
            base_url: base.to_string(),
            ..GraphSettings::default()
        };
        GraphClient::new(&settings, Vec::new()).expect("client")
    }

    #[test]
    fn version_detection() {
        assert!(starts_with_version("v26.0/me"));
        assert!(starts_with_version("v9.1"));
        assert!(!starts_with_version("v26/me"));
        assert!(!starts_with_version("video_posts"));
        assert!(!starts_with_version("me"));
        assert!(!starts_with_version("v.1/x"));
    }

    #[test]
    fn send_paths_are_not_retried() {
        assert!(is_send_path("123/messages"));
        assert!(is_send_path("/123/messages/"));
        assert!(is_send_path("v26.0/123/media"));
        assert!(is_send_path("me/message_attachments"));
        assert!(!is_send_path("123/subscribed_apps"));
        assert!(!is_send_path("123/conversations"));
        assert!(!is_send_path("messages_report"));
    }

    #[test]
    fn url_building() {
        let c = client("https://graph.example.org");
        assert_eq!(
            c.url_for("me").expect("url").as_str(),
            "https://graph.example.org/v26.0/me"
        );
        assert_eq!(
            c.url_for("/123/messages").expect("url").as_str(),
            "https://graph.example.org/v26.0/123/messages"
        );
        assert_eq!(
            c.url_for("v25.0/me/accounts").expect("url").as_str(),
            "https://graph.example.org/v25.0/me/accounts"
        );
        assert_eq!(
            c.url_for("me?fields=id,name").expect("url").as_str(),
            "https://graph.example.org/v26.0/me?fields=id%2Cname"
        );
        let hostile = c.url_for("../../etc/passwd").expect("url");
        assert!(!hostile.path().contains(".."), "{hostile}");
        assert!(c.url_for("///").is_err());
        let prefixed = client("https://proxy.example.org/graph/");
        assert_eq!(
            prefixed.url_for("me").expect("url").as_str(),
            "https://proxy.example.org/graph/v26.0/me"
        );
    }

    #[test]
    fn insecure_or_invalid_base_urls_are_refused() {
        for base in [
            "http://graph.example.org",
            "ftp://localhost",
            "nonsense",
            "https://u:p@x.org",
        ] {
            let settings = GraphSettings {
                base_url: base.to_string(),
                ..GraphSettings::default()
            };
            assert!(GraphClient::new(&settings, Vec::new()).is_err(), "{base}");
        }
        assert!(GraphClient::new(&GraphSettings::default(), Vec::new()).is_ok());
        client("http://127.0.0.1:1234");
    }

    #[test]
    fn proof_matches_known_hmac() {
        let proof = appsecret_proof(
            &Secret::new("key"),
            &Secret::new("The quick brown fox jumps over the lazy dog"),
        );
        assert_eq!(
            proof,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn next_url_is_same_origin_and_token_free() {
        let c = client("http://127.0.0.1:4000");
        let token = Secret::new("tok");
        let u = c
            .next_url(
                "http://127.0.0.1:4000/v26.0/1/conversations?after=abc&access_token=SECRET&limit=2",
                Some(&token),
            )
            .expect("url");
        let q: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            q,
            vec![("after".into(), "abc".into()), ("limit".into(), "2".into())]
        );
        assert!(!u.as_str().contains("SECRET"));
        assert!(c.next_url("http://127.0.0.1:4001/x", None).is_err());
        assert!(c.next_url("https://evil.example/x", None).is_err());
        assert!(c.next_url("nonsense", None).is_err());
        let bare = c
            .next_url("http://127.0.0.1:4000/v26.0/x?access_token=T", None)
            .expect("url");
        assert_eq!(bare.query(), None);
    }
}
