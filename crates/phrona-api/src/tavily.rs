//! Tavily-compatible `/search` endpoint.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::{Json, http::HeaderMap};
use serde::{Deserialize, Serialize};

use phrona::models::{Category, ResultItem, TimeRange};
use phrona::{NormalizedDomain, PolicyReason, SearchOptions, SourceMode, SourcePolicy, SourceTier};

use crate::{AppError, AppResult, AppState, JsonBody};

/// Tavily-compatible request body.
///
/// The Tavily API (<https://docs.tavily.com>) is the de-facto standard for
/// AI search. Clients such as `tavily-python` can target this server by
/// setting `base_url` to it and calling `/search`.
#[derive(Deserialize)]
pub struct TavilyRequest {
    /// The search query.
    pub query: String,
    #[serde(default)]
    /// Optional API key accepted in the body for Tavily SDK compatibility.
    pub api_key: Option<String>,
    #[serde(default)]
    /// `basic` (two engines) or `advanced` (all engines).
    pub search_depth: Option<String>,
    #[serde(default)]
    /// Optional `news` topic; anything else searches the web.
    pub topic: Option<String>,
    #[serde(default)]
    /// Recent-window in days, mapped to a `TimeRange`.
    pub days: Option<u32>,
    #[serde(default)]
    /// Maximum number of results to return (clamped to 1..=20).
    pub max_results: Option<usize>,
    #[serde(default)]
    /// Whether to populate the `images` field via a dedicated image search.
    pub include_images: bool,
    #[serde(default)]
    /// Whether to return an answer (library answer, e.g. grokipedia).
    pub include_answer: bool,
    #[serde(default)]
    /// Whether to fetch and attach raw page text for each result.
    pub include_raw_content: bool,
    /// Accepted for compatibility with Tavily clients; only meaningful for
    /// Tavily's image-search endpoint, which this server does not expose.
    #[serde(default)]
    pub include_image_descriptions: bool,
    #[serde(default)]
    /// Domains to restrict results to (as `site:` filters).
    pub include_domains: Option<Vec<String>>,
    #[serde(default)]
    /// Domains to exclude (as `-site:` filters).
    pub exclude_domains: Option<Vec<String>>,
    /// Additive local source policy. Legacy include/exclude fields remain
    /// supported and are enforced locally as well as used as provider hints.
    #[serde(default)]
    pub source_policy: Option<crate::SourcePolicyParams>,
}

impl TavilyRequest {
    fn source_policy(&self) -> crate::AppResult<SourcePolicy> {
        let Some(request) = &self.source_policy else {
            let include = self.include_domains.clone().unwrap_or_default();
            let mode = (!include.is_empty()).then_some("require-allowed");
            return crate::compile_source_policy(
                mode,
                include,
                self.exclude_domains.clone().unwrap_or_default(),
            );
        };
        let legacy_include = self.include_domains.clone().unwrap_or_default();
        let has_legacy_include = !legacy_include.is_empty();
        let allowed = if !has_legacy_include {
            request.allowed_domains.clone()
        } else if request.allowed_domains.is_empty() {
            legacy_include.clone()
        } else {
            intersect_domain_scopes(&legacy_include, &request.allowed_domains)?
        };
        if has_legacy_include && allowed.is_empty() {
            return Err(crate::AppError::bad_request(
                "include_domains and source_policy.allowed_domains have no intersection",
            ));
        }
        let mode = match request.mode.as_deref() {
            Some(mode) if has_legacy_include => {
                let parsed = mode.parse::<SourceMode>().map_err(|error| {
                    crate::AppError::bad_request(format!("invalid source policy: {error}"))
                })?;
                match parsed {
                    // Legacy include_domains is a hard scope constraint. These
                    // permissive modes cannot be allowed to widen it.
                    SourceMode::Any | SourceMode::PreferOfficial => Some("require-allowed"),
                    SourceMode::RequireAllowed | SourceMode::OfficialOnly => Some(mode),
                }
            }
            Some(mode) => Some(mode),
            None if !allowed.is_empty() => Some("require-allowed"),
            None => None,
        };
        let mut denied = request.excluded_domains.clone();
        denied.extend(self.exclude_domains.clone().unwrap_or_default());
        crate::compile_source_policy(mode, allowed, denied)
    }
}

fn intersect_domain_scopes(
    legacy: &[String],
    requested: &[String],
) -> crate::AppResult<Vec<String>> {
    let parse = |domains: &[String]| {
        domains
            .iter()
            .map(|domain| {
                NormalizedDomain::parse(domain).map_err(|error| {
                    crate::AppError::bad_request(format!("invalid source policy: {error}"))
                })
            })
            .collect::<crate::AppResult<BTreeSet<_>>>()
    };
    let legacy = parse(legacy)?;
    let requested = parse(requested)?;
    let mut intersection = BTreeSet::new();
    for legacy_domain in &legacy {
        for requested_domain in &requested {
            if legacy_domain.matches_host(requested_domain.as_str()) {
                intersection.insert(requested_domain.clone());
            } else if requested_domain.matches_host(legacy_domain.as_str()) {
                intersection.insert(legacy_domain.clone());
            }
        }
    }
    Ok(intersection
        .into_iter()
        .map(NormalizedDomain::into_string)
        .collect())
}

/// Tavily-compatible response body.
#[derive(Serialize)]
pub struct TavilyResponse {
    /// The query echoed back.
    pub query: String,
    /// Reserved for Tavily compatibility; always empty.
    pub follow_up_questions: Vec<String>,
    /// Wall-clock time spent searching, in seconds.
    pub response_time: f64,
    /// Optional answer, populated when `include_answer` is set.
    pub answer: Option<String>,
    /// Image URLs, populated when `include_images` is set.
    pub images: Option<Vec<String>>,
    /// The ranked search results.
    pub results: Vec<TavilyResult>,
}

/// One result of a Tavily-compatible search.
#[derive(Serialize)]
pub struct TavilyResult {
    /// Page title of the result.
    pub title: String,
    /// URL of the result.
    pub url: String,
    /// Readable content snippet or description.
    pub content: String,
    /// Positional relevance score (1.0 down to 0.05).
    pub score: f64,
    /// Raw page text, populated when `include_raw_content` is set.
    pub raw_content: Option<String>,
    /// Additive source-policy metadata; existing Tavily fields are unchanged.
    pub source_metadata: SourceMetadata,
}

/// Source-policy metadata exposed as an additive Tavily result field.
#[derive(Debug, Clone, Serialize)]
pub struct SourceMetadata {
    /// Mode used for this result.
    pub source_policy_mode: SourceMode,
    /// Whether the URL matched the caller's requested scope.
    pub requested_match: bool,
    /// Authority assigned by the operator catalogue.
    pub source_tier: SourceTier,
    /// Local eligibility explanation.
    pub policy_reason: PolicyReason,
}

fn days_to_range(days: u32) -> Option<TimeRange> {
    Some(match days {
        0..=1 => TimeRange::Day,
        2..=7 => TimeRange::Week,
        8..=30 => TimeRange::Month,
        _ => TimeRange::Year,
    })
}

fn apply_domains(query: &mut String, include: &[String], exclude: &[String]) {
    if !include.is_empty() {
        let sites: Vec<String> = include.iter().map(|d| format!("site:{d}")).collect();
        query.push_str(&format!(" ({})", sites.join(" OR ")));
    }
    for d in exclude {
        query.push_str(&format!(" -site:{d}"));
    }
}

fn to_tavily_result(r: &ResultItem, pos: usize) -> (TavilyResult, SourceMetadata) {
    let score = phrona::rank::positional_score(pos);
    let (title, url, content, metadata) = match r {
        ResultItem::Web(w) => (
            &w.title,
            &w.url,
            &w.description,
            SourceMetadata {
                source_policy_mode: w.source_policy_mode,
                requested_match: w.requested_match,
                source_tier: w.source_tier,
                policy_reason: w.policy_reason,
            },
        ),
        ResultItem::News(n) => (
            &n.title,
            &n.url,
            &n.description,
            SourceMetadata {
                source_policy_mode: n.source_policy_mode,
                requested_match: n.requested_match,
                source_tier: n.source_tier,
                policy_reason: n.policy_reason,
            },
        ),
        ResultItem::Video(v) => (
            &v.title,
            &v.url,
            &v.description,
            SourceMetadata {
                source_policy_mode: v.source_policy_mode,
                requested_match: v.requested_match,
                source_tier: v.source_tier,
                policy_reason: v.policy_reason,
            },
        ),
        ResultItem::Image(i) => (
            &i.title,
            &i.url,
            &i.source,
            SourceMetadata {
                source_policy_mode: i.source_policy_mode,
                requested_match: i.requested_match,
                source_tier: i.source_tier,
                policy_reason: i.policy_reason,
            },
        ),
        ResultItem::Book(b) => (
            &b.title,
            &b.url,
            &b.info,
            SourceMetadata {
                source_policy_mode: b.source_policy_mode,
                requested_match: b.requested_match,
                source_tier: b.source_tier,
                policy_reason: b.policy_reason,
            },
        ),
    };
    (
        TavilyResult {
            title: title.clone(),
            url: url.clone(),
            content: content.clone(),
            score,
            raw_content: None,
            source_metadata: metadata.clone(),
        },
        metadata,
    )
}

/// `POST /search`: Tavily-compatible search. Accepts the same body shape as
/// the Tavily API (query, optional `api_key`, `search_depth`, `topic`,
/// `days`, `max_results`, `include_*` flags and domain filters).
pub async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    JsonBody(req): JsonBody<TavilyRequest>,
) -> AppResult<impl IntoResponse> {
    // Tavily SDKs pass credentials as `api_key` in the JSON body
    // (langchain-tavily, llama-index) or as headers; both are honored.
    let key = crate::auth_key(&headers, req.api_key.as_deref());
    if !state.authorized(key.as_deref()) {
        return Err(AppError::unauthorized());
    }

    let mut opts = SearchOptions::new(req.query.clone());
    let topic_is_news = matches!(req.topic.as_deref(), Some("news"));
    opts.category = if topic_is_news {
        Category::News
    } else {
        Category::Web
    };
    let depth = req.search_depth.as_deref().unwrap_or("basic");
    // "advanced" is honored by querying every engine in the category;
    // anything else is rejected loudly instead of silently coerced.
    if !matches!(depth, "basic" | "advanced") {
        return Err(AppError::bad_request(format!(
            "invalid search_depth '{depth}', expected 'basic' or 'advanced'"
        )));
    }
    if depth == "basic" {
        let mut engines = match opts.category {
            Category::News => vec!["bing_news".into(), "duckduckgo_news".into()],
            _ => vec!["bing".into(), "duckduckgo".into()],
        };
        if req.include_answer {
            engines.push("grokipedia".into());
        }
        opts.engines = engines;
    }
    if let Some(days) = req.days {
        opts.time_range = days_to_range(days);
    } else if topic_is_news {
        // news topic without an explicit window: last week, like Tavily
        opts.time_range = Some(TimeRange::Week);
    }
    opts.max_results = req.max_results.unwrap_or(5).clamp(1, 20);
    let source_policy = req.source_policy()?;
    opts.source_policy = source_policy.clone();
    if let Some(include) = &req.include_domains {
        apply_domains(&mut opts.query, include, &[]);
    }
    if let Some(exclude) = &req.exclude_domains {
        apply_domains(&mut opts.query, &[], exclude);
    }

    let started = std::time::Instant::now();
    let resp = state.client.search(opts).await?;
    let response_time = started.elapsed().as_secs_f64();

    let limit = resp.total.min(req.max_results.unwrap_or(5).clamp(1, 20));
    let mut results: Vec<TavilyResult> = Vec::with_capacity(limit);
    let mut images: Vec<String> = Vec::new();
    for (i, r) in resp.results.iter().take(limit).enumerate() {
        let (result, _) = to_tavily_result(r, i);
        if let ResultItem::Image(img) = r
            && !img.image_url.is_empty()
        {
            images.push(img.image_url.clone());
        }
        results.push(result);
    }

    if req.include_images {
        // Tavily's `images` field lists image results alongside the web
        // hits; run a dedicated image search to populate it honestly.
        let mut img_opts = SearchOptions::new(req.query.clone());
        img_opts.category = Category::Images;
        img_opts.max_results = limit.clamp(1, 8);
        img_opts.source_policy = source_policy.clone();
        if let Ok(img_resp) = state.client.search(img_opts).await {
            for r in img_resp.results.iter().take(8) {
                if let ResultItem::Image(img) = r
                    && !img.image_url.is_empty()
                {
                    images.push(img.image_url.clone());
                }
            }
        }
    }

    if req.include_raw_content {
        let client = state.client.http();
        let urls: Vec<String> = results.iter().map(|r| r.url.clone()).collect();
        let pages = phrona::extract_many_with_policy(
            client,
            &source_policy,
            state.client.source_catalogue(),
            &urls,
            8000,
            Some(&req.query),
        )
        .await;
        for (r, page) in results.iter_mut().zip(pages) {
            r.raw_content = Some(match page {
                Ok(p) => p.text,
                Err(e) => format!("extract failed: {e}"),
            });
        }
    }

    Ok(Json(TavilyResponse {
        query: req.query.clone(),
        follow_up_questions: Vec::new(),
        response_time,
        answer: req.include_answer.then_some(resp.answer.clone()).flatten(),
        images: (req.include_images && !images.is_empty()).then_some(images),
        results,
    }))
}

#[cfg(test)]
mod source_policy_tests {
    use super::*;

    #[test]
    fn nested_policy_is_mapped_without_conferring_authority() {
        let request: TavilyRequest = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "source_policy": {
                "mode": "require-allowed",
                "allowed_domains": ["uncatalogued.example"],
                "excluded_domains": []
            }
        }))
        .unwrap();
        let policy = request.source_policy().unwrap();
        let catalogue = phrona::SourceCatalogue::default();
        let assessment = policy
            .assessment_for_url("https://uncatalogued.example/docs", &catalogue)
            .unwrap();
        assert!(assessment.requested_match);
        assert_eq!(assessment.source_tier, phrona::SourceTier::Unknown);
    }

    #[test]
    fn legacy_include_domains_are_a_local_constraint() {
        let request: TavilyRequest = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "include_domains": ["allowed.example"],
            "exclude_domains": ["blocked.allowed.example"]
        }))
        .unwrap();
        let policy = request.source_policy().unwrap();
        let catalogue = phrona::SourceCatalogue::default();
        assert!(
            policy
                .evaluate_url("https://allowed.example/a", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://blocked.allowed.example/a", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://other.example/a", &catalogue)
                .unwrap()
        );
    }

    #[test]
    fn legacy_include_domains_cannot_be_relaxed_by_nested_any_policy() {
        let request: TavilyRequest = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "include_domains": ["allowed.example"],
            "exclude_domains": ["blocked.allowed.example"],
            "source_policy": {
                "mode": "any",
                "allowed_domains": [],
                "excluded_domains": []
            }
        }))
        .unwrap();
        let policy = request.source_policy().unwrap();
        let catalogue = phrona::SourceCatalogue::default();

        assert!(
            policy
                .evaluate_url("https://allowed.example/docs", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://other.example/docs", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://blocked.allowed.example/docs", &catalogue)
                .unwrap()
        );
    }

    #[test]
    fn legacy_and_nested_allowed_domains_are_intersected() {
        let request: TavilyRequest = serde_json::from_value(serde_json::json!({
            "query": "rust",
            "include_domains": ["example.com"],
            "source_policy": {
                "mode": "prefer-official",
                "allowed_domains": ["docs.example.com", "other.example"],
                "excluded_domains": []
            }
        }))
        .unwrap();
        let policy = request.source_policy().unwrap();
        let catalogue = phrona::SourceCatalogue::default();

        assert!(
            policy
                .evaluate_url("https://docs.example.com/guide", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://example.com/guide", &catalogue)
                .unwrap()
        );
        assert!(
            !policy
                .evaluate_url("https://other.example/guide", &catalogue)
                .unwrap()
        );
    }

    #[test]
    fn tavily_result_metadata_is_additive_and_tiered() {
        let raw = phrona::ResultItem::Web(phrona::WebResult {
            title: "Docs".into(),
            url: "https://docs.example".into(),
            description: "text".into(),
            engines: vec!["bing".into()],
            position: 1,
            score: 1.0,
            source_policy_mode: phrona::SourceMode::RequireAllowed,
            requested_match: true,
            source_tier: phrona::SourceTier::Official,
            policy_reason: phrona::PolicyReason::Allowed,
        });
        let (result, _) = to_tavily_result(&raw, 0);
        assert_eq!(
            result.source_metadata.source_tier,
            phrona::SourceTier::Official
        );
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["source_metadata"]["requested_match"], true);
        assert_eq!(json["source_metadata"]["source_tier"], "official");
    }
}
