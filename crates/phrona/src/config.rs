//! Typed YAML configuration for Phrona and its surfaces (CLI, REST API, MCP).
//!
//! Resolution order (lowest to highest precedence):
//! 1. Built-in defaults ([`PhronaConfig::defaults`]).
//! 2. YAML file: `$PHRONA_CONFIG_PATH` when set, otherwise `./phrona.yaml`
//!    in the working directory when it exists.
//! 3. Environment variable overrides ([`PhronaConfig::apply_env_overrides`],
//!    e.g. `PHRONA_SERVER_BIND_ADDR`, `PHRONA_API_KEY`).
//!
//! Load a configuration with [`PhronaConfig::load`]; parse raw YAML with
//! [`PhronaConfig::from_yaml_str`]. Every field has a default, so partial
//! files and `phrona.yaml`-style snippets are valid.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::client::Profile;
use crate::error::Result;
use crate::options::SearchOptions;
use crate::search::SearchClient;
use crate::source_policy::{SourceCatalogue, SourcePolicyError};

/// Name of the default config file looked up in the working directory.
pub const DEFAULT_CONFIG_FILE: &str = "phrona.yaml";

/// Environment variable pointing at a config file.
pub const CONFIG_PATH_ENV: &str = "PHRONA_CONFIG_PATH";

/// All environment variable override names understood by the config loader.
pub const ENV_OVERRIDES: &[&str] = &[
    "PHRONA_SERVER_BIND_ADDR",
    "PHRONA_SERVER_MCP_ADDR",
    "PHRONA_API_KEY",
    "PHRONA_RATE_LIMIT_PER_MINUTE",
    "PHRONA_MAX_BODY_BYTES",
    "PHRONA_SERVER_TRUSTED_PROXIES",
    "PHRONA_SEARCH_TIMEOUT_SECS",
    "PHRONA_SEARCH_MAX_RESULTS_LIMIT",
    "PHRONA_SEARCH_CONCURRENCY_LIMIT",
    "PHRONA_SEARCH_CACHE_TTL_SECS",
    "PHRONA_SECURITY_BLOCK_PRIVATE_IPS",
    "PHRONA_SECURITY_ALLOWED_DOMAINS",
    "PHRONA_SECURITY_DENIED_DOMAINS",
    "PHRONA_ENGINES_PROXIES",
    "PHRONA_ENGINES_PROFILE",
    "PHRONA_ENGINES_AUTO_BOOTSTRAP",
];

/// Errors produced while loading or interpreting a [`PhronaConfig`].
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file could not be parsed as YAML.
    Yaml(serde_yaml::Error),
    /// The file `$PHRONA_CONFIG_PATH` points to does not exist.
    MissingFile(PathBuf),
    /// An environment variable could not be parsed into its target type.
    Env(String),
    /// A bind address is not a valid `SocketAddr`.
    InvalidAddr(String),
    /// The operator source catalogue contains an invalid domain.
    InvalidSourceCatalogue(SourcePolicyError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config: {e}"),
            ConfigError::Yaml(e) => write!(f, "config: invalid yaml: {e}"),
            ConfigError::MissingFile(p) => {
                write!(f, "config: file does not exist: {}", p.display())
            }
            ConfigError::Env(e) => write!(f, "config: {e}"),
            ConfigError::InvalidAddr(a) => write!(f, "config: invalid bind address: {a}"),
            ConfigError::InvalidSourceCatalogue(e) => {
                write!(f, "config: invalid source catalogue: {e}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

fn default_bind_addr() -> String {
    "127.0.0.1:8080".to_string()
}

fn default_mcp_addr() -> String {
    "127.0.0.1:8081".to_string()
}

fn default_rate_limit() -> u32 {
    120
}

fn default_max_body_bytes() -> u64 {
    2_097_152
}

fn default_timeout_secs() -> u64 {
    15
}

fn default_max_results_limit() -> usize {
    100
}

fn default_concurrency_limit() -> usize {
    8
}

fn default_cache_ttl_secs() -> u64 {
    3600
}

fn default_profile() -> String {
    "chrome".to_string()
}

fn default_auto_bootstrap() -> bool {
    // opt-in: browser sessions are never launched unless the operator
    // enables them (config, PHRONA_ENGINES_AUTO_BOOTSTRAP=1, or an
    // explicit `phrona bootstrap <engine>` run)
    false
}

/// Settings for the REST API and MCP-over-TCP servers. Maps to the
/// `server:` section of `phrona.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// REST API bind address.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    /// MCP-over-TCP bind address.
    #[serde(default = "default_mcp_addr")]
    pub mcp_addr: String,
    /// API key required by REST/MCP clients; `None` disables auth.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Fixed-window request cap for search endpoints per window; `0` disables.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Maximum accepted request body size in bytes.
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: u64,
    /// IPs of reverse proxies directly in front of the server. When the
    /// peer address belongs to this list, rate limiting trusts the
    /// leftmost `X-Forwarded-For` address as the client IP; otherwise the
    /// peer address is used. Empty (default) never trusts the header.
    #[serde(default)]
    pub trusted_proxies: Vec<IpAddr>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            mcp_addr: default_mcp_addr(),
            api_key: None,
            rate_limit_per_minute: default_rate_limit(),
            max_body_bytes: default_max_body_bytes(),
            trusted_proxies: Vec::new(),
        }
    }
}

/// Search behavior defaults applied by all surfaces. Maps to the `search:`
/// section of `phrona.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Default search deadline in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Upper bound applied to `max_results` clamps on every surface.
    #[serde(default = "default_max_results_limit")]
    pub max_results_limit: usize,
    /// Maximum simultaneous outbound engine requests per search.
    #[serde(default = "default_concurrency_limit")]
    pub concurrency_limit: usize,
    /// Cache TTL for engine-scoped token/state caches (seconds).
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout_secs(),
            max_results_limit: default_max_results_limit(),
            concurrency_limit: default_concurrency_limit(),
            cache_ttl_secs: default_cache_ttl_secs(),
        }
    }
}

/// SSRF and egress controls for outbound requests. Maps to the `security:`
/// section of `phrona.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Refuse to connect to private/loopback/link-local IPs. The core
    /// extractor always enforces IP safety; this knob reserves future
    /// surfaces that could relax it, and mirrors it for observability.
    #[serde(default = "default_block_private_ips")]
    pub block_private_ips: bool,
    /// Optional allow-list of hostnames for outbound requests (empty = all).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Optional deny-list of hostnames for outbound requests.
    #[serde(default)]
    pub denied_domains: Vec<String>,
}

fn default_block_private_ips() -> bool {
    true
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            block_private_ips: default_block_private_ips(),
            allowed_domains: Vec::new(),
            denied_domains: Vec::new(),
        }
    }
}

/// Engine transport settings: proxy pool and browser impersonation profile.
/// Maps to the `engines:` section of `phrona.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnginesConfig {
    /// Proxy URLs (e.g. `socks5://127.0.0.1:9050`); one pooled HTTP client
    /// per proxy, used round-robin. Empty = direct connections.
    #[serde(default)]
    pub proxies: Vec<String>,
    /// Per-engine session cookies earned in a real browser (see the
    /// `webrief` companion tool), sent as the engine's `Cookie` header on
    /// every request. Needed by engines whose anti-bot grants trust only to
    /// real sessions: google (`__Secure-ENID`), qwant (`datadome`),
    /// annas_archive (`aa_ddg_check`, `__ddg2_`). Values expire; refresh by
    /// re-capturing.
    #[serde(default)]
    pub bootstrap_cookies: HashMap<String, String>,
    /// Silently harvest fresh session cookies with the system Chromium
    /// (headless, a few seconds) when a corresponding engine is blocked and
    /// its cookies are missing/stale. Disable for fully static deployments.
    #[serde(default = "default_auto_bootstrap")]
    pub auto_bootstrap: bool,
    /// Browser impersonation profile: chrome, chrome149, chrome140,
    /// chrome131, chrome120, chrome100, firefox, firefox139, firefox148,
    /// safari, safari26, edge, edge148, opera, opera131, okhttp, random.
    #[serde(default = "default_profile")]
    pub profile: String,
}

impl Default for EnginesConfig {
    fn default() -> Self {
        Self {
            proxies: Vec::new(),
            bootstrap_cookies: HashMap::new(),
            auto_bootstrap: default_auto_bootstrap(),
            profile: default_profile(),
        }
    }
}

/// Operator-owned source authority lists. These are never supplied by a
/// caller request and are compiled before a client is constructed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourcesConfig {
    /// Official source domains.
    #[serde(default)]
    pub official: Vec<String>,
    /// Reputable secondary source domains.
    #[serde(default)]
    pub secondary: Vec<String>,
}

/// Typed YAML configuration for all Phrona surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhronaConfig {
    /// REST/MCP server settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Search defaults (timeout, limits, concurrency).
    #[serde(default)]
    pub search: SearchConfig,
    /// SSRF/egress controls.
    #[serde(default)]
    pub security: SecurityConfig,
    /// Engine transport (proxy pool, impersonation profile).
    #[serde(default)]
    pub engines: EnginesConfig,
    /// Operator-managed source authority catalogue.
    #[serde(default)]
    pub sources: SourcesConfig,
}

impl PhronaConfig {
    /// Built-in defaults, with no file and no environment input.
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Parse a configuration from YAML text. Fields that are absent fall
    /// back to their defaults; environment overrides are NOT applied.
    pub fn from_yaml_str(yaml: &str) -> std::result::Result<Self, ConfigError> {
        serde_yaml::from_str(yaml).map_err(ConfigError::Yaml)
    }

    /// Parse a configuration file and apply real environment overrides.
    pub fn load_from_file(path: &Path) -> std::result::Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let mut cfg = Self::from_yaml_str(&text)?;
        cfg.apply_real_env()?;
        Ok(cfg)
    }

    /// Resolve the configuration: `$PHRONA_CONFIG_PATH` when set (must
    /// exist), else `./phrona.yaml` in the working directory when present,
    /// else defaults; then apply environment variable overrides.
    pub fn load() -> std::result::Result<Self, ConfigError> {
        let mut cfg = match std::env::var(CONFIG_PATH_ENV) {
            Ok(path) if !path.is_empty() => {
                let p = PathBuf::from(path);
                if !p.is_file() {
                    return Err(ConfigError::MissingFile(p));
                }
                Self::load_from_file(&p)?
            }
            _ => match Path::new(DEFAULT_CONFIG_FILE).try_exists() {
                Ok(true) => Self::load_from_file(Path::new(DEFAULT_CONFIG_FILE))?,
                _ => Self::defaults(),
            },
        };
        cfg.apply_real_env()?;
        Ok(cfg)
    }

    /// Apply the real process environment: every variable in
    /// [`ENV_OVERRIDES`] overrides the corresponding field when set.
    pub fn apply_real_env(&mut self) -> std::result::Result<(), ConfigError> {
        let overrides: Vec<(String, String)> = ENV_OVERRIDES
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|v| (name.to_string(), v)))
            .collect();
        self.apply_env_overrides(&overrides)
    }

    /// Apply explicit overrides given as `(variable name, value)` pairs.
    /// Pure (no process environment access) so it is unit-testable and
    /// embeddable. An empty `PHRONA_API_KEY` clears the key.
    pub fn apply_env_overrides(
        &mut self,
        overrides: &[(String, String)],
    ) -> std::result::Result<(), ConfigError> {
        let bad =
            |name: &str, e: String| ConfigError::Env(format!("invalid value for {name}: {e}"));
        for (name, value) in overrides {
            match name.as_str() {
                "PHRONA_SERVER_BIND_ADDR" => self.server.bind_addr = value.clone(),
                "PHRONA_SERVER_MCP_ADDR" => self.server.mcp_addr = value.clone(),
                "PHRONA_API_KEY" => {
                    self.server.api_key = (!value.is_empty()).then(|| value.clone())
                }
                "PHRONA_RATE_LIMIT_PER_MINUTE" => {
                    self.server.rate_limit_per_minute = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_MAX_BODY_BYTES" => {
                    self.server.max_body_bytes = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_SERVER_TRUSTED_PROXIES" => {
                    self.server.trusted_proxies = value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            s.parse::<IpAddr>()
                                .map_err(|e: std::net::AddrParseError| bad(name, e.to_string()))
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?
                }
                "PHRONA_SEARCH_TIMEOUT_SECS" => {
                    self.search.timeout_secs = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_SEARCH_MAX_RESULTS_LIMIT" => {
                    self.search.max_results_limit = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_SEARCH_CONCURRENCY_LIMIT" => {
                    self.search.concurrency_limit = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_SEARCH_CACHE_TTL_SECS" => {
                    self.search.cache_ttl_secs = value
                        .parse()
                        .map_err(|e: std::num::ParseIntError| bad(name, e.to_string()))?
                }
                "PHRONA_SECURITY_BLOCK_PRIVATE_IPS" => {
                    self.security.block_private_ips = parse_bool(value).map_err(|e| bad(name, e))?
                }
                "PHRONA_SECURITY_ALLOWED_DOMAINS" => {
                    self.security.allowed_domains = split_csv(value)
                }
                "PHRONA_SECURITY_DENIED_DOMAINS" => self.security.denied_domains = split_csv(value),
                "PHRONA_ENGINES_PROXIES" => self.engines.proxies = split_csv(value),
                "PHRONA_ENGINES_PROFILE" => self.engines.profile = value.clone(),
                "PHRONA_ENGINES_AUTO_BOOTSTRAP" => {
                    self.engines.auto_bootstrap = parse_bool(value).map_err(|e| bad(name, e))?
                }
                _ => return Err(ConfigError::Env(format!("unknown variable {name}"))),
            }
        }
        Ok(())
    }

    /// Parse `server.bind_addr` as a `SocketAddr`.
    pub fn bind_addr(&self) -> std::result::Result<SocketAddr, ConfigError> {
        self.server
            .bind_addr
            .parse()
            .map_err(|_| ConfigError::InvalidAddr(self.server.bind_addr.clone()))
    }

    /// Parse `server.mcp_addr` as a `SocketAddr`.
    pub fn mcp_addr(&self) -> std::result::Result<SocketAddr, ConfigError> {
        self.server
            .mcp_addr
            .parse()
            .map_err(|_| ConfigError::InvalidAddr(self.server.mcp_addr.clone()))
    }

    /// The configured API key, if any.
    pub fn api_key(&self) -> Option<String> {
        self.server.api_key.clone()
    }

    /// Resolve `engines.profile` into a [`Profile`]; unknown names fall back
    /// to the default Chrome emulation.
    pub fn profile(&self) -> Profile {
        Profile::from_name(&self.engines.profile).unwrap_or(Profile::Chrome)
    }

    /// `search.max_results_limit`, the upper bound applied to result clamps.
    pub fn max_results_limit(&self) -> usize {
        self.search.max_results_limit
    }

    /// `search.concurrency_limit`, the per-search engine concurrency cap.
    pub fn concurrency_limit(&self) -> usize {
        self.search.concurrency_limit
    }

    /// `search.timeout_secs` as a [`Duration`].
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.search.timeout_secs)
    }

    /// A [`SearchOptions`] preloaded with the configured timeout.
    pub fn search_options(&self, query: impl Into<String>) -> SearchOptions {
        let mut opts = SearchOptions::new(query);
        opts.timeout = self.timeout();
        opts
    }

    /// A [`SearchClient`] built from this config: profile, timeout and
    /// proxy pool.
    pub fn search_client(&self) -> Result<SearchClient> {
        SearchClient::with_config(self)
    }

    /// The operator-supplied per-engine bootstrap cookies
    /// (`engines.bootstrap_cookies`).
    pub fn bootstrap_cookies(&self) -> &HashMap<String, String> {
        &self.engines.bootstrap_cookies
    }

    /// Compile the operator-owned source catalogue, rejecting invalid
    /// hostname-only entries before any search client is created.
    pub fn source_catalogue(&self) -> std::result::Result<SourceCatalogue, ConfigError> {
        SourceCatalogue::compile(&self.sources.official, &self.sources.secondary)
            .map_err(ConfigError::InvalidSourceCatalogue)
    }
}

fn parse_bool(s: &str) -> std::result::Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!("expected true/false, got '{other}'")),
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_policy::SourceTier;

    #[test]
    fn defaults_are_sane() {
        let cfg = PhronaConfig::defaults();
        assert_eq!(cfg.server.bind_addr, "127.0.0.1:8080");
        assert_eq!(cfg.server.mcp_addr, "127.0.0.1:8081");
        assert_eq!(cfg.server.api_key, None);
        assert_eq!(cfg.server.rate_limit_per_minute, 120);
        assert_eq!(cfg.server.max_body_bytes, 2_097_152);
        assert_eq!(cfg.search.timeout_secs, 15);
        assert_eq!(cfg.search.max_results_limit, 100);
        assert_eq!(cfg.search.concurrency_limit, 8);
        assert_eq!(cfg.search.cache_ttl_secs, 3600);
        assert!(cfg.security.block_private_ips);
        assert!(cfg.security.allowed_domains.is_empty());
        assert!(cfg.security.denied_domains.is_empty());
        assert!(cfg.engines.proxies.is_empty());
        assert_eq!(cfg.engines.profile, "chrome");
        assert_eq!(cfg.profile(), Profile::Chrome);
        assert_eq!(cfg.bind_addr().unwrap().to_string(), "127.0.0.1:8080");
        assert_eq!(cfg.mcp_addr().unwrap().to_string(), "127.0.0.1:8081");
        assert_eq!(cfg.max_results_limit(), 100);
        assert_eq!(cfg.concurrency_limit(), 8);
        assert_eq!(cfg.timeout(), Duration::from_secs(15));
    }

    #[test]
    fn partial_yaml_keeps_defaults() {
        let cfg = PhronaConfig::from_yaml_str("server:\n  bind_addr: 0.0.0.0:9000\n").unwrap();
        assert_eq!(cfg.server.bind_addr, "0.0.0.0:9000");
        assert_eq!(cfg.server.mcp_addr, "127.0.0.1:8081");
        assert_eq!(cfg.search.timeout_secs, 15);
        assert!(cfg.security.block_private_ips);
        assert_eq!(cfg.engines.profile, "chrome");
        assert!(cfg.sources.official.is_empty());
        assert!(cfg.sources.secondary.is_empty());
    }

    #[test]
    fn source_catalogue_is_operator_owned_and_validated() {
        let cfg = PhronaConfig::from_yaml_str(
            "sources:\n  official: [docs.example.com]\n  secondary: [community.example.com]\n",
        )
        .unwrap();
        let catalogue = cfg.source_catalogue().unwrap();
        assert_eq!(
            catalogue.classify_host("docs.example.com"),
            SourceTier::Official
        );
        assert_eq!(
            catalogue.classify_host("community.example.com"),
            SourceTier::Secondary
        );

        let invalid =
            PhronaConfig::from_yaml_str("sources:\n  official: [https://example.com]\n").unwrap();
        assert!(invalid.source_catalogue().is_err());
    }

    #[test]
    fn full_yaml_roundtrip() {
        let yaml = r#"
server:
  bind_addr: 0.0.0.0:8080
  mcp_addr: 0.0.0.0:8081
  api_key: sekret
  rate_limit_per_minute: 60
  max_body_bytes: 1048576
search:
  timeout_secs: 30
  max_results_limit: 50
  concurrency_limit: 4
  cache_ttl_secs: 1800
security:
  block_private_ips: false
  allowed_domains: [example.com, github.com]
  denied_domains: [ads.example.com]
engines:
  proxies: [socks5://127.0.0.1:9050]
  profile: firefox
"#;
        let cfg = PhronaConfig::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.server.bind_addr, "0.0.0.0:8080");
        assert_eq!(cfg.server.mcp_addr, "0.0.0.0:8081");
        assert_eq!(cfg.server.api_key.as_deref(), Some("sekret"));
        assert_eq!(cfg.server.rate_limit_per_minute, 60);
        assert_eq!(cfg.server.max_body_bytes, 1_048_576);
        assert_eq!(cfg.search.timeout_secs, 30);
        assert_eq!(cfg.search.max_results_limit, 50);
        assert_eq!(cfg.search.concurrency_limit, 4);
        assert_eq!(cfg.search.cache_ttl_secs, 1800);
        assert!(!cfg.security.block_private_ips);
        assert_eq!(
            cfg.security.allowed_domains,
            vec!["example.com".to_string(), "github.com".to_string()]
        );
        assert_eq!(
            cfg.security.denied_domains,
            vec!["ads.example.com".to_string()]
        );
        assert_eq!(
            cfg.engines.proxies,
            vec!["socks5://127.0.0.1:9050".to_string()]
        );
        assert_eq!(cfg.engines.profile, "firefox");
        assert_eq!(cfg.profile(), Profile::Firefox);
        let round: PhronaConfig =
            serde_yaml::from_str(&serde_yaml::to_string(&cfg).expect("serialize config"))
                .expect("reserialize");
        assert_eq!(round.server.bind_addr, cfg.server.bind_addr);
        assert_eq!(round.search.concurrency_limit, 4);
    }

    #[test]
    fn malformed_yaml_is_an_error() {
        assert!(PhronaConfig::from_yaml_str("server: [unclosed").is_err());
    }

    #[test]
    fn env_overrides_override_file_values() {
        let mut cfg = PhronaConfig::from_yaml_str(
            "server:\n  api_key: file-key\nengines:\n  profile: chrome\n",
        )
        .unwrap();
        let overrides: Vec<(String, String)> = vec![
            ("PHRONA_API_KEY".into(), "env-key".into()),
            ("PHRONA_SERVER_BIND_ADDR".into(), "0.0.0.0:9999".into()),
            ("PHRONA_SEARCH_TIMEOUT_SECS".into(), "7".into()),
            ("PHRONA_SEARCH_CONCURRENCY_LIMIT".into(), "3".into()),
            ("PHRONA_RATE_LIMIT_PER_MINUTE".into(), "0".into()),
            ("PHRONA_MAX_BODY_BYTES".into(), "512".into()),
            ("PHRONA_SECURITY_BLOCK_PRIVATE_IPS".into(), "no".into()),
            (
                "PHRONA_SECURITY_ALLOWED_DOMAINS".into(),
                "a.com, b.com".into(),
            ),
            (
                "PHRONA_ENGINES_PROXIES".into(),
                "http://p1, socks5://p2".into(),
            ),
            ("PHRONA_ENGINES_PROFILE".into(), "safari".into()),
        ];
        cfg.apply_env_overrides(&overrides).unwrap();
        assert_eq!(cfg.server.api_key.as_deref(), Some("env-key"));
        assert_eq!(cfg.server.bind_addr, "0.0.0.0:9999");
        assert_eq!(cfg.search.timeout_secs, 7);
        assert_eq!(cfg.search.concurrency_limit, 3);
        assert_eq!(cfg.server.rate_limit_per_minute, 0);
        assert_eq!(cfg.server.max_body_bytes, 512);
        assert!(!cfg.security.block_private_ips);
        assert_eq!(
            cfg.security.allowed_domains,
            vec!["a.com".to_string(), "b.com".to_string()]
        );
        assert_eq!(
            cfg.engines.proxies,
            vec!["http://p1".to_string(), "socks5://p2".to_string()]
        );
        assert_eq!(cfg.profile(), Profile::Safari);
    }

    #[test]
    fn empty_api_key_env_clears_the_key() {
        let mut cfg = PhronaConfig::from_yaml_str("server:\n  api_key: file-key\n").unwrap();
        cfg.apply_env_overrides(&[("PHRONA_API_KEY".into(), String::new())])
            .unwrap();
        assert_eq!(cfg.server.api_key, None);
    }

    #[test]
    fn env_parse_errors_are_reported() {
        let mut cfg = PhronaConfig::defaults();
        let err = cfg
            .apply_env_overrides(&[("PHRONA_RATE_LIMIT_PER_MINUTE".into(), "many".into())])
            .unwrap_err();
        assert!(matches!(err, ConfigError::Env(_)));
        assert!(err.to_string().contains("PHRONA_RATE_LIMIT_PER_MINUTE"));

        let mut cfg = PhronaConfig::defaults();
        assert!(
            cfg.apply_env_overrides(&[(
                "PHRONA_SECURITY_BLOCK_PRIVATE_IPS".into(),
                "sometimes".into(),
            )])
            .is_err()
        );
    }

    #[test]
    fn unknown_override_is_rejected() {
        let mut cfg = PhronaConfig::defaults();
        assert!(
            cfg.apply_env_overrides(&[("PHRONA_BOGUS".into(), "x".into())])
                .is_err()
        );
    }

    #[test]
    fn profile_resolution_and_fallback() {
        assert_eq!(PhronaConfig::defaults().profile(), Profile::Chrome);
        let mut cfg = PhronaConfig::defaults();
        cfg.engines.profile = "firefox".into();
        assert_eq!(cfg.profile(), Profile::Firefox);
        cfg.engines.profile = "opera131".into();
        assert_eq!(cfg.profile(), Profile::Opera);
        cfg.engines.profile = "netscape".into();
        assert_eq!(cfg.profile(), Profile::Chrome, "unknown names fall back");
    }

    #[test]
    fn invalid_bind_addr_is_an_error() {
        let mut cfg = PhronaConfig::defaults();
        cfg.server.bind_addr = "not-an-address".into();
        assert!(matches!(cfg.bind_addr(), Err(ConfigError::InvalidAddr(_))));
    }

    #[test]
    fn search_options_and_client_come_from_config() {
        let mut cfg = PhronaConfig::defaults();
        cfg.search.timeout_secs = 9;
        cfg.search.concurrency_limit = 2;
        let opts = cfg.search_options("hello");
        assert_eq!(opts.query, "hello");
        assert_eq!(opts.timeout, Duration::from_secs(9));
        let client = cfg.search_client().unwrap();
        assert_eq!(client.concurrency_limit(), 2);
    }

    #[test]
    fn load_from_file_applies_overrides_after_yaml() {
        let dir = std::env::temp_dir().join(format!("phrona-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.yaml");
        std::fs::write(
            &path,
            "server:\n  bind_addr: 127.0.0.1:9999\nsearch:\n  timeout_secs: 21\n",
        )
        .unwrap();
        let cfg = PhronaConfig::load_from_file(&path).unwrap();
        assert_eq!(cfg.server.bind_addr, "127.0.0.1:9999");
        assert_eq!(cfg.search.timeout_secs, 21);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn example_file_parses_and_matches_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("phrona.yaml");
        let cfg = PhronaConfig::load_from_file(&path).unwrap();
        let defaults = PhronaConfig::defaults();
        assert_eq!(cfg.server.bind_addr, defaults.server.bind_addr);
        assert_eq!(cfg.server.mcp_addr, defaults.server.mcp_addr);
        assert_eq!(
            cfg.server.rate_limit_per_minute,
            defaults.server.rate_limit_per_minute
        );
        assert_eq!(cfg.server.max_body_bytes, defaults.server.max_body_bytes);
        assert_eq!(cfg.search.timeout_secs, defaults.search.timeout_secs);
        assert_eq!(
            cfg.search.max_results_limit,
            defaults.search.max_results_limit
        );
        assert_eq!(
            cfg.search.concurrency_limit,
            defaults.search.concurrency_limit
        );
        assert_eq!(cfg.search.cache_ttl_secs, defaults.search.cache_ttl_secs);
        assert_eq!(
            cfg.security.block_private_ips,
            defaults.security.block_private_ips
        );
        assert!(cfg.security.allowed_domains.is_empty());
        assert!(cfg.security.denied_domains.is_empty());
        assert!(cfg.engines.proxies.is_empty());
        assert_eq!(cfg.engines.profile, defaults.engines.profile);
    }
}
