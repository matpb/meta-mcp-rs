//! Graph client behaviour against wiremock: auth, proof, errors, retries, paging, downloads.

mod common;

use std::time::Instant;

use hmac::{Hmac, KeyInit, Mac};
use meta_mcp::config::{GraphSettings, Secret};
use meta_mcp::graph::download::TokenMode;
use meta_mcp::graph::{GraphClient, GraphError, token};
use serde_json::{Value, json};
use sha2::Sha256;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(server: &MockServer, app_secret: Option<&str>) -> GraphSettings {
    GraphSettings {
        base_url: server.uri(),
        version: "v26.0".into(),
        app_id: Some("APPID".into()),
        app_secret: app_secret.map(Secret::new),
    }
}

fn client(server: &MockServer, app_secret: Option<&str>) -> GraphClient {
    GraphClient::new(
        &settings(server, app_secret),
        vec!["127.0.0.1".into(), "localhost".into()],
    )
    .expect("client")
}

fn hex_hmac(key: &str, message: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_bytes()).expect("key");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn graph_error(code: i64, subcode: Option<i64>, message: &str) -> Value {
    let mut err =
        json!({"message": message, "type": "OAuthException", "code": code, "fbtrace_id": "X"});
    if let Some(s) = subcode {
        err["error_subcode"] = json!(s);
    }
    json!({"error": err})
}

async fn count(server: &MockServer) -> usize {
    server.received_requests().await.expect("recording").len()
}

#[tokio::test]
async fn bearer_header_and_appsecret_proof() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1", "name": "Shop"})))
        .mount(&server)
        .await;
    let c = client(&server, Some("s3cret"));
    let tok = Secret::new("tok");
    let out = c
        .get("me", Some(&tok), &[("fields", "id,name".to_string())])
        .await
        .expect("get");
    assert_eq!(out["name"], json!("Shop"));

    let reqs = server.received_requests().await.expect("recording");
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(
        req.headers
            .get("authorization")
            .expect("auth")
            .to_str()
            .expect("ascii"),
        "Bearer tok"
    );
    let query: Vec<(String, String)> = req
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let proof = query
        .iter()
        .find(|(k, _)| k == "appsecret_proof")
        .expect("proof")
        .1
        .clone();
    assert_eq!(proof, hex_hmac("s3cret", "tok"));
    assert!(query.contains(&("fields".into(), "id,name".into())));
    assert!(
        !query.iter().any(|(k, _)| k == "access_token"),
        "token must not be in the URL: {query:?}"
    );
    assert!(!req.url.as_str().contains("tok"), "{}", req.url);
}

#[tokio::test]
async fn proof_is_only_sent_with_secret_and_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    let tok = Secret::new("tok");
    client(&server, None)
        .get("me", Some(&tok), &[])
        .await
        .expect("no secret");
    client(&server, Some("s"))
        .get("me", None, &[])
        .await
        .expect("no token");
    for req in server.received_requests().await.expect("recording") {
        assert!(
            req.url.query_pairs().all(|(k, _)| k != "appsecret_proof"),
            "{}",
            req.url
        );
    }
    let reqs = server.received_requests().await.expect("recording");
    assert!(reqs[1].headers.get("authorization").is_none());
}

#[tokio::test]
async fn post_json_form_delete_and_versioned_paths() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v26.0/55/subscribed_apps"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v25.0/55/thing"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "f1"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v26.0/55/subscribed_apps"))
        .and(query_param("x", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .mount(&server)
        .await;
    let c = client(&server, Some("s"));
    let tok = Secret::new("t");
    let a = c
        .post_json(
            "55/subscribed_apps",
            Some(&tok),
            &json!({"subscribed_fields": ["messages"]}),
        )
        .await
        .expect("json");
    assert_eq!(a["success"], json!(true));
    let b = c
        .post_form("v25.0/55/thing", Some(&tok), &[("a", "b c".to_string())])
        .await
        .expect("form");
    assert_eq!(b["id"], json!("f1"));
    let d = c
        .delete("/55/subscribed_apps", Some(&tok), &[("x", "1".to_string())])
        .await
        .expect("delete");
    assert_eq!(d["success"], json!(true));
    let reqs = server.received_requests().await.expect("recording");
    let json_req = &reqs[0];
    assert_eq!(
        serde_json::from_slice::<Value>(&json_req.body).expect("body")["subscribed_fields"][0],
        json!("messages")
    );
    assert!(
        json_req
            .url
            .query_pairs()
            .any(|(k, _)| k == "appsecret_proof")
    );
    assert_eq!(String::from_utf8_lossy(&reqs[1].body), "a=b+c");
}

#[tokio::test]
async fn multipart_posts_once_and_never_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v26.0/PN/media"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let c = client(&server, None);
    let form = reqwest::multipart::Form::new()
        .text("messaging_product", "whatsapp")
        .part(
            "file",
            reqwest::multipart::Part::bytes(b"abc".to_vec()).file_name("a.txt"),
        );
    let err = c
        .post_multipart("PN/media", Some(&Secret::new("t")), form)
        .await
        .expect_err("503");
    assert!(
        matches!(
            err,
            GraphError::Api {
                http_status: 503,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(count(&server).await, 1);
    let body =
        String::from_utf8_lossy(&server.received_requests().await.expect("recording")[0].body)
            .into_owned();
    assert!(
        body.contains("messaging_product") && body.contains("a.txt"),
        "{body}"
    );
}

#[tokio::test]
async fn long_lived_exchange_chain() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("grant_type", "fb_exchange_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": "LONG-TOKEN", "token_type": "bearer", "expires_in": 5183944}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/oauth/access_token"))
        .and(query_param("code", "THE-CODE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": "SHORT-TOKEN", "token_type": "bearer", "expires_in": "3600"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/debug_token"))
        .and(header("authorization", "Bearer APPID|APPSECRET"))
        .and(query_param("input_token", "LONG-TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {
            "app_id": "APPID", "user_id": "77", "is_valid": true, "expires_at": 1_900_000_000,
            "issued_at": 1_700_000_000, "scopes": ["pages_messaging", "pages_show_list"], "type": "USER"
        }})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me/accounts"))
        .and(header("authorization", "Bearer LONG-TOKEN"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            {"id": "100", "name": "Shop", "category": "Retail", "tasks": ["MESSAGING"], "access_token": "PAGE-TOKEN"}
        ]})))
        .mount(&server)
        .await;

    let c = client(&server, Some("APPSECRET"));
    let secret = Secret::new("APPSECRET");
    let short = token::exchange_code(
        &c,
        "APPID",
        &secret,
        "https://localhost/callback",
        "THE-CODE",
    )
    .await
    .expect("code exchange");
    assert_eq!(short.access_token.expose(), "SHORT-TOKEN");
    assert_eq!(short.expires_in, Some(3600));
    let long = token::exchange_long_lived(&c, "APPID", &secret, &short.access_token)
        .await
        .expect("long-lived exchange");
    assert_eq!(long.access_token.expose(), "LONG-TOKEN");
    assert_eq!(long.expires_in, Some(5_183_944));

    let app_token = c.settings().app_token().expect("app token");
    let info = token::debug_token(&c, &app_token, &long.access_token)
        .await
        .expect("debug");
    assert!(info.is_valid);
    assert_eq!(info.scopes, vec!["pages_messaging", "pages_show_list"]);
    assert_eq!(info.expires_at, Some(1_900_000_000));
    let pages = token::accounts(&c, &long.access_token)
        .await
        .expect("accounts");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].access_token.expose(), "PAGE-TOKEN");

    let reqs = server.received_requests().await.expect("recording");
    assert_eq!(reqs.len(), 4);
    let pairs = |i: usize| -> Vec<(String, String)> {
        reqs[i]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    };
    let first = pairs(0);
    assert!(first.contains(&("client_id".into(), "APPID".into())));
    assert!(first.contains(&("client_secret".into(), "APPSECRET".into())));
    assert!(first.contains(&("redirect_uri".into(), "https://localhost/callback".into())));
    let second = pairs(1);
    assert!(
        second.contains(&("fb_exchange_token".into(), "SHORT-TOKEN".into())),
        "{second:?}"
    );
    assert!(second.contains(&("client_secret".into(), "APPSECRET".into())));
    for i in [0, 1] {
        assert!(
            reqs[i].headers.get("authorization").is_none(),
            "oauth calls carry no Bearer"
        );
        assert!(pairs(i).iter().all(|(k, _)| k != "appsecret_proof"));
    }
    let me_accounts = pairs(3);
    assert!(me_accounts.contains(&(
        "fields".into(),
        "id,name,category,tasks,access_token".into()
    )));
    assert!(me_accounts.contains(&(
        "appsecret_proof".into(),
        hex_hmac("APPSECRET", "LONG-TOKEN")
    )));
}

#[tokio::test]
async fn error_hint_190() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            190,
            None,
            "Error validating access token",
        )))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get("me", Some(&Secret::new("old")), &[])
        .await
        .expect_err("400");
    match &err {
        GraphError::Api {
            code,
            subcode,
            hint,
            http_status,
            fbtrace_id,
            error_type,
            ..
        } => {
            assert_eq!((*code, *subcode, *http_status), (190, None, 400));
            assert_eq!(fbtrace_id.as_deref(), Some("X"));
            assert_eq!(error_type.as_deref(), Some("OAuthException"));
            assert!(hint.is_some());
        }
        other => panic!("unexpected {other:?}"),
    }
    let shown = err.to_string();
    assert!(shown.contains("meta-mcp setup"), "{shown}");
    assert!(shown.contains("Error validating access token"), "{shown}");
    assert!(shown.contains("[fbtrace X]"), "{shown}");
}

#[tokio::test]
async fn outside_window_hint() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v26.0/123/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            Some(2018278),
            "This message is sent outside of allowed window.",
        )))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .post_json(
            "123/messages",
            Some(&Secret::new("t")),
            &json!({"message": {"text": "hi"}}),
        )
        .await
        .expect_err("400");
    assert!(
        matches!(
            &err,
            GraphError::Api {
                code: 100,
                subcode: Some(2018278),
                ..
            }
        ),
        "{err}"
    );
    let shown = err.to_string();
    assert!(shown.starts_with("Graph API error 100/2018278:"), "{shown}");
    assert!(shown.contains("24-hour messaging window"), "{shown}");
    assert!(shown.contains("HUMAN_AGENT"), "{shown}");
    assert_eq!(count(&server).await, 1);
}

#[tokio::test]
async fn error_text_never_leaks_tokens() {
    let server = MockServer::start().await;
    let leaked = format!("EAAB{}", "x".repeat(40));
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            190,
            None,
            &format!("Malformed access token {leaked}"),
        )))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get("me", None, &[])
        .await
        .expect_err("400");
    let shown = format!("{err} / {err:?}");
    assert!(!shown.contains(&leaked), "{shown}");
    assert!(shown.contains("EAA***"), "{shown}");
}

#[tokio::test]
async fn no_retry_on_400() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            100,
            None,
            "Invalid parameter",
        )))
        .expect(1)
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get("123", None, &[])
        .await
        .expect_err("400");
    assert!(matches!(err, GraphError::Api { code: 100, .. }));
    assert_eq!(count(&server).await, 1);
}

#[tokio::test]
async fn retry_on_503() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "9"})))
        .expect(1)
        .mount(&server)
        .await;
    let started = Instant::now();
    let out = client(&server, None)
        .get("me", None, &[])
        .await
        .expect("retried");
    assert_eq!(out["id"], json!("9"));
    assert_eq!(count(&server).await, 2);
    assert!(started.elapsed().as_millis() >= 400, "backoff was skipped");
}

#[tokio::test]
async fn retries_stop_after_two() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_json(graph_error(
            2,
            None,
            "Service temporarily unavailable",
        )))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get("me", None, &[])
        .await
        .expect_err("500");
    assert!(
        matches!(
            err,
            GraphError::Api {
                code: 2,
                http_status: 500,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(count(&server).await, 3, "one attempt plus two retries");
}

#[tokio::test]
async fn retry_on_rate_limit_code_but_not_for_sends() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/limited"))
        .respond_with(ResponseTemplate::new(400).set_body_json(graph_error(
            613,
            None,
            "Calls to this api have exceeded the rate limit.",
        )))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/limited"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": 1})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v26.0/55/messages"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let c = client(&server, None);
    assert_eq!(
        c.get("limited", None, &[]).await.expect("retried")["ok"],
        json!(1)
    );
    let before = count(&server).await;
    assert_eq!(before, 2);
    let err = c
        .post_json("55/messages", None, &json!({}))
        .await
        .expect_err("503");
    assert!(matches!(
        err,
        GraphError::Api {
            http_status: 503,
            ..
        }
    ));
    assert_eq!(count(&server).await, before + 1, "sends are never retried");
}

#[tokio::test]
async fn transport_errors_are_typed_and_url_free() {
    let settings = GraphSettings {
        base_url: "http://127.0.0.1:1".into(),
        ..GraphSettings::default()
    };
    let c = GraphClient::new(&settings, Vec::new()).expect("client");
    let secret = Secret::new("SUPERSECRETVALUE");
    let err = c
        .get(
            "oauth/access_token",
            None,
            &[("client_secret", secret.expose().to_string())],
        )
        .await
        .expect_err("refused");
    assert!(matches!(err, GraphError::Transport(_)), "{err}");
    let shown = format!("{err} {err:?}");
    assert!(
        !shown.contains("SUPERSECRETVALUE") && !shown.contains("127.0.0.1:1/"),
        "{shown}"
    );
}

#[tokio::test]
async fn non_json_answers_are_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v26.0/html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/empty"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/gone"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;
    let c = client(&server, None);
    assert!(matches!(
        c.get("html", None, &[]).await,
        Err(GraphError::Json(_))
    ));
    assert_eq!(
        c.get("empty", None, &[]).await.expect("empty ok"),
        Value::Null
    );
    assert!(matches!(
        c.get("gone", None, &[]).await,
        Err(GraphError::Transport(_))
    ));
    assert_eq!(count(&server).await, 3, "404 is not retried");
}

#[tokio::test]
async fn error_member_in_a_200_body_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(graph_error(
            551,
            None,
            "Person unavailable",
        )))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get("1", None, &[])
        .await
        .expect_err("error body");
    assert!(err.to_string().contains("unavailable"), "{err}");
    assert!(matches!(
        err,
        GraphError::Api {
            code: 551,
            http_status: 200,
            ..
        }
    ));
}

#[tokio::test]
async fn follows_paging_next() {
    let server = MockServer::start().await;
    let next = format!(
        "{}/v26.0/me/accounts?after=CURSOR&limit=2&access_token=LEAKED",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/v26.0/me/accounts"))
        .and(query_param("after", "CURSOR"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"n": 3}, {"n": 4}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v26.0/me/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data": [{"n": 1}, {"n": 2}], "paging": {"cursors": {"after": "CURSOR"}, "next": next}}),
        ))
        .mount(&server)
        .await;
    let c = client(&server, Some("s"));
    let tok = Secret::new("PAGE-TOK");
    let all = c
        .get_all("me/accounts", Some(&tok), &[("limit", "2".to_string())], 10)
        .await
        .expect("pages");
    let ns: Vec<i64> = all.iter().map(|v| v["n"].as_i64().expect("n")).collect();
    assert_eq!(ns, vec![1, 2, 3, 4]);
    let reqs = server.received_requests().await.expect("recording");
    assert_eq!(reqs.len(), 2);
    for r in &reqs {
        assert_eq!(
            r.headers
                .get("authorization")
                .expect("auth")
                .to_str()
                .expect("ascii"),
            "Bearer PAGE-TOK"
        );
        assert!(
            r.url.query_pairs().all(|(k, _)| k != "access_token"),
            "{}",
            r.url
        );
        assert!(
            r.url
                .query_pairs()
                .any(|(k, v)| k == "appsecret_proof" && v == hex_hmac("s", "PAGE-TOK"))
        );
    }

    let capped = c
        .get_all("me/accounts", Some(&tok), &[("limit", "2".to_string())], 3)
        .await
        .expect("capped");
    assert_eq!(capped.len(), 3);
    assert!(
        c.get_all("me/accounts", Some(&tok), &[], 0)
            .await
            .expect("zero")
            .is_empty()
    );
}

#[tokio::test]
async fn refuses_paging_to_another_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"n": 1}],
            "paging": {"next": "https://evil.example/v26.0/steal?after=1"}
        })))
        .mount(&server)
        .await;
    let err = client(&server, None)
        .get_all("me/accounts", Some(&Secret::new("t")), &[], 10)
        .await
        .expect_err("foreign host");
    assert!(matches!(err, GraphError::Refused(_)), "{err}");
    assert_eq!(count(&server).await, 1);
}

#[tokio::test]
async fn download_scopes_the_token_per_hop() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(method("GET"))
        .and(path("/media"))
        .and(header("authorization", "Bearer WA-TOKEN"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://localhost:{port}/final")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "Image/PNG; charset=binary")
                .set_body_bytes(b"\x89PNGdata".to_vec()),
        )
        .mount(&server)
        .await;
    let c = client(&server, Some("s"));
    let token = Secret::new("WA-TOKEN");
    let hosts = ["127.0.0.1".to_string()];
    let url = url::Url::parse(&format!("http://127.0.0.1:{port}/media")).expect("url");
    let got = c
        .download(
            &url,
            TokenMode::OnHosts {
                token: &token,
                hosts: &hosts,
            },
            1024,
        )
        .await
        .expect("download");
    assert_eq!(&got.bytes[..], b"\x89PNGdata");
    assert_eq!(got.mime.as_deref(), Some("image/png"));
    assert_eq!(got.final_url.host_str(), Some("localhost"));
    let reqs = server.received_requests().await.expect("recording");
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].headers.get("authorization").is_some());
    assert!(
        reqs[1].headers.get("authorization").is_none(),
        "token must not follow to an unlisted host"
    );
}

#[tokio::test]
async fn download_never_sends_a_token_without_a_mode() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"x".to_vec()))
        .mount(&server)
        .await;
    let c = client(&server, None);
    let url = url::Url::parse(&format!("{}/f", server.uri())).expect("url");
    c.download(&url, TokenMode::Never, 10)
        .await
        .expect("download");
    let reqs = server.received_requests().await.expect("recording");
    assert!(reqs[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn download_enforces_host_size_and_redirect_limits() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 2048]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/evil"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://evil.example/steal"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let c = client(&server, None);
    let base = format!("http://127.0.0.1:{port}");
    let get = |p: &str, cap: u64| {
        let c = &c;
        let url = url::Url::parse(&format!("{base}{p}")).expect("url");
        async move { c.download(&url, TokenMode::Never, cap).await }
    };
    assert!(matches!(
        get("/big", 1024).await,
        Err(GraphError::Refused(_))
    ));
    assert_eq!(get("/big", 4096).await.expect("fits").bytes.len(), 2048);
    assert!(matches!(
        get("/loop", 1024).await,
        Err(GraphError::Refused(_))
    ));
    assert!(matches!(
        get("/evil", 1024).await,
        Err(GraphError::Refused(_))
    ));
    assert!(matches!(
        get("/missing", 1024).await,
        Err(GraphError::Transport(_))
    ));

    let foreign = url::Url::parse("https://evil.example/file.jpg").expect("url");
    let err = c
        .download(&foreign, TokenMode::Never, 10)
        .await
        .expect_err("foreign");
    assert!(matches!(err, GraphError::Refused(_)), "{err}");
    let plain = url::Url::parse("http://scontent.xx.fbcdn.net/file.jpg").expect("url");
    assert!(matches!(
        c.download(&plain, TokenMode::Never, 10).await,
        Err(GraphError::Refused(_))
    ));
    let strict = GraphClient::new(&settings(&server, None), Vec::new()).expect("client");
    let local = url::Url::parse(&format!("{base}/big")).expect("url");
    assert!(
        matches!(
            strict.download(&local, TokenMode::Never, 4096).await,
            Err(GraphError::Refused(_))
        ),
        "loopback is only allowed when listed"
    );
}

#[tokio::test]
async fn download_cap_applies_without_content_length() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 2048];
                let _ = sock.read(&mut request).await;
                let head = b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(head).await;
                for _ in 0..8 {
                    if sock.write_all(&[1u8; 1024]).await.is_err() {
                        break;
                    }
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    let settings = GraphSettings::default();
    let c = GraphClient::new(&settings, vec!["127.0.0.1".into()]).expect("client");
    let url = url::Url::parse(&format!("http://127.0.0.1:{port}/stream")).expect("url");
    let err = c
        .download(&url, TokenMode::Never, 4096)
        .await
        .expect_err("over the cap");
    assert!(matches!(err, GraphError::Refused(_)), "{err}");
    let ok = c
        .download(&url, TokenMode::Never, 8192)
        .await
        .expect("fits exactly");
    assert_eq!(ok.bytes.len(), 8192);
}
