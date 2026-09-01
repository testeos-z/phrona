//! Response models: categories, typed results, engine reports.

use serde::{Deserialize, Serialize};

use crate::source_policy::{PolicyReason, SourceAssessment, SourceMode, SourceTier};

/// The search category, which determines which engines run and how results
/// are typed. Parse from a string with `"images".parse::<Category>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// General web search.
    Web,
    /// Image search.
    Images,
    /// News search.
    News,
    /// Video search.
    Videos,
    /// Book search.
    Books,
}

impl Category {
    /// All categories, in a stable order.
    pub const ALL: [Category; 5] = [
        Category::Web,
        Category::Images,
        Category::News,
        Category::Videos,
        Category::Books,
    ];

    /// The lowercase string form used by the REST API and JSON serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Web => "web",
            Category::Images => "images",
            Category::News => "news",
            Category::Videos => "videos",
            Category::Books => "books",
        }
    }
}

impl std::str::FromStr for Category {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "web" | "text" | "general" => Ok(Category::Web),
            "images" | "image" | "img" => Ok(Category::Images),
            "news" => Ok(Category::News),
            "videos" | "video" | "vid" => Ok(Category::Videos),
            "books" | "book" => Ok(Category::Books),
            _ => Err(()),
        }
    }
}

/// Safe-search strictness level. Parse from a string with
/// `"moderate".parse::<SafeSearch>()` (also accepts `off`/`strict`, `0`/`1`/`2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SafeSearch {
    /// No filtering.
    Off,
    /// Moderate filtering (the default).
    Moderate,
    /// Strict filtering.
    Strict,
}

impl std::str::FromStr for SafeSearch {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Ok(SafeSearch::Off),
            "moderate" | "1" => Ok(SafeSearch::Moderate),
            "strict" | "on" | "2" => Ok(SafeSearch::Strict),
            _ => Err(()),
        }
    }
}

/// Result-time filter window: only results published/updated within the
/// window. Parse from a string with `"week".parse::<TimeRange>()`.
/// Engines that cannot honor it ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeRange {
    /// Past 24 hours.
    Day,
    /// Past 7 days.
    Week,
    /// Past 30 days.
    Month,
    /// Past 365 days.
    Year,
}

impl std::str::FromStr for TimeRange {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "day" | "d" | "24h" => Ok(TimeRange::Day),
            "week" | "w" | "7d" => Ok(TimeRange::Week),
            "month" | "m" | "30d" => Ok(TimeRange::Month),
            "year" | "y" | "365d" => Ok(TimeRange::Year),
            _ => Err(()),
        }
    }
}

/// A unified raw result produced by an engine; fields not applicable to the
/// category are left empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RawResult {
    /// Result title.
    pub title: String,
    /// Source page URL.
    pub url: String,
    /// Description or snippet text.
    pub description: String,
    /// Direct image URL (image categories).
    pub image_url: String,
    /// Thumbnail URL.
    pub thumbnail_url: String,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Publication date string as reported by the engine, if any.
    pub published: Option<String>,
    /// Source site or outlet name.
    pub source: String,
    /// Author name (books/news).
    pub author: String,
    /// Human-readable video duration.
    pub duration: String,
    /// Video view count.
    pub views: u64,
    /// Publisher name (books).
    pub publisher: String,
    /// Video uploader.
    pub uploader: String,
    /// Engine that produced this result.
    pub engine: String,
    /// Position within the engine's own results.
    pub position: u32,
    /// Local source assessment added after provider output; not serialized as
    /// part of the raw provider contract.
    #[serde(skip)]
    pub source_assessment: Option<SourceAssessment>,
}

/// A web search result. `engines` lists which providers returned the URL;
/// `position` is 1-based; `score` is the merged ranking score in (0, 1].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebResult {
    /// Result title.
    pub title: String,
    /// Page URL.
    pub url: String,
    /// Result description or snippet.
    pub description: String,
    /// Engines that returned this result, in first-seen order.
    pub engines: Vec<String>,
    /// 1-based position in the final merged results.
    pub position: usize,
    /// Merged ranking score in (0, 1].
    pub score: f64,
    /// Source-policy mode used for this result.
    #[serde(default)]
    pub source_policy_mode: SourceMode,
    /// Whether the source matched the caller's requested scope.
    #[serde(default)]
    pub requested_match: bool,
    /// Operator-assigned source authority.
    #[serde(default)]
    pub source_tier: SourceTier,
    /// Explainability reason for the local source decision.
    #[serde(default)]
    pub policy_reason: PolicyReason,
}

/// An image search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageResult {
    /// Image title.
    pub title: String,
    /// The page that hosts the image.
    pub url: String,
    /// Direct URL of the full-size image.
    pub image_url: String,
    /// URL of the small thumbnail shown in results.
    pub thumbnail_url: String,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Source site or host of the image.
    pub source: String,
    /// Engines that returned this result, in first-seen order.
    pub engines: Vec<String>,
    /// 1-based position in the final merged results.
    pub position: usize,
    /// Merged ranking score in (0, 1].
    pub score: f64,
    /// Source-policy mode used for this result.
    #[serde(default)]
    pub source_policy_mode: SourceMode,
    /// Whether the source matched the caller's requested scope.
    #[serde(default)]
    pub requested_match: bool,
    /// Operator-assigned source authority.
    #[serde(default)]
    pub source_tier: SourceTier,
    /// Explainability reason for the local source decision.
    #[serde(default)]
    pub policy_reason: PolicyReason,
}

/// A news search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsResult {
    /// News headline.
    pub title: String,
    /// Article URL.
    pub url: String,
    /// Article summary or snippet.
    pub description: String,
    /// Publication timestamp string as reported by the engine.
    pub published: Option<String>,
    /// Publisher or outlet name.
    pub source: String,
    /// URL of the article's lead image, if any.
    pub image_url: String,
    /// Engines that returned this result, in first-seen order.
    pub engines: Vec<String>,
    /// 1-based position in the final merged results.
    pub position: usize,
    /// Merged ranking score in (0, 1].
    pub score: f64,
    /// Source-policy mode used for this result.
    #[serde(default)]
    pub source_policy_mode: SourceMode,
    /// Whether the source matched the caller's requested scope.
    #[serde(default)]
    pub requested_match: bool,
    /// Operator-assigned source authority.
    #[serde(default)]
    pub source_tier: SourceTier,
    /// Explainability reason for the local source decision.
    #[serde(default)]
    pub policy_reason: PolicyReason,
}

/// A video search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoResult {
    /// Video title.
    pub title: String,
    /// Video page URL.
    pub url: String,
    /// Video description or snippet.
    pub description: String,
    /// Human-readable duration (e.g. `"12:34"`).
    pub duration: String,
    /// Publication timestamp string as reported by the engine, if any.
    pub published: Option<String>,
    /// Video uploader.
    pub uploader: String,
    /// Video view count.
    pub views: u64,
    /// Thumbnail URL.
    pub thumbnail_url: String,
    /// Engines that returned this result, in first-seen order.
    pub engines: Vec<String>,
    /// 1-based position in the final merged results.
    pub position: usize,
    /// Merged ranking score in (0, 1].
    pub score: f64,
    /// Source-policy mode used for this result.
    #[serde(default)]
    pub source_policy_mode: SourceMode,
    /// Whether the source matched the caller's requested scope.
    #[serde(default)]
    pub requested_match: bool,
    /// Operator-assigned source authority.
    #[serde(default)]
    pub source_tier: SourceTier,
    /// Explainability reason for the local source decision.
    #[serde(default)]
    pub policy_reason: PolicyReason,
}

/// A book search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookResult {
    /// Book title.
    pub title: String,
    /// Book author name.
    pub author: String,
    /// Publisher name.
    pub publisher: String,
    /// Short description or metadata blurb.
    pub info: String,
    /// Book page or listing URL.
    pub url: String,
    /// Thumbnail URL.
    pub thumbnail_url: String,
    /// Engines that returned this result, in first-seen order.
    pub engines: Vec<String>,
    /// 1-based position in the final merged results.
    pub position: usize,
    /// Merged ranking score in (0, 1].
    pub score: f64,
    /// Source-policy mode used for this result.
    #[serde(default)]
    pub source_policy_mode: SourceMode,
    /// Whether the source matched the caller's requested scope.
    #[serde(default)]
    pub requested_match: bool,
    /// Operator-assigned source authority.
    #[serde(default)]
    pub source_tier: SourceTier,
    /// Explainability reason for the local source decision.
    #[serde(default)]
    pub policy_reason: PolicyReason,
}

/// A search result tagged by category. JSON serializes as
/// `{"type": "web" | "image" | "news" | "video" | "book", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ResultItem {
    /// A web result.
    Web(WebResult),
    /// An image result.
    Image(ImageResult),
    /// A news result.
    News(NewsResult),
    /// A video result.
    Video(VideoResult),
    /// A book result.
    Book(BookResult),
}

#[cfg(test)]
mod source_metadata_tests {
    use super::*;

    #[test]
    fn result_metadata_is_additive_and_serializes_independently() {
        let result = WebResult {
            title: "Docs".into(),
            url: "https://docs.example.com".into(),
            description: "".into(),
            engines: vec!["bing".into()],
            position: 1,
            score: 0.9,
            source_policy_mode: SourceMode::RequireAllowed,
            requested_match: true,
            source_tier: SourceTier::Unknown,
            policy_reason: PolicyReason::Allowed,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["source_policy_mode"], "require-allowed");
        assert_eq!(json["requested_match"], true);
        assert_eq!(json["source_tier"], "unknown");
        assert_eq!(json["policy_reason"], "allowed");

        let legacy: WebResult = serde_json::from_value(serde_json::json!({
            "title": "Docs", "url": "https://docs.example.com", "description": "",
            "engines": ["bing"], "position": 1, "score": 0.9
        }))
        .unwrap();
        assert_eq!(legacy.source_policy_mode, SourceMode::Any);
        assert_eq!(legacy.source_tier, SourceTier::Unknown);
        assert_eq!(legacy.policy_reason, PolicyReason::Allowed);
    }
}

/// Per-engine outcome of a search. `status` is `ok`, `empty` or `error`;
/// on errors, `scope`/`kind` carry the structured failure labels (see
/// [`crate::error::ErrorScope`] / [`crate::error::ErrorKind`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineReport {
    /// Engine name.
    pub name: String,
    /// "ok" / "empty" / "error" / "enabled".
    pub status: String,
    /// Number of results the engine returned.
    pub results: usize,
    /// Human-readable error message, if the engine failed.
    pub error: Option<String>,
    /// Structured [`crate::error::ErrorScope`] debug label (set on errors).
    #[serde(default)]
    pub scope: Option<String>,
    /// Structured [`crate::error::ErrorKind`] debug label (set on errors).
    #[serde(default)]
    pub kind: Option<String>,
}

/// The full result of a [`crate::SearchClient::search`]: merged, deduplicated
/// and ranked results plus diagnostics per engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    /// The query that was run.
    pub query: String,
    /// The search category that was run.
    pub category: Category,
    /// The requested result page (1-based).
    pub page: u32,
    /// Number of results kept after merging, dedup and ranking.
    pub total: usize,
    /// Merged, deduplicated and ranked results.
    pub results: Vec<ResultItem>,
    /// Search-suggestion strings (web category, page 1 only).
    pub suggestions: Vec<String>,
    /// An answer text when an answer engine (e.g. grokipedia) contributed
    /// one; otherwise `None`.
    pub answer: Option<String>,
    /// Per-engine outcome reports from the search.
    pub engines: Vec<EngineReport>,
    /// Wall-clock time of the whole search in milliseconds.
    pub elapsed_ms: u64,
}

impl SearchResponse {
    /// Iterate over just the web results.
    pub fn web(&self) -> impl Iterator<Item = &WebResult> {
        self.results.iter().filter_map(|r| match r {
            ResultItem::Web(w) => Some(w),
            _ => None,
        })
    }
}
