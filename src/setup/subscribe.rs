//! Webhook subscription: callback self-check, app, Page and WhatsApp subscriptions and their read-backs.

use std::time::Duration;

use reqwest::header::LOCATION;
use serde_json::Value;
use url::Url;

use crate::config::is_loopback_host;
use crate::config::secret::Secret;
use crate::graph::{GraphClient, GraphError};

/// Page webhook fields subscribed at both the app and the Page level.
pub const PAGE_FIELDS: [&str; 12] = [
    "messages",
    "message_echoes",
    "message_reads",
    "message_deliveries",
    "message_reactions",
    "message_edits",
    "messaging_postbacks",
    "messaging_referrals",
    "messaging_optins",
    "messaging_handovers",
    "standby",
    "messaging_policy_enforcement",
];

/// Where to configure WhatsApp webhooks by hand when the API refuses.
pub const WHATSAPP_DASHBOARD_STEPS: &str = "App Dashboard > WhatsApp > Configuration (or Use cases > Customize > Configuration): set the Callback URL and Verify token, press Verify and save, then subscribe to messages";

const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROBE_BODY: usize = 4096;

pub fn page_fields_csv() -> String {
    PAGE_FIELDS.join(",")
}

/// A webhook callback must be https; plain http is accepted for loopback hosts only.
pub fn parse_webhook_url(raw: &str) -> Result<Url, String> {
    let url =
        Url::parse(raw.trim()).map_err(|e| format!("`{}` is not a valid URL ({e})", raw.trim()))?;
    let host = url.host_str().ok_or("the URL has no host")?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        "http" => {
            return Err(
                "plain http is only accepted for localhost; Meta needs an https callback URL"
                    .to_string(),
            );
        }
        other => return Err(format!("unsupported scheme `{other}`; use https")),
    }
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return Err("the URL must not carry a fragment or credentials".to_string());
    }
    if url.query_pairs().any(|(k, _)| k.starts_with("hub.")) {
        return Err(
            "give the bare callback URL: Meta adds the hub.* parameters itself".to_string(),
        );
    }
    Ok(url)
}

/// True when a callback URL from Graph and ours point to the same place, trailing slash aside.
pub fn same_url(a: &str, b: &Url) -> bool {
    Url::parse(a).is_ok_and(|u| u == *b)
        || a.trim_end_matches('/') == b.as_str().trim_end_matches('/')
}

fn describe(e: reqwest::Error) -> String {
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
    msg
}

async fn read_capped(mut resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    while out.len() < MAX_PROBE_BODY {
        match resp.chunk().await {
            Ok(Some(chunk)) => out.extend_from_slice(&chunk),
            _ => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replays Meta's verification handshake against `url`; the error says why it would fail.
pub async fn check_callback(url: &Url, verify_token: &Secret) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("meta-mcp/", env!("CARGO_PKG_VERSION")))
        .timeout(SELF_CHECK_TIMEOUT)
        .connect_timeout(SELF_CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("cannot build the HTTP client: {}", e.without_url()))?;
    let challenge = rand::random_range(1_000_000_000_u64..=9_999_999_999_u64).to_string();
    let mut probe = url.clone();
    probe
        .query_pairs_mut()
        .append_pair("hub.mode", "subscribe")
        .append_pair("hub.verify_token", verify_token.expose())
        .append_pair("hub.challenge", &challenge);
    let resp = client.get(probe).send().await.map_err(describe)?;
    let status = resp.status().as_u16();
    let location = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = read_capped(resp).await;
    match status {
        200 if body.trim() == challenge => Ok(()),
        200 => Err("it answered 200 but did not echo the challenge: this is not the meta-mcp receiver, or a proxy rewrote the answer".to_string()),
        403 => Err("it answered 403: the verify token differs from the one the receiver runs with".to_string()),
        404 => Err("it answered 404: the receiver serves /webhook, check the path of the URL".to_string()),
        301 | 302 | 303 | 307 | 308 => Err(format!(
            "it answered {status} and redirects to {}: use that final URL",
            bare_location(url, location.as_deref()).unwrap_or_else(|| "another address".to_string())
        )),
        502..=504 => Err(format!(
            "it answered {status}: the tunnel or proxy is up but nothing answers behind it"
        )),
        other => Err(format!("it answered HTTP {other}")),
    }
}

/// The redirect target without query or fragment, which may echo the probe's hub.* parameters.
fn bare_location(base: &Url, location: Option<&str>) -> Option<String> {
    let mut target = base.join(location?).ok()?;
    target.set_query(None);
    target.set_fragment(None);
    Some(target.to_string())
}

fn names(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => i.get("name").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// One entry of `GET {app_id}/subscriptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSubscription {
    pub object: String,
    pub callback_url: String,
    pub fields: Vec<String>,
    pub active: bool,
}

pub async fn app_subscriptions(
    graph: &GraphClient,
    app_token: &Secret,
    app_id: &str,
) -> Result<Vec<AppSubscription>, GraphError> {
    let body = graph
        .get(&format!("{app_id}/subscriptions"), Some(app_token), &[])
        .await?;
    Ok(body
        .get("data")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    Some(AppSubscription {
                        object: i.get("object")?.as_str()?.to_string(),
                        callback_url: i
                            .get("callback_url")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        fields: names(i.get("fields")),
                        active: i.get("active").and_then(Value::as_bool).unwrap_or(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// App-level subscription; Meta calls `url` with the handshake before it answers.
pub async fn subscribe_app(
    graph: &GraphClient,
    app_token: &Secret,
    app_id: &str,
    object: &str,
    url: &Url,
    verify_token: &Secret,
    fields: &str,
) -> Result<(), GraphError> {
    let mut form = vec![
        ("object", object.to_string()),
        ("callback_url", url.to_string()),
        ("fields", fields.to_string()),
        ("verify_token", verify_token.expose().to_string()),
    ];
    if object == "page" {
        form.push(("include_values", "true".to_string()));
    }
    graph
        .post_form(&format!("{app_id}/subscriptions"), Some(app_token), &form)
        .await
        .map(|_| ())
}

pub async fn subscribe_page(
    graph: &GraphClient,
    page_id: &str,
    page_token: &Secret,
    fields: &str,
) -> Result<(), GraphError> {
    let form = [("subscribed_fields", fields.to_string())];
    graph
        .post_form(
            &format!("{page_id}/subscribed_apps"),
            Some(page_token),
            &form,
        )
        .await
        .map(|_| ())
}

/// Fields this app is subscribed to on the Page, from `GET {page_id}/subscribed_apps`.
pub async fn page_subscription(
    graph: &GraphClient,
    page_id: &str,
    page_token: &Secret,
    app_id: &str,
) -> Result<Vec<String>, GraphError> {
    let body = graph
        .get(&format!("{page_id}/subscribed_apps"), Some(page_token), &[])
        .await?;
    let entries = body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ours = entries
        .iter()
        .find(|e| id_of(e.get("id")).as_deref() == Some(app_id))
        .or_else(|| {
            if app_id.is_empty() {
                entries.first()
            } else {
                entries.iter().find(|e| e.get("id").is_none())
            }
        });
    Ok(names(ours.and_then(|e| e.get("subscribed_fields"))))
}

fn id_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Subscribes the app to a WABA; `over` adds a WABA-level callback override.
pub async fn subscribe_waba(
    graph: &GraphClient,
    waba_id: &str,
    token: &Secret,
    over: Option<(&Url, &Secret)>,
) -> Result<(), GraphError> {
    let path = format!("{waba_id}/subscribed_apps");
    match over {
        Some((url, verify_token)) => {
            let body = serde_json::json!({
                "override_callback_uri": url.to_string(),
                "verify_token": verify_token.expose(),
            });
            graph.post_json(&path, Some(token), &body).await.map(|_| ())
        }
        None => graph.post_form(&path, Some(token), &[]).await.map(|_| ()),
    }
}

/// What `GET {waba_id}/subscribed_apps` says about this app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WabaSubscription {
    pub subscribed: bool,
    pub override_uri: Option<String>,
}

pub async fn waba_subscription(
    graph: &GraphClient,
    waba_id: &str,
    token: &Secret,
    app_id: &str,
) -> Result<WabaSubscription, GraphError> {
    let body = graph
        .get(&format!("{waba_id}/subscribed_apps"), Some(token), &[])
        .await?;
    let ours = body
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|i| {
                let id = i
                    .pointer("/whatsapp_business_api_data/id")
                    .or_else(|| i.get("id"));
                id_of(id).as_deref() == Some(app_id)
            })
        });
    Ok(WabaSubscription {
        subscribed: ours.is_some(),
        override_uri: ours
            .and_then(|i| i.get("override_callback_uri"))
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::inbox::Inbox;
    use crate::webhook::{WebhookState, router};

    #[test]
    fn webhook_urls_need_https_unless_loopback() {
        assert!(parse_webhook_url("https://hooks.example.org/webhook").is_ok());
        assert!(parse_webhook_url("http://127.0.0.1:8787/webhook").is_ok());
        assert!(parse_webhook_url("http://localhost:8787/webhook").is_ok());
        assert!(parse_webhook_url("http://[::1]:8787/webhook").is_ok());
        assert!(parse_webhook_url("http://hooks.example.org/webhook").is_err());
        assert!(parse_webhook_url("ftp://hooks.example.org/webhook").is_err());
        assert!(parse_webhook_url("https://u:p@hooks.example.org/webhook").is_err());
        assert!(parse_webhook_url("https://hooks.example.org/webhook#x").is_err());
        assert!(parse_webhook_url("nonsense").is_err());
        assert!(parse_webhook_url("https://hooks.example.org/webhook?hub.mode=subscribe").is_err());
        assert!(parse_webhook_url("https://hooks.example.org/webhook?tenant=a").is_ok());
    }

    #[test]
    fn redirect_targets_lose_their_query() {
        let base = Url::parse("https://hooks.example.org/webhook").expect("url");
        let shown = bare_location(
            &base,
            Some("/webhook/?hub.mode=subscribe&hub.verify_token=ab%21&hub.challenge=1#x"),
        );
        assert_eq!(shown.as_deref(), Some("https://hooks.example.org/webhook/"));
        assert_eq!(
            bare_location(&base, Some("https://other.example.org/in?x=1")).as_deref(),
            Some("https://other.example.org/in")
        );
        assert_eq!(bare_location(&base, None), None);
    }

    #[test]
    fn page_fields_cover_the_messaging_events() {
        let csv = page_fields_csv();
        for f in ["messages", "message_echoes", "message_reads", "standby"] {
            assert!(csv.split(',').any(|x| x == f), "{f}");
        }
        assert_eq!(csv.split(',').count(), PAGE_FIELDS.len());
    }

    #[test]
    fn field_lists_parse_from_strings_objects_and_csv() {
        let v = json!(["messages", {"name": "message_echoes", "version": "v26.0"}, 5]);
        assert_eq!(names(Some(&v)), vec!["messages", "message_echoes"]);
        assert_eq!(names(Some(&json!("a, b,,c"))), vec!["a", "b", "c"]);
        assert!(names(None).is_empty());
    }

    async fn receiver(verify_token: &str) -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tmp");
        let inbox = Inbox::open(&dir.path().join("inbox.db")).expect("inbox");
        let state = Arc::new(WebhookState::new(
            Secret::new("app-secret"),
            Secret::new(verify_token),
            inbox,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.ok();
        });
        (base, dir)
    }

    #[tokio::test]
    async fn self_check_never_prints_the_verify_token_from_a_redirect() {
        use axum::http::{StatusCode, header};
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/webhook",
            get(|| async {
                (
                    StatusCode::MOVED_PERMANENTLY,
                    [(
                        header::LOCATION,
                        "/webhook/?hub.mode=subscribe&hub.verify_token=ab%21&hub.challenge=1",
                    )],
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/webhook")).expect("url");
        let err = check_callback(&url, &Secret::new("ab!"))
            .await
            .expect_err("redirect");
        assert!(err.contains("301") && err.contains("/webhook/"), "{err}");
        for leaked in ["hub.", "ab%21", "ab!", "challenge"] {
            assert!(!err.contains(leaked), "{leaked} in {err}");
        }
    }

    #[tokio::test]
    async fn self_check_accepts_the_receiver_and_explains_failures() {
        let (base, _dir) = receiver("right-token").await;
        let url = Url::parse(&format!("{base}/webhook")).expect("url");
        check_callback(&url, &Secret::new("right-token"))
            .await
            .expect("handshake");

        let err = check_callback(&url, &Secret::new("wrong-token"))
            .await
            .expect_err("403");
        assert!(err.contains("403") && err.contains("verify token"), "{err}");
        assert!(!err.contains("wrong-token"), "{err}");

        let missing = Url::parse(&format!("{base}/nowhere")).expect("url");
        let err = check_callback(&missing, &Secret::new("right-token"))
            .await
            .expect_err("404");
        assert!(err.contains("404"), "{err}");

        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr").port()
        };
        let url = Url::parse(&format!("http://127.0.0.1:{closed}/webhook")).expect("url");
        let err = check_callback(&url, &Secret::new("secret-verify-token"))
            .await
            .expect_err("refused");
        assert!(err.contains("could not connect"), "{err}");
        assert!(
            !err.contains("secret-verify-token") && !err.contains("hub."),
            "{err}"
        );
    }
}
