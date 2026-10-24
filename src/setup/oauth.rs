//! Facebook Login: dialog URL, redirect handling (loopback listener or pasted URL) and code exchange.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use clap::ValueEnum;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use super::report::guard;
use crate::config::is_loopback_host;
use crate::config::secret::{Secret, random_hex};
use crate::graph::error::as_i64;
use crate::graph::{GraphClient, GraphError, token};
use crate::webhook::signature::ct_str_eq;

pub const DEFAULT_PORT: u16 = 8766;
pub const MESSENGER_SCOPES: &[&str] = &[
    "pages_show_list",
    "pages_manage_metadata",
    "pages_messaging",
    "pages_read_engagement",
    "business_management",
];
pub const WHATSAPP_SCOPES: &[&str] = &[
    "whatsapp_business_messaging",
    "whatsapp_business_management",
];
pub const INSTAGRAM_SCOPES: &[&str] = &["instagram_basic", "instagram_manage_messages"];

const DIALOG_HOST: &str = "https://www.facebook.com";
const SERVER_GRACE: Duration = Duration::from_secs(2);

/// How the authorization code gets back to the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RedirectMode {
    /// Listen on http://localhost:PORT/callback (default)
    Loopback,
    /// Nothing listens; paste the address bar URL back
    Paste,
}

/// The redirect URI sent to Facebook and, in loopback mode, where the listener serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub mode: RedirectMode,
    pub uri: String,
    pub port: u16,
    pub path: String,
}

/// Scopes to ask for, in a stable order without duplicates.
pub fn scopes_for(messenger: bool, whatsapp: bool, instagram: bool) -> Vec<String> {
    let mut scopes: Vec<&str> = Vec::new();
    if messenger {
        scopes.extend(MESSENGER_SCOPES);
    }
    if whatsapp {
        scopes.push("business_management");
        scopes.extend(WHATSAPP_SCOPES);
    }
    if instagram {
        scopes.push("pages_show_list");
        scopes.extend(INSTAGRAM_SCOPES);
    }
    let mut seen = Vec::new();
    for s in scopes {
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    seen.into_iter().map(str::to_string).collect()
}

/// A fresh CSRF state: 32 random bytes as lowercase hex.
pub fn new_state() -> String {
    random_hex(32)
}

/// Constant-time comparison of the state sent with the state that came back.
pub fn state_matches(expected: &str, got: &str) -> bool {
    ct_str_eq(expected, got)
}

/// What goes into the Facebook Login dialog URL.
pub struct Dialog<'a> {
    pub version: &'a str,
    pub app_id: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub scopes: &'a [String],
    pub config_id: Option<&'a str>,
    pub rerequest: bool,
}

/// The `dialog/oauth` URL; a `config_id` replaces `scope`, which Meta recommends leaving out then.
pub fn dialog_url(d: &Dialog<'_>) -> String {
    let mut url = Url::parse(DIALOG_HOST).expect("static dialog host");
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.push(d.version).push("dialog").push("oauth");
    }
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", d.app_id)
            .append_pair("redirect_uri", d.redirect_uri)
            .append_pair("state", d.state)
            .append_pair("response_type", "code");
        match d.config_id {
            Some(id) => {
                query.append_pair("config_id", id);
            }
            None if !d.scopes.is_empty() => {
                query.append_pair("scope", &d.scopes.join(","));
            }
            None => {}
        }
        if d.rerequest {
            query.append_pair("auth_type", "rerequest");
        }
    }
    url.to_string()
}

/// The parameters Facebook appends to the redirect URI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedirectParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub error_description: Option<String>,
}

impl RedirectParams {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Reads the redirect parameters out of a raw query string.
pub fn parse_query(query: &str) -> RedirectParams {
    let mut params = RedirectParams::default();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match k.as_ref() {
            "code" => &mut params.code,
            "state" => &mut params.state,
            "error" => &mut params.error,
            "error_reason" => &mut params.error_reason,
            "error_description" => &mut params.error_description,
            _ => continue,
        };
        *slot = Some(v.into_owned());
    }
    params
}

/// Accepts what a person pastes: the full redirect URL, its query string or a bare code.
pub fn parse_pasted(input: &str) -> Result<RedirectParams, String> {
    let trimmed = input
        .trim()
        .trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'))
        .trim();
    let trimmed = trimmed.strip_suffix("#_=_").unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Err("nothing was pasted".to_string());
    }
    if trimmed.contains("://") {
        let url = Url::parse(trimmed).map_err(|_| "that is not a valid URL".to_string())?;
        let params = url.query().map(parse_query).unwrap_or_default();
        if !params.is_empty() {
            return Ok(params);
        }
        return Ok(url.fragment().map(parse_query).unwrap_or_default());
    }
    if trimmed.contains('=') {
        return Ok(parse_query(trimmed.trim_start_matches('?')));
    }
    let code = trimmed;
    if code.chars().any(char::is_whitespace) {
        return Err("that does not look like a URL or an authorization code".to_string());
    }
    Ok(RedirectParams {
        code: Some(code.to_string()),
        ..RedirectParams::default()
    })
}

/// A loopback listener serves plain http on IPv4, so the redirect URI must be exactly that.
fn serve_port_for(url: &Url, port: Option<u16>) -> Result<u16, String> {
    if url.scheme() != "http" {
        return Err(
            "a loopback listener serves plain http: use an http:// redirect URI, or --redirect-mode paste for https"
                .to_string(),
        );
    }
    if matches!(url.host(), Some(url::Host::Ipv6(_))) {
        return Err(
            "the loopback listener is IPv4: use localhost or 127.0.0.1 in --redirect-uri"
                .to_string(),
        );
    }
    let Some(uri_port) = url.port() else {
        return Err(
            "give --redirect-uri an explicit port, for example http://localhost:8766/callback: the listener cannot sit on the default port 80"
                .to_string(),
        );
    };
    match port {
        Some(p) if p != uri_port => Err(format!(
            "--port {p} contradicts port {uri_port} in --redirect-uri: the browser will go to {uri_port}"
        )),
        _ => Ok(uri_port),
    }
}

/// Picks the redirect URI and listener port for a mode; `--redirect-uri` overrides the URI.
pub fn resolve_redirect(
    mode: RedirectMode,
    uri: Option<&str>,
    port: Option<u16>,
) -> Result<Redirect, String> {
    if port == Some(0) {
        return Err("--port must be between 1 and 65535".to_string());
    }
    let Some(raw) = uri.map(str::trim).filter(|s| !s.is_empty()) else {
        let port = port.unwrap_or(DEFAULT_PORT);
        let scheme = match mode {
            RedirectMode::Loopback => "http",
            RedirectMode::Paste => "https",
        };
        return Ok(Redirect {
            mode,
            uri: format!("{scheme}://localhost:{port}/callback"),
            port,
            path: "/callback".to_string(),
        });
    };
    let url = Url::parse(raw).map_err(|e| format!("--redirect-uri is not a valid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("--redirect-uri must be an http(s) URL with a host".to_string());
    }
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err("--redirect-uri cannot carry a fragment or credentials".to_string());
    }
    let loopback = url.host_str().is_some_and(is_loopback_host);
    let served_port = if mode == RedirectMode::Loopback && loopback {
        serve_port_for(&url, port)?
    } else {
        let local_port = url
            .host_str()
            .filter(|h| is_loopback_host(h))
            .and_then(|_| url.port());
        port.or(local_port).unwrap_or(DEFAULT_PORT)
    };
    Ok(Redirect {
        mode,
        uri: raw.to_string(),
        port: served_port,
        path: url.path().to_string(),
    })
}

/// Why the browser leg produced no authorization code.
#[derive(Debug)]
pub enum CallbackError {
    Timeout { wrong_state: u32 },
    Denied { reason: String, description: String },
    Server(String),
}

impl fmt::Display for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout { wrong_state: 0 } => f.write_str("no callback received"),
            Self::Timeout { wrong_state } => write!(
                f,
                "no valid callback received ({wrong_state} request(s) carried a wrong state and were refused)"
            ),
            Self::Denied {
                reason,
                description,
            } => {
                write!(f, "Facebook reported `{reason}`")?;
                if !description.is_empty() {
                    write!(f, ": {description}")?;
                }
                Ok(())
            }
            Self::Server(m) => f.write_str(m),
        }
    }
}

struct Shared {
    path: String,
    state: String,
    tx: mpsc::Sender<Result<String, CallbackError>>,
    wrong_state: AtomicU32,
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>meta-mcp</title></head>\
         <body style=\"font-family:sans-serif;max-width:32rem;margin:4rem auto\"><h1>meta-mcp</h1><p>{}</p></body></html>",
        html_escape(message)
    );
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

async fn callback(State(shared): State<Arc<Shared>>, method: Method, uri: Uri) -> Response {
    if uri.path() != shared.path {
        return page(StatusCode::NOT_FOUND, "Not found.");
    }
    if method != Method::GET {
        return page(StatusCode::METHOD_NOT_ALLOWED, "Use GET.");
    }
    let params = parse_query(uri.query().unwrap_or_default());
    let state_ok = params
        .state
        .as_deref()
        .is_some_and(|s| state_matches(&shared.state, s));
    if !state_ok {
        shared.wrong_state.fetch_add(1, Ordering::Relaxed);
        return page(
            StatusCode::BAD_REQUEST,
            "Sign-in refused: the state value does not match this attempt. Start again with meta-mcp setup.",
        );
    }
    if params.error.is_some() || params.error_reason.is_some() {
        let reason = params
            .error_reason
            .or(params.error)
            .unwrap_or_else(|| "unknown".to_string());
        let description = params.error_description.unwrap_or_default();
        let shown = format!("Sign-in cancelled: Facebook reported {reason} {description}");
        let _ = shared.tx.try_send(Err(CallbackError::Denied {
            reason,
            description,
        }));
        return page(StatusCode::BAD_REQUEST, shown.trim());
    }
    match params.code.filter(|c| !c.is_empty()) {
        Some(code) => {
            let _ = shared.tx.try_send(Ok(code));
            page(StatusCode::OK, "Signed in. You can close this tab.")
        }
        None => page(StatusCode::BAD_REQUEST, "The callback carried no code."),
    }
}

/// Serves the callback on `listener` until a valid one arrives, Facebook reports an error or time runs out.
pub async fn wait_for_callback(
    listener: TcpListener,
    path: &str,
    state: &str,
    timeout: Duration,
) -> Result<String, CallbackError> {
    let (tx, mut rx) = mpsc::channel(1);
    let shared = Arc::new(Shared {
        path: path.to_string(),
        state: state.to_string(),
        tx,
        wrong_state: AtomicU32::new(0),
    });
    let app = Router::new()
        .fallback(callback)
        .with_state(Arc::clone(&shared));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
    });
    let outcome = tokio::time::timeout(timeout, rx.recv()).await;
    let _ = stop_tx.send(());
    if tokio::time::timeout(SERVER_GRACE, &mut server)
        .await
        .is_err()
    {
        server.abort();
    }
    match outcome {
        Ok(Some(result)) => result,
        Ok(None) => Err(CallbackError::Server(
            "the callback listener stopped".into(),
        )),
        Err(_) => Err(CallbackError::Timeout {
            wrong_state: shared.wrong_state.load(Ordering::Relaxed),
        }),
    }
}

/// Reads one line from stdin on a detached thread so a stuck read never blocks process exit.
pub async fn read_stdin_line(timeout: Duration) -> Result<String, String> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::stdin().read_line(&mut line).map(|n| (n, line));
        let _ = tx.send(read);
    });
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok((0, _)))) => Err("input closed before the redirect URL arrived".to_string()),
        Ok(Ok(Ok((_, line)))) => Ok(line),
        Ok(Ok(Err(e))) => Err(format!("cannot read the pasted URL: {e}")),
        Ok(Err(_)) => Err("the input reader stopped".to_string()),
        Err(_) => Err(format!(
            "nothing was pasted within {} seconds",
            timeout.as_secs()
        )),
    }
}

/// A long-lived user token as Graph issued it.
pub struct LongLived {
    pub token: Secret,
    pub expires_in: Option<i64>,
}

/// Code to short-lived token to long-lived token, neither step carrying a Bearer header.
pub async fn exchange_code_chain(
    graph: &GraphClient,
    app_id: &str,
    app_secret: &Secret,
    redirect_uri: &str,
    code: &str,
) -> Result<LongLived, GraphError> {
    let short = token::exchange_code(graph, app_id, app_secret, redirect_uri, code).await?;
    guard(&short.access_token);
    let long = token::exchange_long_lived(graph, app_id, app_secret, &short.access_token).await?;
    guard(&long.access_token);
    Ok(LongLived {
        token: long.access_token,
        expires_in: long.expires_in,
    })
}

/// `fb_exchange_token` with extra query pairs, for refreshes that need `set_token_expires_in_60_days`.
pub async fn exchange_fb_token(
    graph: &GraphClient,
    app_id: &str,
    app_secret: &Secret,
    input: &Secret,
    sixty_days: bool,
) -> Result<LongLived, GraphError> {
    let mut query = vec![
        ("grant_type", "fb_exchange_token".to_string()),
        ("client_id", app_id.to_string()),
        ("client_secret", app_secret.expose().to_string()),
        ("fb_exchange_token", input.expose().to_string()),
    ];
    if sixty_days {
        query.push(("set_token_expires_in_60_days", "true".to_string()));
    }
    let body: Value = graph.get("oauth/access_token", None, &query).await?;
    let token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GraphError::Json("the answer has no access_token".into()))?;
    let token = Secret::new(token);
    guard(&token);
    Ok(LongLived {
        token,
        expires_in: body.get("expires_in").and_then(as_i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_of(url: &str) -> Vec<(String, String)> {
        Url::parse(url)
            .expect("url")
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn dialog_url_carries_scopes_state_and_options() {
        let scopes = scopes_for(true, true, false);
        let state = new_state();
        let url = dialog_url(&Dialog {
            version: "v26.0",
            app_id: "123",
            redirect_uri: "http://localhost:8766/callback",
            state: &state,
            scopes: &scopes,
            config_id: None,
            rerequest: false,
        });
        assert!(
            url.starts_with("https://www.facebook.com/v26.0/dialog/oauth?"),
            "{url}"
        );
        let q = query_of(&url);
        assert_eq!(value(&q, "client_id"), Some("123"));
        assert_eq!(
            value(&q, "redirect_uri"),
            Some("http://localhost:8766/callback")
        );
        assert_eq!(value(&q, "state"), Some(state.as_str()));
        assert_eq!(value(&q, "response_type"), Some("code"));
        let scope = value(&q, "scope").expect("scope");
        for wanted in [
            "pages_messaging",
            "pages_manage_metadata",
            "pages_read_engagement",
            "pages_show_list",
            "business_management",
            "whatsapp_business_messaging",
            "whatsapp_business_management",
        ] {
            assert!(scope.split(',').any(|s| s == wanted), "{wanted} in {scope}");
        }
        assert_eq!(value(&q, "auth_type"), None);
        assert_eq!(value(&q, "config_id"), None);

        let rerequest = dialog_url(&Dialog {
            rerequest: true,
            ..Dialog {
                version: "v26.0",
                app_id: "123",
                redirect_uri: "https://localhost:8766/callback",
                state: "s",
                scopes: &scopes,
                config_id: Some("999"),
                rerequest: false,
            }
        });
        let q = query_of(&rerequest);
        assert_eq!(value(&q, "auth_type"), Some("rerequest"));
        assert_eq!(value(&q, "config_id"), Some("999"));
        assert_eq!(value(&q, "scope"), None, "config_id replaces scope");
    }

    #[test]
    fn scopes_follow_the_flags_without_duplicates() {
        let messenger = scopes_for(true, false, false);
        assert_eq!(messenger.len(), 5);
        assert!(!messenger.iter().any(|s| s.starts_with("whatsapp")));
        let both = scopes_for(true, true, true);
        assert_eq!(
            both.iter().filter(|s| *s == "business_management").count(),
            1
        );
        assert_eq!(both.iter().filter(|s| *s == "pages_show_list").count(), 1);
        assert!(both.contains(&"instagram_manage_messages".to_string()));
        let wa_only = scopes_for(false, true, false);
        assert_eq!(
            wa_only,
            vec![
                "business_management",
                "whatsapp_business_messaging",
                "whatsapp_business_management"
            ]
        );
    }

    #[test]
    fn redirect_urls_parse_code_state_and_errors() {
        let ok = parse_pasted("https://localhost:8766/callback?code=AQB-x_y&state=abc123#_=_")
            .expect("parsed");
        assert_eq!(ok.code.as_deref(), Some("AQB-x_y"));
        assert_eq!(ok.state.as_deref(), Some("abc123"));
        assert!(ok.error.is_none());

        let denied = parse_pasted(
            "http://localhost:8766/callback?error_reason=user_denied&error=access_denied&error_description=Permissions+error&state=s1",
        )
        .expect("parsed");
        assert_eq!(denied.error_reason.as_deref(), Some("user_denied"));
        assert_eq!(denied.error.as_deref(), Some("access_denied"));
        assert_eq!(
            denied.error_description.as_deref(),
            Some("Permissions error")
        );
        assert_eq!(denied.state.as_deref(), Some("s1"));
        assert!(denied.code.is_none());

        let query_only = parse_pasted("?code=c1&state=s2").expect("parsed");
        assert_eq!(
            (query_only.code.as_deref(), query_only.state.as_deref()),
            (Some("c1"), Some("s2"))
        );
        let bare = parse_pasted("  AQDabc123#_=_ ").expect("parsed");
        assert_eq!(bare.code.as_deref(), Some("AQDabc123"));
        assert!(bare.state.is_none());
        let quoted = parse_pasted("\"http://localhost:1/cb?code=q&state=z\"").expect("parsed");
        assert_eq!(quoted.code.as_deref(), Some("q"));
        assert!(parse_pasted("   ").is_err());
        assert!(parse_pasted("two words").is_err());
        assert!(parse_pasted("http://").is_err());
    }

    #[test]
    fn state_is_32_random_bytes_of_lowercase_hex() {
        let a = new_state();
        let b = new_state();
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_ne!(a, b);
        assert!(state_matches(&a, &a.clone()));
        assert!(!state_matches(&a, &b));
        assert!(!state_matches(&a, "short"));
    }

    #[test]
    fn redirect_defaults_and_overrides() {
        let loopback = resolve_redirect(RedirectMode::Loopback, None, None).expect("redirect");
        assert_eq!(loopback.uri, "http://localhost:8766/callback");
        assert_eq!((loopback.port, loopback.path.as_str()), (8766, "/callback"));
        let paste = resolve_redirect(RedirectMode::Paste, None, Some(9000)).expect("redirect");
        assert_eq!(paste.uri, "https://localhost:9000/callback");
        let custom = resolve_redirect(
            RedirectMode::Loopback,
            Some("http://127.0.0.1:9100/cb"),
            None,
        )
        .expect("redirect");
        assert_eq!((custom.port, custom.path.as_str()), (9100, "/cb"));
        assert_eq!(custom.uri, "http://127.0.0.1:9100/cb");
        let tunnel = resolve_redirect(
            RedirectMode::Loopback,
            Some("https://abc.example.org/auth/callback"),
            Some(7000),
        )
        .expect("redirect");
        assert_eq!(
            (tunnel.port, tunnel.path.as_str()),
            (7000, "/auth/callback")
        );
        assert!(resolve_redirect(RedirectMode::Paste, Some("ftp://x"), None).is_err());
        assert!(
            resolve_redirect(RedirectMode::Paste, Some("https://x.org/cb#frag"), None).is_err()
        );
        assert!(resolve_redirect(RedirectMode::Paste, Some("nonsense"), None).is_err());
        assert!(resolve_redirect(RedirectMode::Loopback, None, Some(0)).is_err());
    }

    #[test]
    fn a_loopback_listener_only_accepts_redirects_it_can_serve() {
        let loopback = |uri: &str, port: Option<u16>| {
            resolve_redirect(RedirectMode::Loopback, Some(uri), port)
        };
        let https = loopback("https://localhost:8766/callback", None).expect_err("https");
        assert!(https.contains("--redirect-mode paste"), "{https}");
        let v6 = loopback("http://[::1]:9000/cb", None).expect_err("ipv6");
        assert!(v6.contains("IPv4"), "{v6}");
        let clash = loopback("http://localhost:9000/cb", Some(9001)).expect_err("clash");
        assert!(clash.contains("9001") && clash.contains("9000"), "{clash}");
        assert_eq!(
            loopback("http://localhost:9000/cb", Some(9000))
                .expect("same")
                .port,
            9000
        );
        let no_port = loopback("http://localhost/cb", None).expect_err("port-less");
        assert!(no_port.contains("explicit port"), "{no_port}");
        let tunnel = loopback("https://abc.example.org/cb", None).expect("tunnel");
        assert_eq!(tunnel.port, DEFAULT_PORT);
        let paste = resolve_redirect(
            RedirectMode::Paste,
            Some("https://localhost:8766/callback"),
            None,
        )
        .expect("paste mode never listens");
        assert_eq!(paste.uri, "https://localhost:8766/callback");
    }

    async fn serve(
        path: &str,
        state: &str,
        timeout: Duration,
    ) -> (u16, tokio::task::JoinHandle<Result<String, CallbackError>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (path, state) = (path.to_string(), state.to_string());
        let handle =
            tokio::spawn(async move { wait_for_callback(listener, &path, &state, timeout).await });
        (port, handle)
    }

    #[tokio::test]
    async fn listener_refuses_wrong_state_then_accepts_the_right_one() {
        let (port, handle) = serve("/callback", "good-state", Duration::from_secs(10)).await;
        let http = reqwest::Client::new();
        let bad = http
            .get(format!(
                "http://127.0.0.1:{port}/callback?code=evil&state=bad"
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(bad.status(), 400);
        let missing = http
            .get(format!("http://127.0.0.1:{port}/callback?code=evil"))
            .send()
            .await
            .expect("send");
        assert_eq!(missing.status(), 400);
        let other = http
            .get(format!("http://127.0.0.1:{port}/favicon.ico"))
            .send()
            .await
            .expect("send");
        assert_eq!(other.status(), 404);
        let good = http
            .get(format!(
                "http://127.0.0.1:{port}/callback?code=the-code&state=good-state"
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(good.status(), 200);
        assert!(
            good.text()
                .await
                .expect("body")
                .contains("Signed in. You can close this tab.")
        );
        assert_eq!(handle.await.expect("join").expect("code"), "the-code");
    }

    #[tokio::test]
    async fn listener_reports_facebook_errors_and_escapes_them() {
        let (port, handle) = serve("/callback", "s", Duration::from_secs(10)).await;
        let resp = reqwest::Client::new()
            .get(format!(
                "http://127.0.0.1:{port}/callback?state=s&error_reason=user_denied&error=access_denied&error_description=%3Cb%3Eno%3C/b%3E"
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 400);
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("user_denied") && !body.contains("<b>"),
            "{body}"
        );
        match handle.await.expect("join") {
            Err(CallbackError::Denied { reason, .. }) => assert_eq!(reason, "user_denied"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn listener_times_out_and_counts_refused_requests() {
        let (port, handle) = serve("/callback", "s", Duration::from_millis(600)).await;
        let resp = reqwest::Client::new()
            .get(format!(
                "http://127.0.0.1:{port}/callback?code=x&state=nope"
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 400);
        match handle.await.expect("join") {
            Err(CallbackError::Timeout { wrong_state }) => assert_eq!(wrong_state, 1),
            other => panic!("unexpected {other:?}"),
        }
    }
}
