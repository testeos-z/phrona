//! Phrona MCP server.
//!
//! Exposes the phrona library to AI agents over stdio (JSON-RPC) and
//! Streamable HTTP. Tools are compartmentalized per capability:
//! per-category search, suggestions, page extraction and grounded search
//! for RAG.

#![warn(missing_docs)]

use rmcp::handler::server::wrapper::Parameters;
use rmcp::tool_router;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, tool};
use schemars::JsonSchema;
use tokio_util::sync::CancellationToken;

use phrona::models::{Category, TimeRange};
use phrona::{PhronaConfig, ResultItem, SearchClient, SearchOptions};

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct SearchParams {
    #[schemars(description = "Search query")]
    query: String,
    #[schemars(
        description = "Comma-separated engine names (default: all available for the category). See list_engines."
    )]
    #[serde(default)]
    engines: Option<String>,
    #[schemars(description = "Maximum number of results (default 20)")]
    #[serde(default)]
    max_results: Option<usize>,
    #[schemars(description = "Region code, e.g. us-en (default from client)")]
    #[serde(default)]
    region: Option<String>,
    #[schemars(description = "Language code, e.g. en")]
    #[serde(default)]
    language: Option<String>,
    #[schemars(description = "Time range: day, week, month or year")]
    #[serde(default)]
    time_range: Option<String>,
    #[schemars(description = "SafeSearch level: off, moderate or strict (default moderate)")]
    #[serde(default)]
    safesearch: Option<String>,
    #[schemars(description = "Engine-specific filter string, e.g. site:example.com")]
    #[serde(default)]
    filters: Option<String>,
    #[schemars(description = "Result page (default 1)")]
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct FetchParams {
    #[schemars(description = "URL to fetch and extract readable content from")]
    url: String,
    #[schemars(description = "Maximum characters of extracted text (default 8000)")]
    #[serde(default)]
    max_chars: Option<usize>,
    #[schemars(
        description = "Query used to bias the excerpt toward the relevant section (optional)"
    )]
    #[serde(default)]
    query: Option<String>,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct SuggestParams {
    #[schemars(description = "Partial query to complete")]
    query: String,
    #[schemars(
        description = "Source: duckduckgo, google, bing, brave, startpage, qwant or wikipedia (default: all)"
    )]
    #[serde(default)]
    source: Option<String>,
    #[schemars(description = "Region code, e.g. us-en (default us-en)")]
    #[serde(default)]
    region: Option<String>,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
struct EnginesParams {
    #[schemars(description = "Category: web, images, news, videos or books (default: all)")]
    #[serde(default)]
    category: Option<String>,
}

#[derive(Clone)]
struct PhronaMcp {
    client: std::sync::Arc<SearchClient>,
    max_results_limit: usize,
}

impl PhronaMcp {
    /// Build the server from a typed config: profile, timeout, proxies and
    /// the `max_results` clamp all come from it.
    fn with_config(cfg: &PhronaConfig) -> Self {
        Self {
            client: std::sync::Arc::new(cfg.search_client().expect("build search client")),
            max_results_limit: cfg.max_results_limit(),
        }
    }

    /// Map tool arguments to search options; invalid enums are rejected
    /// loudly instead of silently coerced.
    fn build_opts(
        p: &SearchParams,
        category: Category,
        max_results_limit: usize,
    ) -> Result<SearchOptions, String> {
        let mut opts = SearchOptions::new(p.query.clone());
        opts.category = category;
        if let Some(es) = &p.engines {
            opts.engines = es
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(m) = p.max_results {
            opts.max_results = m.clamp(1, max_results_limit);
        }
        opts.region = p.region.clone();
        opts.language = p.language.clone();
        if let Some(t) = &p.time_range {
            opts.time_range = Some(
                t.parse::<TimeRange>()
                    .map_err(|_| "invalid time_range, expected day|week|month|year".to_string())?,
            );
        }
        if let Some(s) = &p.safesearch {
            opts.safesearch = s
                .parse::<phrona::SafeSearch>()
                .map_err(|_| "invalid safesearch, expected off|moderate|strict".to_string())?;
        }
        opts.filters = p.filters.clone();
        if let Some(page) = p.page {
            opts.page = page.max(1);
        }
        Ok(opts)
    }

    async fn run_search(&self, p: &SearchParams, category: Category) -> String {
        let opts = match Self::build_opts(p, category, self.max_results_limit) {
            Ok(opts) => opts,
            Err(msg) => {
                return envelope(serde_json::json!({
                    "query": p.query,
                    "total": 0,
                    "results": [],
                    "error": msg,
                }));
            }
        };
        match self.client.search(opts).await {
            Ok(resp) => envelope(serde_json::to_value(&resp).unwrap_or_else(
                |_| serde_json::json!({"query": p.query, "total": 0, "results": []}),
            )),
            Err(e) => envelope(serde_json::json!({
                "query": p.query,
                "total": 0,
                "results": [],
                "error": e.to_string(),
            })),
        }
    }
}

/// Serialize a tool result as a single-line JSON string. MCP clients parse
/// tool outputs as JSON, so every handler must return valid JSON — never an
/// empty string or a debug-formatted value.
fn envelope(v: serde_json::Value) -> String {
    serde_json::to_string(&v)
        .unwrap_or_else(|_| r#"{"error":"internal serialization failure"}"#.to_string())
}

#[tool_router(server_handler)]
impl PhronaMcp {
    #[tool(
        description = "Search the web across multiple metasearch engines. Returns ranked results with title, url, description and the engines that found each one."
    )]
    async fn web_search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.run_search(&p, Category::Web).await
    }

    #[tool(
        description = "Search images across multiple engines. Returns direct image urls, thumbnails and dimensions."
    )]
    async fn image_search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.run_search(&p, Category::Images).await
    }

    #[tool(
        description = "Search news across multiple engines. Returns articles with published date and source."
    )]
    async fn news_search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.run_search(&p, Category::News).await
    }

    #[tool(
        description = "Search videos across multiple engines. Returns video url, duration, views and uploader."
    )]
    async fn video_search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.run_search(&p, Category::Videos).await
    }

    #[tool(description = "Search books and academic material.")]
    async fn book_search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        self.run_search(&p, Category::Books).await
    }

    #[tool(
        description = "Fetch a URL and extract its readable main content. Use for grounding answers on the sources returned by web_search."
    )]
    async fn fetch_page(&self, Parameters(p): Parameters<FetchParams>) -> String {
        match phrona::extract(
            self.client.http(),
            &p.url,
            p.max_chars.unwrap_or(8000),
            p.query.as_deref(),
        )
        .await
        {
            Ok(page) => envelope(serde_json::json!({
                "url": page.url,
                "title": page.title,
                "description": page.description,
                "text": page.text,
            })),
            Err(e) => envelope(serde_json::json!({"error": e.to_string()})),
        }
    }

    #[tool(description = "Get query suggestions for auto-completion from search engines.")]
    async fn suggest(&self, Parameters(p): Parameters<SuggestParams>) -> String {
        let region = p.region.as_deref().unwrap_or("us-en");
        match &p.source {
            Some(name) => {
                let Some(source) = phrona::SuggestSource::from_name(name) else {
                    return envelope(serde_json::json!({
                        "query": p.query,
                        "suggestions": [],
                        "error": format!("unknown source '{name}'"),
                    }));
                };
                match phrona::suggest(self.client.http(), source, &p.query, region).await {
                    Ok(list) => envelope(serde_json::json!({
                        "query": p.query,
                        "source": name,
                        "suggestions": list,
                    })),
                    Err(e) => envelope(serde_json::json!({
                        "query": p.query,
                        "suggestions": [],
                        "error": e.to_string(),
                    })),
                }
            }
            None => {
                let all = phrona::suggest_all(self.client.http(), &p.query, region).await;
                let map: serde_json::Map<String, _> = all
                    .into_iter()
                    .map(|(s, list)| (s.name().to_string(), serde_json::json!(list)))
                    .collect();
                envelope(serde_json::json!({"query": p.query, "suggestions": map}))
            }
        }
    }

    #[tool(
        description = "List the search engines available per category. Pass engine names to other tools to restrict them."
    )]
    fn list_engines(&self, Parameters(p): Parameters<EnginesParams>) -> String {
        let cats: Vec<Category> = match p.category.as_deref() {
            Some(c) => match c.parse::<Category>() {
                Ok(c) => vec![c],
                Err(_) => {
                    return envelope(serde_json::json!({
                        "error": format!(
                            "invalid category '{c}', expected one of: web, images, news, videos, books"
                        )
                    }));
                }
            },
            None => Category::ALL.to_vec(),
        };
        let mut out = serde_json::Map::new();
        for cat in cats {
            let names: Vec<String> = phrona::available_engines(cat)
                .iter()
                .map(|e| e.name.clone())
                .collect();
            out.insert(cat.as_str().to_string(), serde_json::json!(names));
        }
        envelope(serde_json::json!({"engines": out}))
    }

    #[tool(
        description = "Grounded search for RAG: returns a synthesized answer plus ranked sources with content. Prefer this over web_search + fetch_page for single-shot questions."
    )]
    async fn search_grounded(&self, Parameters(p): Parameters<SearchParams>) -> String {
        let opts = match Self::build_opts(&p, Category::Web, self.max_results_limit) {
            Ok(opts) => opts,
            Err(msg) => {
                return envelope(serde_json::json!({
                    "query": p.query,
                    "answer": "",
                    "sources": [],
                    "error": msg,
                }));
            }
        };
        match self.client.search(opts).await {
            Ok(resp) => {
                let sources: Vec<serde_json::Value> = resp
                    .results
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        let (title, url, content) = match r {
                            ResultItem::Web(w) => (&w.title, &w.url, &w.description),
                            ResultItem::News(n) => (&n.title, &n.url, &n.description),
                            ResultItem::Video(v) => (&v.title, &v.url, &v.description),
                            ResultItem::Image(im) => (&im.title, &im.url, &im.source),
                            ResultItem::Book(b) => (&b.title, &b.url, &b.info),
                        };
                        if content.is_empty() {
                            return None;
                        }
                        Some(serde_json::json!({
                            "title": title,
                            "url": url,
                            "content": content,
                            "score": phrona::rank::positional_score(i),
                        }))
                    })
                    .collect();
                let answer = resp.answer.clone().unwrap_or_else(|| {
                    format!(
                        "Found {} sources for \"{}\". Inspect the sources for the full picture.",
                        sources.len(),
                        resp.query
                    )
                });
                envelope(serde_json::json!({
                    "query": resp.query,
                    "answer": answer,
                    "sources": sources,
                }))
            }
            Err(e) => envelope(serde_json::json!({
                "query": p.query,
                "answer": "",
                "sources": [],
                "error": e.to_string(),
            })),
        }
    }
}

/// Serve the MCP server over stdio (JSON-RPC 2.0, newline-delimited).
/// Blocks until the client disconnects.
pub async fn run_stdio(cfg: &PhronaConfig) -> anyhow::Result<()> {
    let service = PhronaMcp::with_config(cfg);
    let server = service.serve(stdio()).await?;
    let _ = server.waiting().await?;
    Ok(())
}

/// Serve MCP over the standard Streamable HTTP transport.
///
/// The supplied listener determines the bind address. The MCP endpoint is
/// mounted at `/mcp`, uses the official `rmcp` Streamable HTTP transport and
/// keeps sessions in memory with [`LocalSessionManager`].
///
/// `shutdown` fires on SIGTERM/Ctrl+C and gracefully stops the HTTP server.
pub async fn serve_tcp(
    listener: tokio::net::TcpListener,
    cfg: PhronaConfig,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
    let cancellation = CancellationToken::new();
    let transport_cancellation = cancellation.child_token();
    let shutdown_cancellation = cancellation.clone();
    let service_cfg = cfg.clone();

    let service = StreamableHttpService::new(
        move || Ok(PhronaMcp::with_config(&service_cfg)),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(transport_cancellation),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    tracing::info!("phrona-mcp Streamable HTTP listening on http://{addr}/mcp");

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
            shutdown_cancellation.cancel();
        })
        .await?;
    Ok(())
}

/// Build a TCP listener from an addr string (for example `0.0.0.0:8081`).
/// The listener is used by the Streamable HTTP MCP server.
pub async fn tcp_listener(addr: &str) -> anyhow::Result<tokio::net::TcpListener> {
    Ok(tokio::net::TcpListener::bind(addr).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(query: &str) -> SearchParams {
        SearchParams {
            query: query.to_string(),
            engines: None,
            max_results: None,
            region: None,
            language: None,
            time_range: None,
            safesearch: None,
            filters: None,
            page: None,
        }
    }

    #[test]
    fn build_opts_defaults() {
        let opts = PhronaMcp::build_opts(&params("rust"), Category::Web, 100).unwrap();
        assert_eq!(opts.category, Category::Web);
        assert_eq!(opts.max_results, 20);
        assert_eq!(opts.safesearch, phrona::SafeSearch::Moderate);
        assert_eq!(opts.page, 1);
    }

    #[test]
    fn build_opts_clamps_and_maps() {
        let mut p = params("rust");
        p.max_results = Some(5000);
        p.page = Some(0);
        let opts = PhronaMcp::build_opts(&p, Category::News, 50).unwrap();
        assert_eq!(opts.max_results, 50);
        assert_eq!(opts.page, 1);
        assert_eq!(opts.category, Category::News);
    }

    #[test]
    fn build_opts_rejects_bad_enums() {
        let mut p = params("rust");
        p.time_range = Some("yesterday".into());
        assert!(PhronaMcp::build_opts(&p, Category::Web, 100).is_err());
        p.time_range = None;
        p.safesearch = Some("medium".into());
        assert!(PhronaMcp::build_opts(&p, Category::Web, 100).is_err());
        p.safesearch = Some("strict".into());
        assert!(PhronaMcp::build_opts(&p, Category::Web, 100).is_ok());
    }

    #[test]
    fn envelope_is_always_valid_json() {
        let v: serde_json::Value = serde_json::from_str(&envelope(serde_json::json!({
            "query": "rust",
            "total": 0,
            "results": [],
        })))
        .unwrap();
        assert_eq!(v["query"], "rust");
        assert_eq!(v["total"], 0);
        assert!(v["results"].is_array());
    }

    #[tokio::test]
    async fn tool_errors_return_json_envelopes_with_query_total_results() {
        let mut p = params("rust");
        p.time_range = Some("bogus".into());
        let out = PhronaMcp::with_config(&PhronaConfig::defaults())
            .run_search(&p, Category::Web)
            .await;
        let v: serde_json::Value = serde_json::from_str(&out).expect("tool output is JSON");
        assert_eq!(v["query"], "rust");
        assert_eq!(v["total"], 0);
        assert!(v["results"].is_array());
        assert!(
            v["error"]
                .as_str()
                .is_some_and(|e| e.contains("time_range"))
        );
    }
}
