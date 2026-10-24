//! Webhook receiver: verification handshake, signed ingest into the inbox, health.

pub mod normalize;
pub mod signature;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::config::Secret;
use crate::inbox::{Inbox, now_secs};
use signature::{HEADER, ct_str_eq, verify};

/// Largest accepted webhook body.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Secrets and storage shared by the webhook handlers.
#[derive(Clone)]
pub struct WebhookState {
    pub app_secret: Secret,
    pub verify_token: Secret,
    pub inbox: Inbox,
}

impl WebhookState {
    pub fn new(app_secret: Secret, verify_token: Secret, inbox: Inbox) -> Self {
        Self {
            app_secret,
            verify_token,
            inbox,
        }
    }
}

/// `GET`/`POST /webhook` only, for mounting next to other routes.
pub fn routes(state: Arc<WebhookState>) -> Router {
    Router::new()
        .route("/webhook", get(verify_handshake).post(ingest))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// The standalone receiver: `/webhook` plus `GET /health`.
pub fn router(state: Arc<WebhookState>) -> Router {
    routes(state.clone()).merge(
        Router::new()
            .route("/health", get(health))
            .with_state(state),
    )
}

fn query_value(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "verification failed").into_response()
}

async fn verify_handshake(
    State(state): State<Arc<WebhookState>>,
    RawQuery(query): RawQuery,
) -> Response {
    let query = query.unwrap_or_default();
    let token_ok = query_value(&query, "hub.verify_token").is_some_and(|t| {
        !state.verify_token.is_empty() && ct_str_eq(&t, state.verify_token.expose())
    });
    let subscribe = query_value(&query, "hub.mode").as_deref() == Some("subscribe");
    match query_value(&query, "hub.challenge") {
        Some(challenge) if token_ok && subscribe => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            challenge,
        )
            .into_response(),
        _ => {
            tracing::warn!("webhook verification refused");
            forbidden()
        }
    }
}

async fn ingest(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signature = headers.get(HEADER).and_then(|v| v.to_str().ok());
    if !verify(state.app_secret.expose(), &body, signature) {
        tracing::warn!("webhook rejected: missing or invalid signature");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
    };
    let events = normalize::normalize(&payload, now_secs());
    let count = events.len();
    match state.inbox.insert_async(events).await {
        Ok(stored) => {
            tracing::info!(events = count, stored, "webhook batch accepted");
            (StatusCode::OK, "EVENT_RECEIVED").into_response()
        }
        Err(e) => {
            tracing::error!("webhook storage failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response()
        }
    }
}

async fn health(State(state): State<Arc<WebhookState>>) -> Response {
    match state.inbox.stats_async().await {
        Ok(s) => axum::Json(json!({
            "ok": true,
            "events": s.total,
            "unprocessed": s.unprocessed,
            "last_received_at": s.last_received_at,
        }))
        .into_response(),
        Err(e) => {
            tracing::error!("health check failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"ok": false, "error": "inbox unavailable"})),
            )
                .into_response()
        }
    }
}
