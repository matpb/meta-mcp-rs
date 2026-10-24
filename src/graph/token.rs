//! OAuth and token inspection helpers used by setup, doctor and status.

use serde_json::Value;

use super::error::as_i64;
use super::{GraphClient, GraphError};
use crate::config::secret::Secret;

/// The body of an `oauth/access_token` answer.
#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access_token: Secret,
    pub token_type: Option<String>,
    pub expires_in: Option<i64>,
}

/// What `debug_token` reports about a token; `expires_at` is `None` for tokens that never expire.
#[derive(Debug, Clone)]
pub struct DebugToken {
    pub app_id: String,
    pub user_id: Option<String>,
    pub is_valid: bool,
    pub expires_at: Option<i64>,
    pub issued_at: Option<i64>,
    pub scopes: Vec<String>,
    pub token_type: Option<String>,
    pub granular_scopes: Value,
}

/// A Page from `me/accounts`; `access_token` is empty when Graph did not return one.
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub tasks: Vec<String>,
    pub access_token: Secret,
}

fn string_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_token_response(v: &Value) -> Result<TokenResponse, GraphError> {
    let token = v
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GraphError::Json("the answer has no access_token".into()))?;
    Ok(TokenResponse {
        access_token: Secret::new(token),
        token_type: v
            .get("token_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_in: v.get("expires_in").and_then(as_i64),
    })
}

fn parse_debug_token(v: &Value) -> Result<DebugToken, GraphError> {
    let data = v
        .get("data")
        .filter(|d| d.is_object())
        .ok_or_else(|| GraphError::Json("debug_token answered without data".into()))?;
    let instant = |key: &str| data.get(key).and_then(as_i64).filter(|n| *n > 0);
    Ok(DebugToken {
        app_id: data.get("app_id").and_then(string_of).unwrap_or_default(),
        user_id: data.get("user_id").and_then(string_of),
        is_valid: data
            .get("is_valid")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        expires_at: instant("expires_at"),
        issued_at: instant("issued_at"),
        scopes: string_list(data.get("scopes")),
        token_type: data.get("type").and_then(Value::as_str).map(str::to_string),
        granular_scopes: data.get("granular_scopes").cloned().unwrap_or(Value::Null),
    })
}

fn parse_page(v: &Value) -> Option<PageInfo> {
    Some(PageInfo {
        id: v.get("id").and_then(string_of)?,
        name: v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        category: v
            .get("category")
            .and_then(Value::as_str)
            .map(str::to_string),
        tasks: string_list(v.get("tasks")),
        access_token: Secret::new(
            v.get("access_token")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
    })
}

/// Trades an OAuth `code` for a short-lived user token (no Bearer, no proof).
pub async fn exchange_code(
    graph: &GraphClient,
    app_id: &str,
    app_secret: &Secret,
    redirect_uri: &str,
    code: &str,
) -> Result<TokenResponse, GraphError> {
    let query = [
        ("client_id", app_id.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_secret", app_secret.expose().to_string()),
        ("code", code.to_string()),
    ];
    parse_token_response(&graph.get("oauth/access_token", None, &query).await?)
}

/// Trades a short-lived user token for a long-lived (about 60 days) one.
pub async fn exchange_long_lived(
    graph: &GraphClient,
    app_id: &str,
    app_secret: &Secret,
    short: &Secret,
) -> Result<TokenResponse, GraphError> {
    let query = [
        ("grant_type", "fb_exchange_token".to_string()),
        ("client_id", app_id.to_string()),
        ("client_secret", app_secret.expose().to_string()),
        ("fb_exchange_token", short.expose().to_string()),
    ];
    parse_token_response(&graph.get("oauth/access_token", None, &query).await?)
}

/// Inspects `input` using the app token as the credential.
pub async fn debug_token(
    graph: &GraphClient,
    app_token: &Secret,
    input: &Secret,
) -> Result<DebugToken, GraphError> {
    let query = [("input_token", input.expose().to_string())];
    parse_debug_token(&graph.get("debug_token", Some(app_token), &query).await?)
}

/// `GET me?fields=id,name`: the user or Page a token belongs to.
pub async fn me(graph: &GraphClient, token: &Secret) -> Result<Value, GraphError> {
    graph
        .get("me", Some(token), &[("fields", "id,name".to_string())])
        .await
}

/// Every Page the user manages, with its Page access token.
pub async fn accounts(graph: &GraphClient, token: &Secret) -> Result<Vec<PageInfo>, GraphError> {
    let query = [("fields", "id,name,category,tasks,access_token".to_string())];
    let items = graph
        .get_all("me/accounts", Some(token), &query, 200)
        .await?;
    Ok(items.iter().filter_map(parse_page).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn token_response_parsing() {
        let t = parse_token_response(
            &json!({"access_token": "A", "token_type": "bearer", "expires_in": "5183944"}),
        )
        .expect("token");
        assert_eq!(t.access_token.expose(), "A");
        assert_eq!(t.token_type.as_deref(), Some("bearer"));
        assert_eq!(t.expires_in, Some(5_183_944));
        assert!(parse_token_response(&json!({"token_type": "bearer"})).is_err());
        assert!(parse_token_response(&json!({"access_token": ""})).is_err());
        let bare = parse_token_response(&json!({"access_token": "B"})).expect("token");
        assert!(bare.expires_in.is_none() && bare.token_type.is_none());
    }

    #[test]
    fn debug_token_parsing() {
        let d = parse_debug_token(&json!({"data": {
            "app_id": 123, "user_id": "77", "is_valid": true, "expires_at": 1_900_000_000,
            "issued_at": "1700",
            "scopes": ["pages_messaging", "pages_show_list"], "type": "USER",
            "granular_scopes": [{"scope": "pages_messaging", "target_ids": ["1"]}]
        }}))
        .expect("debug");
        assert_eq!(d.app_id, "123");
        assert_eq!(d.user_id.as_deref(), Some("77"));
        assert!(d.is_valid);
        assert_eq!(d.expires_at, Some(1_900_000_000));
        assert_eq!(d.issued_at, Some(1700));
        assert_eq!(d.scopes, vec!["pages_messaging", "pages_show_list"]);
        assert_eq!(d.token_type.as_deref(), Some("USER"));
        assert!(d.granular_scopes.is_array());
        let never =
            parse_debug_token(&json!({"data": {"app_id": "1", "is_valid": true, "expires_at": 0}}))
                .expect("debug");
        assert_eq!(never.expires_at, None);
        assert!(parse_debug_token(&json!({"nope": 1})).is_err());
    }

    #[test]
    fn page_parsing_keeps_pages_without_tokens() {
        let p = parse_page(&json!({"id": "9", "name": "Shop", "category": "Retail", "tasks": ["MESSAGING", "MANAGE"], "access_token": "EAAp"}))
            .expect("page");
        assert_eq!((p.id.as_str(), p.name.as_str()), ("9", "Shop"));
        assert_eq!(p.tasks.len(), 2);
        assert_eq!(p.access_token.expose(), "EAAp");
        let bare = parse_page(&json!({"id": 10})).expect("page");
        assert!(bare.access_token.is_empty());
        assert!(parse_page(&json!({"name": "no id"})).is_none());
    }
}
