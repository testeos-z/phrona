//! Search parameters shared by every engine.

use std::time::Duration;

use crate::models::{Category, SafeSearch, TimeRange};
use crate::source_policy::SourcePolicy;

/// Search parameters shared by every engine.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// The search query text.
    pub query: String,
    /// Search category; selects which engines run.
    pub category: Category,
    /// Restrict to these engines. Empty means "all enabled engines for the category".
    pub engines: Vec<String>,
    /// 1-based result page to fetch.
    pub page: u32,
    /// Maximum number of results to return.
    pub max_results: usize,
    /// Safe-search strictness level.
    pub safesearch: SafeSearch,
    /// Region hint (e.g. `"us-en"`), passed to engines.
    pub region: Option<String>,
    /// Language hint (e.g. `"en"`), passed to engines.
    pub language: Option<String>,
    /// Restrict results to a time window (engines that support it).
    pub time_range: Option<TimeRange>,
    /// Engine-specific free-form filter string (e.g. DDG image filters
    /// "size:Large,color:red"). Passed through to engines that support it.
    pub filters: Option<String>,
    /// Run every resolved engine to completion instead of cancelling
    /// in-flight engines once `max_results` merged results exist. Used by
    /// availability probing (`phrona test`, `/v1/test`) so every engine's
    /// report is observed; normal searches keep the early exit for latency.
    pub probe_all: bool,
    /// Overall search deadline (the streaming orchestrator cancels
    /// in-flight engines past this point).
    pub timeout: Duration,
    /// Local source eligibility and caller-requested scope. Defaults to the
    /// compatibility-preserving `any` mode.
    pub source_policy: SourcePolicy,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            category: Category::Web,
            engines: Vec::new(),
            page: 1,
            max_results: 20,
            safesearch: SafeSearch::Moderate,
            region: None,
            language: None,
            time_range: None,
            filters: None,
            probe_all: false,
            timeout: Duration::from_secs(10),
            source_policy: SourcePolicy::default(),
        }
    }
}

impl SearchOptions {
    /// Create options with only a query set; everything else takes its
    /// default (web category, page 1, `max_results` 20, moderate safe
    /// search, 10s timeout). Mutate the public fields to customize.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            ..Default::default()
        }
    }

    /// "us-en" style region; returns the language part and the country
    /// part. `"us-en"` yields `("en", "us")`, `"de"` yields `("de", "us")`,
    /// nothing yields `("en", "us")`.
    pub fn lang_country(&self) -> (String, String) {
        let valid = |s: &str| !s.is_empty() && s.len() <= 10;
        let (region_lang, region_country) = match self.region.as_deref() {
            Some(r) if r.contains('-') => {
                let (country, lang) = r.split_once('-').unwrap();
                (lang, country)
            }
            Some(r) => (r, "us"),
            None => ("en", "us"),
        };
        let lang = self.language.as_deref().unwrap_or(region_lang);
        let lang = if valid(lang) { lang } else { "en" };
        let country = if valid(region_country) {
            region_country
        } else {
            "us"
        };
        (lang.to_string(), country.to_string())
    }

    /// The region to send to engines as a `lang-country` parameter,
    /// defaulting to `en-us`.
    pub fn region_param(&self) -> String {
        self.region.clone().unwrap_or_else(|| {
            let (lang, country) = self.lang_country();
            format!("{lang}-{country}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_policy::SourceMode;

    #[test]
    fn enums_parse_from_str() {
        for (s, expect) in [
            ("web", Category::Web),
            ("images", Category::Images),
            ("news", Category::News),
            ("videos", Category::Videos),
            ("books", Category::Books),
        ] {
            assert_eq!(s.parse::<Category>().unwrap(), expect);
        }
        assert!("nope".parse::<Category>().is_err());

        for (s, expect) in [
            ("off", SafeSearch::Off),
            ("moderate", SafeSearch::Moderate),
            ("strict", SafeSearch::Strict),
        ] {
            assert_eq!(s.parse::<SafeSearch>().unwrap(), expect);
        }
        assert!("x".parse::<SafeSearch>().is_err());

        for (s, expect) in [
            ("day", TimeRange::Day),
            ("week", TimeRange::Week),
            ("month", TimeRange::Month),
            ("year", TimeRange::Year),
        ] {
            assert_eq!(s.parse::<TimeRange>().unwrap(), expect);
        }
        assert!("yesterday".parse::<TimeRange>().is_err());
    }

    #[test]
    fn defaults_are_sane() {
        let o = SearchOptions::default();
        assert_eq!(o.category, Category::Web);
        assert_eq!(o.page, 1);
        assert_eq!(o.max_results, 20);
        assert_eq!(o.safesearch, SafeSearch::Moderate);
        assert_eq!(o.engines, Vec::<String>::new());
        assert_eq!(o.source_policy.mode(), SourceMode::Any);
    }

    #[test]
    fn new_sets_query_only() {
        let o = SearchOptions::new("hello world");
        assert_eq!(o.query, "hello world");
        assert_eq!(o.category, Category::Web);
    }

    #[test]
    fn lang_country_derivation() {
        let o = SearchOptions {
            region: Some("de-de".into()),
            language: None,
            ..Default::default()
        };
        assert_eq!(o.lang_country(), ("de".into(), "de".into()));
        assert_eq!(o.region_param(), "de-de");

        let o = SearchOptions::default();
        assert_eq!(o.lang_country(), ("en".into(), "us".into()));
        assert_eq!(o.region_param(), "en-us");

        let o = SearchOptions {
            language: Some("fr".into()),
            region: Some("fr-fr".into()),
            ..Default::default()
        };
        assert_eq!(o.lang_country(), ("fr".into(), "fr".into()));

        // asymmetric "country-language" convention: "us-en" is en/us
        let o = SearchOptions {
            region: Some("us-en".into()),
            ..Default::default()
        };
        assert_eq!(o.lang_country(), ("en".into(), "us".into()));
        assert_eq!(o.region_param(), "us-en");

        // bare region without a hyphen: language only
        let o = SearchOptions {
            region: Some("de".into()),
            ..Default::default()
        };
        assert_eq!(o.lang_country(), ("de".into(), "us".into()));
    }
}
