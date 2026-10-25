//! Transports: stdio, streamable HTTP (optionally with the webhook receiver) and the webhook alone.

pub mod auth;
pub mod host_guard;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use axum::Router;
use axum::http::Request;
use axum::middleware::from_fn_with_state;
use axum::routing::{any_service, get};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, Secret, is_loopback_host};
use crate::graph::{GraphClient, token};
use crate::inbox::Inbox;
use crate::mcp::MetaMcp;
use crate::webhook::{self, WebhookState};

const MCP_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Logs to stderr (stdout carries the MCP stream); filter from `META_MCP_LOG`.
pub fn init_tracing() {
    init_tracing_ansi(true);
}

/// `init_tracing` with ANSI colours on or off (off when stderr is a log file).
pub fn init_tracing_ansi(ansi: bool) {
    let filter =
        EnvFilter::try_from_env("META_MCP_LOG").unwrap_or_else(|_| EnvFilter::new("meta_mcp=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Fills in the id of an env-provided Page token with `GET /me`; fails fast when that fails.
pub async fn resolve_env_page(cfg: &mut Config) -> anyhow::Result<()> {
    if !cfg.needs_page_resolution() {
        return Ok(());
    }
    let graph = GraphClient::new(&cfg.graph, Vec::new())?;
    for page in cfg.pages.iter_mut().filter(|p| p.id.is_empty()) {
        let me = token::me(&graph, &page.token).await.map_err(|e| {
            anyhow!("cannot resolve the Page id from META_PAGE_ACCESS_TOKEN ({e}); set META_PAGE_ID or fix the token")
        })?;
        let id = me
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        page.id = id
            .ok_or_else(|| anyhow!("GET /me returned no id; set META_PAGE_ID"))?
            .to_string();
        if let Some(name) = me.get("name").and_then(|v| v.as_str()) {
            page.name = name.to_string();
        }
        tracing::info!("resolved the Page id for {}", page.name);
    }
    Ok(())
}

/// Serves MCP over stdin and stdout until the client disconnects.
pub async fn run_stdio(mut cfg: Config) -> anyhow::Result<()> {
    resolve_env_page(&mut cfg).await?;
    let mcp = MetaMcp::new(cfg)?;
    tracing::info!("serving Meta MCP over stdio");
    let running = rmcp::serve_server(mcp, (tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| anyhow!("stdio initialization failed: {e}"))?;
    if let Err(e) = running.waiting().await {
        tracing::error!("stdio session ended with error: {e}");
    }
    Ok(())
}

/// The app secret and verify token the webhook needs, or an error naming where to set them.
pub fn webhook_secrets(cfg: &Config) -> anyhow::Result<(Secret, Secret)> {
    let secret = cfg.graph.app_secret.clone();
    let verify = cfg.webhook.verify_token.clone();
    match (secret, verify) {
        (Some(s), Some(v)) => Ok((s, v)),
        _ => Err(anyhow!(
            "the webhook needs the app secret and a verify token: set META_APP_SECRET and \
             META_WEBHOOK_VERIFY_TOKEN, or run `meta-mcp setup`"
        )),
    }
}

fn trace_layer() -> TraceLayer<
    tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>,
    impl Fn(&Request<axum::body::Body>) -> tracing::Span + Clone,
> {
    TraceLayer::new_for_http().make_span_with(|req: &Request<axum::body::Body>| {
        tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
    })
}

fn bind_addr(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    format!("{host}:{port}")
        .parse()
        .or_else(|_| format!("[{host}]:{port}").parse())
        .map_err(|_| anyhow!("META_MCP_HOST must be an IP address"))
}

fn webhook_host_entry(cfg: &Config) -> Option<String> {
    let url = cfg.webhook.public_url.as_ref()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// The HTTP app: `/health` open, `/mcp` behind the API key, optional `/webhook`, Host guard on all.
pub fn build_http_router(cfg: Config, with_webhook: bool) -> anyhow::Result<Router> {
    let secrets = if with_webhook {
        Some(webhook_secrets(&cfg)?)
    } else {
        None
    };
    let mut extra_hosts = cfg.http.allowed_hosts.clone();
    if with_webhook {
        extra_hosts.extend(webhook_host_entry(&cfg));
    }
    let allowed = Arc::new(host_guard::build_allowlist(&cfg.http.host, &extra_hosts));
    let api_keys = Arc::new(cfg.http.api_keys.clone());
    let mcp = MetaMcp::new_http(cfg)?;
    let inbox = mcp.inbox.clone();

    let mut mcp_config = StreamableHttpServerConfig::default();
    mcp_config.legacy_session_mode = false;
    mcp_config.json_response = true;
    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(NeverSessionManager::default()),
        mcp_config.disable_allowed_hosts(),
    );
    let mcp_route = Router::new()
        .route("/mcp", any_service(service))
        .layer(RequestBodyLimitLayer::new(MCP_BODY_LIMIT))
        .route_layer(from_fn_with_state(api_keys, auth::require_api_key));
    let mut app = Router::new()
        .route("/health", get(|| async { axum::Json(json!({"ok": true})) }))
        .merge(mcp_route);
    if let Some((app_secret, verify_token)) = secrets {
        let state = Arc::new(WebhookState::new(app_secret, verify_token, inbox));
        app = app.merge(webhook::routes(state));
    }
    Ok(app
        .layer(from_fn_with_state(
            allowed,
            host_guard::require_allowed_host,
        ))
        .layer(trace_layer()))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

async fn serve(addr: SocketAddr, app: Router, what: &str) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot bind {addr}"))?;
    if !is_loopback_host(&addr.ip().to_string()) {
        tracing::warn!(
            "binding {addr}, which is not loopback: put TLS and access control in front"
        );
    }
    tracing::info!("serving {what} on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Serves MCP over streamable HTTP, with the webhook receiver mounted when asked.
pub async fn run_http(mut cfg: Config, with_webhook: bool) -> anyhow::Result<()> {
    cfg.require_api_keys()?;
    resolve_env_page(&mut cfg).await?;
    let addr = bind_addr(&cfg.http.host, cfg.http.port)?;
    if with_webhook && cfg.webhook.public_url.is_none() {
        tracing::warn!(
            "the webhook has no META_WEBHOOK_PUBLIC_URL: add its public host to META_MCP_ALLOWED_HOSTS or Meta gets 403"
        );
    }
    let keys = cfg.http.api_keys.len();
    let app = build_http_router(cfg, with_webhook)?;
    tracing::info!("MCP over HTTP with {keys} API key(s), webhook: {with_webhook}");
    serve(addr, app, "Meta MCP over HTTP").await
}

/// Serves only the webhook receiver (`/webhook` and `/health`); needs no API keys.
pub async fn run_webhook(cfg: Config) -> anyhow::Result<()> {
    let (app_secret, verify_token) = webhook_secrets(&cfg)?;
    let inbox = Inbox::open(&cfg.inbox_db)?;
    let state = Arc::new(WebhookState::new(app_secret, verify_token, inbox));
    let app = webhook::router(state).layer(trace_layer());
    serve(cfg.webhook.listen, app, "the Meta webhook receiver").await
}
