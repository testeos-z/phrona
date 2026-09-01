//! Cross-engine ranking and scoring.

use crate::dedup::GroupedResult;
use crate::source_policy::{SourceMode, SourceTier};

/// Split a query into the alphanumeric terms used for matching, using the
/// exact same tokenizer as document titles and bodies. Matching the
/// document side keeps scoring symmetric: short or symbol-heavy queries
/// ("Go", "C#", "AI", "R") still yield BM25 terms instead of an empty set.
pub fn query_terms(query: &str) -> Vec<String> {
    tokenize(query)
}

/// Raw (un-normalized) relevance score, shared by [`rank`] (ordering) and
/// [`calculate_score`] (display).
///
/// Components:
/// 1. engine consensus: +1.5 per agreeing engine,
/// 2. position: `(10 / position).min(3)`,
/// 3. domain authority: +2.5 for Wikipedia / Grokipedia,
/// 4. BM25-style term-frequency saturation on title (2x weight) and body.
fn raw_score(g: &GroupedResult, terms: &[String]) -> f64 {
    let mut score = 0.0;
    // 1. cross-engine agreement: +1.5 per agreeing engine
    score += g.count as f64 * 1.5;
    // 2. engine position: earlier is better
    let pos = g.result.position.max(1) as f64;
    score += (10.0 / pos).min(3.0);
    // 3. wikipedia preference (answer-like, high precision)
    let host = crate::parse::host_of(&g.result.url).unwrap_or_default();
    if host.contains("wikipedia.org") || host.contains("grokipedia") {
        score += 2.5;
    }
    // 4. BM25-style term match on title and description
    score += bm25_match(&g.result.title, &g.result.description, terms);
    score
}

/// Relevance scoring: engines rank results per-position; we blend
/// cross-engine frequency, per-engine position, and content matching.
/// Returns `(raw_score, group)` pairs sorted best-first. The raw score can
/// be turned into the display score with [`normalize_score`] without
/// recomputing it.
pub fn rank(groups: Vec<GroupedResult>, query: &str) -> Vec<(f64, GroupedResult)> {
    rank_with_policy(groups, query, SourceMode::Any)
}

/// Rank grouped results using the selected source-policy ordering. Authority
/// is a bounded ordering key, never a numeric relevance bonus: `any` keeps
/// relevance first and uses authority only for equal scores, while
/// `prefer-official` puts official and secondary sources before relevance.
pub fn rank_with_policy(
    groups: Vec<GroupedResult>,
    query: &str,
    mode: SourceMode,
) -> Vec<(f64, GroupedResult)> {
    let terms = query_terms(query);
    let mut scored: Vec<(f64, GroupedResult)> = groups
        .into_iter()
        .map(|g| (raw_score(&g, &terms), g))
        .collect();

    scored.sort_by(|a, b| {
        let authority = || {
            authority_rank_key(a.1.result.source_assessment.unwrap_or_default().source_tier).cmp(
                &authority_rank_key(b.1.result.source_assessment.unwrap_or_default().source_tier),
            )
        };
        let relevance = || b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal);
        match mode {
            SourceMode::PreferOfficial => authority().reverse().then_with(relevance),
            SourceMode::Any | SourceMode::RequireAllowed | SourceMode::OfficialOnly => {
                relevance().then_with(|| authority().reverse())
            }
        }
    });
    scored
}

/// Bounded authority key used only for deterministic policy-aware ordering.
pub const fn authority_rank_key(tier: SourceTier) -> u8 {
    match tier {
        SourceTier::Official => 2,
        SourceTier::Secondary => 1,
        SourceTier::Unknown => 0,
    }
}

/// Normalize an already-computed raw score into the display score bounded
/// strictly between 0.001 and 1.000, rounded to 3 decimals. Monotonic in
/// the raw score. Equivalent to [`calculate_score`] without the recompute.
pub fn normalize_score(raw: f64) -> f64 {
    let norm = raw / (1.0 + raw);
    let rounded = (norm * 1000.0).round() / 1000.0;
    rounded.clamp(0.001, 0.999)
}

/// Positional relevance score for AI-facing surfaces (grounding sources,
/// Tavily-compatible responses): 1.0 at position 0, decaying by 0.05 per
/// result, floored at 0.05. Shared so every surface reports identical
/// scores for the same ordering.
pub fn positional_score(index: usize) -> f64 {
    (1.0 - index as f64 * 0.05).max(0.05)
}

/// Unified cross-category score for a grouped result, normalized to a float
/// bounded strictly between 0.001 and 1.000 (rounded to 3 decimal places).
/// Higher is better; the value is monotonic in the raw score.
pub fn calculate_score(grouped: &GroupedResult, query_terms: &[String]) -> f64 {
    normalize_score(raw_score(grouped, query_terms))
}

/// BM25-style term match: saturating term-frequency weighting
/// (`tf / (tf + k1)` with k1 = 1.2), no length normalization (no corpus),
/// with title hits weighted 2x over description hits.
fn bm25_match(title: &str, body: &str, terms: &[String]) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    const K1: f64 = 1.2;
    let title_words = tokenize(title);
    let body_words = tokenize(body);
    let mut score = 0.0;
    for t in terms {
        let tf_t = title_words.iter().filter(|w| *w == t).count() as f64;
        if tf_t > 0.0 {
            score += 2.0 * (K1 * tf_t) / (tf_t + K1);
        }
        let tf_b = body_words.iter().filter(|w| *w == t).count() as f64;
        if tf_b > 0.0 {
            score += (K1 * tf_b) / (tf_b + K1);
        }
    }
    score
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dedup::GroupedResult;
    use crate::models::RawResult;
    use crate::source_policy::SourceMode;

    fn group(title: &str, url: &str, desc: &str, engine: &str, position: u32) -> GroupedResult {
        GroupedResult {
            result: RawResult {
                title: title.into(),
                url: url.into(),
                description: desc.into(),
                engine: engine.into(),
                position,
                ..Default::default()
            },
            engines: vec![engine.into()],
            count: 1,
        }
    }

    fn rerank(g: &GroupedResult, n: usize, engines: Vec<&str>) -> GroupedResult {
        let mut g = g.clone();
        g.count = n;
        g.engines = engines.iter().map(|s| s.to_string()).collect();
        g
    }

    #[test]
    fn query_terms_matches_document_tokenizer() {
        // Short and symbol-heavy queries must produce BM25 terms, just
        // like the tokenizer on the document side does.
        assert_eq!(query_terms("Go"), vec!["go".to_string()]);
        assert_eq!(query_terms("C#"), vec!["c".to_string()]);
        assert_eq!(query_terms("AI"), vec!["ai".to_string()]);
        assert_eq!(query_terms("R"), vec!["r".to_string()]);
        assert_eq!(
            query_terms("rust book"),
            vec!["rust".to_string(), "book".to_string()]
        );
        assert_eq!(
            query_terms("rust-book 2.0"),
            vec![
                "rust".to_string(),
                "book".to_string(),
                "2".to_string(),
                "0".to_string()
            ]
        );
    }

    #[test]
    fn agreement_dominates() {
        let singles = group(
            "rust book",
            "https://a.com",
            "rust programming book",
            "bing",
            1,
        );
        let agreed = rerank(
            &group(
                "rust book",
                "https://b.com",
                "rust programming book",
                "brave",
                3,
            ),
            3,
            vec!["bing", "brave", "duckduckgo"],
        );
        let ranked = rank(vec![singles, agreed], "rust book");
        assert_eq!(ranked[0].1.result.url, "https://b.com");
        assert!(ranked[0].0 > ranked[1].0);
        // agreement adds 1.5 per extra engine
        assert!((ranked[0].0 - ranked[1].0 - 3.0).abs() < 1e-9);
    }

    #[test]
    fn position_and_text_matter() {
        let pos1 = group("rust", "https://a.com", "", "bing", 1);
        let pos5 = group("rust", "https://b.com", "", "bing", 5);
        let ranked = rank(vec![pos1, pos5], "rust");
        assert_eq!(ranked[0].1.result.url, "https://a.com");
    }

    #[test]
    fn wikipedia_gets_bonus() {
        let wiki = group(
            "rust",
            "https://en.wikipedia.org/wiki/Rust",
            "",
            "wikipedia",
            10,
        );
        let other = group("rust", "https://c.com", "", "bing", 1);
        let ranked = rank(vec![other, wiki], "rust");
        assert_eq!(ranked[0].1.result.url, "https://en.wikipedia.org/wiki/Rust");
    }

    #[test]
    fn query_terms_boost_title_matches() {
        let title_hit = group("learn rust fast", "https://a.com", "", "bing", 1);
        let no_hit = group("something else", "https://b.com", "", "bing", 1);
        let ranked = rank(vec![no_hit, title_hit], "learn rust");
        assert_eq!(ranked[0].1.result.url, "https://a.com");
    }

    #[test]
    fn ranking_is_stable_and_total() {
        let a = group("x", "https://a.com", "same", "bing", 1);
        let b = group("x", "https://b.com", "same", "brave", 2);
        let mut ranked = rank(vec![a.clone(), b.clone()], "x");
        assert_eq!(ranked.len(), 2);
        let total: f64 = ranked.iter().map(|(s, _)| s).sum();
        ranked.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
        assert!((total - ranked.iter().map(|(s, _)| s).sum::<f64>()).abs() < 1e-9);
    }

    #[test]
    fn calculate_score_normalizes_to_unit_interval() {
        let g = group("rust book", "https://a.com", "rust programming", "bing", 1);
        for count in [1usize, 2, 3, 5] {
            let s = calculate_score(&rerank(&g, count, vec!["bing"]), &query_terms("rust book"));
            assert!((0.001..=0.999).contains(&s), "count={count}: {s}");
            let rounded = (s * 1000.0).fract().abs();
            assert!(rounded < 1e-9, "count={count}: not 3 decimals: {s}");
        }
        // every result scores; nothing can reach the exclusive 1.000 bound
        let top = calculate_score(
            &rerank(
                &group(
                    "rust book",
                    "https://en.wikipedia.org/wiki/Rust",
                    "rust programming book",
                    "wikipedia",
                    1,
                ),
                5,
                vec!["bing", "brave", "ddg", "google", "mojeek"],
            ),
            &query_terms("rust book rust book rust book"),
        );
        assert!((0.001..1.000).contains(&top), "top={top}");
    }

    #[test]
    fn calculate_score_is_monotonic_in_components() {
        let terms = query_terms("rust book");
        let base = group("rust book", "https://a.com", "rust programming", "bing", 1);
        // consensus: more agreeing engines -> higher
        let agreed = rerank(&base, 3, vec!["bing", "brave", "ddg"]);
        assert!(calculate_score(&agreed, &terms) > calculate_score(&base, &terms));
        // position: earlier -> higher
        let late = group("rust book", "https://a.com", "rust programming", "bing", 8);
        assert!(calculate_score(&base, &terms) > calculate_score(&late, &terms));
        // domain authority: wikipedia gets the boost
        let wiki = group(
            "rust book",
            "https://en.wikipedia.org/wiki/Rust",
            "rust",
            "wikipedia",
            1,
        );
        assert!(calculate_score(&wiki, &terms) > calculate_score(&base, &terms));
        // BM25: title match beats none
        let none = group("totally unrelated", "https://a.com", "x", "bing", 1);
        assert!(calculate_score(&base, &terms) > calculate_score(&none, &terms));
    }

    #[test]
    fn positional_score_decays_with_floor() {
        assert_eq!(positional_score(0), 1.0);
        assert_eq!(positional_score(4), 0.8);
        assert_eq!(positional_score(19), 0.05);
        assert_eq!(positional_score(100), 0.05);
    }

    #[test]
    fn authority_rank_key_is_bounded_and_ordered() {
        assert!(
            authority_rank_key(SourceTier::Official) > authority_rank_key(SourceTier::Secondary)
        );
        assert!(
            authority_rank_key(SourceTier::Secondary) > authority_rank_key(SourceTier::Unknown)
        );
        assert_eq!(authority_rank_key(SourceTier::Official), 2);
        assert_eq!(authority_rank_key(SourceTier::Unknown), 0);
    }

    #[test]
    fn policy_ranking_prefers_authority_before_relevance_when_requested() {
        let mut official = group("unrelated", "https://official.example/a", "", "bing", 10);
        official.result.source_assessment = Some(crate::source_policy::SourceAssessment {
            requested_match: false,
            source_tier: SourceTier::Official,
            reason: crate::source_policy::PolicyReason::Allowed,
        });
        let unknown = group("rust", "https://unknown.example/b", "rust", "bing", 1);

        let ranked = rank_with_policy(vec![unknown, official], "rust", SourceMode::PreferOfficial);
        assert_eq!(ranked[0].1.result.url, "https://official.example/a");
    }

    #[test]
    fn policy_ranking_keeps_relevance_and_uses_authority_only_as_tiebreaker() {
        let mut official = group("rust", "https://official.example/a", "", "bing", 1);
        official.result.source_assessment = Some(crate::source_policy::SourceAssessment {
            requested_match: false,
            source_tier: SourceTier::Official,
            reason: crate::source_policy::PolicyReason::Allowed,
        });
        let mut unknown = group("rust", "https://unknown.example/b", "", "bing", 1);
        unknown.result.source_assessment = Some(crate::source_policy::SourceAssessment {
            requested_match: false,
            source_tier: SourceTier::Unknown,
            reason: crate::source_policy::PolicyReason::Allowed,
        });

        let ranked = rank_with_policy(vec![unknown, official.clone()], "rust", SourceMode::Any);
        assert_eq!(ranked[0].1.result.url, "https://official.example/a");

        let mut more_relevant_unknown = group(
            "rust rust rust",
            "https://unknown.example/c",
            "rust rust",
            "bing",
            1,
        );
        more_relevant_unknown.result.source_assessment =
            Some(crate::source_policy::SourceAssessment {
                requested_match: false,
                source_tier: SourceTier::Unknown,
                reason: crate::source_policy::PolicyReason::Allowed,
            });
        let ranked = rank_with_policy(
            vec![more_relevant_unknown, official],
            "rust",
            SourceMode::Any,
        );
        assert_eq!(ranked[0].1.result.url, "https://unknown.example/c");
    }

    #[test]
    fn normalize_score_matches_calculate_score() {
        let g = group("rust book", "https://a.com", "rust programming", "bing", 1);
        let raw = raw_score(&g, &query_terms("rust book"));
        assert_eq!(
            normalize_score(raw),
            calculate_score(&g, &query_terms("rust book"))
        );
    }
}
