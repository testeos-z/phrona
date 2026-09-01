use std::io;

use clap::{Args, CommandFactory, Parser, Subcommand};

use phrona::{Category, Profile, SearchOptions, TimeRange};

/// Top-level CLI: global flags (`--json`, `--profile`, `--proxy`,
/// `--timeout`) plus a subcommand.
#[derive(Parser)]
#[command(
    name = "phrona",
    version,
    about = "Phrona command-line interface",
    long_about = "Search 26 engines across 5 categories, get suggestions, extract pages, \
                  produce AI-grounded answers, and probe engine availability. \
                  All commands accept --json for machine-readable output."
)]
pub struct Cli {
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,

    /// Browser impersonation profile (default: engines.profile from
    /// phonona.yaml / PHRONA_ENGINES_PROFILE, else chrome)
    #[arg(long, global = true, value_parser = profile_parser)]
    pub profile: Option<Profile>,

    /// Proxy URL (e.g. socks5://127.0.0.1:9050), repeatable; overrides the
    /// configured proxy list
    #[arg(long, global = true)]
    pub proxy: Vec<String>,
    /// Bootstrap session cookies for an engine, as `engine=Cookie-header`
    /// (e.g. `google="__Secure-ENID=...; SOCS=CAI"`), repeatable; overrides
    /// `engines.bootstrap_cookies` from the config
    #[arg(long, global = true)]
    pub cookie: Vec<String>,

    /// Allow a headless browser to refresh engine sessions automatically
    /// when blocked (opt-in: off by default). Same as
    /// `engines.auto_bootstrap: true` or `PHRONA_AUTO_BOOTSTRAP=1`
    #[arg(long = "auto-bootstrap", global = true)]
    pub auto_bootstrap: bool,

    /// Request timeout in seconds (default: search.timeout_secs, else 15)
    #[arg(long, global = true)]
    pub timeout: Option<u64>,

    #[command(subcommand)]
    pub command: Command,
}

/// The `phrona` subcommands.
#[derive(Subcommand)]
pub enum Command {
    /// Search engines and print merged, ranked results
    Search(SearchArgs),
    /// Query autocomplete sources
    Suggest(SuggestArgs),
    /// Extract readable text from a web page
    Extract(ExtractArgs),
    /// Grounded search: synthesized answer plus ranked sources (RAG)
    Ground(GroundArgs),
    /// List engines per category
    Engines(EnginesArgs),
    /// Probe engine availability across every category
    Test(TestArgs),
    /// Start the full server: REST API plus MCP-over-TCP
    Serve(ServeArgs),
    /// Serve MCP over stdio only (for MCP clients)
    Mcp,
    /// Refresh session cookies for cookie-gated engines (google,
    /// annas_archive, qwant) by driving the system Chromium headless for a
    /// few seconds. Default: all supported engines.
    Bootstrap(BootstrapArgs),
    /// Generate shell completion script
    Completions(CompletionsArgs),
}

/// Arguments for `phrona search`: merged, ranked results across engines.
#[derive(Args)]
pub struct SearchArgs {
    /// Search query
    pub query: String,

    /// Result category: web | images | news | videos | books
    #[arg(long, value_parser = category_parser, default_value = "web")]
    pub category: Category,

    /// Comma-separated engine names (default: all of the category)
    #[arg(long)]
    pub engines: Option<String>,

    /// Maximum merged results
    #[arg(long, default_value_t = 20)]
    pub max_results: usize,

    /// SafeSearch level: off | moderate | strict
    #[arg(long, value_parser = safesearch_parser, default_value = "moderate")]
    pub safesearch: phrona::SafeSearch,

    /// Region (e.g. us-en, de-de)
    #[arg(long)]
    pub region: Option<String>,

    /// Language (e.g. en)
    #[arg(long)]
    pub language: Option<String>,

    /// Time range: day | week | month | year
    #[arg(long, value_parser = time_range_parser)]
    pub time_range: Option<TimeRange>,

    /// Engine filter string (e.g. site:github.com)
    #[arg(long)]
    pub filters: Option<String>,

    /// Result page
    #[arg(long, default_value_t = 1)]
    pub page: u32,

    /// Source policy mode: any | prefer-official | require-allowed | official-only
    #[arg(long = "source-policy-mode", default_value = "any", value_parser = source_policy_mode_parser)]
    pub source_policy_mode: String,

    /// Caller-requested hostname; repeat for multiple domains.
    #[arg(long = "allowed-domain")]
    pub allowed_domains: Vec<String>,

    /// Caller-excluded hostname; repeat for multiple domains.
    #[arg(long = "excluded-domain")]
    pub excluded_domains: Vec<String>,
}

/// Arguments for `phrona suggest`: query autocomplete from search sources.
#[derive(Args)]
pub struct SuggestArgs {
    /// Query prefix
    pub query: String,

    /// Comma-separated sources (default: all)
    #[arg(long)]
    pub source: Option<String>,

    /// Region (e.g. us-en)
    #[arg(long, default_value = "us-en")]
    pub region: String,
}

/// Arguments for `phrona extract`: readable text extraction from pages.
#[derive(Args)]
pub struct ExtractArgs {
    /// Page URLs (several may be given; they are fetched in parallel)
    #[arg(required = true)]
    pub urls: Vec<String>,

    /// Maximum characters of extracted text
    #[arg(long, default_value_t = 5000)]
    pub max_chars: usize,

    /// Bias the excerpt toward this query
    #[arg(long)]
    pub query: Option<String>,

    /// Source policy mode for the initial URL and redirects.
    #[arg(long = "source-policy-mode", default_value = "any", value_parser = source_policy_mode_parser)]
    pub source_policy_mode: String,
    /// Caller-requested hostname; repeat for multiple domains.
    #[arg(long = "allowed-domain")]
    pub allowed_domains: Vec<String>,
    /// Caller-excluded hostname; repeat for multiple domains.
    #[arg(long = "excluded-domain")]
    pub excluded_domains: Vec<String>,
}

/// Arguments for `phrona ground`: grounded search with a synthesized
/// answer plus ranked sources.
#[derive(Args)]
pub struct GroundArgs {
    /// Search query
    pub query: String,

    /// Maximum sources to return
    #[arg(long, default_value_t = 8)]
    pub max_results: usize,

    /// Comma-separated engine names
    #[arg(long)]
    pub engines: Option<String>,

    /// Result category: web | images | news | videos | books
    #[arg(long, value_parser = category_parser, default_value = "web")]
    pub category: Category,

    /// Region (e.g. us-en, de-de)
    #[arg(long)]
    pub region: Option<String>,

    /// Language (e.g. en)
    #[arg(long)]
    pub language: Option<String>,

    /// Time range: day | week | month | year
    #[arg(long, value_parser = time_range_parser)]
    pub time_range: Option<TimeRange>,

    /// SafeSearch level: off | moderate | strict
    #[arg(long, value_parser = safesearch_parser, default_value = "moderate")]
    pub safesearch: phrona::SafeSearch,

    /// Engine filter string (e.g. site:github.com)
    #[arg(long)]
    pub filters: Option<String>,

    /// Result page
    #[arg(long, default_value_t = 1)]
    pub page: u32,

    /// Source policy mode: any | prefer-official | require-allowed | official-only
    #[arg(long = "source-policy-mode", default_value = "any", value_parser = source_policy_mode_parser)]
    pub source_policy_mode: String,
    /// Caller-requested hostname; repeat for multiple domains.
    #[arg(long = "allowed-domain")]
    pub allowed_domains: Vec<String>,
    /// Caller-excluded hostname; repeat for multiple domains.
    #[arg(long = "excluded-domain")]
    pub excluded_domains: Vec<String>,
}

/// Arguments for `phrona engines`: list engines for a category.
#[derive(Args)]
pub struct EnginesArgs {
    /// Filter by category
    #[arg(long, value_parser = category_parser)]
    pub category: Option<Category>,
}

/// Arguments for `phrona test`: probe engine availability across
/// Arguments for `phrona bootstrap`.
#[derive(Args)]
pub struct BootstrapArgs {
    /// Engines to refresh (default: all with bootstrap support)
    pub engines: Vec<String>,
}

/// categories.
#[derive(Args)]
pub struct TestArgs {
    /// Query used for the probe (default: "rust programming")
    #[arg(long, default_value = "rust programming")]
    pub query: String,

    /// Probe a single category only
    #[arg(long, value_parser = category_parser)]
    pub category: Option<Category>,

    /// Maximum merged results per category
    #[arg(long, default_value_t = 5)]
    pub max_results: usize,
}

/// Arguments for `phrona serve`: start the REST API and MCP-over-TCP
/// servers.
#[derive(Args)]
pub struct ServeArgs {
    /// REST API bind address (default: server.bind_addr)
    #[arg(long)]
    pub addr: Option<String>,

    /// MCP-over-TCP bind address (default: server.mcp_addr)
    #[arg(long)]
    pub mcp_addr: Option<String>,

    /// API key required by clients (default: server.api_key)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Disable the MCP-over-TCP listener (REST only)
    #[arg(long)]
    pub no_mcp: bool,

    /// Disable the REST listener (MCP only)
    #[arg(long)]
    pub no_rest: bool,
}

/// Arguments for `phrona completions`: generate a shell completion script.
#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_parser = ["bash", "zsh", "fish", "powershell", "elvish"])]
    pub shell: String,
}

/// Parse a CLI category argument into a [`Category`], with a friendly error.
pub fn category_parser(s: &str) -> Result<Category, String> {
    s.parse::<Category>().map_err(|_| {
        "invalid category, expected one of: web, images, news, videos, books".to_string()
    })
}

fn safesearch_parser(s: &str) -> Result<phrona::SafeSearch, String> {
    s.parse::<phrona::SafeSearch>()
        .map_err(|_| "invalid safesearch, expected one of: off, moderate, strict".to_string())
}

fn time_range_parser(s: &str) -> Result<TimeRange, String> {
    s.parse::<TimeRange>()
        .map_err(|_| "invalid time_range, expected one of: day, week, month, year".to_string())
}

fn source_policy_mode_parser(s: &str) -> Result<String, String> {
    s.parse::<phrona::SourceMode>()
        .map(|mode| mode.to_string())
        .map_err(|e| e.to_string())
}

fn profile_parser(s: &str) -> Result<Profile, String> {
    Profile::from_name(s).ok_or_else(|| {
        format!(
            "unknown profile '{s}', expected chrome, firefox, safari, edge, opera, okhttp, random"
        )
    })
}

impl Cli {
    /// Build the base [`SearchOptions`] for a command: query plus the CLI
    /// timeout (global `--timeout` defaults are resolved by the caller).
    pub fn base_options(
        &self,
        timeout: std::time::Duration,
        query: impl Into<String>,
    ) -> SearchOptions {
        let mut opts = SearchOptions::new(query);
        opts.timeout = timeout;
        opts
    }
}

/// Print the shell completion script for `shell` to stdout.
pub fn print_completions(shell: &str) -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    let shell = match shell {
        "bash" => clap_complete::Shell::Bash,
        "zsh" => clap_complete::Shell::Zsh,
        "fish" => clap_complete::Shell::Fish,
        "powershell" => clap_complete::Shell::PowerShell,
        "elvish" => clap_complete::Shell::Elvish,
        _ => unreachable!("validated by clap"),
    };
    clap_complete::generate(shell, &mut cmd, "phrona", &mut io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_parser_accepts_aliases() {
        for s in [
            "chrome",
            "chrome148",
            "chrome149",
            "chrome140",
            "firefox",
            "safari26",
            "random",
        ] {
            assert!(profile_parser(s).is_ok(), "{s} should parse");
        }
        assert!(profile_parser("netscape").is_err());
    }

    #[test]
    fn category_and_range_parsers() {
        assert_eq!(category_parser("news").unwrap(), Category::News);
        assert!(category_parser("nope").is_err());
        assert_eq!(time_range_parser("week").unwrap(), TimeRange::Week);
        assert!(time_range_parser("yesterday").is_err());
        assert!(safesearch_parser("strict").is_ok());
        assert!(safesearch_parser("x").is_err());
    }

    #[test]
    fn search_accepts_source_policy_flags() {
        let cli = Cli::try_parse_from([
            "phrona",
            "search",
            "rust",
            "--source-policy-mode",
            "official-only",
            "--allowed-domain",
            "docs.example",
            "--excluded-domain",
            "ads.example",
        ])
        .unwrap();
        let Command::Search(args) = cli.command else {
            panic!("expected search")
        };
        assert_eq!(args.source_policy_mode, "official-only");
        assert_eq!(args.allowed_domains, ["docs.example"]);
        assert_eq!(args.excluded_domains, ["ads.example"]);
    }
}
