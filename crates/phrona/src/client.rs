//! HTTP client with browser impersonation, proxy pool and SSRF guards.

#[cfg(test)]
use std::collections::HashMap;
use std::net::IpAddr;
#[cfg(test)]
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use wreq::Uri;
use wreq::header::{HeaderMap, HeaderValue, USER_AGENT};
use wreq::redirect;
use wreq_util::Emulation;

use crate::config::SecurityConfig;
use crate::error::{Error, Result};
use crate::extract::is_safe_ip;

/// Operator-defined domain allow/deny list applied to every outbound target
/// (extract URLs and each redirect hop). `denied` wins over `allowed`; an
/// empty `allowed` list permits any host. Matching is case-insensitive and
/// covers subdomains (e.g. `example.com` matches `www.example.com`).
#[derive(Clone, Debug, Default)]
pub struct TargetPolicy {
    /// Domains always allowed; empty permits any host.
    pub allowed: Vec<String>,
    /// Domains always denied; denied wins over allowed.
    pub denied: Vec<String>,
}

impl TargetPolicy {
    /// Build from the operator's `security` configuration section. These
    /// lists were previously dead config; they are now enforced.
    pub fn from_security(sec: &SecurityConfig) -> Self {
        Self {
            allowed: sec.allowed_domains.clone(),
            denied: sec.denied_domains.clone(),
        }
    }

    /// Whether an outbound target host passes the allow/deny policy.
    pub fn domain_allowed(&self, host: &str) -> bool {
        let h = host.trim().to_ascii_lowercase();
        let matches = |list: &[String]| {
            list.iter().any(|d| {
                let d = d.trim().to_ascii_lowercase();
                h == d || h.ends_with(&format!(".{d}"))
            })
        };
        !matches(&self.denied) && (self.allowed.is_empty() || matches(&self.allowed))
    }
}

/// Parse an IP literal from a URL/Uri host string, tolerating the brackets
/// that authority serialization adds around IPv6 addresses (`[::1]`).
fn parse_host_ip(host: &str) -> Option<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    if host.starts_with('[') && host.ends_with(']') {
        if let Ok(v6) = host[1..host.len() - 1].parse::<std::net::Ipv6Addr>() {
            return Some(IpAddr::V6(v6));
        }
    }
    None
}

/// Validate a target URL for SSRF safety: only `http`/`https` schemes, and
/// every address the hostname resolves to must pass [`is_safe_ip`]. Used for
/// the initial request (in `extract`) and for every redirect hop. The
/// operator's domain allow/deny policy is enforced first.
pub(crate) async fn validate_target(uri: &Uri, policy: &TargetPolicy) -> Result<()> {
    let scheme = uri.scheme_str().unwrap_or("");
    if scheme != "http" && scheme != "https" {
        return Err(Error::invalid_query(
            "client",
            "unsupported URL scheme (http/https only)",
        ));
    }
    let host = uri
        .host()
        .ok_or_else(|| Error::invalid_query("client", "URL has no host"))?;
    if !policy.domain_allowed(host) {
        return Err(Error::invalid_query(
            "client",
            "target host is blocked by the domain allow/deny policy",
        ));
    }
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let safe = if let Some(ip) = parse_host_ip(host) {
        is_safe_ip(ip)
    } else {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| Error::invalid_query("client", "host resolution failed"))?;
        addrs.into_iter().all(|sa| is_safe_ip(sa.ip()))
    };
    if safe {
        Ok(())
    } else {
        Err(Error::invalid_query(
            "client",
            "SSRF blocked: IP address is in a private/restricted range",
        ))
    }
}

/// Redirect policy that intercepts every hop: enforces the redirect limit
/// and validates scheme + destination IP + domain policy before following.
/// A non-`http(s)`, policy-blocked or private/restricted hop fails the
/// request instead of being followed.
fn ssrf_redirect_policy(max_redirects: usize, policy: TargetPolicy) -> redirect::Policy {
    redirect::Policy::custom(move |attempt| {
        let policy = policy.clone();
        attempt.pending(move |attempt| async move {
            if attempt.previous.len() > max_redirects {
                return attempt.error(Error::internal("client", "too many redirects"));
            }
            match validate_target(&attempt.uri, &policy).await {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        })
    })
}

/// Browser profile used to impersonate a real browser over TLS/HTTP2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Chrome 148 impersonation (default).
    Chrome,
    /// Chrome 100 impersonation.
    Chrome100,
    /// Chrome 120 impersonation.
    Chrome120,
    /// Chrome 131 impersonation.
    Chrome131,
    /// Chrome 140 impersonation.
    Chrome140,
    /// Chrome 149 impersonation.
    Chrome149,
    /// Firefox 148 impersonation (default).
    Firefox,
    /// Firefox 139 impersonation.
    Firefox139,
    /// Firefox 148 impersonation.
    Firefox148,
    /// Edge 148 impersonation (default).
    Edge,
    /// Edge 148 impersonation.
    Edge148,
    /// Safari 26 impersonation (default).
    Safari,
    /// Safari 26 impersonation.
    Safari26,
    /// Opera 131 impersonation (default).
    Opera,
    /// Opera 131 impersonation.
    Opera131,
    /// Android OkHttp impersonation.
    OkHttp,
    /// Rotate to a random profile per request.
    Random,
}

impl Profile {
    /// Resolve a lowercase profile name (family names and versioned
    /// variants as used by `phrona.yaml` / `PHRONA_ENGINES_PROFILE`).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "chrome" | "chrome148" => Some(Profile::Chrome),
            "chrome149" => Some(Profile::Chrome149),
            "chrome140" => Some(Profile::Chrome140),
            "chrome131" => Some(Profile::Chrome131),
            "chrome120" => Some(Profile::Chrome120),
            "chrome100" => Some(Profile::Chrome100),
            "firefox" | "firefox148" => Some(Profile::Firefox),
            "firefox139" => Some(Profile::Firefox139),
            "safari" | "safari26" => Some(Profile::Safari),
            "edge" | "edge148" => Some(Profile::Edge),
            "opera" | "opera131" => Some(Profile::Opera),
            "okhttp" => Some(Profile::OkHttp),
            "random" => Some(Profile::Random),
            _ => None,
        }
    }

    fn to_emulation(self) -> Emulation {
        use wreq_util::Profile as P;
        let profile = match self {
            Profile::Chrome => P::Chrome148,
            Profile::Chrome100 => P::Chrome100,
            Profile::Chrome120 => P::Chrome120,
            Profile::Chrome131 => P::Chrome131,
            Profile::Chrome140 => P::Chrome140,
            Profile::Chrome149 => P::Chrome149,
            Profile::Firefox => P::Firefox148,
            Profile::Firefox139 => P::Firefox139,
            Profile::Firefox148 => P::Firefox148,
            Profile::Edge => P::Edge148,
            Profile::Edge148 => P::Edge148,
            Profile::Safari => P::Safari26,
            Profile::Safari26 => P::Safari26,
            Profile::Opera => P::Opera131,
            Profile::Opera131 => P::Opera131,
            Profile::OkHttp => P::OkHttp5,
            Profile::Random => return Emulation::random(),
        };
        Emulation::builder().profile(profile).build()
    }
}

/// Browser User-Agent strings matching the TLS/HTTP2 impersonation profiles
/// of [`Profile`]. Variants of a family (versioned profiles) use the family
/// UA; [`Profile::Random`] picks a fresh random family UA on every call so
/// each client instance rotates instead of sharing one process-global UA.
const UA_CHROME: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const UA_FIREFOX: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:148.0) Gecko/20100101 Firefox/148.0";
const UA_SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.4 Safari/605.1.15";
const UA_EDGE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0";
const UA_OPERA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 OPR/134.0.0.0";
const UA_OKHTTP: &str = "okhttp/5.0.0-alpha.14";
const UA_POOL: [&str; 5] = [UA_CHROME, UA_FIREFOX, UA_SAFARI, UA_EDGE, UA_OPERA];

/// UA for a browser profile, matching the exact TLS impersonation family.
pub fn default_user_agent(profile: Profile) -> &'static str {
    match profile {
        Profile::Firefox | Profile::Firefox139 | Profile::Firefox148 => UA_FIREFOX,
        Profile::Safari | Profile::Safari26 => UA_SAFARI,
        Profile::Edge | Profile::Edge148 => UA_EDGE,
        Profile::Opera | Profile::Opera131 => UA_OPERA,
        Profile::OkHttp => UA_OKHTTP,
        // Random emulation: rotate through the known browser families so
        // clients do not share one cached UA for the process lifetime.
        Profile::Random => {
            use rand::RngExt;
            UA_POOL[rand::rng().random_range(0..UA_POOL.len())]
        }
        // Chrome and all versioned Chrome variants.
        Profile::Chrome
        | Profile::Chrome100
        | Profile::Chrome120
        | Profile::Chrome131
        | Profile::Chrome140
        | Profile::Chrome149 => UA_CHROME,
    }
}

/// A sticky pool of persistent impersonated HTTP clients: one per proxy URL
/// (each with its own connection pool and cookie jar), or a single direct
/// client when no proxies are configured. [`ProxyPool::get_client`] assigns
/// clients round-robin; an engine task keeps its client for its whole
/// lifetime so multi-step flows (vqd -> i.js, sc -> search) stay pinned to
/// the same proxy and cookies.
pub struct ProxyPool {
    clients: Vec<HttpClient>,
    counter: AtomicUsize,
}

impl ProxyPool {
    /// Build one persistent client per proxy URL. An empty `proxies` list
    /// yields a single direct client. `policy` is enforced on every
    /// outbound target (initial URL and redirect hops).
    pub fn new(
        proxies: Vec<String>,
        profile: Profile,
        timeout: Duration,
        policy: TargetPolicy,
    ) -> Result<Self> {
        let mut clients = Vec::with_capacity(proxies.len().max(1));
        for proxy in proxies {
            clients.push(
                HttpClient::builder()
                    .profile(profile)
                    .timeout(timeout)
                    .proxy(Some(proxy))
                    .target_policy(policy.clone())
                    .build()?,
            );
        }
        if clients.is_empty() {
            clients.push(
                HttpClient::builder()
                    .profile(profile)
                    .timeout(timeout)
                    .target_policy(policy)
                    .build()?,
            );
        }
        Ok(Self {
            clients,
            counter: AtomicUsize::new(0),
        })
    }

    /// Number of pooled clients (one per proxy URL, or 1 for direct
    /// connections).
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Whether the pool holds no clients. A pool built from an empty proxy
    /// list always contains a single direct client, so this is only `true`
    /// for a manually constructed empty pool.
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Deterministic round-robin client selection.
    pub fn get_client(&self) -> &HttpClient {
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        &self.clients[idx]
    }

    /// The first (or only) client — for non-engine flows such as `extract`.
    pub fn first(&self) -> &HttpClient {
        &self.clients[0]
    }
}

/// A persistent HTTP client with browser impersonation, a cookie jar,
/// per-request timeout, and SSRF-guarded redirects.
///
/// Build one with [`HttpClient::builder`], or skip the details and use
/// [`crate::SearchClient`] (which owns a pool of these) or the convenience
/// [`crate::search()`] / [`crate::search_sync()`] helpers.
pub struct HttpClient {
    client: wreq::Client,
    target_policy: TargetPolicy,
    #[cfg(test)]
    test_resolutions: HashMap<String, (SocketAddr, IpAddr)>,
}

impl HttpClient {
    /// Start building a client; defaults are Chrome impersonation, 10s
    /// timeout, cookies enabled, 10 redirect hops and an open domain policy.
    pub fn builder() -> HttpClientBuilder {
        HttpClientBuilder::default()
    }

    /// The domain allow/deny policy applied to outbound targets.
    pub(crate) fn target_policy(&self) -> &TargetPolicy {
        &self.target_policy
    }

    #[cfg(test)]
    pub(crate) fn test_safe_ip(&self, host: &str) -> Option<IpAddr> {
        self.test_resolutions
            .get(&host.to_ascii_lowercase())
            .map(|(_, safe_ip)| *safe_ip)
    }

    /// Perform a GET request. Redirects are followed up to the configured
    /// limit, validating each hop's scheme and destination against the
    /// domain/IP policy.
    pub async fn get(&self, url: &str) -> Result<wreq::Response> {
        Ok(self.client.get(url).send().await?)
    }

    /// Single-hop GET with redirects disabled. The caller is responsible for
    /// following (and validating) any redirect itself — used by SSRF-guarded
    /// flows such as `extract`.
    pub async fn get_no_redirect(&self, url: &str) -> Result<wreq::Response> {
        Ok(self
            .client
            .get(url)
            .redirect(redirect::Policy::none())
            .send()
            .await?)
    }

    /// Perform a GET with extra request headers merged over the defaults.
    pub async fn get_with_headers(&self, url: &str, headers: &HeaderMap) -> Result<wreq::Response> {
        let mut rb = self.client.get(url);
        for (k, v) in headers {
            // replace (not append) so caller headers override defaults;
            // duplicated header values make many upstreams answer 400
            rb = rb.header(k, v);
        }
        Ok(rb.send().await?)
    }

    /// Perform a POST with an `application/x-www-form-urlencoded` body
    /// (e.g. as returned by [`crate::parse::form_encode`]).
    pub async fn post_form(&self, url: &str, form: &str) -> Result<wreq::Response> {
        Ok(self
            .client
            .post(url)
            .header(
                wreq::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form.to_string())
            .send()
            .await?)
    }

    /// Perform a form POST with extra request headers merged over the
    /// defaults. A caller-provided `Content-Type` replaces the default
    /// `application/x-www-form-urlencoded` (needed for multipart bodies).
    pub async fn post_form_with_headers(
        &self,
        url: &str,
        form: &str,
        headers: &HeaderMap,
    ) -> Result<wreq::Response> {
        let mut rb = self.client.post(url).body(form.to_string());
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        if !headers.contains_key(wreq::header::CONTENT_TYPE) {
            rb = rb.header(
                wreq::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            );
        }
        Ok(rb.send().await?)
    }
}

/// Builder for [`HttpClient`]. All methods are chainable and consume `self`;
/// finalize with [`HttpClientBuilder::build`].
pub struct HttpClientBuilder {
    profile: Profile,
    timeout: Duration,
    cookies: bool,
    redirects: usize,
    headers: HeaderMap,
    proxy: Option<String>,
    target_policy: TargetPolicy,
    #[cfg(test)]
    test_resolutions: HashMap<String, (SocketAddr, IpAddr)>,
}

impl Default for HttpClientBuilder {
    fn default() -> Self {
        let profile = Profile::Chrome;
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(default_user_agent(profile)),
        );
        Self {
            profile,
            timeout: Duration::from_secs(10),
            cookies: true,
            redirects: 10,
            headers,
            proxy: None,
            target_policy: TargetPolicy::default(),
            #[cfg(test)]
            test_resolutions: HashMap::new(),
        }
    }
}

impl HttpClientBuilder {
    /// Set the browser impersonation profile; the User-Agent header is kept
    /// in lockstep with the TLS/HTTP2 fingerprint.
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        // keep the UA in lockstep with the TLS/HTTP2 impersonation profile
        self.headers.insert(
            USER_AGENT,
            HeaderValue::from_static(default_user_agent(profile)),
        );
        self
    }

    /// Set the per-request timeout (default 10s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Route traffic through a proxy (e.g. `socks5://127.0.0.1:9050`).
    /// `None` (the default) connects directly.
    pub fn proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy = proxy;
        self
    }

    /// Apply the operator domain allow/deny policy to every outbound target
    /// (initial URL and each redirect hop).
    pub fn target_policy(mut self, policy: TargetPolicy) -> Self {
        self.target_policy = policy;
        self
    }

    /// Route one hostname to a loopback listener while reporting a safe
    /// public address to the test-only extraction preflight.
    #[cfg(test)]
    pub(crate) fn resolve_for_test(
        mut self,
        host: impl Into<String>,
        connect_addr: SocketAddr,
        safe_addr: IpAddr,
    ) -> Self {
        self.test_resolutions
            .insert(host.into().to_ascii_lowercase(), (connect_addr, safe_addr));
        self
    }

    /// Build the client. Returns an [`Error::invalid_query`] for an invalid
    /// proxy URL and an internal error if the underlying client cannot be
    /// constructed.
    pub fn build(self) -> Result<HttpClient> {
        #[cfg(test)]
        let test_resolutions = self.test_resolutions.clone();
        let mut builder = wreq::Client::builder()
            .emulation(self.profile.to_emulation())
            .timeout(self.timeout)
            .redirect(ssrf_redirect_policy(
                self.redirects,
                self.target_policy.clone(),
            ));
        #[cfg(test)]
        {
            // Test fixtures must reach their loopback spy directly even when
            // the process inherits HTTP(S)_PROXY/ALL_PROXY from the host.
            builder = builder.no_proxy();
        }
        #[cfg(test)]
        for (host, (connect_addr, _)) in &test_resolutions {
            builder = builder.resolve(host.clone(), *connect_addr);
        }
        if self.cookies {
            builder = builder.cookie_store(true);
        }
        if !self.headers.is_empty() {
            builder = builder.default_headers(self.headers);
        }
        if let Some(proxy) = self.proxy {
            let p = wreq::Proxy::all(&proxy)
                .map_err(|_| Error::invalid_query("client", "invalid proxy URL"))?;
            builder = builder.proxy(p);
        }
        let client = builder
            .build()
            .map_err(|_| Error::internal("client", "client build failed"))?;
        Ok(HttpClient {
            client,
            target_policy: self.target_policy,
            #[cfg(test)]
            test_resolutions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchClient;
    use crate::error::ErrorKind;
    use crate::source_policy::SourceCatalogue;
    use futures::FutureExt;

    #[test]
    fn proxy_pool_round_robin_is_deterministic() {
        let pool = ProxyPool::new(
            vec![
                "http://127.0.0.1:1".into(),
                "http://127.0.0.1:2".into(),
                "http://127.0.0.1:3".into(),
            ],
            Profile::Chrome,
            Duration::from_secs(5),
            TargetPolicy::default(),
        )
        .unwrap();
        assert_eq!(pool.len(), 3);
        let a = pool.get_client() as *const HttpClient;
        let b = pool.get_client() as *const HttpClient;
        let c = pool.get_client() as *const HttpClient;
        let a2 = pool.get_client() as *const HttpClient;
        let b2 = pool.get_client() as *const HttpClient;
        // strict rotation: a b c a b ...
        assert!(!std::ptr::eq(a, b));
        assert!(!std::ptr::eq(b, c));
        assert!(!std::ptr::eq(a, c));
        assert!(std::ptr::eq(a, a2));
        assert!(std::ptr::eq(b, b2));
    }

    #[test]
    fn proxy_pool_empty_yields_single_direct_client() {
        let pool = ProxyPool::new(
            vec![],
            Profile::Firefox,
            Duration::from_secs(5),
            TargetPolicy::default(),
        )
        .unwrap();
        assert_eq!(pool.len(), 1);
        assert!(std::ptr::eq(
            pool.get_client() as *const HttpClient,
            pool.get_client() as *const HttpClient,
        ));
        assert!(std::ptr::eq(pool.first(), pool.get_client()));
    }

    #[test]
    fn default_user_agent_matches_family() {
        assert!(default_user_agent(Profile::Firefox).contains("Firefox/"));
        assert!(default_user_agent(Profile::Firefox139).contains("Firefox/"));
        assert!(default_user_agent(Profile::Firefox148).contains("Firefox/"));
        assert!(default_user_agent(Profile::Safari).contains("Safari/"));
        assert!(default_user_agent(Profile::Safari26).contains("Version/"));
        assert!(default_user_agent(Profile::Chrome).contains("Chrome/"));
        assert!(default_user_agent(Profile::Chrome100).contains("Chrome/"));
        assert!(default_user_agent(Profile::Edge).contains("Edg/"));
        assert!(default_user_agent(Profile::Opera).contains("OPR/"));
        assert!(default_user_agent(Profile::OkHttp).starts_with("okhttp/"));
        let r1 = default_user_agent(Profile::Random);
        let r2 = default_user_agent(Profile::Random);
        // Deterministic stand-in for "rotates per call": across 8 draws a
        // 5-entry pool can only collide 8 times in a row with negligible
        // probability (1/5^7), so at least two distinct UAs must appear.
        let mut draws = std::collections::HashSet::new();
        for _ in 0..8 {
            draws.insert(default_user_agent(Profile::Random));
        }
        assert!(
            draws.len() >= 2,
            "random UA rotates per call, not cached once"
        );
        assert!(UA_POOL.contains(&r1));
        assert!(UA_POOL.contains(&r2));
        assert!(draws.iter().all(|ua| UA_POOL.contains(ua)));
    }

    #[test]
    fn target_policy_allow_deny_and_subdomains() {
        let policy = TargetPolicy {
            allowed: vec!["Example.com".into()],
            denied: vec!["evil.example.com".into()],
        };
        assert!(policy.domain_allowed("example.com"));
        assert!(policy.domain_allowed("WWW.EXAMPLE.COM"));
        assert!(policy.domain_allowed("blog.example.com"));
        assert!(!policy.domain_allowed("evil.example.com"));
        assert!(!policy.domain_allowed("other.org"));
        assert!(!policy.domain_allowed("notexample.com"));
        let open = TargetPolicy::default();
        assert!(open.domain_allowed("anything.example"));
        let deny_only = TargetPolicy {
            allowed: vec![],
            denied: vec!["blocked.org".into()],
        };
        assert!(!deny_only.domain_allowed("blocked.org"));
        assert!(!deny_only.domain_allowed("www.blocked.org"));
        assert!(deny_only.domain_allowed("fine.org"));
    }

    #[test]
    fn extract_blocks_denied_domain_before_dns() {
        let policy = TargetPolicy {
            allowed: vec![],
            denied: vec!["denied.example".into()],
        };
        let client = HttpClient::builder().target_policy(policy).build().unwrap();
        let err = crate::extract::extract(&client, "http://denied.example/page", 2000, None)
            .now_or_never();
        assert!(matches!(
            err,
            Some(Err(Error {
                kind: ErrorKind::InvalidQuery { .. },
                ..
            }))
        ));
    }

    #[test]
    fn source_catalogue_can_be_attached_to_an_explicit_client() {
        let catalogue =
            SourceCatalogue::compile(["official.example"], std::iter::empty::<&str>()).unwrap();
        let client = SearchClient::with_options(
            Profile::Chrome,
            Some(Duration::from_secs(1)),
            None,
            TargetPolicy::default(),
        )
        .unwrap()
        .with_source_catalogue(catalogue.clone());
        assert_eq!(client.source_catalogue(), &catalogue);
    }
}
