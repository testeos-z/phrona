use phrona::{
    Category, EngineReport, PolicyReason, ResultItem, SearchResponse, SourceMode, SourceTier,
};

/// Print one result line: ` 3. [web   ] Title` plus the URL and optional
/// metadata indented below.
pub fn result_line(kind: &str, position: usize, title: &str, url: &str, meta: &str) {
    println!(
        "{position:>3}. [{kind:6}] {title}\n     {url}{}",
        if meta.is_empty() {
            String::new()
        } else {
            format!("\n     {meta}")
        }
    );
}

/// Print a full [`SearchResponse`] in human-readable form: summary line,
/// answer, suggestions, per-engine report, then each result.
pub fn print_response(r: &SearchResponse) {
    println!(
        "query: {}\ncategory: {} | page: {} | results: {} | elapsed: {} ms",
        r.query,
        r.category.as_str(),
        r.page,
        r.total,
        r.elapsed_ms
    );
    if let Some(answer) = r.answer.as_deref().filter(|a| !a.trim().is_empty()) {
        println!("\nanswer: {answer}");
    }
    if !r.suggestions.is_empty() {
        println!("\nsuggestions: {}", r.suggestions.join(" | "));
    }
    for (i, item) in r.results.iter().enumerate() {
        print_item(i + 1, item);
    }
    if !r.engines.is_empty() {
        println!("\nengines:");
        for e in &r.engines {
            println!(
                "  {:16} {:6} {:>4} results{}",
                e.name,
                e.status,
                e.results,
                e.error
                    .as_deref()
                    .map(|err| format!("  ({err})"))
                    .unwrap_or_default()
            );
        }
    }
}

/// Print one typed result item at `position`, formatting category-specific
/// metadata.
pub fn print_item(position: usize, item: &ResultItem) {
    match item {
        ResultItem::Web(w) => result_line(
            "web",
            position,
            &w.title,
            &w.url,
            &with_policy(
                &w.description,
                w.source_policy_mode,
                w.requested_match,
                w.source_tier,
                w.policy_reason,
            ),
        ),
        ResultItem::Image(i) => result_line(
            "image",
            position,
            &i.title,
            &i.url,
            &with_policy(
                &format!("{} ({}x{})", i.image_url, i.width, i.height),
                i.source_policy_mode,
                i.requested_match,
                i.source_tier,
                i.policy_reason,
            ),
        ),
        ResultItem::News(n) => result_line(
            "news",
            position,
            &n.title,
            &n.url,
            &with_policy(
                &format!(
                    "{}{}",
                    n.source,
                    n.published
                        .as_deref()
                        .map(|p| format!(" - {p}"))
                        .unwrap_or_default()
                ),
                n.source_policy_mode,
                n.requested_match,
                n.source_tier,
                n.policy_reason,
            ),
        ),
        ResultItem::Video(v) => result_line(
            "video",
            position,
            &v.title,
            &v.url,
            &with_policy(
                &format!(
                    "{} | {} views | {}{}",
                    v.uploader,
                    v.views,
                    v.duration,
                    v.published
                        .as_deref()
                        .map(|p| format!(" | {p}"))
                        .unwrap_or_default()
                ),
                v.source_policy_mode,
                v.requested_match,
                v.source_tier,
                v.policy_reason,
            ),
        ),
        ResultItem::Book(b) => result_line(
            "book",
            position,
            &b.title,
            &b.url,
            &with_policy(
                &format!("{} | {}{}", b.author, b.publisher, b.info),
                b.source_policy_mode,
                b.requested_match,
                b.source_tier,
                b.policy_reason,
            ),
        ),
    }
}

fn with_policy(
    detail: &str,
    mode: SourceMode,
    requested_match: bool,
    tier: SourceTier,
    reason: PolicyReason,
) -> String {
    format!(
        "{detail} | source_policy={mode} requested_match={requested_match} source_tier={tier:?} policy_reason={reason:?}"
    )
}

/// Print the list of registered engines for a category (`phrona engines`).
pub fn print_engines_table(category: Category) {
    let names: Vec<String> = phrona::available_engines(category)
        .iter()
        .map(|e| e.name.clone())
        .collect();
    println!(
        "{} ({}): {}",
        category.as_str(),
        names.len(),
        names.join(", ")
    );
}

/// Print the availability matrix from a `phrona test` run: one line per
/// category plus a deduplicated per-engine OK/error table.
pub fn print_test_report(reports: Vec<(Category, SearchResponse)>) {
    let mut matrix: Vec<EngineReport> = Vec::new();
    let mut printed = std::collections::BTreeSet::new();
    for (cat, resp) in &reports {
        println!(
            "category: {:8} total: {:>3} elapsed: {:>4} ms  answer: {}",
            cat.as_str(),
            resp.total,
            resp.elapsed_ms,
            resp.answer.as_deref().is_some_and(|a| !a.is_empty())
        );
        for e in &resp.engines {
            matrix.push(e.clone());
        }
    }
    println!("\navailability matrix:");
    for e in &matrix {
        if printed.insert(e.name.clone()) {
            println!(
                "  {:16} {:6}",
                e.name,
                if e.status == "ok" {
                    "OK".to_string()
                } else {
                    e.status.clone()
                }
            );
        }
    }
}

/// Print a grounded search result: the answer plus up to `max_results`
/// sources with their content.
pub fn print_grounded(query: &str, resp: &SearchResponse, max_results: usize) {
    let answer = resp
        .answer
        .clone()
        .unwrap_or_else(|| format!("Found {} sources for \"{query}\".", resp.total));
    println!("query: {query}\nanswer: {answer}");
    let mut shown = 0;
    for (i, item) in resp.results.iter().enumerate() {
        if shown >= max_results {
            break;
        }
        let (title, url, content) = match item {
            ResultItem::Web(w) => (&w.title, &w.url, &w.description.as_str()),
            ResultItem::News(n) => (&n.title, &n.url, &n.description.as_str()),
            ResultItem::Video(v) => (&v.title, &v.url, &v.description.as_str()),
            ResultItem::Image(im) => (&im.title, &im.url, &im.source.as_str()),
            ResultItem::Book(b) => (&b.title, &b.url, &b.info.as_str()),
        };
        if content.trim().is_empty() {
            continue;
        }
        println!("\n{}. {title}\n   {url}\n   {content}", i + 1);
        shown += 1;
    }
}
