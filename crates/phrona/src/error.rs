//! Structured errors with scope and kind classification.

use std::fmt;
use std::time::Duration;

/// The layer of the stack a failure belongs to. This lets the orchestrator
/// and callers react differently to egress blocks (disable/switch the
/// engine), provider outages (retry later), schema drift (alert the parser)
/// and query problems (fix the request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorScope {
    /// Egress IP blocked, TLS fingerprint flagged, proxy/tunnel failure.
    Egress,
    /// Upstream provider is down, global 5xx, true 429 rate limit.
    Provider,
    /// Upstream DOM or JSON schema mutated; a parser failed.
    Schema,
    /// The request itself is invalid (bad query, no engines, ...).
    Query,
    /// A local/internal failure (I/O, client construction, ...).
    Internal,
}

/// The physical, observable failure behind an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// True HTTP 429 with an optional `Retry-After` hint.
    RateLimited {
        /// Optional seconds to wait before retrying, from `Retry-After`.
        retry_after: Option<Duration>,
    },
    /// Blocked by an anti-bot system.
    Blocked(BlockDetails),
    /// The response deviates from the expected schema and could not be
    /// parsed (DOM/Schema mutation, wrong content type, invalid JSON).
    MalformedPayload {
        /// Static description of the deviation.
        context: &'static str,
    },
    /// Upstream returned a non-2xx error status.
    UpstreamUnavailable {
        /// The HTTP status code returned.
        status: u16,
    },
    /// Every engine failed; the search as a whole produced nothing. Carries
    /// a short per-engine summary (`name: error`) for diagnostics.
    AllProvidersFailed {
        /// Per-engine `name: error` summaries.
        details: Vec<String>,
    },
    /// The request timed out.
    Timeout,
    /// The network failed (connect error, reset, TLS, ...).
    NetworkFailure,
    /// The request is invalid.
    InvalidQuery {
        /// Static description of what is invalid.
        context: &'static str,
    },
    /// A local/internal failure.
    Internal {
        /// Static description of the failure.
        context: &'static str,
    },
}

/// The specific anti-bot system that blocked a request, carried by
/// [`ErrorKind::Blocked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockDetails {
    /// Challenge page served by Cloudflare.
    Cloudflare,
    /// A CAPTCHA was required.
    Captcha,
    /// The egress IP was banned.
    IpBan,
    /// Generic bot detection (rate-based or fingerprinting).
    BotDetection,
}

/// Structured error: the observable failure (`kind`) plus the layer it
/// belongs to (`scope`), the producing `engine`, the HTTP status when
/// known, and an optional static message. Construction and Display are
/// allocation-free on every path except `AllProvidersFailed`, which carries
/// a per-engine diagnostic summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The layer of the stack the failure belongs to.
    pub scope: ErrorScope,
    /// The observable failure.
    pub kind: ErrorKind,
    /// Producing engine, or `"client"` / `"orchestrator"` for non-engine
    /// failures.
    pub engine: &'static str,
    /// HTTP status when the failure carried one.
    pub http_status: Option<u16>,
    /// Optional static message.
    pub message: Option<&'static str>,
}

impl Error {
    /// True 429 (optionally carrying `Retry-After`).
    pub fn rate_limited(engine: &'static str, retry_after: Option<Duration>) -> Self {
        Self {
            scope: ErrorScope::Provider,
            kind: ErrorKind::RateLimited { retry_after },
            engine,
            http_status: Some(429),
            message: None,
        }
    }

    /// Blocked by an anti-bot system.
    pub fn blocked(engine: &'static str, details: BlockDetails) -> Self {
        Self {
            scope: ErrorScope::Egress,
            kind: ErrorKind::Blocked(details),
            engine,
            http_status: None,
            message: None,
        }
    }

    /// The response deviated from the expected schema.
    pub fn schema(engine: &'static str, context: &'static str) -> Self {
        Self {
            scope: ErrorScope::Schema,
            kind: ErrorKind::MalformedPayload { context },
            engine,
            http_status: None,
            message: None,
        }
    }

    /// Upstream returned a non-2xx error status.
    pub fn unavailable(engine: &'static str, status: u16) -> Self {
        Self {
            scope: ErrorScope::Provider,
            kind: ErrorKind::UpstreamUnavailable { status },
            engine,
            http_status: Some(status),
            message: None,
        }
    }

    /// The request timed out.
    pub fn timeout(engine: &'static str) -> Self {
        Self {
            scope: ErrorScope::Egress,
            kind: ErrorKind::Timeout,
            engine,
            http_status: None,
            message: None,
        }
    }

    /// The network failed at the transport level.
    pub fn network(engine: &'static str) -> Self {
        Self {
            scope: ErrorScope::Egress,
            kind: ErrorKind::NetworkFailure,
            engine,
            http_status: None,
            message: None,
        }
    }

    /// The request itself is invalid.
    pub fn invalid_query(engine: &'static str, context: &'static str) -> Self {
        Self {
            scope: ErrorScope::Query,
            kind: ErrorKind::InvalidQuery { context },
            engine,
            http_status: None,
            message: None,
        }
    }

    /// A local/internal failure.
    pub fn internal(engine: &'static str, context: &'static str) -> Self {
        Self {
            scope: ErrorScope::Internal,
            kind: ErrorKind::Internal { context },
            engine,
            http_status: None,
            message: None,
        }
    }

    /// Every engine failed for a search.
    pub fn all_failed(engine: &'static str, details: Vec<String>) -> Self {
        Self {
            scope: ErrorScope::Provider,
            kind: ErrorKind::AllProvidersFailed { details },
            engine,
            http_status: None,
            message: None,
        }
    }

    /// The layer of the stack the error belongs to ([`ErrorScope`]).
    pub fn scope(&self) -> ErrorScope {
        self.scope
    }

    /// The observable failure ([`ErrorKind`]).
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }
}

/// Select the smallest safe `Retry-After` hint from attempted providers.
/// Hints outside the operator-safe 1..=3600 second range are ignored rather
/// than allowing an upstream to impose an unbounded wait.
pub(crate) fn smallest_retry_after<I>(hints: I) -> Option<Duration>
where
    I: IntoIterator<Item = Option<Duration>>,
{
    hints
        .into_iter()
        .flatten()
        .filter(|hint| *hint >= Duration::from_secs(1) && *hint <= Duration::from_secs(3600))
        .min()
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::RateLimited { retry_after } => match retry_after {
                Some(d) => write!(f, "rate limited (retry after {}s)", d.as_secs()),
                None => write!(f, "rate limited"),
            },
            ErrorKind::Blocked(d) => write!(f, "blocked ({d:?})"),
            ErrorKind::MalformedPayload { context } => write!(f, "malformed payload: {context}"),
            ErrorKind::UpstreamUnavailable { status } => {
                write!(f, "upstream unavailable (status {status})")
            }
            ErrorKind::AllProvidersFailed { details } => {
                if details.is_empty() {
                    write!(f, "all search providers failed")
                } else {
                    write!(f, "all search providers failed: {}", details.join("; "))
                }
            }
            ErrorKind::Timeout => write!(f, "timeout"),
            ErrorKind::NetworkFailure => write!(f, "network failure"),
            ErrorKind::InvalidQuery { context } => write!(f, "invalid query: {context}"),
            ErrorKind::Internal { context } => write!(f, "internal error: {context}"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        write!(f, " [scope={:?}, engine={}]", self.scope, self.engine)?;
        if let Some(status) = self.http_status {
            write!(f, " [status={status}]")?;
        }
        if let Some(m) = self.message {
            write!(f, ": {m}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(_e: std::io::Error) -> Self {
        Error::internal("io", "i/o failure")
    }
}

impl From<wreq::Error> for Error {
    fn from(e: wreq::Error) -> Self {
        if e.is_timeout() {
            Error::timeout("client")
        } else if e.is_connect() || e.is_connection_reset() {
            Error::network("client")
        } else if e.is_redirect() || e.is_decode() {
            Error::internal("client", "request failed")
        } else {
            Error::network("client")
        }
    }
}

/// Alias for `Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_and_kind_classify() {
        let e = Error::rate_limited("qwant", Some(Duration::from_secs(30)));
        assert_eq!(e.scope(), ErrorScope::Provider);
        assert_eq!(
            e.kind(),
            &ErrorKind::RateLimited {
                retry_after: Some(Duration::from_secs(30))
            }
        );
        assert_eq!(e.http_status, Some(429));

        let e = Error::blocked("google", BlockDetails::BotDetection);
        assert_eq!(e.scope(), ErrorScope::Egress);
        assert_eq!(e.kind(), &ErrorKind::Blocked(BlockDetails::BotDetection));

        let e = Error::schema("bing", "unexpected content-type");
        assert_eq!(e.scope(), ErrorScope::Schema);

        let e = Error::invalid_query("orchestrator", "no engines");
        assert_eq!(e.scope(), ErrorScope::Query);

        let e = Error::internal("client", "build failed");
        assert_eq!(e.scope(), ErrorScope::Internal);
    }

    #[test]
    fn display_is_readable() {
        let e = Error::unavailable("mojeek", 503);
        let s = e.to_string();
        assert!(s.contains("upstream unavailable"));
        assert!(s.contains("mojeek"));
        assert!(s.contains("503"));
        let e = Error::blocked("google", BlockDetails::Cloudflare);
        assert!(e.to_string().contains("blocked (Cloudflare)"));
    }

    #[test]
    fn retry_after_uses_smallest_valid_bounded_hint() {
        let hints = [
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(2)),
            Some(Duration::from_secs(3601)),
            Some(Duration::from_secs(0)),
            None,
        ];
        assert_eq!(smallest_retry_after(hints), Some(Duration::from_secs(2)));
        assert_eq!(
            smallest_retry_after([
                Some(Duration::from_secs(0)),
                Some(Duration::from_secs(3601))
            ]),
            None
        );
    }
}
