//! Orchestration: concurrent engines, merge, suggestions.

use std::sync::Arc;
use std::time::Instant;

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Semaphore;

use crate::client::{HttpClient, Profile, ProxyPool, TargetPolicy};
use crate::config::PhronaConfig;
use crate::dedup::{GroupedResult, group};
use crate::engine::{EngineContext, EngineShared, resolve};
use crate::error::{Error, ErrorKind, Result, smallest_retry_after};
use crate::models::{Category, EngineReport, RawResult, ResultItem, SearchResponse, WebResult};
use crate::options::SearchOptions;
use crate::rank::rank_with_policy;
use crate::source_policy::{SourceCatalogue, SourcePolicy};

/// Default maximum number of simultaneous outbound engine requests per
/// search (overridable via [`SearchClient::with_config`] /
/// `search.concurrency_limit`).
const MAX_CONCURRENT_ENGINES: usize = 8;

/// Apply local source eligibility to one provider batch. Answer markers
/// (empty URLs) remain available to the answer aggregator, while every URL
/// is assessed and annotated before quota, early-exit, grouping, agreement,
/// ranking, or conversion can observe it. Malformed or rejected provider
/// URLs are simply removed from the batch; they cannot consume quota.
pub(crate) fn apply_source_policy(
    items: Vec<RawResult>,
    policy: &SourcePolicy,
    catalogue: &SourceCatalogue,
) -> Vec<RawResult> {
    items
        .into_iter()
        .filter_map(|mut result| {
            if result.url.is_empty() {
                return Some(result);
            }
            let assessment = policy.assessment_for_url(&result.url, catalogue).ok()?;
            if !assessment.allowed() {
                return None;
            }
            result.source_assessment = Some(assessment);
            Some(result)
        })
        .collect()
}

/// Whether every provider attempt represented by the reports failed with a
/// rate limit. Empty reports are deliberately false: there was no attempted
/// provider to justify a global 429.
pub(crate) fn all_attempted_rate_limited(
    reports: &[EngineReport],
    expected_attempts: usize,
) -> bool {
    expected_attempts > 0
        && reports.len() == expected_attempts
        && reports.iter().all(|report| {
            report.status == "error"
                && report
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind.starts_with("RateLimited"))
        })
}

/// Observes completed engine requests. Implemented by higher layers (e.g.
/// the REST API's Prometheus metrics); the default is a no-op so libraries
/// and CLI tools never pay for telemetry they don't serve.
///
/// `status` is one of `ok`, `empty` or `error`. `scope`/`kind` describe the
/// failure reason and are `None` on success.
pub trait EngineObserver: Send + Sync {
    /// Called after an engine request completes, with the engine name, its
    /// status (`ok` / `empty` / `error`), optional structured failure labels
    /// and the elapsed time.
    fn on_engine_done(
        &self,
        engine: &str,
        status: &str,
        scope: Option<&str>,
        kind: Option<&str>,
        elapsed: std::time::Duration,
    );
}

/// Default observer that does nothing.
#[derive(Default)]
pub struct NoopEngineObserver;

impl EngineObserver for NoopEngineObserver {
    fn on_engine_done(
        &self,
        _engine: &str,
        _status: &str,
        _scope: Option<&str>,
        _kind: Option<&str>,
        _elapsed: std::time::Duration,
    ) {
    }
}

/// High-level search client. Shares a persistent pool of impersonated HTTP
/// clients (one per proxy) across engines; each engine task is pinned to one
/// client so multi-step flows keep the same proxy and cookie jar.
pub struct SearchClient {
    pool: ProxyPool,
    shared: Arc<EngineShared>,
    concurrency: usize,
    observer: Arc<dyn EngineObserver>,
    /// Whether blocked bootstrap engines may be refreshed by briefly
    /// launching a headless browser (see `crate::bootstrap`).
    /// Default: disabled - browser use is opt-in.
    auto_bootstrap: bool,
    /// Operator-owned source authority catalogue shared by searches and
    /// fetches. Request scope remains in [`SearchOptions`].
    source_catalogue: SourceCatalogue,
    #[cfg(feature = "test-support")]
    test_engines: Option<Vec<&'static dyn crate::engine::Engine>>,
}

/// Environment opt-in for automatic session refresh. Accepts
/// `PHRONA_AUTO_BOOTSTRAP` (canonical) and the config-layer alias
/// `PHRONA_ENGINES_AUTO_BOOTSTRAP`; truthy values: 1/true/yes/on.
fn env_auto_bootstrap() -> Option<bool> {
    for key in ["PHRONA_AUTO_BOOTSTRAP", "PHRONA_ENGINES_AUTO_BOOTSTRAP"] {
        if let Ok(v) = std::env::var(key) {
            return Some(matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ));
        }
    }
    None
}

impl SearchClient {
    /// Build a client with default settings.
    pub fn new() -> Result<Self> {
        Self::with_options(Profile::Chrome, None, None, TargetPolicy::default())
    }

    /// Build a client with explicit transport settings: impersonation
    /// profile, per-request timeout, an optional list of proxy URLs (one
    /// pooled client per proxy, used round-robin; empty = direct), and the
    /// operator's domain allow/deny policy.
    pub fn with_options(
        profile: Profile,
        timeout: Option<std::time::Duration>,
        proxies: Option<Vec<String>>,
        policy: TargetPolicy,
    ) -> Result<Self> {
        let timeout = timeout.unwrap_or_else(|| std::time::Duration::from_secs(10));
        let pool = ProxyPool::new(proxies.unwrap_or_default(), profile, timeout, policy)?;
        let client = Self {
            pool,
            shared: Arc::new(EngineShared::new()),
            concurrency: MAX_CONCURRENT_ENGINES,
            observer: Arc::new(NoopEngineObserver),
            // opt-in: no browser is ever launched unless explicitly
            // enabled via builder, config, or environment
            auto_bootstrap: env_auto_bootstrap().unwrap_or(false),
            source_catalogue: SourceCatalogue::default(),
            #[cfg(feature = "test-support")]
            test_engines: None,
        };
        Self::warm_start(&client);
        Ok(client)
    }

    /// Build a client from a [`PhronaConfig`]: impersonation profile,
    /// timeout, proxy pool, domain policy and per-search concurrency limit.
    pub fn with_config(cfg: &PhronaConfig) -> Result<Self> {
        let source_catalogue = cfg
            .source_catalogue()
            .map_err(|_| Error::invalid_query("config", "invalid source catalogue"))?;
        let mut client = Self::with_options(
            cfg.profile(),
            Some(cfg.timeout()),
            Some(cfg.engines.proxies.clone()),
            TargetPolicy::from_security(&cfg.security),
        )?;
        client.concurrency = cfg.concurrency_limit().max(1);
        client.auto_bootstrap = cfg.engines.auto_bootstrap;
        client.source_catalogue = source_catalogue;
        for (engine, cookies) in &cfg.engines.bootstrap_cookies {
            client.shared.set_bootstrap(engine, cookies.clone());
        }
        // manual pins win over the local cache
        for engine in cfg.engines.bootstrap_cookies.keys() {
            client.shared.bootstrap_at.write().remove(engine);
        }
        Self::warm_start(&client);
        Ok(client)
    }

    /// Load sessions from the local cache (`phrona.cookies.json` next to
    /// the config) and seed per-engine refresh clocks from their ages, so
    /// restarts reuse recent sessions without any browsing.
    fn warm_start(client: &SearchClient) {
        for (engine, _, _) in crate::bootstrap::SEEDS {
            if let Some((jar, at)) = crate::bootstrap::load_cached(engine) {
                if jar.is_empty() {
                    continue;
                }
                if client.shared.bootstrap_for(engine).is_some() {
                    continue; // an explicit pin already provides this engine
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(at);
                client.shared.set_bootstrap(engine, jar);
                client
                    .shared
                    .seed_bootstrap_age(engine, now.saturating_sub(at));
            }
        }
    }

    /// Enable/disable automatic session refresh via a brief headless
    /// browser when a bootstrap engine is blocked. Off by default; the
    /// `PHRONA_AUTO_BOOTSTRAP` environment variable sets the initial
    /// value for every client.
    pub fn with_auto_bootstrap(mut self, enabled: bool) -> Self {
        self.auto_bootstrap = enabled;
        self
    }

    /// Whether automatic session refresh is currently enabled.
    pub fn auto_bootstrap_enabled(&self) -> bool {
        self.auto_bootstrap
    }

    /// Per-engine spacing between automatic harvest attempts.
    fn refresh_spacing_ok(&self, engine: &str) -> bool {
        match self.shared.bootstrap_at.read().get(engine) {
            Some(at) => at.elapsed() >= crate::bootstrap::min_refresh_interval(engine),
            None => true,
        }
    }

    /// Register session cookies for an engine on this client in place
    /// (interior-mutable variant of [`Self::with_bootstrap_cookie`]).
    pub fn register_bootstrap_cookie(&self, engine: impl Into<String>, cookies: impl Into<String>) {
        self.shared.set_bootstrap(&engine.into(), cookies);
    }

    /// Register operator-supplied session cookies for an engine (e.g.
    /// Google's `__Secure-ENID`). Chainable builder method.
    pub fn with_bootstrap_cookie(
        self,
        engine: impl Into<String>,
        cookies: impl Into<String>,
    ) -> Self {
        self.shared.set_bootstrap(&engine.into(), cookies);
        self
    }

    /// The configured per-search engine concurrency cap.
    pub fn concurrency_limit(&self) -> usize {
        self.concurrency
    }

    /// Attach an observer notified after every engine request completes
    /// (`ok` / `empty` / `error` plus scope, kind and elapsed time).
    pub fn with_observer(mut self, observer: Arc<dyn EngineObserver>) -> Self {
        self.observer = observer;
        self
    }

    /// The first (or only) pooled client — used by non-engine flows such as
    /// `extract` and `suggest`.
    pub fn http(&self) -> &HttpClient {
        self.pool.first()
    }

    /// The validated operator-owned source authority catalogue.
    pub fn source_catalogue(&self) -> &SourceCatalogue {
        &self.source_catalogue
    }

    /// Attach the validated operator-owned source catalogue to a client.
    /// Request-specific scope remains in [`SearchOptions`].
    pub fn with_source_catalogue(mut self, catalogue: SourceCatalogue) -> Self {
        self.source_catalogue = catalogue;
        self
    }

    /// Attach a deterministic engine set for offline adapter integration
    /// tests. This API is only compiled with the `test-support` feature and
    /// is never used by the registry-backed production path.
    #[cfg(feature = "test-support")]
    pub fn with_test_engines(mut self, engines: Vec<&'static dyn crate::engine::Engine>) -> Self {
        self.test_engines = Some(engines);
        self
    }

    /// Run a search across all enabled engines for the category.
    ///
    /// Each engine task is assigned one sticky `HttpClient` from the proxy
    /// pool, and runs under a [`Semaphore`] limiting concurrency to the
    /// client's concurrency cap (default `MAX_CONCURRENT_ENGINES`).
    /// Engines run concurrently (`FuturesUnordered`) under a single adaptive
    /// deadline (`opts.timeout`). As soon as the merged result set reaches
    /// `opts.max_results` the remaining in-flight engine futures are dropped
    /// (cancelled) and we return early. An engine that returns an `Ok` —
    /// even with zero results — counts as a success; an error is only raised
    /// when every engine failed. On page 1 of Web searches, suggestions are
    /// fetched in parallel with the scraping via `tokio::join!`.
    pub async fn search(&self, opts: SearchOptions) -> Result<SearchResponse> {
        #[cfg(feature = "test-support")]
        if let Some(engines) = &self.test_engines {
            return self.search_with_engines(opts, engines.clone()).await;
        }
        let engines = resolve(&opts, opts.category);
        if engines.is_empty() {
            return Err(Error::invalid_query(
                "orchestrator",
                "no engines available for category",
            ));
        }
        self.search_with_engines(opts, engines).await
    }

    /// Run the orchestration against a deterministic engine set.
    ///
    /// The production [`Self::search`] method supplies the registered engine
    /// set. Keeping the orchestration independent from that registry gives
    /// offline tests a fake-engine seam without introducing network calls or
    /// changing the public engine selection contract.
    async fn search_with_engines(
        &self,
        opts: SearchOptions,
        engines: Vec<&'static dyn crate::engine::Engine>,
    ) -> Result<SearchResponse> {
        let started = Instant::now();
        let deadline = started + opts.timeout;
        let max_results = opts.max_results;
        let category = opts.category;

        let sem = Arc::new(Semaphore::new(self.concurrency));

        let futs = engines.iter().map(|engine| {
            let client = self.pool.get_client();
            let shared = Arc::clone(&self.shared);
            let sem = Arc::clone(&sem);
            let opts = &opts;
            async move {
                let ctx = EngineContext {
                    client,
                    opts,
                    shared: &shared,
                };
                let started = Instant::now();
                let _permit = sem.acquire().await.expect("semaphore closed");
                let r = engine.search(&ctx).await;
                (engine.name(), r, started.elapsed())
            }
        });
        let mut in_flight = FuturesUnordered::from_iter(futs);
        let source_policy = &opts.source_policy;
        let source_catalogue = &self.source_catalogue;

        let scrape = async move {
            let mut answers: Vec<RawResult> = Vec::new();
            let mut raw: Vec<RawResult> = Vec::new();
            let mut reports: Vec<EngineReport> = Vec::new();
            let mut retry_after_hints = Vec::new();
            let mut any_ok = false;

            while let Some((name, result, elapsed)) = in_flight.next().await {
                if Instant::now() >= deadline {
                    drop(in_flight);
                    break;
                }
                match result {
                    Ok(items) => {
                        any_ok = true;
                        let items = apply_source_policy(items, source_policy, source_catalogue);
                        if items.is_empty() {
                            self.observer
                                .on_engine_done(name, "empty", None, None, elapsed);
                            reports.push(EngineReport {
                                name: name.to_string(),
                                status: "empty".into(),
                                results: 0,
                                error: None,
                                scope: None,
                                kind: None,
                            });
                            continue;
                        }
                        let n = items.len();
                        self.observer
                            .on_engine_done(name, "ok", None, None, elapsed);
                        let (answers_part, raw_part): (Vec<_>, Vec<_>) =
                            items.into_iter().partition(|r| r.url.is_empty());
                        answers.extend(answers_part);
                        raw.extend(raw_part);
                        reports.push(EngineReport {
                            name: name.to_string(),
                            status: "ok".into(),
                            results: n,
                            error: None,
                            scope: None,
                            kind: None,
                        });
                    }
                    Err(e) => {
                        if let ErrorKind::RateLimited { retry_after } = e.kind() {
                            retry_after_hints.push(*retry_after);
                        }
                        let scope = format!("{:?}", e.scope());
                        let kind = format!("{:?}", e.kind());
                        self.observer.on_engine_done(
                            name,
                            "error",
                            Some(&scope),
                            Some(&kind),
                            elapsed,
                        );
                        reports.push(EngineReport {
                            name: name.to_string(),
                            status: "error".into(),
                            results: 0,
                            error: Some(e.to_string()),
                            scope: Some(scope),
                            kind: Some(kind),
                        });
                    }
                }
                if !opts.probe_all && raw.len() >= max_results {
                    drop(in_flight);
                    break;
                }
            }
            (raw, answers, reports, retry_after_hints, any_ok)
        };

        let suggestions = async {
            if category == Category::Web && opts.page == 1 {
                let client = self.pool.get_client();
                crate::engines::suggest::suggest_all(client, &opts.query, &opts.region_param())
                    .await
                    .into_iter()
                    .flat_map(|(_, s)| s)
                    .filter(|s| !s.is_empty())
                    .take(10)
                    .collect()
            } else {
                Vec::new()
            }
        };

        let ((mut raw, mut answers, mut reports, mut retry_after_hints, mut any_ok), suggestions) =
            tokio::join!(scrape, suggestions);

        // Silent bypass: engines whose anti-bot trusts only real-browser
        // cookies get one headless harvest + retry when blocked.
        if self.auto_bootstrap {
            // NB: reports arrive in COMPLETION order - match by name
            let by_name: std::collections::HashMap<&str, &'static dyn crate::engine::Engine> =
                engines.iter().map(|e| (e.name(), *e)).collect();
            let stale: Vec<&'static dyn crate::engine::Engine> = reports
                .iter()
                .filter(|r| {
                    r.status == "error"
                        // ErrorKind::Debug renders as "Blocked(...)" /
                        // "NetworkFailure" - both mean the session cookies
                        // may be missing/stale for a bootstrap engine
                        && r.kind.as_deref().is_some_and(|k| {
                            k.starts_with("Blocked") || k.starts_with("NetworkFailure")
                        })
                        && crate::bootstrap::seed_for(&r.name).is_some()
                        && self.shared.bootstrap_stale(&r.name)
                        && self.refresh_spacing_ok(&r.name)
                })
                .filter_map(|r| by_name.get(r.name.as_str()).copied())
                .collect();
            if std::env::var_os("PHRONA_DEBUG_BOOTSTRAP").is_some() && !stale.is_empty() {
                eprintln!(
                    "[dbg bootstrap] refreshing {:?}",
                    stale.iter().map(|e| e.name()).collect::<Vec<_>>()
                );
            }
            if !stale.is_empty() {
                for engine in &stale {
                    let name = engine.name();
                    match tokio::task::spawn_blocking({
                        move || crate::bootstrap::harvest_blocking(name)
                    })
                    .await
                    {
                        Ok(Ok(jar)) => {
                            if std::env::var_os("PHRONA_DEBUG_BOOTSTRAP").is_some() {
                                eprintln!("[dbg bootstrap] {name}: harvested {} bytes", jar.len());
                            }
                            // persist for the next run of a local install
                            let name2 = name;
                            let jar2 = jar.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                crate::bootstrap::store_cached(name2, &jar2)
                            })
                            .await;
                            self.shared.set_bootstrap(name, jar);
                        }
                        Ok(Err(e)) => {
                            if std::env::var_os("PHRONA_DEBUG_BOOTSTRAP").is_some() {
                                eprintln!("[dbg bootstrap] {name}: harvest failed: {e}");
                            }
                            continue;
                        }
                        Err(_) => continue,
                    }
                    self.shared.mark_bootstrap_refreshed(name);
                }

                // rerun the blocked engines once with fresh cookies
                let sem2 = Arc::new(Semaphore::new(self.concurrency));
                let futs = stale.iter().map(|engine| {
                    let client = self.pool.get_client();
                    let shared = Arc::clone(&self.shared);
                    let sem = Arc::clone(&sem2);
                    let opts = &opts;
                    async move {
                        let ctx = EngineContext {
                            client,
                            opts,
                            shared: &shared,
                        };
                        let started = Instant::now();
                        let _permit = sem.acquire().await.expect("semaphore closed");
                        (engine.name(), engine.search(&ctx).await, started.elapsed())
                    }
                });
                let mut retry = FuturesUnordered::from_iter(futs);
                while let Some((name, result, elapsed)) = retry.next().await {
                    let slot = reports.iter_mut().find(|r| r.name == name);
                    match result {
                        Ok(items) if items.is_empty() => {
                            any_ok = true;
                            if let Some(r) = slot {
                                *r = EngineReport {
                                    name: name.to_string(),
                                    status: "empty".into(),
                                    results: 0,
                                    error: None,
                                    scope: None,
                                    kind: None,
                                };
                            }
                            self.observer
                                .on_engine_done(name, "empty", None, None, elapsed);
                        }
                        Ok(items) => {
                            any_ok = true;
                            let items = apply_source_policy(items, source_policy, source_catalogue);
                            if items.is_empty() {
                                if let Some(r) = slot {
                                    *r = EngineReport {
                                        name: name.to_string(),
                                        status: "empty".into(),
                                        results: 0,
                                        error: None,
                                        scope: None,
                                        kind: None,
                                    };
                                }
                                self.observer
                                    .on_engine_done(name, "empty", None, None, elapsed);
                                continue;
                            }
                            let n = items.len();
                            if let Some(r) = slot {
                                *r = EngineReport {
                                    name: name.to_string(),
                                    status: "ok".into(),
                                    results: n,
                                    error: None,
                                    scope: None,
                                    kind: None,
                                };
                            }
                            self.observer
                                .on_engine_done(name, "ok", None, None, elapsed);
                            let (a_part, r_part): (Vec<_>, Vec<_>) =
                                items.into_iter().partition(|x| x.url.is_empty());
                            answers.extend(a_part);
                            raw.extend(r_part);
                        }
                        Err(e) => {
                            if let ErrorKind::RateLimited { retry_after } = e.kind() {
                                retry_after_hints.push(*retry_after);
                            }
                            if std::env::var_os("PHRONA_DEBUG_BOOTSTRAP").is_some() {
                                eprintln!("[dbg bootstrap] {name} retry failed: {e}");
                            }
                            let scope_s = format!("{:?}", e.scope());
                            let kind_s = format!("{:?}", e.kind());
                            if let Some(r) = slot {
                                *r = EngineReport {
                                    name: name.to_string(),
                                    status: "error".into(),
                                    results: 0,
                                    error: Some(e.to_string()),
                                    scope: Some(scope_s.clone()),
                                    kind: Some(kind_s.clone()),
                                };
                            }
                            self.observer.on_engine_done(
                                name,
                                "error",
                                Some(&scope_s),
                                Some(&kind_s),
                                elapsed,
                            );
                        }
                    }
                }
            }
        }

        if !any_ok {
            if all_attempted_rate_limited(&reports, engines.len()) {
                return Err(Error::rate_limited(
                    "orchestrator",
                    smallest_retry_after(retry_after_hints),
                ));
            }
            // Availability probing wants the full per-engine report even for
            // a category where every engine failed; normal searches surface
            // the failure as an error instead.
            if opts.probe_all {
                return Ok(SearchResponse {
                    query: opts.query.clone(),
                    category,
                    page: opts.page,
                    total: 0,
                    results: Vec::new(),
                    suggestions,
                    answer: None,
                    engines: reports,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            let details = reports
                .iter()
                .filter_map(|r| r.error.as_ref().map(|e| format!("{}: {}", r.name, e)))
                .collect();
            return Err(Error::all_failed("orchestrator", details));
        }

        let answer = answers
            .into_iter()
            .map(|a| a.description)
            .max_by_key(|a| a.chars().count());

        let groups = group(raw);
        let ranked = rank_with_policy(groups, &opts.query, opts.source_policy.mode());
        let mut results: Vec<ResultItem> = Vec::new();
        for (raw_score, g) in ranked.into_iter() {
            // unified cross-category score, normalized to (0.001, 1.000),
            // derived from the raw score `rank` already computed
            let score = crate::rank::normalize_score(raw_score);
            let item = to_result_item_with_mode(g, score, results.len(), opts.source_policy.mode());
            if let Some(item) = item {
                results.push(item);
            }
            if results.len() >= opts.max_results {
                break;
            }
        }

        Ok(SearchResponse {
            query: opts.query.clone(),
            category,
            page: opts.page,
            total: results.len(),
            results,
            suggestions,
            answer,
            engines: reports,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Blocking API for use from plain (non-tokio) threads.
    ///
    /// Calling this from inside an active Tokio runtime would deadlock or
    /// panic (`block_on` inside a worker thread), so it refuses and asks the
    /// caller to use the async [`SearchClient::search`] instead.
    pub fn search_sync(&self, opts: SearchOptions) -> Result<SearchResponse> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(Error::internal(
                "search",
                "search_sync cannot be called from within an active Tokio runtime thread pool; use async search().await instead",
            ));
        }
        block_on(self.search(opts))
    }
}

/// Convert a merged, ranked group into a typed [`ResultItem`] for the
/// response. The category is inferred from the engine that introduced the
/// result; unknown engines map to `Web`. `idx` is the zero-based result
/// index (position becomes `idx + 1`). Returns `None` only when a result
/// carries no URL and cannot be placed.
pub fn to_result_item(g: GroupedResult, score: f64, idx: usize) -> Option<ResultItem> {
    to_result_item_with_mode(g, score, idx, crate::source_policy::SourceMode::Any)
}

/// Convert a merged group while retaining its source-policy mode in result
/// metadata. The legacy [`to_result_item`] helper defaults to `any`.
pub fn to_result_item_with_mode(
    g: GroupedResult,
    score: f64,
    idx: usize,
    source_policy_mode: crate::source_policy::SourceMode,
) -> Option<ResultItem> {
    let raw = g.result;
    let category = crate::engine::category_of_engine(&raw.engine);
    let position = idx + 1;
    let assessment = raw.source_assessment.unwrap_or_default();
    match category {
        Category::Web => Some(ResultItem::Web(WebResult {
            title: raw.title,
            url: raw.url,
            description: raw.description,
            engines: g.engines,
            position,
            score,
            source_policy_mode,
            requested_match: assessment.requested_match,
            source_tier: assessment.source_tier,
            policy_reason: assessment.reason,
        })),
        Category::Images => Some(ResultItem::Image(crate::models::ImageResult {
            title: raw.title,
            url: raw.url,
            image_url: raw.image_url,
            thumbnail_url: raw.thumbnail_url,
            width: raw.width,
            height: raw.height,
            source: raw.source,
            engines: g.engines,
            position,
            score,
            source_policy_mode,
            requested_match: assessment.requested_match,
            source_tier: assessment.source_tier,
            policy_reason: assessment.reason,
        })),
        Category::News => Some(ResultItem::News(crate::models::NewsResult {
            title: raw.title,
            url: raw.url,
            description: raw.description,
            published: raw.published,
            source: raw.source,
            image_url: raw.image_url,
            engines: g.engines,
            position,
            score,
            source_policy_mode,
            requested_match: assessment.requested_match,
            source_tier: assessment.source_tier,
            policy_reason: assessment.reason,
        })),
        Category::Videos => Some(ResultItem::Video(crate::models::VideoResult {
            title: raw.title,
            url: raw.url,
            description: raw.description,
            duration: raw.duration,
            published: raw.published,
            uploader: raw.uploader,
            views: raw.views,
            thumbnail_url: raw.thumbnail_url,
            engines: g.engines,
            position,
            score,
            source_policy_mode,
            requested_match: assessment.requested_match,
            source_tier: assessment.source_tier,
            policy_reason: assessment.reason,
        })),
        Category::Books => Some(ResultItem::Book(crate::models::BookResult {
            title: raw.title,
            author: raw.author,
            publisher: raw.publisher,
            info: raw.description,
            url: raw.url,
            thumbnail_url: raw.thumbnail_url,
            engines: g.engines,
            position,
            score,
            source_policy_mode,
            requested_match: assessment.requested_match,
            source_tier: assessment.source_tier,
            policy_reason: assessment.reason,
        })),
    }
}

/// Block a future on the ambient runtime, or a shared one otherwise.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => RUNTIME
            .get_or_init(|| tokio::runtime::Runtime::new().expect("tokio runtime"))
            .block_on(fut),
    }
}

static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

/// Convenience: single-shot async search with default client.
pub async fn search(opts: SearchOptions) -> Result<SearchResponse> {
    SearchClient::new()?.search(opts).await
}

/// Convenience: single-shot blocking search.
pub fn search_sync(opts: SearchOptions) -> Result<SearchResponse> {
    SearchClient::new()?.search_sync(opts)
}

/// List engines available for a category (name + metadata).
pub fn available_engines(category: Category) -> Vec<crate::models::EngineReport> {
    crate::engine::engines_for(category)
        .iter()
        .map(|e| EngineReport {
            name: e.name().to_string(),
            status: "enabled".into(),
            results: 0,
            error: None,
            scope: None,
            kind: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::dedup::GroupedResult;
    use crate::engine::{Engine, EngineContext};
    use crate::error::Error;
    use crate::models::RawResult;
    use crate::models::ResultItem;
    use crate::search::{all_attempted_rate_limited, apply_source_policy, to_result_item};
    use crate::source_policy::{SourceCatalogue, SourcePolicy, SourceTier};
    use crate::{EngineReport, SearchClient, SearchOptions};

    enum FakeOutcome {
        Results(Vec<RawResult>),
        Error(Error),
        Pending,
    }

    struct FakeEngine {
        name: &'static str,
        outcome: FakeOutcome,
        calls: Arc<AtomicUsize>,
        cancelled: Arc<AtomicBool>,
    }

    struct CancellationProbe(Arc<AtomicBool>);

    impl Drop for CancellationProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Engine for FakeEngine {
        fn name(&self) -> &'static str {
            self.name
        }

        fn category(&self) -> crate::models::Category {
            crate::models::Category::Web
        }

        async fn search(&self, _ctx: &EngineContext<'_>) -> crate::Result<Vec<RawResult>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.outcome {
                FakeOutcome::Results(items) => Ok(items
                    .iter()
                    .cloned()
                    .map(|mut item| {
                        item.engine = self.name.into();
                        item
                    })
                    .collect()),
                FakeOutcome::Error(error) => Err(error.clone()),
                FakeOutcome::Pending => {
                    let _probe = CancellationProbe(Arc::clone(&self.cancelled));
                    tokio::task::yield_now().await;
                    pending::<()>().await;
                    unreachable!("pending fake engine must be cancelled")
                }
            }
        }
    }

    fn fake_engine(name: &'static str, outcome: FakeOutcome) -> &'static FakeEngine {
        Box::leak(Box::new(FakeEngine {
            name,
            outcome,
            calls: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }))
    }

    fn raw(title: &str, url: &str) -> RawResult {
        RawResult {
            title: title.into(),
            url: url.into(),
            description: "desc".into(),
            engine: "bing".into(),
            ..Default::default()
        }
    }

    #[test]
    fn merge_keeps_results_and_answers() {
        // answer marker (empty url) + real results must all survive merging
        let items = vec![
            raw("answer", ""),
            raw("A", "https://example.com/a?utm_source=x"),
            raw("B", "https://example.org/b"),
        ];
        let (answers, rest): (Vec<_>, Vec<_>) =
            items.clone().into_iter().partition(|r| r.url.is_empty());
        assert_eq!(answers.len(), 1);
        assert_eq!(rest.len(), 2);
        let groups = crate::dedup::group(rest);
        assert_eq!(groups.len(), 2);
        assert_eq!(
            crate::dedup::dedup_key(&items[1].url),
            "https://example.com/a"
        );
    }

    #[test]
    fn search_sync_refuses_inside_active_runtime() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let client = SearchClient::new().unwrap();
            let err = client.search_sync(SearchOptions::new("x")).unwrap_err();
            assert!(
                err.to_string().contains("search_sync cannot be called"),
                "got: {err}"
            );
        });
    }

    #[test]
    fn to_result_item_covers_all_categories() {
        for (engine, expect) in [
            ("bing", "web"),
            ("bing_images", "image"),
            ("bing_news", "news"),
            ("bing_videos", "video"),
            ("annas_archive", "book"),
        ] {
            let mut r = raw("title", "https://example.com/x");
            r.engine = engine.into();
            let g = GroupedResult {
                result: r,
                engines: vec!["engine1".into(), "engine2".into()],
                count: 2,
            };
            let item = to_result_item(g, 0.9, 3).expect("engine maps to a category");
            match item {
                ResultItem::Web(w) => {
                    assert_eq!(expect, "web");
                    assert_eq!(w.position, 4);
                    assert_eq!(w.score, 0.9);
                    assert_eq!(w.url, "https://example.com/x");
                    assert_eq!(w.engines, ["engine1", "engine2"]);
                }
                ResultItem::Image(i) => {
                    assert_eq!(expect, "image");
                    assert_eq!(i.position, 4);
                    assert_eq!(i.title, "title");
                }
                ResultItem::News(n) => {
                    assert_eq!(expect, "news");
                    assert_eq!(n.position, 4);
                    assert_eq!(n.description, "desc");
                }
                ResultItem::Video(v) => {
                    assert_eq!(expect, "video");
                    assert_eq!(v.position, 4);
                    assert_eq!(v.uploader, "");
                }
                ResultItem::Book(b) => {
                    assert_eq!(expect, "book");
                    assert_eq!(b.position, 4);
                    assert_eq!(b.author, "");
                }
            }
        }
        // unknown engines map to web
        let mut r = raw("t", "https://example.com/y");
        r.engine = "not_an_engine".into();
        assert!(matches!(
            to_result_item(
                GroupedResult {
                    result: r,
                    engines: vec![],
                    count: 1
                },
                0.5,
                0,
            ),
            Some(ResultItem::Web(_))
        ));
    }

    #[test]
    fn source_filter_precedes_quota_dedup_and_conversion() {
        let policy = SourcePolicy::compile(
            "require-allowed",
            ["allowed.example"],
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let catalogue = SourceCatalogue::default();
        let filtered = apply_source_policy(
            vec![
                raw("blocked", "https://blocked.example/duplicate"),
                raw("allowed", "https://allowed.example/page"),
                raw(
                    "allowed duplicate",
                    "https://allowed.example/page?utm_source=engine",
                ),
            ],
            &policy,
            &catalogue,
        );

        // The rejected provider result never reaches quota accounting or the
        // deduplicator. Both eligible reports remain one grouped source.
        assert_eq!(filtered.len(), 2);
        assert!(
            filtered
                .iter()
                .all(|result| result.url.contains("allowed.example"))
        );
        let mut groups = crate::dedup::group(filtered);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2);
        let item = to_result_item(groups.remove(0), 0.8, 0).unwrap();
        let ResultItem::Web(item) = item else {
            panic!("web fixture must remain a web result")
        };
        assert!(item.requested_match);
        assert_eq!(item.source_tier, SourceTier::Unknown);
    }

    #[test]
    fn strict_policy_returns_sparse_results_without_padding() {
        let policy = SourcePolicy::compile(
            "official-only",
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let catalogue =
            SourceCatalogue::compile(["official.example"], std::iter::empty::<&str>()).unwrap();
        let filtered = apply_source_policy(
            vec![
                raw("official", "https://official.example/a"),
                raw("unknown", "https://unknown.example/b"),
                raw("also unknown", "https://unknown.example/c"),
            ],
            &policy,
            &catalogue,
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].url, "https://official.example/a");
        assert_eq!(
            filtered[0].source_assessment.unwrap().source_tier,
            SourceTier::Official
        );
    }

    #[test]
    fn only_all_rate_limited_provider_reports_are_promoted_to_429() {
        let limited = EngineReport {
            name: "one".into(),
            status: "error".into(),
            results: 0,
            error: Some("rate limited".into()),
            scope: Some("Provider".into()),
            kind: Some("RateLimited { retry_after: None }".into()),
        };
        let limited_two = EngineReport {
            name: "two".into(),
            ..limited.clone()
        };
        assert!(all_attempted_rate_limited(
            &[limited.clone(), limited_two.clone()],
            2
        ));

        let mixed = EngineReport {
            name: "network".into(),
            kind: Some("NetworkFailure".into()),
            ..limited
        };
        assert!(!all_attempted_rate_limited(std::slice::from_ref(&mixed), 1));
        assert!(!all_attempted_rate_limited(
            &[
                EngineReport {
                    name: "limited".into(),
                    kind: Some("RateLimited { retry_after: Some(2s) }".into()),
                    ..mixed.clone()
                },
                mixed,
            ],
            2
        ));
        assert!(!all_attempted_rate_limited(&[limited_two], 2));
        assert!(!all_attempted_rate_limited(&[], 0));
    }

    #[tokio::test]
    async fn search_orchestration_filters_before_quota_dedup_and_early_exit() {
        let policy = SourcePolicy::compile(
            "require-allowed",
            ["allowed.example"],
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let batch = fake_engine(
            "fake-batch",
            FakeOutcome::Results(vec![
                raw("blocked duplicate", "https://blocked.example/page"),
                raw("allowed", "https://allowed.example/page"),
                raw(
                    "allowed duplicate",
                    "https://allowed.example/page?utm_source=fake",
                ),
            ]),
        );
        let pending_engine = fake_engine("fake-pending", FakeOutcome::Pending);
        let client = SearchClient::new().unwrap().with_source_catalogue(
            SourceCatalogue::compile(["official.example"], std::iter::empty::<&str>()).unwrap(),
        );
        let mut opts = SearchOptions::new("offline orchestration");
        opts.max_results = 1;
        opts.page = 2;
        opts.source_policy = policy;

        let response = client
            .search_with_engines(opts, vec![pending_engine, batch])
            .await
            .unwrap();

        assert_eq!(response.total, 1);
        let ResultItem::Web(result) = &response.results[0] else {
            panic!("offline web fixture must produce a web result")
        };
        assert_eq!(result.url, "https://allowed.example/page");
        assert_eq!(result.engines, ["fake-batch"]);
        assert!(result.requested_match);
        let batch_report = response
            .engines
            .iter()
            .find(|report| report.name == "fake-batch")
            .expect("batch fake report must be retained");
        assert_eq!(batch_report.status, "ok");
        assert_eq!(batch_report.results, 2);
        assert_eq!(batch.calls.load(Ordering::SeqCst), 1);
        assert_eq!(response.engines.len(), 1);
        assert!(pending_engine.cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn strict_orchestration_returns_sparse_results_without_filling_or_waiting() {
        let official = fake_engine(
            "fake-official",
            FakeOutcome::Results(vec![raw("official", "https://official.example/page")]),
        );
        let client = SearchClient::new().unwrap().with_source_catalogue(
            SourceCatalogue::compile(["official.example"], std::iter::empty::<&str>()).unwrap(),
        );
        let mut opts = SearchOptions::new("offline strict sparse");
        opts.max_results = 5;
        opts.source_policy = SourcePolicy::compile(
            "official-only",
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
        )
        .unwrap();

        let response = client
            .search_with_engines(opts, vec![official])
            .await
            .unwrap();

        assert_eq!(response.total, 1);
        assert_eq!(response.results.len(), 1);
        let ResultItem::Web(result) = &response.results[0] else {
            panic!("offline web fixture must produce a web result")
        };
        assert_eq!(result.source_tier, SourceTier::Official);
    }

    #[tokio::test]
    async fn search_orchestration_preserves_mixed_provider_outcomes() {
        let limited = fake_engine(
            "fake-limited",
            FakeOutcome::Error(Error::rate_limited(
                "fake-limited",
                Some(std::time::Duration::from_secs(5)),
            )),
        );
        let failed = fake_engine(
            "fake-failed",
            FakeOutcome::Error(Error::network("fake-failed")),
        );
        let client = SearchClient::new().unwrap();
        let mut opts = SearchOptions::new("offline mixed outcomes");
        opts.engines = vec!["fake-limited".into(), "fake-failed".into()];

        let error = client
            .search_with_engines(opts, vec![limited, failed])
            .await
            .unwrap_err();

        assert!(matches!(
            error.kind(),
            crate::error::ErrorKind::AllProvidersFailed { .. }
        ));
        assert_ne!(error.http_status, Some(429));
    }
}
