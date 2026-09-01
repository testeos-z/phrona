//! Readable page extraction for AI grounding.

use std::net::{IpAddr, Ipv4Addr};

use futures::StreamExt;
use serde::Serialize;
use serde::Serializer;

use scraper::{Html, Selector};
use url::Url;

use crate::client::HttpClient;
use crate::error::{Error, Result};
use crate::parse;
use crate::source_policy::{SourceAssessment, SourceCatalogue, SourcePolicy};

/// Reject addresses that are never safe to fetch from the internet:
/// loopback, RFC1918 private / IPv6 ULA, CGNAT, link-local (incl. cloud
/// metadata), broadcast, documentation, 6to4, NAT64, multicast, reserved and
/// unspecified ranges. IPv4-compatible IPv6 addresses are unwrapped and
/// judged by their embedded IPv4 address; the entire IPv4-mapped range
/// (`::ffff:0:0/96`) is rejected.
pub fn is_safe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.octets()[0] == 0 // 0.0.0.0/8
            || v4.is_private() // 10/8, 172.16/12, 192.168/16
            || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 0x40) // 100.64/10 CGNAT
            || v4.is_loopback() // 127/8
            || v4.is_link_local() // 169.254/16
            || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0) // 192.0.0/24
            || v4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
            || (v4.octets()[0] == 192 && v4.octets()[1] == 88 && v4.octets()[2] == 99) // 192.88.99/24
            || (v4.octets()[0] == 198 && v4.octets()[1] & 0xfe == 0x12) // 198.18/15
            || v4.is_multicast() // 224/4
            || v4.octets()[0] & 0xf0 == 0xf0 // 240/4
            || v4.is_broadcast())
        } // 255.255.255.255/32
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false; // ::1/128, ::/128
            }
            let seg0 = v6.segments()[0];
            if (seg0 & 0xff00) == 0xff00 {
                return false; // ff00::/8 multicast (v4 224/4 equivalent)
            }
            if seg0 == 0 && v6.segments()[1] == 0 && v6.segments()[2] == 0 {
                if v6.segments()[3] != 0 {
                    return false; // ::ffff:0:0/96 IPv4-mapped
                }
                if v6.segments()[4] != 0 || v6.segments()[5] != 0 {
                    return false; // IPv4-compatible ::/96
                }
                let lo = (u32::from(v6.segments()[6]) << 16) | u32::from(v6.segments()[7]);
                return is_safe_ip(IpAddr::V4(Ipv4Addr::from(lo)));
            }
            if (seg0 & 0xffc0) == 0xfe80 {
                return false; // fe80::/10 link-local
            }
            if (seg0 & 0xfe00) == 0xfc00 {
                return false; // fc00::/7 unique-local
            }
            if seg0 == 0x64 && v6.segments()[1] == 0xff9b && v6.segments()[2] == 1 {
                return false; // 64:ff9b:1::/48 NAT64
            }
            if seg0 == 0x100
                && v6.segments()[1] == 0
                && v6.segments()[2] == 0
                && v6.segments()[3] == 0
            {
                return false; // 100::/64 discard
            }
            if seg0 == 0x2001 && v6.segments()[1] == 0xdb8 {
                return false; // 2001:db8::/32 documentation
            }
            if seg0 == 0x2002 {
                return false; // 2002::/16 6to4
            }
            true
        }
    }
}

/// A readable-text extraction of a web page (AI grounding).
#[derive(Debug, Clone)]
pub struct ExtractedPage {
    /// The final URL after any redirects.
    pub url: String,
    /// Page title, or the URL when no title exists.
    pub title: String,
    /// `meta` description, or an empty string.
    pub description: String,
    /// Main readable text, truncated/excerpted per the extraction options.
    pub text: String,
    /// Absolute http(s) image URLs found on the page (up to 10).
    pub images: Vec<String>,
}

impl Serialize for ExtractedPage {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ExtractedPage", 5)?;
        st.serialize_field("url", &self.url)?;
        st.serialize_field("title", &self.title)?;
        st.serialize_field("description", &self.description)?;
        st.serialize_field("text", &self.text)?;
        st.serialize_field("images", &self.images)?;
        st.end()
    }
}

/// Maximum redirect hops followed by [`extract`], each re-validated for SSRF.
const MAX_REDIRECTS: usize = 5;

/// Hard cap on response bodies read by [`extract`] (5 MiB): protects against
/// memory-exhaustion (OOM) DoS from huge or unbounded pages.
const MAX_BODY_BYTES: usize = 5_242_880;

/// Append `chunk` to `buf`, never growing `buf` beyond [`MAX_BODY_BYTES`].
/// Returns `true` when the cap has been reached and the caller should stop
/// reading.
fn push_capped(buf: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let room = MAX_BODY_BYTES.saturating_sub(buf.len());
    if room == 0 {
        return true;
    }
    let take = chunk.len().min(room);
    buf.extend_from_slice(&chunk[..take]);
    take < chunk.len()
}

/// Fetch and extract the main content of a page.
/// `query` optionally highlights the most relevant excerpt.
///
/// SSRF guard: every hop (initial URL and each redirect) is parsed,
/// DNS-resolved and validated against [`is_safe_ip`] *before* a request is
/// sent; a private/restricted destination aborts immediately. Redirects are
/// followed manually (max `MAX_REDIRECTS` hops) with the client's
/// automatic redirect handling disabled.
pub async fn extract(
    client: &HttpClient,
    url: &str,
    max_chars: usize,
    query: Option<&str>,
) -> Result<ExtractedPage> {
    let policy = SourcePolicy::default();
    let catalogue = SourceCatalogue::default();
    extract_with_policy(client, &policy, &catalogue, url, max_chars, query).await
}

/// Fetch and extract a page while enforcing a caller source policy and the
/// operator-owned catalogue at the initial URL and every redirect hop.
/// Source eligibility is checked before the unchanged TargetPolicy, DNS and
/// private-IP safeguards, so a rejected source never causes outbound work.
pub async fn extract_with_policy(
    client: &HttpClient,
    source_policy: &SourcePolicy,
    source_catalogue: &SourceCatalogue,
    url: &str,
    max_chars: usize,
    query: Option<&str>,
) -> Result<ExtractedPage> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        source_target_assessment(&current, source_policy, source_catalogue)?;
        let parsed =
            Url::parse(&current).map_err(|_| Error::invalid_query("extract", "invalid URL"))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(Error::invalid_query(
                "extract",
                "unsupported URL scheme (http/https only)",
            ));
        }
        let host = parsed
            .host()
            .ok_or_else(|| Error::invalid_query("extract", "URL has no host"))?;
        if !client.target_policy().domain_allowed(&host.to_string()) {
            return Err(Error::invalid_query(
                "extract",
                "target host is blocked by the domain allow/deny policy",
            ));
        }
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| Error::invalid_query("extract", "URL has no port"))?;
        let safe = match host {
            url::Host::Ipv4(v4) => is_safe_ip(IpAddr::V4(v4)),
            url::Host::Ipv6(v6) => is_safe_ip(IpAddr::V6(v6)),
            url::Host::Domain(name) => {
                #[cfg(test)]
                if let Some(safe_ip) = client.test_safe_ip(name) {
                    is_safe_ip(safe_ip)
                } else {
                    let addrs = tokio::net::lookup_host((name, port))
                        .await
                        .map_err(|_| Error::invalid_query("extract", "host resolution failed"))?;
                    addrs.into_iter().all(|sa| is_safe_ip(sa.ip()))
                }
                #[cfg(not(test))]
                let addrs = tokio::net::lookup_host((name, port))
                    .await
                    .map_err(|_| Error::invalid_query("extract", "host resolution failed"))?;
                #[cfg(not(test))]
                addrs.into_iter().all(|sa| is_safe_ip(sa.ip()))
            }
        };
        if !safe {
            return Err(Error::invalid_query(
                "extract",
                "SSRF blocked: IP address is in a private/restricted range",
            ));
        }

        let resp = client.get_no_redirect(&current).await?;
        let status = resp.status();
        if is_redirect_status(status) {
            let loc = resp
                .headers()
                .get(wreq::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::internal("extract", "redirect without Location header"))?;
            current = parsed
                .join(loc)
                .map_err(|_| Error::invalid_query("extract", "invalid redirect Location"))?
                .to_string();
            continue;
        }
        if !status.is_success() {
            return Err(Error::unavailable("extract", status.as_u16()));
        }
        let mut bytes: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(Error::from)?;
            if push_capped(&mut bytes, &chunk) {
                break;
            }
        }
        let html = String::from_utf8_lossy(&bytes).into_owned();
        return Ok(extract_from_html(&html, &current, max_chars, query));
    }
    Err(Error::internal("extract", "too many redirects"))
}

/// Pure source-policy guard shared by the initial URL and redirect hops.
/// Keeping this separate makes the ordering/security contract testable without
/// making a network request.
fn source_target_assessment(
    url: &str,
    source_policy: &SourcePolicy,
    source_catalogue: &SourceCatalogue,
) -> Result<SourceAssessment> {
    let assessment = source_policy
        .assessment_for_url(url, source_catalogue)
        .map_err(|_| Error::invalid_query("extract", "source URL is invalid"))?;
    if !assessment.allowed() {
        return Err(Error::invalid_query(
            "extract",
            "source URL is blocked by source policy",
        ));
    }
    Ok(assessment)
}

#[cfg(test)]
fn source_target_allowed(url: &str, policy: &SourcePolicy, catalogue: &SourceCatalogue) -> bool {
    source_target_assessment(url, policy, catalogue).is_ok()
}

fn is_redirect_status(status: wreq::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Pure function: parse HTML and extract readable content.
pub fn extract_from_html(
    html: &str,
    url: &str,
    max_chars: usize,
    query: Option<&str>,
) -> ExtractedPage {
    let doc = Html::parse_document(html);
    let title = parse::doc_text(&doc, "title").unwrap_or_else(|| url.to_string());
    let description = parse::doc_attr(&doc, "meta[name=\"description\"]", "content")
        .or_else(|| parse::doc_attr(&doc, "meta[property=\"og:description\"]", "content"))
        .unwrap_or_default();

    let mut text = String::new();
    for sel_str in ["article", "main", "body"] {
        let Ok(sel) = Selector::parse(sel_str) else {
            continue;
        };
        for node in doc.select(&sel) {
            let t = collect_text(&node);
            if t.chars().count() > text.chars().count() {
                text = t;
            }
        }
        if text.chars().count() > 200 {
            break;
        }
    }
    let text = parse::collapse(&text);
    let text = match query {
        Some(q) if !q.is_empty() => parse::excerpt(&text, q, max_chars / 2),
        _ => parse::truncate(&text, max_chars),
    };

    let mut images = Vec::new();
    let img_sel = Selector::parse("img[src]").unwrap();
    for node in doc.select(&img_sel) {
        if let Some(src) = node.value().attr("src") {
            if (src.starts_with("http://") || src.starts_with("https://")) && images.len() < 10 {
                images.push(src.to_string());
            }
        }
    }

    ExtractedPage {
        url: url.to_string(),
        title,
        description,
        text,
        images,
    }
}

fn collect_text(node: &scraper::ElementRef) -> String {
    let mut out = String::new();
    for child in node
        .select(&Selector::parse("p, h1, h2, h3, h4, h5, h6, li, blockquote, pre, td, th").unwrap())
    {
        let t = parse::text_of(&child);
        if !t.is_empty() {
            out.push_str(&t);
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        collect_text_nodes(node, 0, &mut out);
    }
    out
}

/// Fallback for pages without semantic text blocks: walk child nodes and
/// keep visible text while skipping `<script>`, `<style>`, `<noscript>`,
/// `<svg>` and `<nav>` subtrees, so inline code and navigation noise never
/// reach the extracted output. Depth-capped against pathological nesting.
fn collect_text_nodes(node: &scraper::ElementRef, depth: usize, out: &mut String) {
    if depth > 32 {
        return;
    }
    for child in node.children() {
        match child.value() {
            scraper::Node::Text(text) => {
                let t = text.text.trim();
                if !t.is_empty() {
                    out.push_str(t);
                    out.push(' ');
                }
            }
            // Skip noise subtrees (inline code, navigation) entirely.
            scraper::Node::Element(element)
                if matches!(
                    element.name(),
                    "script" | "style" | "noscript" | "svg" | "nav"
                ) => {}
            scraper::Node::Element(_) => {
                if let Some(el) = scraper::ElementRef::wrap(child) {
                    collect_text_nodes(&el, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// Extract several pages in parallel (used by AI grounding endpoints).
/// Concurrency is bounded so a large batch cannot spawn unbounded
/// outbound connections; results are returned in input order.
pub async fn extract_many(
    client: &HttpClient,
    urls: &[String],
    max_chars: usize,
    query: Option<&str>,
) -> Vec<Result<ExtractedPage>> {
    const CONCURRENCY: usize = 16;
    let mut out = Vec::with_capacity(urls.len());
    for chunk in urls.chunks(CONCURRENCY) {
        let batch = chunk
            .iter()
            .map(|url| extract(client, url, max_chars, query));
        out.extend(futures::future::join_all(batch).await);
    }
    out
}

/// Extract several pages in parallel with one source policy and catalogue.
/// Results remain in input order and use the same bounded concurrency as
/// [`extract_many`].
pub async fn extract_many_with_policy(
    client: &HttpClient,
    source_policy: &SourcePolicy,
    source_catalogue: &SourceCatalogue,
    urls: &[String],
    max_chars: usize,
    query: Option<&str>,
) -> Vec<Result<ExtractedPage>> {
    const CONCURRENCY: usize = 16;
    let mut out = Vec::with_capacity(urls.len());
    for chunk in urls.chunks(CONCURRENCY) {
        let batch = chunk.iter().map(|url| {
            extract_with_policy(
                client,
                source_policy,
                source_catalogue,
                url,
                max_chars,
                query,
            )
        });
        out.extend(futures::future::join_all(batch).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use super::*;
    use crate::error::ErrorKind;

    async fn redirect_spy(
        listener: tokio::net::TcpListener,
        location: String,
        requests: Arc<Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), listener.accept()).await
        else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let Ok(Ok(size)) =
            tokio::time::timeout(std::time::Duration::from_secs(1), socket.read(&mut buffer)).await
        else {
            return;
        };
        requests
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&buffer[..size]).into_owned());
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }

    async fn proxy_spy(listener: tokio::net::TcpListener, requests: Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(std::time::Duration::from_secs(1), listener.accept()).await
        else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let Ok(Ok(size)) =
            tokio::time::timeout(std::time::Duration::from_secs(1), socket.read(&mut buffer)).await
        else {
            return;
        };
        requests
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&buffer[..size]).into_owned());
        let response =
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(response).await;
    }

    struct ProxyEnvGuard {
        _lock: MutexGuard<'static, ()>,
        values: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ProxyEnvGuard {
        fn force_proxy(proxy_url: &str) -> Self {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
            let names = [
                "HTTP_PROXY",
                "http_proxy",
                "HTTPS_PROXY",
                "https_proxy",
                "ALL_PROXY",
                "all_proxy",
                "NO_PROXY",
                "no_proxy",
            ];
            let values = names
                .into_iter()
                .map(|name| {
                    let previous = std::env::var_os(name);
                    // SAFETY: tests serialize proxy environment changes with LOCK and
                    // restore every touched variable when the guard is dropped.
                    unsafe {
                        if name == "NO_PROXY" || name == "no_proxy" {
                            std::env::remove_var(name);
                        } else {
                            std::env::set_var(name, proxy_url);
                        }
                    }
                    (name, previous)
                })
                .collect();
            Self {
                _lock: lock,
                values,
            }
        }
    }

    impl Drop for ProxyEnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.values {
                // SAFETY: the guard owns the serialized environment mutation and
                // restores the value captured before the test.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    const HTML: &str = r#"
<!doctype html><html><head>
<title>Rust Book</title>
<meta name="description" content="Learn the Rust language">
</head><body>
<main>
<h1>Ownership</h1>
<p>Rust ownership is a set of rules that govern memory management.</p>
<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.</p>
<p>Borrowing lets you use data without taking it. The borrow checker enforces these rules at compile time.</p>
<img src="https://example.com/a.png">
</main>
</body></html>"#;

    #[test]
    fn extracts_title_description_images() {
        let page = extract_from_html(HTML, "https://doc.rust-lang.org", 500, None);
        assert_eq!(page.title, "Rust Book");
        assert_eq!(page.description, "Learn the Rust language");
        assert_eq!(page.images, ["https://example.com/a.png"]);
    }

    #[test]
    fn truncates_to_max_chars() {
        let page = extract_from_html(HTML, "u", 20, None);
        assert!(page.text.chars().count() <= 20);
    }

    #[test]
    fn query_bias_excerpts() {
        let page = extract_from_html(HTML, "u", 300, Some("borrowing"));
        assert!(page.text.contains("Borrowing"));
        assert!(!page.text.contains("memory management"));
        assert!(page.text.starts_with("..."));
    }

    #[test]
    fn empty_and_tiny_html_do_not_panic() {
        let page = extract_from_html("", "u", 100, None);
        assert!(page.text.is_empty());
        let page = extract_from_html("<p>hi</p>", "u", 100, Some("q"));
        assert!(!page.text.is_empty());
    }

    #[test]
    fn body_cap_limits_accumulation() {
        let mut buf = Vec::new();
        let big = vec![0u8; MAX_BODY_BYTES + 1000];
        assert!(push_capped(&mut buf, &big));
        assert_eq!(buf.len(), MAX_BODY_BYTES);
        assert!(push_capped(&mut buf, &[1, 2, 3]));
        assert_eq!(buf.len(), MAX_BODY_BYTES);
        assert!(buf.iter().all(|&b| b == 0));

        let mut buf = Vec::new();
        let mut done = false;
        for _ in 0..6000 {
            done = push_capped(&mut buf, &[7; 1024]);
            if done {
                break;
            }
        }
        assert_eq!(buf.len(), MAX_BODY_BYTES);
        assert!(done);
    }

    #[test]
    fn is_safe_ip_rejects_restricted_ranges() {
        for ip in [
            "127.0.0.1",
            "127.8.8.8",
            "10.0.0.1",
            "192.168.1.1",
            "172.16.0.1",
            "172.31.255.254",
            "169.254.169.254",
            "169.254.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456:789a::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!is_safe_ip(ip), "{ip} must be rejected");
        }
    }

    #[test]
    fn is_safe_ip_allows_public_addresses() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "169.255.0.1",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(is_safe_ip(ip), "{ip} must be allowed");
        }
    }

    #[tokio::test]
    async fn extract_rejects_private_destinations_before_network() {
        let client = HttpClient::builder().build().unwrap();
        for url in [
            "http://127.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[fc00::1]/",
            "http://localhost/",
        ] {
            let err = extract(&client, url, 100, None).await.unwrap_err();
            assert!(
                matches!(err.kind(), ErrorKind::InvalidQuery { .. }),
                "{url}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn extract_rejects_non_http_schemes() {
        let client = HttpClient::builder().build().unwrap();
        for url in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
        ] {
            let err = extract(&client, url, 100, None).await.unwrap_err();
            assert!(
                matches!(err.kind(), ErrorKind::InvalidQuery { .. }),
                "{url}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn extract_with_policy_rejects_excluded_initial_target_before_network() {
        let client = HttpClient::builder().build().unwrap();
        let policy = crate::source_policy::SourcePolicy::compile(
            "any",
            std::iter::empty::<&str>(),
            ["blocked.example"],
        )
        .unwrap();
        let err = extract_with_policy(
            &client,
            &policy,
            &crate::source_policy::SourceCatalogue::default(),
            "https://blocked.example/page",
            100,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("source policy"));
    }

    #[test]
    fn source_policy_is_rechecked_for_redirect_destinations() {
        let policy = crate::source_policy::SourcePolicy::compile(
            "any",
            std::iter::empty::<&str>(),
            ["blocked.example"],
        )
        .unwrap();
        let catalogue = crate::source_policy::SourceCatalogue::default();

        // The same pure guard is used immediately before the initial request
        // and before every manually-followed redirect.
        assert!(source_target_allowed(
            "https://public.example/page",
            &policy,
            &catalogue
        ));
        assert!(!source_target_allowed(
            "https://blocked.example/page",
            &policy,
            &catalogue
        ));
        // Source policy does not replace SSRF: an IP literal is source-policy
        // eligible in `any`, then the existing private-IP guard rejects it.
        assert!(source_target_allowed(
            "http://127.0.0.1/private",
            &policy,
            &catalogue
        ));
        assert!(!is_safe_ip("127.0.0.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn fetch_spy_rejects_excluded_redirect_before_next_hop() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let proxy_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let proxy_url = format!("http://{}", proxy_listener.local_addr().unwrap());
        let proxy_requests = Arc::new(Mutex::new(Vec::new()));
        let proxy = tokio::spawn(proxy_spy(proxy_listener, Arc::clone(&proxy_requests)));
        let _proxy_env = ProxyEnvGuard::force_proxy(&proxy_url);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let excluded_url = format!("http://excluded.example.test:{port}/blocked");
        let spy = tokio::spawn(redirect_spy(
            listener,
            excluded_url.clone(),
            Arc::clone(&requests),
        ));

        let client = HttpClient::builder()
            .resolve_for_test(
                "allowed.example.test",
                (Ipv4Addr::LOCALHOST, port).into(),
                "93.184.216.34".parse::<IpAddr>().unwrap(),
            )
            .resolve_for_test(
                "excluded.example.test",
                (Ipv4Addr::LOCALHOST, port).into(),
                "93.184.216.34".parse::<IpAddr>().unwrap(),
            )
            .build()
            .unwrap();
        let policy = crate::source_policy::SourcePolicy::compile(
            "any",
            std::iter::empty::<&str>(),
            ["excluded.example.test"],
        )
        .unwrap();

        let result = extract_with_policy(
            &client,
            &policy,
            &crate::source_policy::SourceCatalogue::default(),
            &format!("http://allowed.example.test:{port}/start"),
            100,
            None,
        )
        .await;

        spy.await.unwrap();
        let requests = requests.lock().unwrap().clone();
        assert!(
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("source policy")),
            "expected source-policy rejection, got {result:?}"
        );
        assert_eq!(requests.len(), 1, "recorded requests: {requests:?}");
        assert!(
            requests[0].contains("GET /start "),
            "request: {}",
            requests[0]
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains(&format!("host: allowed.example.test:{port}")),
            "request: {}",
            requests[0]
        );
        assert!(
            requests.iter().all(|request| !request.contains("/blocked")),
            "excluded redirect was requested: {requests:?}"
        );
        proxy.abort();
        let _ = proxy.await;
        let proxy_requests = proxy_requests.lock().unwrap().clone();
        assert!(
            proxy_requests.is_empty(),
            "system proxy intercepted the loopback fixture: {:?}",
            proxy_requests
        );
    }

    #[tokio::test]
    async fn fetch_spy_still_sees_no_request_for_target_policy_or_private_ip_denials() {
        for (url, target_policy, expected_error, map_host) in [
            (
                "http://denied.example.test/target-policy".to_string(),
                crate::client::TargetPolicy {
                    allowed: Vec::new(),
                    denied: vec!["denied.example.test".into()],
                },
                "domain allow/deny policy",
                Some("denied.example.test"),
            ),
            (
                "http://127.0.0.1/private".to_string(),
                crate::client::TargetPolicy::default(),
                "SSRF blocked",
                None,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let spy = tokio::spawn(redirect_spy(
                listener,
                format!("http://unused.example.test:{port}/unused"),
                Arc::clone(&requests),
            ));
            let mut builder = HttpClient::builder().target_policy(target_policy);
            if let Some(host) = map_host {
                builder = builder.resolve_for_test(
                    host,
                    (Ipv4Addr::LOCALHOST, port).into(),
                    "93.184.216.34".parse::<IpAddr>().unwrap(),
                );
            }
            let client = builder.build().unwrap();
            let result = extract_with_policy(
                &client,
                &crate::source_policy::SourcePolicy::default(),
                &crate::source_policy::SourceCatalogue::default(),
                &url,
                100,
                None,
            )
            .await;

            spy.abort();
            let _ = spy.await;
            let requests = requests.lock().unwrap();
            assert!(
                result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.to_string().contains(expected_error)),
                "{url}: expected {expected_error}, got {result:?}"
            );
            assert!(requests.is_empty(), "{url} made requests: {requests:?}");
        }
    }
}
