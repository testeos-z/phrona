//! # Phrona API
//!
//! The server surface for Phrona: a REST API plus an MCP-over-TCP endpoint,
//! with rate limiting, API-key auth and a bundled web console.
//!
//! Build the router with [`router`] and serve it with `axum`, or run the
//! whole thing with the `phrona serve` CLI.

#![warn(missing_docs)]

pub mod frontend;
pub mod grounding;
pub mod metrics;
pub mod tavily;
pub mod tools;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::{BytesRejection, FailedToBufferBody, JsonRejection, QueryRejection};
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequest, FromRequestParts, Query, Request, State,
};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use phrona::engine;
use phrona::error::{ErrorKind as PhronaErrorKind, ErrorScope};
use phrona::models::{Category, SearchResponse, TimeRange};
use phrona::{PhronaConfig, SearchClient, SearchOptions, SourcePolicy, suggest, suggest_all};

/// Fixed-window rate-limit bucket for one client.
struct RateWindow {
    started: Instant,
    count: u32,
}

/// Shared server state: the search client, rate limiter, API key and
/// per-request bounds. Construct with [`AppState::new`].
pub struct AppState {
    /// The search client serving every endpoint.
    pub client: SearchClient,
    /// Time the server started, used for `uptime_s`.
    pub started: Instant,
    /// Optional API key; when set, requests must authenticate.
    pub api_key: Option<String>,
    /// Upper bound applied to `max_results` (from `search.max_results_limit`).
    pub max_results_limit: usize,
    /// Requests allowed per window per client; `0` disables the limiter.
    pub rate_limit_per_minute: u32,
    /// Maximum accepted request body size in bytes.
    pub max_body_bytes: u64,
    /// Reverse-proxy IPs whose `X-Forwarded-For` header is trusted for
    /// client IP extraction (see the `client_ip` helper).
    pub trusted_proxies: Vec<IpAddr>,
    rate: Mutex<HashMap<Option<IpAddr>, RateWindow>>,
}

impl AppState {
    /// Build server state from a ready [`SearchClient`] and the parsed
    /// configuration values.
    pub fn new(
        client: SearchClient,
        api_key: Option<String>,
        max_results_limit: usize,
        rate_limit_per_minute: u32,
        max_body_bytes: u64,
        trusted_proxies: Vec<IpAddr>,
    ) -> Self {
        Self {
            client,
            started: Instant::now(),
            api_key,
            max_results_limit,
            rate_limit_per_minute,
            max_body_bytes,
            trusted_proxies,
            rate: Mutex::new(HashMap::new()),
        }
    }

    /// Constant-time API-key comparison: a configured key is only ever
    /// compared with XOR folds of equal-length byte buffers, never with
    /// early-exit string equality, so timing cannot leak key prefixes.
    pub fn authorized(&self, key: Option<&str>) -> bool {
        match (&self.api_key, key) {
            (None, _) => true,
            (Some(want), Some(got)) => {
                let w = want.as_bytes();
                let g = got.as_bytes();
                w.len() == g.len() && w.iter().zip(g).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
            }
            (Some(_), None) => false,
        }
    }

    /// Fixed-window rate limit keyed on the client IP (falling back to a
    /// single global bucket when the client address is unknown, e.g. in
    /// unit tests). Returns `true` when the request is allowed.
    pub fn check_rate(&self, ip: Option<IpAddr>) -> bool {
        if self.rate_limit_per_minute == 0 {
            return true;
        }
        let mut rate = self.rate.lock();
        // Bounded memory: under public traffic (or IPv6 address rotation) the
        // per-IP bucket map would otherwise grow forever. Once it exceeds a
        // threshold, sweep windows whose 60s window has already elapsed.
        const RATE_BUCKET_LIMIT: usize = 10_000;
        if rate.len() > RATE_BUCKET_LIMIT {
            rate.retain(|_, w| w.started.elapsed() < Duration::from_secs(60));
        }
        let window = rate.entry(ip).or_insert(RateWindow {
            started: Instant::now(),
            count: 0,
        });
        if window.started.elapsed() >= Duration::from_secs(60) {
            window.started = Instant::now();
            window.count = 0;
        }
        if window.count >= self.rate_limit_per_minute {
            return false;
        }
        window.count += 1;
        true
    }
}

/// An API error with a JSON-friendly representation: bad request,
/// unauthorized, rate limited, body too large, or an internal [`phrona`]
/// failure.
#[derive(Debug)]
pub struct AppError(ErrorKind);

#[derive(Debug)]
enum ErrorKind {
    BadRequest(String),
    Unauthorized,
    RateLimited(String),
    BodyTooLarge(u64),
    Internal(phrona::Error),
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(ErrorKind::BadRequest(msg.into()))
    }

    fn unauthorized() -> Self {
        Self(ErrorKind::Unauthorized)
    }

    fn rate_limited(limit: u32) -> Self {
        Self(ErrorKind::RateLimited(format!(
            "rate limit exceeded: at most {limit} requests per minute"
        )))
    }

    fn body_too_large(max: u64) -> Self {
        Self(ErrorKind::BodyTooLarge(max))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body, retry_after) = match self.0 {
            ErrorKind::BadRequest(msg) => (StatusCode::BAD_REQUEST, json!({"error": msg}), None),
            ErrorKind::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                json!({"error": "invalid api key"}),
                None,
            ),
            ErrorKind::RateLimited(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, json!({"error": msg}), None)
            }
            ErrorKind::BodyTooLarge(max) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error": format!("request body exceeds the {max}-byte limit")}),
                None,
            ),
            ErrorKind::Internal(e) => {
                tracing::error!("search failed: {e}");
                let status = if matches!(e.kind(), PhronaErrorKind::RateLimited { .. }) {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    match e.scope() {
                        ErrorScope::Query => StatusCode::BAD_REQUEST,
                        ErrorScope::Internal => StatusCode::INTERNAL_SERVER_ERROR,
                        ErrorScope::Provider => StatusCode::SERVICE_UNAVAILABLE,
                        ErrorScope::Egress | ErrorScope::Schema => StatusCode::BAD_GATEWAY,
                    }
                };
                let retry_after = match e.kind() {
                    PhronaErrorKind::RateLimited {
                        retry_after: Some(delay),
                    } => Some(delay.as_secs().to_string()),
                    _ => None,
                };
                (status, json!({"error": e.to_string()}), retry_after)
            }
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(value) = retry_after {
            if let Ok(value) = value.parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}

impl From<phrona::Error> for AppError {
    fn from(e: phrona::Error) -> Self {
        Self(ErrorKind::Internal(e))
    }
}

type AppResult<T> = Result<T, AppError>;

/// Query-string extractor whose rejection is a JSON 400 instead of axum's
/// default plain-text response.
pub struct JsonQuery<T>(pub T);

impl<S, T: DeserializeOwned> FromRequestParts<S> for JsonQuery<T>
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> AppResult<Self> {
        let q = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|e| {
                use std::error::Error as _;
                let msg = match e {
                    QueryRejection::FailedToDeserializeQueryString(e) => e
                        .source()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| e.to_string()),
                    other => other.to_string(),
                };
                AppError::bad_request(format!("invalid query parameters: {msg}"))
            })?;
        Ok(Self(q.0))
    }
}

/// JSON body extractor whose rejection (missing body, malformed JSON,
/// wrong content type) is a JSON 400 instead of axum's defaults. Bodies
/// larger than the configured `server.max_body_bytes` are rejected with a
/// 413 before deserialization.
pub struct JsonBody<T>(pub T);

impl<T: DeserializeOwned> FromRequest<Arc<AppState>> for JsonBody<T> {
    type Rejection = AppError;

    async fn from_request(req: Request, state: &Arc<AppState>) -> AppResult<Self> {
        if let Some(len) = req
            .headers()
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            && len > state.max_body_bytes
        {
            return Err(AppError::body_too_large(state.max_body_bytes));
        }
        let Json(v) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| match &e {
                JsonRejection::BytesRejection(BytesRejection::FailedToBufferBody(
                    FailedToBufferBody::LengthLimitError(_),
                )) => AppError::body_too_large(state.max_body_bytes),
                _ => AppError::bad_request(format!("invalid JSON body: {e}")),
            })?;
        Ok(Self(v))
    }
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    category: Option<String>,
    engines: Option<String>,
    page: Option<u32>,
    max_results: Option<usize>,
    safesearch: Option<String>,
    region: Option<String>,
    language: Option<String>,
    time_range: Option<String>,
    filters: Option<String>,
    source_policy_mode: Option<String>,
    allowed_domains: Option<String>,
    excluded_domains: Option<String>,
}

/// Additive policy object shared by JSON adapters (Tavily and MCP).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourcePolicyParams {
    /// Admission mode; omitted means `any`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Caller-requested domains.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Caller-excluded domains.
    #[serde(default)]
    pub excluded_domains: Vec<String>,
}

pub(crate) fn compile_source_policy<IA, ID, A, D>(
    mode: Option<&str>,
    allowed: IA,
    denied: ID,
) -> AppResult<SourcePolicy>
where
    IA: IntoIterator<Item = A>,
    ID: IntoIterator<Item = D>,
    A: AsRef<str>,
    D: AsRef<str>,
{
    SourcePolicy::compile(mode.unwrap_or("any"), allowed, denied)
        .map_err(|e| AppError::bad_request(format!("invalid source policy: {e}")))
}

pub(crate) fn split_domains(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|domain| !domain.is_empty())
        .map(str::to_string)
        .collect()
}

fn build_options(p: &SearchParams, max_results_limit: usize) -> AppResult<SearchOptions> {
    let mut opts = SearchOptions::new(p.q.clone());
    if let Some(c) = &p.category {
        opts.category = c.parse::<Category>().map_err(|_| {
            AppError::bad_request(format!(
                "invalid category '{c}', expected one of: web, images, news, videos, books"
            ))
        })?;
    }
    if let Some(es) = &p.engines {
        for name in es.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if engine::engine_by_name(name).is_none() {
                return Err(AppError::bad_request(format!(
                    "unknown engine '{name}'. Available: {}",
                    engine::list()
                        .iter()
                        .map(|e| e.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            opts.engines.push(name.to_string());
        }
    }
    if let Some(page) = p.page {
        opts.page = page.max(1);
    }
    if let Some(m) = p.max_results {
        opts.max_results = m.clamp(1, max_results_limit);
    }
    if let Some(s) = &p.safesearch {
        opts.safesearch = s.parse::<phrona::SafeSearch>().map_err(|_| {
            AppError::bad_request("invalid safesearch, expected off|moderate|strict")
        })?;
    }
    if let Some(t) = &p.time_range {
        opts.time_range = Some(t.parse::<TimeRange>().map_err(|_| {
            AppError::bad_request("invalid time_range, expected day|week|month|year")
        })?);
    }
    opts.region = p.region.clone();
    opts.language = p.language.clone();
    opts.filters = p.filters.clone();
    opts.source_policy = compile_source_policy(
        p.source_policy_mode.as_deref(),
        split_domains(p.allowed_domains.as_deref()),
        split_domains(p.excluded_domains.as_deref()),
    )?;
    Ok(opts)
}

fn header_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
        })
}

/// Resolve the API key for endpoints that accept credentials in the JSON
/// body (e.g. the Tavily-compatible routes): the body key wins, headers are
/// the fallback. Query strings are never consulted.
pub(crate) fn auth_key(headers: &HeaderMap, body_key: Option<&str>) -> Option<String> {
    body_key.map(str::to_string).or_else(|| header_key(headers))
}

/// Header-only auth for GET endpoints. Query-string `api_key` parameters are
/// rejected with a 400: credentials in URLs leak through logs, proxies and
/// referrers and are a classic SSRF/credential-theft vector.
pub struct HeaderAuth {
    key: Option<String>,
}

impl HeaderAuth {
    /// The extracted API key, if any was supplied.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

impl<S> FromRequestParts<S> for HeaderAuth
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> AppResult<Self> {
        if let Some(query) = parts.uri.query()
            && url::form_urlencoded::parse(query.as_bytes()).any(|(k, _)| k == "api_key")
        {
            return Err(AppError::bad_request(
                "api_key in the query string is disallowed for security; use the x-api-key header or Authorization: Bearer instead",
            ));
        }
        Ok(Self {
            key: header_key(&parts.headers),
        })
    }
}

/// Resolve the client IP for rate limiting. The peer address is used
/// directly unless it belongs to `trusted_proxies` (an operator-configured
/// list of reverse proxies), in which case the leftmost `X-Forwarded-For`
/// address is trusted — a proxy chain appends each hop, so the first entry
/// is the original client. Without a trusted proxy, the header is never
/// consulted, so it cannot be spoofed.
fn client_ip(trusted: &[IpAddr], peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if trusted.contains(&peer.ip()) {
        if let Some(ff) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            if let Ok(ip) = ff.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    peer.ip()
}

/// `server.rate_limit_per_minute` per client IP (falling back to a single
/// global bucket when the peer address is unknown, e.g. in tests).
async fn rate_limit(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> AppResult<Response> {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| client_ip(&state.trusted_proxies, c.0, req.headers()));
    if !state.check_rate(ip) {
        return Err(AppError::rate_limited(state.rate_limit_per_minute));
    }
    Ok(next.run(req).await)
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let web = engine::engines_for(Category::Web).len();
    let images = engine::engines_for(Category::Images).len();
    let news = engine::engines_for(Category::News).len();
    let videos = engine::engines_for(Category::Videos).len();
    let books = engine::engines_for(Category::Books).len();
    Json(json!({
        "status": "ok",
        "version": phrona::version(),
        "uptime_s": state.started.elapsed().as_secs(),
        "engines": {"web": web, "images": images, "news": news, "videos": videos, "books": books},
        "auth": state.api_key.is_some(),
    }))
}

#[derive(Deserialize)]
struct EnginesParams {
    category: Option<String>,
}

async fn engines(JsonQuery(p): JsonQuery<EnginesParams>) -> AppResult<Json<Value>> {
    let cats: Vec<Category> = match p.category.as_deref() {
        Some(c) => vec![c.parse::<Category>().map_err(|_| {
            AppError::bad_request(
                "invalid category, expected one of: web, images, news, videos, books",
            )
        })?],
        None => Category::ALL.to_vec(),
    };
    let mut out = serde_json::Map::new();
    for cat in cats {
        out.insert(
            cat.as_str().to_string(),
            json!(
                phrona::available_engines(cat)
                    .iter()
                    .map(|e| e.name.clone())
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(Json(Value::Object(out)))
}

async fn search_route(
    State(state): State<Arc<AppState>>,
    auth: HeaderAuth,
    JsonQuery(p): JsonQuery<SearchParams>,
) -> AppResult<Json<SearchResponse>> {
    if !state.authorized(auth.key()) {
        return Err(AppError(ErrorKind::Unauthorized));
    }
    let opts = build_options(&p, state.max_results_limit)?;
    let resp = state.client.search(opts).await?;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct SuggestParams {
    q: String,
    source: Option<String>,
    region: Option<String>,
}

async fn suggest_route(
    State(state): State<Arc<AppState>>,
    auth: HeaderAuth,
    JsonQuery(p): JsonQuery<SuggestParams>,
) -> AppResult<Json<Value>> {
    if !state.authorized(auth.key()) {
        return Err(AppError(ErrorKind::Unauthorized));
    }
    let region = p.region.unwrap_or_else(|| "us-en".to_string());
    match p.source.as_deref() {
        Some(name) => {
            let source = phrona::SuggestSource::from_name(name).ok_or_else(|| {
                AppError::bad_request(format!(
                    "unknown suggest source '{name}', expected one of: {}",
                    phrona::SuggestSource::ALL
                        .iter()
                        .map(|s| s.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            let suggestions = suggest(state.client.http(), source, &p.q, &region).await?;
            Ok(Json(json!({
                "query": p.q,
                "source": name,
                "suggestions": suggestions,
            })))
        }
        None => {
            let all = suggest_all(state.client.http(), &p.q, &region).await;
            let suggestions: serde_json::Map<String, Value> = all
                .into_iter()
                .map(|(s, list)| (s.name().to_string(), json!(list)))
                .collect();
            Ok(Json(json!({
                "query": p.q,
                "suggestions": suggestions,
            })))
        }
    }
}

/// Build the axum router from a [`PhronaConfig`]: the search client
/// (profile / timeout / proxies / concurrency), the API key, the
/// `max_results` clamp, the rate limit and the body-size cap all come from
/// the config.
///
/// Protected endpoints (every `/v1/*` data route plus the Tavily-compatible
/// aliases) run under the per-IP rate limiter; `GET /metrics`, `/health` and
/// the frontend stay unauthenticated and unthrottled.
pub fn router(cfg: PhronaConfig) -> Router {
    let client = cfg
        .search_client()
        .expect("build search client")
        .with_observer(Arc::new(metrics::EngineMetricsObserver));
    router_with_state(AppState::new(
        client,
        cfg.server.api_key.clone(),
        cfg.max_results_limit(),
        cfg.server.rate_limit_per_minute,
        cfg.server.max_body_bytes,
        cfg.server.trusted_proxies.clone(),
    ))
}

/// Build the same REST router from caller-supplied state.
///
/// This additive constructor is useful for deterministic integration tests
/// that inject a configured [`phrona::SearchClient`]; route handlers,
/// middleware and response shapes are identical to [`router`].
pub fn router_with_state(state: AppState) -> Router {
    let max_body_bytes = state.max_body_bytes;
    let state = Arc::new(state);

    let protected = Router::new()
        .route("/v1/search", get(search_route))
        .route("/v1/suggest", get(suggest_route))
        .route(
            "/v1/extract",
            get(tools::extract_get).post(tools::extract_post),
        )
        .route("/v1/test", get(tools::test))
        .route("/v1/grounding", get(grounding::get).post(grounding::post))
        .route("/search", post(tavily::search))
        .route("/v1/tavily", post(tavily::search))
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit));

    protected
        .merge(
            Router::new()
                .route("/", get(frontend::index))
                .route("/health", get(health))
                .route("/metrics", get(metrics::metrics_route))
                .route("/v1/engines", get(engines))
                .nest_service(
                    "/static",
                    tower_http::services::ServeDir::new(frontend::frontend_dir()),
                )
                .fallback(frontend::index),
        )
        .layer(DefaultBodyLimit::max(max_body_bytes as usize))
        .layer(middleware::from_fn(metrics::http_layer))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Wait for Ctrl+C or SIGTERM, then return so the server can drain
/// in-flight requests gracefully.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Serve the REST API on `addr`. Blocks until the server stops (Ctrl+C or
/// SIGTERM trigger a graceful shutdown that drains in-flight requests).
pub async fn serve(addr: SocketAddr, cfg: PhronaConfig) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("phrona-api listening on http://{addr}");
    let app = router(cfg).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Default bind address when none is configured.
pub fn default_addr() -> SocketAddr {
    "127.0.0.1:8080".parse().expect("static addr")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    fn router_with(mut cfg: PhronaConfig) -> Router {
        cfg.server.api_key = Some("test-secret".into());
        super::router(cfg)
    }

    #[test]
    fn app_error_maps_to_status_codes() {
        for (status, kind) in [
            (StatusCode::BAD_REQUEST, ErrorKind::BadRequest("x".into())),
            (StatusCode::UNAUTHORIZED, ErrorKind::Unauthorized),
            (
                StatusCode::BAD_GATEWAY,
                ErrorKind::Internal(phrona::Error::schema("e", "bad body")),
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorKind::Internal(phrona::Error::rate_limited("e", None)),
            ),
            (
                StatusCode::BAD_REQUEST,
                ErrorKind::Internal(phrona::Error::invalid_query("e", "bad")),
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorKind::Internal(phrona::Error::internal("e", "boom")),
            ),
        ] {
            let resp = AppError(kind).into_response();
            assert_eq!(resp.status(), status);
        }
    }

    #[tokio::test]
    async fn app_error_body_is_json() {
        let resp = AppError(ErrorKind::BadRequest("bad input".into())).into_response();
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "bad input");
    }

    #[test]
    fn rate_limit_error_exposes_safe_retry_after_header() {
        let response = AppError::from(phrona::Error::rate_limited(
            "orchestrator",
            Some(std::time::Duration::from_secs(30)),
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "30");
    }

    #[test]
    fn authorized_semantics() {
        let client = PhronaConfig::defaults().search_client().unwrap();
        let state = AppState::new(
            client,
            Some("test-secret".into()),
            1000,
            0,
            100_000,
            Vec::new(),
        );
        assert!(state.authorized(Some("test-secret")));
        // wrong key, shorter key, longer key, missing key
        assert!(!state.authorized(Some("wrong")));
        assert!(!state.authorized(Some("test-secre")));
        assert!(!state.authorized(Some("test-secret2")));
        assert!(!state.authorized(None));
        // no configured key: every request is accepted
        let open = AppState::new(
            PhronaConfig::defaults().search_client().unwrap(),
            None,
            1000,
            0,
            100_000,
            Vec::new(),
        );
        assert!(open.authorized(None));
        assert!(open.authorized(Some("anything")));
    }

    #[test]
    fn client_ip_honors_trusted_proxies_only() {
        let peer = "10.0.0.5:443".parse::<SocketAddr>().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("203.0.113.9, 10.0.0.1"),
        );
        // untrusted peer: the header is ignored (no spoofing)
        assert_eq!(client_ip(&[], peer, &headers), peer.ip());
        // trusted peer: leftmost forwarded address wins
        assert_eq!(
            client_ip(&["10.0.0.5".parse().unwrap()], peer, &headers),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        // trusted peer with malformed header falls back to the peer
        let mut bad = HeaderMap::new();
        bad.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_static("not-an-ip"),
        );
        assert_eq!(
            client_ip(&["10.0.0.5".parse().unwrap()], peer, &bad),
            peer.ip()
        );
        // a different trusted proxy listed but not the actual peer: no trust
        assert_eq!(
            client_ip(&["10.0.0.9".parse().unwrap()], peer, &headers),
            peer.ip()
        );
    }

    #[tokio::test]
    async fn json_query_rejects_missing_fields_as_bad_request() {
        use tower::ServiceExt;
        let router = router_with(PhronaConfig::defaults());
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/v1/search")
                    .header("x-api-key", "test-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["error"].as_str().unwrap().to_lowercase(),
            "invalid query parameters: missing field `q`"
        );
    }

    #[tokio::test]
    async fn query_string_api_key_is_rejected_with_400() {
        use tower::ServiceExt;
        let router = router_with(PhronaConfig::defaults());
        for path in [
            "/v1/search?q=rust&api_key=test-secret",
            "/v1/suggest?q=ru&api_key=test-secret",
            "/v1/test?query=rust&api_key=test-secret",
            "/v1/extract?url=https://example.com&api_key=test-secret",
            "/v1/grounding?query=rust&api_key=test-secret",
        ] {
            let resp = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "path {path}");
            let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            let msg = v["error"].as_str().unwrap_or("");
            assert!(
                msg.contains("query string"),
                "path {path} got unexpected message: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn header_auth_grants_access_before_validation() {
        use tower::ServiceExt;
        let router = router_with(PhronaConfig::defaults());
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/v1/search?q=rust&category=bogus")
                    .header("x-api-key", "test-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // auth passed (no 401); validation then rejects the category
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v["error"]
                .as_str()
                .unwrap_or("")
                .contains("invalid category")
        );
    }

    #[tokio::test]
    async fn bearer_token_is_accepted() {
        use tower::ServiceExt;
        let router = router_with(PhronaConfig::defaults());
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/v1/search?q=rust&category=bogus")
                    .header("authorization", "Bearer test-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn auth_key_resolves_body_then_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "header-key".parse().unwrap());
        assert_eq!(auth_key(&headers, None).as_deref(), Some("header-key"));
        assert_eq!(
            auth_key(&headers, Some("body-key")).as_deref(),
            Some("body-key")
        );
        assert_eq!(auth_key(&headers, Some("")), Some(String::new()));
        let mut bearer = HeaderMap::new();
        bearer.insert("authorization", "Bearer b-key".parse().unwrap());
        assert_eq!(auth_key(&bearer, None).as_deref(), Some("b-key"));
        assert_eq!(auth_key(&HeaderMap::new(), None), None);
    }

    #[tokio::test]
    async fn rate_limit_returns_429_after_window_exhausted() {
        use tower::ServiceExt;
        let mut cfg = PhronaConfig::defaults();
        cfg.server.rate_limit_per_minute = 2;
        let router = router_with(cfg);
        for _ in 0..2 {
            let resp = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/v1/search?q=rust&category=bogus")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            // auth is checked before the search runs, so no network happens
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/search?q=rust")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("rate limit"));
    }

    #[tokio::test]
    async fn oversized_body_is_413_json() {
        use tower::ServiceExt;
        let mut cfg = PhronaConfig::defaults();
        cfg.server.max_body_bytes = 8;
        let router = super::router(cfg);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/extract")
                    .header("content-type", "application/json")
                    .header("content-length", "30")
                    .body(Body::from(r#"{"url": "https://example.com"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("8-byte limit"));
    }

    #[tokio::test]
    async fn rate_limit_honors_connect_info_ip() {
        use tower::ServiceExt;
        let mut cfg = PhronaConfig::defaults();
        cfg.server.api_key = Some("test-secret".into());
        cfg.server.rate_limit_per_minute = 1;
        let router = super::router(cfg);
        // Same IP, different ephemeral ports (as real TCP clients produce):
        // the window must be keyed on the IP, not the socket address, or a
        // fresh bucket would be allocated per connection and the limiter
        // would never trip.
        let req = |port: u16| {
            Request::builder()
                .uri("/v1/search?q=rust&category=bogus")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], port))))
                .header("x-api-key", "test-secret")
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            router.clone().oneshot(req(4242)).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            router.clone().oneshot(req(4243)).await.unwrap().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn source_policy_query_fields_compile_to_core_policy() {
        let params = SearchParams {
            q: "rust".into(),
            category: None,
            engines: None,
            page: None,
            max_results: None,
            safesearch: None,
            region: None,
            language: None,
            time_range: None,
            filters: None,
            source_policy_mode: Some("require-allowed".into()),
            allowed_domains: Some("Docs.Example.com,unknown.example".into()),
            excluded_domains: Some("private.example".into()),
        };
        let options = build_options(&params, 100).unwrap();
        assert_eq!(
            options.source_policy.mode(),
            phrona::SourceMode::RequireAllowed
        );
        assert_eq!(
            options.source_policy.allowed()[0].as_str(),
            "docs.example.com"
        );
        assert_eq!(
            options.source_policy.denied()[0].as_str(),
            "private.example"
        );
    }

    #[test]
    fn omitted_source_policy_preserves_any_mode() {
        let params = SearchParams {
            q: "rust".into(),
            category: None,
            engines: None,
            page: None,
            max_results: None,
            safesearch: None,
            region: None,
            language: None,
            time_range: None,
            filters: None,
            source_policy_mode: None,
            allowed_domains: None,
            excluded_domains: None,
        };
        assert_eq!(
            build_options(&params, 100).unwrap().source_policy.mode(),
            phrona::SourceMode::Any
        );
    }
}
