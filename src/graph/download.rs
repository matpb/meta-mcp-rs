//! Safe media download: host allowlist, never the Graph host, manual redirects, token only on listed hosts, size cap.

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
    "*.whatsapp.net",
    "lookaside.fbsbx.com",
];

/// Whether `download` attaches a Bearer token, and on which exact hosts.
/// An entry is `host` (any port) or `host:port` (only that port).
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

/// Splits `host` or `host:port` (a bracketed IPv6 host is fine); a bare entry has no port.
fn split_entry(entry: &str) -> (&str, Option<u16>) {
    match entry.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.ends_with(']') => {
            port.parse().map_or((entry, None), |p| (host, Some(p)))
        }
        _ => (entry, None),
    }
}

fn token_for<'a>(mode: &TokenMode<'a>, url: &Url) -> Option<&'a Secret> {
    match mode {
        TokenMode::Never => None,
        TokenMode::OnHosts { token, hosts } => {
            let host = url.host_str()?;
            let port = url.port_or_known_default();
            hosts
                .iter()
                .any(|entry| {
                    let (name, wanted) = split_entry(entry);
                    name.eq_ignore_ascii_case(host) && wanted.is_none_or(|p| Some(p) == port)
                })
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

fn refused_graph_host(url: &Url) -> GraphError {
    GraphError::Refused(format!(
        "refusing to download from `{}`: it is the Graph API host, where a fetch is an API call made with your credentials, not a media download; nothing was fetched",
        url.host_str().unwrap_or("?")
    ))
}

/// A host name without case or a trailing root dot, for comparing two spellings of one host.
fn bare_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

impl GraphClient {
    /// True when `url` is on the Graph base URL's host, whatever the port or path.
    pub fn is_graph_host(&self, url: &Url) -> bool {
        match (url.host_str(), self.base.host_str()) {
            (Some(a), Some(b)) => bare_host(a) == bare_host(b),
            _ => false,
        }
    }

    /// Fetches `url` with every hop checked against the allowlist, the Graph host and the size cap.
    pub async fn download(
        &self,
        url: &Url,
        token_mode: TokenMode<'_>,
        max_bytes: u64,
    ) -> Result<Downloaded, GraphError> {
        let mut current = url.clone();
        for hop in 0..=MAX_HOPS {
            if self.is_graph_host(&current) {
                return Err(refused_graph_host(&current));
            }
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
            "https://static.xx.fbcdn.net/x",
            "https://mmg.whatsapp.net/x",
            "https://SCONTENT.xx.fbcdn.net/x",
        ] {
            assert!(is_allowed_host(&u(ok), &none), "{ok}");
        }
        for bad in [
            "https://www.facebook.com/x",
            "https://graph.facebook.com/v26.0/me?fields=access_token",
            "https://business.facebook.com/x",
            "https://api.messenger.com/x",
            "https://facebook.com/x",
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

    fn graph_client(base: &str, hosts: &[&str]) -> GraphClient {
        let settings = crate::config::GraphSettings {
            base_url: base.to_string(),
            ..crate::config::GraphSettings::default()
        };
        let hosts = hosts.iter().map(|h| (*h).to_string()).collect();
        GraphClient::new(&settings, hosts).expect("client")
    }

    #[test]
    fn the_graph_host_is_recognised_in_every_spelling() {
        let c = graph_client("https://graph.facebook.com", &[]);
        for same in [
            "https://graph.facebook.com/v26.0/me",
            "https://GRAPH.facebook.com/x",
            "https://graph.facebook.com./x",
            "https://graph.facebook.com:8443/x",
            "http://graph.facebook.com/x",
        ] {
            assert!(c.is_graph_host(&u(same)), "{same}");
        }
        for other in [
            "https://scontent.xx.fbcdn.net/x",
            "https://x.graph.facebook.com/x",
            "https://graph.facebook.com.evil.example/x",
            "https://facebook.com/x",
            "https://lookaside.fbsbx.com/x",
        ] {
            assert!(!c.is_graph_host(&u(other)), "{other}");
        }
        let proxy = graph_client("http://127.0.0.1:4000/graph/", &[]);
        assert!(proxy.is_graph_host(&u("http://127.0.0.1:9/x")));
        assert!(proxy.is_graph_host(&u("http://127.1/x")));
        assert!(!proxy.is_graph_host(&u("http://localhost:4000/x")));
    }

    #[tokio::test]
    async fn the_graph_host_is_refused_in_every_mode_before_any_request() {
        let c = graph_client("https://graph.facebook.com", &["graph.facebook.com"]);
        let token = Secret::new("t");
        let hosts = ["graph.facebook.com".to_string()];
        let on_hosts = TokenMode::OnHosts {
            token: &token,
            hosts: &hosts,
        };
        for mode in [TokenMode::Never, on_hosts] {
            for target in [
                "https://graph.facebook.com/v26.0/me?fields=access_token",
                "https://graph.facebook.com:443/v26.0/1234/conversations",
            ] {
                let err = c.download(&u(target), mode, 1024).await.expect_err(target);
                let text = err.to_string();
                assert!(
                    matches!(err, GraphError::Refused(_))
                        && text.contains("Graph API host")
                        && text.contains("graph.facebook.com"),
                    "{target}: {text}"
                );
            }
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

    #[test]
    fn an_explicit_port_in_the_host_list_must_match() {
        let token = Secret::new("t");
        let send = |hosts: &[&str], url: &str| {
            let hosts: Vec<String> = hosts.iter().map(|h| (*h).to_string()).collect();
            let mode = TokenMode::OnHosts {
                token: &token,
                hosts: &hosts,
            };
            token_for(&mode, &u(url)).is_some()
        };
        assert!(send(&["127.0.0.1:9000"], "http://127.0.0.1:9000/x"));
        assert!(!send(&["127.0.0.1:9000"], "http://127.0.0.1:9001/x"));
        assert!(!send(&["127.0.0.1:9000"], "https://127.0.0.1/x"));
        assert!(send(
            &["lookaside.fbsbx.com:443"],
            "https://lookaside.fbsbx.com/x"
        ));
        assert!(!send(
            &["lookaside.fbsbx.com:443"],
            "https://lookaside.fbsbx.com:8443/x"
        ));
        assert!(send(
            &["LOOKASIDE.fbsbx.com:443"],
            "https://lookaside.fbsbx.com:443/x"
        ));
        assert!(send(&["[::1]:8080"], "http://[::1]:8080/x"));
        assert!(!send(&["[::1]:8080"], "http://[::1]:8081/x"));
        assert!(
            send(&["[::1]"], "http://[::1]:8081/x"),
            "a bare host has no port rule"
        );
        assert!(send(
            &["lookaside.fbsbx.com"],
            "https://lookaside.fbsbx.com:8443/x"
        ));
        assert!(!send(
            &["lookaside.fbsbx.com:abc"],
            "https://lookaside.fbsbx.com/x"
        ));
        assert!(!send(&[], "https://lookaside.fbsbx.com/x"));
    }

    #[test]
    fn host_entries_split_into_name_and_port() {
        assert_eq!(split_entry("example.org"), ("example.org", None));
        assert_eq!(split_entry("example.org:8443"), ("example.org", Some(8443)));
        assert_eq!(split_entry("[::1]:80"), ("[::1]", Some(80)));
        assert_eq!(split_entry("[::1]"), ("[::1]", None));
        assert_eq!(split_entry("::1"), ("::1", None));
        assert_eq!(
            split_entry("example.org:99999"),
            ("example.org:99999", None)
        );
    }
}
