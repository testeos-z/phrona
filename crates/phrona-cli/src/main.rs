mod args;
mod output;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use serde_json::json;

use phrona::SuggestSource;
use phrona::config::PhronaConfig;

use args::{BootstrapArgs, Cli, Command, TestArgs};

/// Load the typed configuration; a broken file degrades to defaults with a
/// warning so the CLI stays usable.
fn load_config() -> PhronaConfig {
    match PhronaConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("config warning: {e}");
            PhronaConfig::defaults()
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = load_config();
    let source_catalogue = cfg.source_catalogue()?;
    let profile = cli.profile.unwrap_or_else(|| cfg.profile());
    let timeout = Duration::from_secs(cli.timeout.unwrap_or(cfg.search.timeout_secs));
    let proxies = if cli.proxy.is_empty() {
        cfg.engines.proxies.clone()
    } else {
        cli.proxy.clone()
    };
    let mut client = phrona::SearchClient::with_options(
        profile,
        Some(timeout),
        (!proxies.is_empty()).then_some(proxies),
        phrona::TargetPolicy::from_security(&cfg.security),
    )?
    .with_source_catalogue(source_catalogue)
    .with_auto_bootstrap(cli.auto_bootstrap || cfg.engines.auto_bootstrap);
    // config-provided bootstrap cookies first, then --cookie overrides
    for (engine, cookies) in cfg.bootstrap_cookies() {
        client = client.with_bootstrap_cookie(engine.clone(), cookies.clone());
    }
    for spec in &cli.cookie {
        let Some((engine, cookies)) = spec.split_once('=') else {
            anyhow::bail!("invalid --cookie '{spec}', expected engine=Cookie-header");
        };
        let engine = engine.trim();
        if phrona::engine::engine_by_name(engine).is_none() {
            anyhow::bail!("unknown engine '{engine}' in --cookie; see 'phrona engines'");
        }
        client = client.with_bootstrap_cookie(engine, cookies.trim().to_string());
    }

    match &cli.command {
        Command::Search(args) => {
            let mut opts = cli.base_options(timeout, &args.query);
            opts.category = args.category;
            opts.engines = split_engines(args.engines.as_deref());
            opts.max_results = args.max_results.clamp(1, cfg.max_results_limit());
            opts.safesearch = args.safesearch;
            opts.region = args.region.clone();
            opts.language = args.language.clone();
            opts.time_range = args.time_range;
            opts.filters = args.filters.clone();
            opts.page = args.page.max(1);
            opts.source_policy = source_policy(
                &args.source_policy_mode,
                &args.allowed_domains,
                &args.excluded_domains,
            )?;
            let resp = client.search(opts).await?;
            if cli.json {
                print_json(&resp);
            } else {
                output::print_response(&resp);
            }
        }
        Command::Suggest(args) => {
            let region = &args.region;
            let sources = split_sources(args.source.as_deref())?;
            if sources.is_empty() {
                let all = phrona::suggest_all(client.http(), &args.query, region).await;
                if cli.json {
                    let map: serde_json::Map<String, _> = all
                        .into_iter()
                        .map(|(s, list)| (s.name().to_string(), json!(list)))
                        .collect();
                    println!("{}", json!(map));
                } else {
                    for (s, list) in all {
                        println!("{}: {}", s.name(), list.join(" | "));
                    }
                }
            } else {
                for s in sources {
                    let list = phrona::suggest(client.http(), s, &args.query, region).await?;
                    if cli.json {
                        println!("{}", json!({"source": s.name(), "suggestions": list}));
                    } else {
                        println!("{}: {}", s.name(), list.join(" | "));
                    }
                }
            }
        }
        Command::Extract(args) => {
            let policy = source_policy(
                &args.source_policy_mode,
                &args.allowed_domains,
                &args.excluded_domains,
            )?;
            let results = phrona::extract_many_with_policy(
                client.http(),
                &policy,
                client.source_catalogue(),
                &args.urls,
                args.max_chars,
                args.query.as_deref(),
            )
            .await;
            let mut failed = false;
            for (url, result) in args.urls.iter().zip(results) {
                match result {
                    Ok(page) => {
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&page)?);
                        } else {
                            if args.urls.len() > 1 {
                                println!("== {url}");
                            }
                            println!("title: {}\n", page.title);
                            if !page.description.is_empty() {
                                println!("description: {}\n", page.description);
                            }
                            println!("{}", page.text);
                            if !page.images.is_empty() {
                                println!("\nimages: {}", page.images.join(" | "));
                            }
                            println!();
                        }
                    }
                    Err(e) => {
                        failed = true;
                        eprintln!("{url}: {e}");
                    }
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        Command::Ground(args) => {
            let mut opts = cli.base_options(timeout, &args.query);
            opts.max_results = args.max_results.clamp(1, cfg.max_results_limit());
            opts.engines = split_engines(args.engines.as_deref());
            opts.category = args.category;
            opts.region = args.region.clone();
            opts.language = args.language.clone();
            opts.time_range = args.time_range;
            opts.safesearch = args.safesearch;
            opts.filters = args.filters.clone();
            opts.page = args.page.max(1);
            opts.source_policy = source_policy(
                &args.source_policy_mode,
                &args.allowed_domains,
                &args.excluded_domains,
            )?;
            let resp = client.search(opts).await?;
            if cli.json {
                print_json(&resp);
            } else {
                output::print_grounded(&args.query, &resp, args.max_results);
            }
        }
        Command::Engines(args) => {
            let cats: Vec<phrona::Category> = match args.category {
                Some(c) => vec![c],
                None => phrona::Category::ALL.to_vec(),
            };
            if cli.json {
                let mut map = serde_json::Map::new();
                for c in cats {
                    let names: Vec<String> = phrona::available_engines(c)
                        .iter()
                        .map(|e| e.name.clone())
                        .collect();
                    map.insert(c.as_str().to_string(), json!(names));
                }
                println!("{}", json!(map));
            } else {
                for c in cats {
                    output::print_engines_table(c);
                }
            }
        }
        Command::Test(args) => {
            run_test(&cli, &client, args, timeout).await?;
        }
        Command::Serve(args) => {
            run_serve(args, &cfg).await?;
        }
        Command::Bootstrap(args) => {
            run_bootstrap(&client, args).await?;
        }
        Command::Mcp => {
            init_tracing();
            phrona_mcp::run_stdio(&cfg).await?;
        }
        Command::Completions(args) => {
            args::print_completions(&args.shell)?;
        }
    }
    Ok(())
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .init();
}

/// Full server: REST API (axum) plus MCP-over-TCP, in one process. Addresses
/// and the API key default to `server.bind_addr` / `server.mcp_addr` /
/// `server.api_key` from the config (env overrides included).
async fn run_serve(args: &args::ServeArgs, cfg: &PhronaConfig) -> anyhow::Result<()> {
    init_tracing();
    let mut cfg = cfg.clone();
    if let Some(k) = &args.api_key {
        cfg.server.api_key = Some(k.clone());
    }

    let rest_addr = match &args.addr {
        Some(a) => a.parse()?,
        None => std::env::var("PHRONA_ADDR")
            .ok()
            .filter(|a| !a.is_empty())
            .map(|a| a.parse())
            .transpose()?
            .unwrap_or(cfg.bind_addr()?),
    };
    let mcp_addr = args
        .mcp_addr
        .clone()
        .unwrap_or_else(|| cfg.server.mcp_addr.clone());

    // One shared shutdown trigger: SIGTERM/Ctrl+C fans out to the REST
    // server (graceful drain), the MCP TCP server (accept-loop stop + drain
    // window) and the select arms below.
    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            phrona_api::shutdown_signal().await;
            shutdown.notify_waiters();
        });
    }

    let rest_fut = async {
        if !args.no_rest {
            phrona_api::serve(rest_addr, cfg.clone()).await?;
        }
        anyhow::Ok(())
    };
    let mcp_fut = async {
        if !args.no_mcp {
            let listener = phrona_mcp::tcp_listener(&mcp_addr).await?;
            tracing::info!("phrona-mcp listening on tcp://{mcp_addr} (newline-delimited JSON-RPC)");
            phrona_mcp::serve_tcp(listener, cfg.clone(), shutdown.clone()).await?;
        }
        anyhow::Ok(())
    };

    match (!args.no_rest, !args.no_mcp) {
        // Both listeners: SIGTERM/Ctrl+C drains the REST server gracefully
        // (axum waits for in-flight requests) and then the process exits.
        // `biased` prefers the completed REST future so the drain is never
        // preempted by the signal branch.
        (true, true) => {
            tokio::select! {
                biased;
                r = rest_fut => r?,
                m = mcp_fut => m?,
                _ = shutdown.notified() => {}
            }
        }
        (true, false) => {
            tokio::select! {
                biased;
                r = rest_fut => r?,
                _ = shutdown.notified() => {}
            }
        }
        (false, true) => {
            tokio::select! {
                biased;
                m = mcp_fut => m?,
                _ = shutdown.notified() => {}
            }
        }
        (false, false) => {
            anyhow::bail!("nothing to serve: both --no-rest and --no-mcp are set");
        }
    }
    Ok(())
}

/// Manual bootstrap: harvest fresh session cookies and register them on the
/// current client (useful as a smoke test; servers should rely on the
/// automatic silent bypass).
async fn run_bootstrap(client: &phrona::SearchClient, args: &BootstrapArgs) -> anyhow::Result<()> {
    // cookies register on the shared EngineShared through interior
    // mutability, so `&SearchClient` suffices
    let known: Vec<&str> = ["google", "annas_archive", "qwant"].to_vec();
    let engines: Vec<String> = if args.engines.is_empty() {
        known.iter().map(|s| s.to_string()).collect()
    } else {
        for e in &args.engines {
            if !known.contains(&e.as_str()) {
                anyhow::bail!(
                    "unknown bootstrap engine '{e}', expected one of: {}",
                    known.join(", ")
                );
            }
        }
        args.engines.clone()
    };
    for engine in engines {
        print!("{engine}: ");
        match tokio::task::spawn_blocking({
            let engine = engine.clone();
            move || phrona::bootstrap::harvest_blocking(&engine)
        })
        .await?
        {
            Ok(jar) => {
                println!(
                    "OK ({} cookies, {} bytes)",
                    jar.split("; ").count(),
                    jar.len()
                );
                client.register_bootstrap_cookie(&engine, jar.clone());
                phrona::bootstrap::store_cached(&engine, &jar);
            }
            Err(e) => println!("FAILED ({e})"),
        }
    }
    Ok(())
}

fn split_engines(s: Option<&str>) -> Vec<String> {
    s.map(|s| s.split(',').map(|e| e.trim().to_string()).collect())
        .unwrap_or_default()
}

fn source_policy(
    mode: &str,
    allowed: &[String],
    denied: &[String],
) -> anyhow::Result<phrona::SourcePolicy> {
    phrona::SourcePolicy::compile(mode, allowed, denied)
        .map_err(|e| anyhow::anyhow!("invalid source policy: {e}"))
}

fn split_sources(s: Option<&str>) -> anyhow::Result<Vec<SuggestSource>> {
    let s = s.unwrap_or_default();
    let mut out = Vec::new();
    for n in s.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match SuggestSource::from_name(n) {
            Some(src) => out.push(src),
            None => anyhow::bail!(
                "unknown suggest source '{n}', expected one of: {}",
                SuggestSource::ALL
                    .iter()
                    .map(|s| s.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    Ok(out)
}

fn print_json(resp: &phrona::SearchResponse) {
    println!(
        "{}",
        serde_json::to_string_pretty(resp).expect("serialize response")
    );
}

async fn run_test(
    cli: &Cli,
    client: &phrona::SearchClient,
    args: &TestArgs,
    timeout: Duration,
) -> Result<()> {
    let cats: Vec<phrona::Category> = match args.category {
        Some(c) => vec![c],
        None => phrona::Category::ALL.to_vec(),
    };
    let mut reports = Vec::new();
    let mut any_success = false;
    for cat in cats {
        let mut opts = cli.base_options(timeout, &args.query);
        opts.category = cat;
        opts.max_results = args.max_results.clamp(1, 10);
        // availability probing must observe every engine, not stop at the
        // first ones that fill max_results
        opts.probe_all = true;
        match client.search(opts).await {
            Ok(resp) => {
                any_success = true;
                reports.push((cat, resp));
            }
            Err(e) => {
                reports.push((
                    cat,
                    phrona::SearchResponse {
                        query: args.query.clone(),
                        category: cat,
                        page: 1,
                        total: 0,
                        results: Vec::new(),
                        suggestions: Vec::new(),
                        answer: None,
                        engines: Vec::new(),
                        elapsed_ms: 0,
                    },
                ));
                eprintln!("category {}: {e}", cat.as_str());
            }
        }
    }
    if cli.json {
        let out: Vec<_> = reports
            .iter()
            .map(|(cat, r)| {
                json!({
                    "category": cat.as_str(),
                    "total": r.total,
                    "elapsed_ms": r.elapsed_ms,
                    "answer": r.answer,
                    "engines": r.engines,
                })
            })
            .collect();
        println!("{}", json!(out));
    } else {
        output::print_test_report(reports);
    }
    if !any_success {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_engines_handles_comma_lists() {
        assert_eq!(split_engines(None), Vec::<String>::new());
        assert_eq!(
            split_engines(Some(" bing, duckduckgo , mojeek ")),
            ["bing", "duckduckgo", "mojeek"]
        );
    }
}
