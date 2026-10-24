//! Safe media download: host allowlist, manual redirects, token only on listed hosts, size cap.

use bytes::{Bytes, BytesMut};
use reqwest::header::{CONTENT_TYPE, LOCATION};
use url::Url;

use super::{GraphClient, GraphError, transport_error};
use crate::config::is_loopback_host;
use crate::config::secret::Secret;

const MAX_HOPS: usize = 5;
const DEFAULT_HOSTS: &[&str] = &[
    "*.fbcdn.net",
    "*.fbsbx.com",
    "*.facebook.com",
    "*.messenger.com",
    "*.whatsapp.net",
    "lookaside.fbsbx.com",
];

/// Whether `download` attaches a Bearer token, and on which exact hosts.
#[derive(Debug, Clone, Copy)]
pub enum TokenMode<'a> {
    Never,
    OnHosts {
        token: &'a Secret,
        hosts: &'a [String],
    },
}

/// A fetched file held in memory.
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub bytes: Bytes,
    pub mime: Option<String>,
    pub final_url: Url,
}

/// True when the URL may be fetched: https (http only for loopback), no userinfo, listed host.
pub fn is_allowed_host(url: &Url, extra: &[String]) -> bool {
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(&host));
    if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    extra.contains(&host)
        || DEFAULT_HOSTS
            .iter()
            .any(|pattern| match pattern.strip_prefix("*.") {
                Some(suffix) => {
                    host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
                }
                None => host == *pattern,
            })
}

fn token_for<'a>(mode: &TokenMode<'a>, url: &Url) -> Option<&'a Secret> {
    match mode {
        TokenMode::Never => None,
        TokenMode::OnHosts { token, hosts } => {
            let host = url.host_str()?;
            hosts
                .iter()
                .any(|h| h.eq_ignore_ascii_case(host))
                .then_some(*token)
        }
    }
}

fn refused_host(url: &Url) -> GraphError {
    GraphError::Refused(format!(
        "refusing to download from host `{}`: not an allowed Meta media host, and no token was sent",
        url.host_str().unwrap_or("?")
    ))
}

impl GraphClient {
    /// Fetches `url` with every hop checked against the allowlist and the size cap.
    pub async fn download(
        &self,
        url: &Url,
        token_mode: TokenMode<'_>,
        max_bytes: u64,
    ) -> Result<Downloaded, GraphError> {
        let mut current = url.clone();
        for hop in 0..=MAX_HOPS {
            if !is_allowed_host(&current, &self.download_hosts) {
                return Err(refused_host(&current));
            }
            let mut req = self.dl_http.get(current.clone());
            if let Some(token) = token_for(&token_mode, &current) {
                req = req.bearer_auth(token.expose());
            }
            let mut resp = req.send().await.map_err(transport_error)?;
            let status = resp.status();
            if status.is_redirection() {
                if hop == MAX_HOPS {
                    return Err(GraphError::Refused(format!(
                        "refusing download: more than {MAX_HOPS} redirects"
                    )));
                }
                current = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| current.join(l).ok())
                    .ok_or_else(|| {
                        GraphError::Refused("refusing download: invalid redirect target".into())
                    })?;
                continue;
            }
            if !status.is_success() {
                return Err(GraphError::transport(format!(
                    "download answered HTTP {status}"
                )));
            }
            if let Some(len) = resp.content_length()
                && len > max_bytes
            {
                return Err(too_large(max_bytes));
            }
            let mime = resp
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| {
                    s.split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase()
                })
                .filter(|s| !s.is_empty());
            let mut buf = BytesMut::new();
            while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
                if (buf.len() + chunk.len()) as u64 > max_bytes {
                    return Err(too_large(max_bytes));
                }
                buf.extend_from_slice(&chunk);
            }
            return Ok(Downloaded {
                bytes: buf.freeze(),
                mime,
                final_url: current,
            });
        }
        Err(GraphError::Refused(
            "refusing download: redirect loop".into(),
        ))
    }
}

fn too_large(max_bytes: u64) -> GraphError {
    GraphError::Refused(format!(
        "download is larger than the limit of {max_bytes} bytes (META_MCP_MAX_FILE_BYTES)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).expect("url")
    }

    #[test]
    fn default_allowlist() {
        let none: [String; 0] = [];
        for ok in [
            "https://scontent.xx.fbcdn.net/v/t1.0/a.jpg",
            "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1",
            "https://cdn.fbsbx.com/x",
            "https://www.facebook.com/x",
            "https://static.xx.fbcdn.net/x",
            "https://mmg.whatsapp.net/x",
            "https://api.messenger.com/x",
            "https://SCONTENT.xx.fbcdn.net/x",
        ] {
            assert!(is_allowed_host(&u(ok), &none), "{ok}");
        }
        for bad in [
            "https://fbcdn.net/x",
            "https://evilfbcdn.net/x",
            "https://scontent.fbcdn.net.evil.com/x",
            "https://fbcdn.net@evil.com/x",
            "https://user:pw@scontent.fbcdn.net/x",
            "http://scontent.fbcdn.net/x",
            "https://evil.example/x",
            "https://127.0.0.1/x",
            "file:///etc/passwd",
        ] {
            assert!(!is_allowed_host(&u(bad), &none), "{bad}");
        }
    }

    #[test]
    fn extra_hosts_are_exact_and_http_is_loopback_only() {
        let extra = ["127.0.0.1".to_string(), "files.example.org".to_string()];
        assert!(is_allowed_host(&u("http://127.0.0.1:9/x"), &extra));
        assert!(is_allowed_host(&u("https://files.example.org/x"), &extra));
        assert!(!is_allowed_host(&u("http://files.example.org/x"), &extra));
        assert!(!is_allowed_host(
            &u("https://a.files.example.org/x"),
            &extra
        ));
        assert!(!is_allowed_host(&u("http://localhost:9/x"), &extra));
    }

    #[test]
    fn token_only_on_exact_listed_hosts() {
        let token = Secret::new("t");
        let hosts = ["lookaside.fbsbx.com".to_string()];
        let mode = TokenMode::OnHosts {
            token: &token,
            hosts: &hosts,
        };
        assert!(token_for(&mode, &u("https://lookaside.fbsbx.com/x")).is_some());
        assert!(token_for(&mode, &u("https://other.fbsbx.com/x")).is_none());
        assert!(token_for(&mode, &u("https://x.lookaside.fbsbx.com/x")).is_none());
        assert!(token_for(&TokenMode::Never, &u("https://lookaside.fbsbx.com/x")).is_none());
    }
}
