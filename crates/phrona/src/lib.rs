//! # Phrona
//!
//! A high-performance metasearch engine library: it runs many search
//! providers concurrently with per-provider browser impersonation, then
//! merges, deduplicates and ranks the results by cross-engine agreement.
//!
//! ## Quick start
//!
//! ```no_run
//! use phrona::{SearchOptions, search};
//!
//! # async fn demo() -> Result<(), phrona::Error> {
//! let opts = SearchOptions::new("rust async runtime");
//! let resp = search(opts).await?;
//! for r in resp.web() {
//!     println!("{:>3}. {} - {}", r.position, r.title, r.url);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## What it offers
//!
//! - **Multi-engine search** over 26 providers across 5 categories (web,
//!   images, news, videos, books): [`search()`], [`SearchClient`].
//! - **Browser impersonation** (TLS + HTTP/2 fingerprints and matching
//!   User-Agents), per profile and proxy: [`Profile`], [`HttpClient`].
//! - **Merging, deduplication and ranking**: [`dedup`], [`rank`].
//! - **Structured errors** you can branch on: [`Error`], [`error`].
//! - **Page extraction for AI grounding** with SSRF guards: [`extract()`].
//! - **Autocomplete suggestions**: [`suggest`], [`suggest_all`].
//! - **Typed configuration** from YAML + environment: [`PhronaConfig`],
//!   [`config`].
//!
//! ## Resources
//!
//! - [Repository](https://github.com/alvaro-co/phrona) — the crate README is
//!   the full user guide.
//! - `docs/` — architecture, engines, CLI, REST API and library references.
//! - The `phrona-cli` crate ships a full command line, `phrona-api` the REST
//!   + MCP server, and `phrona-python` the Python bindings.

#![warn(missing_docs)]

pub mod bootstrap;
pub mod client;
pub mod config;
pub mod crypto;
pub mod dedup;
pub mod engine;
pub mod engines;
pub mod error;
pub mod extract;
pub mod models;
pub mod options;
pub mod parse;
pub mod rank;
pub mod search;
pub mod source_policy;

pub use client::{HttpClient, HttpClientBuilder, Profile, TargetPolicy};
pub use config::{ConfigError, PhronaConfig, SourcesConfig};
pub use error::{Error, Result};
pub use extract::{
    ExtractedPage, extract, extract_from_html, extract_many, extract_many_with_policy,
    extract_with_policy, is_safe_ip,
};
pub use models::*;
pub use options::SearchOptions;
pub use search::{
    EngineObserver, NoopEngineObserver, SearchClient, available_engines, search, search_sync,
};
pub use source_policy::{
    DomainSet, NormalizedDomain, PolicyReason, SourceAssessment, SourceCatalogue, SourceMode,
    SourcePolicy, SourcePolicyError, SourceTier,
};

/// The crate version, read from `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The crate version as a string.
pub fn version() -> &'static str {
    VERSION
}

pub use engines::suggest::{SuggestSource, suggest, suggest_all};

pub use engine::{Engine, EngineContext, EngineShared};

#[cfg(test)]
mod tests {
    use crate::client::{HttpClient, Profile};

    /// Live-network smoke test; excluded from `cargo test` runs (needs the
    /// internet and an unblocked IP).
    #[tokio::test]
    #[ignore]
    async fn smoke_request() {
        let client = HttpClient::builder()
            .profile(Profile::Chrome)
            .build()
            .unwrap();
        let resp = client.get("https://html.duckduckgo.com/").await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
