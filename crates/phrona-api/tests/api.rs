//! Integration tests for the REST API. All tests are offline: they exercise
//! routing, auth, error mapping and static file serving without making any
//! network requests.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use phrona::PhronaConfig;
use serde_json::Value;
use tower::ServiceExt;

fn key_router() -> axum::Router {
    let mut cfg = PhronaConfig::defaults();
    cfg.server.api_key = Some("test-secret".into());
    phrona_api::router(cfg)
}

fn open_router() -> axum::Router {
    phrona_api::router(PhronaConfig::defaults())
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    read(res).await
}

async fn get_header(
    router: &axum::Router,
    path: &str,
    name: &str,
    value: &str,
) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(name, value)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    read(res).await
}

async fn post_json(router: &axum::Router, path: &str, body: &str) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    read(res).await
}

async fn post_json_header(
    router: &axum::Router,
    path: &str,
    body: &str,
) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", "test-secret")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    read(res).await
}

async fn read(res: axum::response::Response) -> (StatusCode, Value, String) {
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, json, content_type)
}

#[tokio::test]
async fn health_ok() {
    let router = key_router();
    let (status, json, _) = get(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
    assert!(json["version"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(json["engines"]["web"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(json["auth"], true);
}

#[tokio::test]
async fn health_no_auth_reports_false() {
    let router = open_router();
    let (status, json, _) = get(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["auth"], false);
}

#[tokio::test]
async fn engines_lists_all_categories() {
    let router = key_router();
    let (status, json, _) = get(&router, "/v1/engines").await;
    assert_eq!(status, StatusCode::OK);
    for cat in ["web", "images", "news", "videos"] {
        assert!(json[cat].is_array(), "missing category {cat}");
    }
    let web = json["web"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();
    assert!(web.contains(&"marginalia"));
}

#[tokio::test]
async fn engines_category_filter() {
    let router = key_router();
    let (status, json, _) = get(&router, "/v1/engines?category=images").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["images"].is_array());
    assert!(json.get("web").is_none());
}

#[tokio::test]
async fn engines_invalid_category_is_400() {
    let router = key_router();
    let (status, json, _) = get(&router, "/v1/engines?category=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn search_requires_key_when_configured() {
    let router = key_router();
    let (status, _, _) = get(&router, "/v1/search?q=rust").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_wrong_key_is_401() {
    let router = key_router();
    let (status, _, _) = get_header(&router, "/v1/search?q=rust", "x-api-key", "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn search_query_string_api_key_is_rejected_with_400() {
    let router = key_router();
    let (status, json, _) = get(&router, "/v1/search?q=rust&api_key=test-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let msg = json["error"].as_str().unwrap_or("");
    assert!(msg.contains("query string"), "got: {msg}");
}

#[tokio::test]
async fn search_invalid_category_is_400() {
    let router = key_router();
    let (status, json, _) = get_header(
        &router,
        "/v1/search?q=rust&category=bogus",
        "x-api-key",
        "test-secret",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn search_unknown_engine_is_400() {
    let router = key_router();
    let (status, json, _) = get_header(
        &router,
        "/v1/search?q=rust&engines=nope",
        "x-api-key",
        "test-secret",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn search_invalid_safesearch_is_400() {
    let router = key_router();
    let (status, _, _) = get_header(
        &router,
        "/v1/search?q=rust&safesearch=extreme",
        "x-api-key",
        "test-secret",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_no_key_needed_when_unconfigured() {
    let router = open_router();
    let (status, _, _) = get(&router, "/v1/search?q=rust&category=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn suggest_requires_key() {
    let router = key_router();
    let (status, _, _) = get(&router, "/v1/suggest?q=rust").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn suggest_missing_q_is_400_json() {
    let router = key_router();
    let (status, json, _) = get_header(&router, "/v1/suggest", "x-api-key", "test-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid query parameters: missing field `q`");
}

#[tokio::test]
async fn search_missing_q_is_400_json() {
    let router = key_router();
    let (status, json, _) = get_header(&router, "/v1/search", "x-api-key", "test-secret").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "invalid query parameters: missing field `q`");
}

#[tokio::test]
async fn extract_requires_key() {
    let router = key_router();
    let (status, _, _) = get(&router, "/v1/extract?url=https://example.com").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) =
        post_json(&router, "/v1/extract", r#"{"url": "https://example.com"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn grounding_requires_key() {
    let router = key_router();
    let (status, _, _) = get(&router, "/v1/grounding?query=rust").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = post_json(&router, "/v1/grounding", r#"{"query": "rust"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_requires_key() {
    let router = key_router();
    let (status, _, _) = get(&router, "/v1/test?query=rust").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tavily_requires_key() {
    let router = key_router();
    let (status, _, _) = post_json(&router, "/v1/tavily", r#"{"query": "rust"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = post_json(&router, "/search", r#"{"query": "rust"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tavily_wrong_body_key_is_401() {
    let router = key_router();
    // the JSON body api_key is consulted (Tavily SDK drop-in auth)
    let (status, _, _) = post_json(
        &router,
        "/search",
        r#"{"query": "rust", "api_key": "wrong"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn tavily_post_body_api_key_is_accepted_before_payload_validation() {
    let router = key_router();
    let (status, json, _) = post_json(&router, "/search", r#"{"api_key": "test-secret"}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    assert!(
        json["error"]
            .as_str()
            .unwrap_or("")
            .contains("missing field `query`")
    );
}

#[test]
fn grounding_auth_example_marks_get_headers_and_post_body_boundary() {
    let docs = include_str!("../../../docs/api.md");
    let grounding = docs
        .split_once("## GET|POST /v1/grounding")
        .and_then(|(_, rest)| rest.split_once("## Frontend"))
        .map(|(section, _)| section)
        .expect("grounding documentation section");
    let shared_example = grounding
        .split_once("Query params (GET) or JSON body (POST):")
        .and_then(|(_, rest)| rest.split_once("```json"))
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(example, _)| example)
        .expect("shared grounding example");

    assert!(grounding.contains("For `GET`, authentication uses headers only"));
    assert!(grounding.contains("POST-body-only `api_key`"));
    assert!(!shared_example.contains("\"api_key\""));
}

#[tokio::test]
async fn grounding_malformed_json_is_400_json() {
    let router = key_router();
    let (status, json, ct) = post_json_header(&router, "/v1/grounding", "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
    assert!(ct.starts_with("application/json"));
}

#[tokio::test]
async fn tavily_malformed_json_is_400_json() {
    let router = key_router();
    let (status, json, _) = post_json_header(&router, "/v1/tavily", "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn source_policy_validation_is_local_and_shared_by_rest_and_tavily() {
    let router = open_router();
    let (status, json, _) = get_header(
        &router,
        "/v1/search?q=rust&source_policy_mode=require-allowed&allowed_domains=https%3A%2F%2Fbad.example",
        "x-api-key",
        "ignored",
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]
            .as_str()
            .unwrap_or("")
            .contains("source policy")
    );

    let (status, json, _) = post_json(
        &router,
        "/search",
        r#"{"query":"rust","source_policy":{"mode":"require-allowed","allowed_domains":["https://bad.example"]}}"#,
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]
            .as_str()
            .unwrap_or("")
            .contains("source policy")
    );
}

#[tokio::test]
async fn grounding_missing_query_is_400_json() {
    let router = key_router();
    let (status, json, _) = post_json_header(&router, "/v1/grounding", r#"{}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = json["error"].as_str().unwrap_or("");
    assert!(err.starts_with("invalid JSON body"), "got {err}");
    assert!(err.contains("missing field `query`"), "got {err}");
}

#[tokio::test]
async fn extract_missing_url_is_400_json() {
    let router = key_router();
    let (status, json, _) = post_json_header(&router, "/v1/extract", r#"{}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let err = json["error"].as_str().unwrap_or("");
    assert!(err.starts_with("invalid JSON body"), "got {err}");
    assert!(err.contains("missing field `url`"), "got {err}");
}

#[tokio::test]
async fn extract_get_accepts_bearer_header_auth() {
    let router = key_router();
    // auth passes with a Bearer token, then validation rejects the missing
    // url without touching the network
    let (status, json, _) = get_header(
        &router,
        "/v1/extract",
        "authorization",
        "Bearer test-secret",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        json["error"]
            .as_str()
            .unwrap_or("")
            .contains("missing field")
    );
}

#[tokio::test]
async fn root_serves_app_shell() {
    let router = key_router();
    let (status, _, ct) = get(&router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.starts_with("text/html"));
}

#[tokio::test]
async fn unknown_path_falls_back_to_shell() {
    let router = key_router();
    let (status, _, ct) = get(&router, "/some/route").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.starts_with("text/html"));
}

#[tokio::test]
async fn static_assets_served() {
    let router = key_router();
    for (path, prefix) in [
        ("/static/style.css", "text/css"),
        ("/static/app.js", "text/javascript"),
    ] {
        let (status, _, ct) = get(&router, path).await;
        assert_eq!(status, StatusCode::OK, "path {path}");
        assert!(ct.starts_with(prefix), "path {path} got {ct}");
    }
}
