//! Local source eligibility and operator-owned authority classification.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use url::{Host, Url};

/// A validated, canonical DNS domain.
///
/// The value is lowercase IDNA ASCII with at most one terminal dot removed.
/// It can therefore be used for exact or dot-boundary subdomain matching
/// without accidentally matching hosts such as `example.com.evil`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedDomain(String);

impl NormalizedDomain {
    /// Parse and canonicalize a hostname-only domain.
    pub fn parse(input: &str) -> Result<Self, SourcePolicyError> {
        let input = input.trim();
        if input.is_empty()
            || input.contains("://")
            || input.contains('/')
            || input.contains('?')
            || input.contains('#')
            || input.contains('@')
            || input.contains(':')
            || input.contains('*')
        {
            return Err(SourcePolicyError::invalid_domain(input));
        }

        let url = Url::parse(&format!("http://{input}/"))
            .map_err(|_| SourcePolicyError::invalid_domain(input))?;
        if url.username() != ""
            || url.password().is_some()
            || url.port().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SourcePolicyError::invalid_domain(input));
        }

        let Some(Host::Domain(host)) = url.host() else {
            return Err(SourcePolicyError::invalid_domain(input));
        };
        Self::from_canonical_host(host)
    }

    fn from_canonical_host(host: &str) -> Result<Self, SourcePolicyError> {
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty() || is_public_suffix(host) {
            return Err(SourcePolicyError::invalid_domain(host));
        }
        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2
            || labels.iter().any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || label.len() > 63
            })
            || host.len() > 253
        {
            return Err(SourcePolicyError::invalid_domain(host));
        }
        Ok(Self(host.to_ascii_lowercase()))
    }

    /// The canonical ASCII representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the canonical ASCII domain.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Whether this domain matches a host exactly or at a dot boundary.
    pub fn matches_host(&self, host: &str) -> bool {
        let Ok(host) = canonicalize_host(host) else {
            return false;
        };
        host == self.0 || host.ends_with(&format!(".{}", self.0))
    }
}

impl AsRef<str> for NormalizedDomain {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for NormalizedDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for NormalizedDomain {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NormalizedDomain {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for NormalizedDomain {
    type Err = SourcePolicyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Source authority assigned by the operator catalogue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceTier {
    /// The host is in the operator's official catalogue.
    Official,
    /// The host is in the operator's reputable-secondary catalogue.
    Secondary,
    /// The host is not present in either catalogue.
    #[default]
    Unknown,
}

/// Explain why a source was accepted or rejected by a policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyReason {
    /// The source satisfies the selected policy.
    #[default]
    Allowed,
    /// The source matched the request exclusion list.
    Excluded,
    /// The selected mode requires a requested source and it was not requested.
    NotAllowed,
    /// The selected mode requires operator authority and the source lacks it.
    NotOfficial,
}

/// The four local source-admission modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceMode {
    /// Admit every valid, non-excluded source.
    #[default]
    Any,
    /// Admit every valid, non-excluded source; ranking may prefer authority.
    PreferOfficial,
    /// Admit requested sources or operator-curated secondary sources.
    RequireAllowed,
    /// Admit only operator-curated official sources.
    OfficialOnly,
}

impl SourceMode {
    /// Stable wire representation used by adapters and result metadata.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::PreferOfficial => "prefer-official",
            Self::RequireAllowed => "require-allowed",
            Self::OfficialOnly => "official-only",
        }
    }
}

impl AsRef<str> for SourceMode {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SourceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SourceMode {
    type Err = SourcePolicyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "any" => Ok(Self::Any),
            "prefer-official" => Ok(Self::PreferOfficial),
            "require-allowed" => Ok(Self::RequireAllowed),
            "official-only" => Ok(Self::OfficialOnly),
            _ => Err(SourcePolicyError::InvalidMode(s.to_string())),
        }
    }
}

/// A validation error raised before a search can execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourcePolicyError {
    /// The request supplied an unknown source mode.
    InvalidMode(String),
    /// A policy or catalogue domain was not a valid hostname.
    InvalidDomain(String),
    /// `require-allowed` cannot operate without a request scope.
    EmptyAllowed,
    /// A source URL has no valid host.
    InvalidUrl(String),
}

impl SourcePolicyError {
    fn invalid_domain(input: &str) -> Self {
        Self::InvalidDomain(input.to_string())
    }
}

impl fmt::Display for SourcePolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMode(mode) => write!(f, "unknown source policy mode: {mode}"),
            Self::InvalidDomain(domain) => write!(f, "invalid source domain: {domain}"),
            Self::EmptyAllowed => {
                f.write_str("require-allowed needs a non-empty allowed domain list")
            }
            Self::InvalidUrl(url) => write!(f, "source URL has no valid host: {url}"),
        }
    }
}

impl std::error::Error for SourcePolicyError {}

/// An operator-owned set of validated domains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainSet(BTreeSet<NormalizedDomain>);

impl DomainSet {
    /// Compile a domain set from hostname-only strings.
    pub fn compile<I, S>(domains: I) -> Result<Self, SourcePolicyError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        domains
            .into_iter()
            .map(|domain| NormalizedDomain::parse(domain.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()
            .map(Self)
    }

    fn matches_host(&self, host: &str) -> bool {
        self.0.iter().any(|domain| domain.matches_host(host))
    }

    /// Number of canonical domains in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no domains were configured.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The immutable, operator-owned authority catalogue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceCatalogue {
    official: DomainSet,
    secondary: DomainSet,
}

impl SourceCatalogue {
    /// Compile official and secondary hostname lists from operator config.
    pub fn compile<I, J, A, B>(official: I, secondary: J) -> Result<Self, SourcePolicyError>
    where
        I: IntoIterator<Item = A>,
        J: IntoIterator<Item = B>,
        A: AsRef<str>,
        B: AsRef<str>,
    {
        Ok(Self {
            official: DomainSet::compile(official)?,
            secondary: DomainSet::compile(secondary)?,
        })
    }

    /// Classify a host using official precedence when lists overlap.
    pub fn classify_host(&self, host: &str) -> SourceTier {
        if self.official.matches_host(host) {
            SourceTier::Official
        } else if self.secondary.matches_host(host) {
            SourceTier::Secondary
        } else {
            SourceTier::Unknown
        }
    }

    /// The official domain set.
    pub fn official(&self) -> &DomainSet {
        &self.official
    }

    /// The secondary domain set.
    pub fn secondary(&self) -> &DomainSet {
        &self.secondary
    }
}

/// Immutable caller request scope and admission mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourcePolicy {
    mode: SourceMode,
    allowed: Vec<NormalizedDomain>,
    denied: Vec<NormalizedDomain>,
}

impl<'de> Deserialize<'de> for SourcePolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SerializedPolicy {
            mode: SourceMode,
            allowed: Vec<String>,
            denied: Vec<String>,
        }

        let policy = SerializedPolicy::deserialize(deserializer)?;
        Self::new(policy.mode, policy.allowed, policy.denied).map_err(serde::de::Error::custom)
    }
}

impl SourcePolicy {
    /// Compile a request policy from its adapter-friendly string mode and
    /// hostname lists. Validation is entirely local.
    pub fn compile<M, I, J, A, B>(mode: M, allowed: I, denied: J) -> Result<Self, SourcePolicyError>
    where
        M: AsRef<str>,
        I: IntoIterator<Item = A>,
        J: IntoIterator<Item = B>,
        A: AsRef<str>,
        B: AsRef<str>,
    {
        let mode = mode.as_ref().parse::<SourceMode>()?;
        let allowed = compile_list(allowed)?;
        let denied = compile_list(denied)?;
        if mode == SourceMode::RequireAllowed && allowed.is_empty() {
            return Err(SourcePolicyError::EmptyAllowed);
        }
        Ok(Self {
            mode,
            allowed,
            denied,
        })
    }

    /// Compile a policy from an already-parsed mode.
    pub fn new<I, J, A, B>(
        mode: SourceMode,
        allowed: I,
        denied: J,
    ) -> Result<Self, SourcePolicyError>
    where
        I: IntoIterator<Item = A>,
        J: IntoIterator<Item = B>,
        A: AsRef<str>,
        B: AsRef<str>,
    {
        Self::compile(mode.as_str(), allowed, denied)
    }

    /// The selected admission mode.
    pub fn mode(&self) -> SourceMode {
        self.mode
    }

    /// Canonical caller-requested domains.
    pub fn allowed(&self) -> &[NormalizedDomain] {
        &self.allowed
    }

    /// Canonical caller-excluded domains.
    pub fn denied(&self) -> &[NormalizedDomain] {
        &self.denied
    }

    /// Assess a source URL against this request and the operator catalogue.
    pub fn assessment_for_url(
        &self,
        url: &str,
        catalogue: &SourceCatalogue,
    ) -> Result<SourceAssessment, SourcePolicyError> {
        if let Some((scheme, authority)) = url.split_once("://")
            && matches!(scheme, "http" | "https")
            && authority.starts_with('/')
        {
            return Err(SourcePolicyError::InvalidUrl(url.to_string()));
        }
        let parsed = Url::parse(url).map_err(|_| SourcePolicyError::InvalidUrl(url.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || url.contains('@')
            || parsed.username() != ""
            || parsed.password().is_some()
            || parsed.host().is_none()
        {
            return Err(SourcePolicyError::InvalidUrl(url.to_string()));
        }
        match parsed.host() {
            Some(Host::Domain(host)) => match self.assessment_for_host(host, catalogue) {
                Ok(assessment) => Ok(assessment),
                // A single-label URL host (for example, `localhost`) is a
                // valid input for the legacy extractor. In permissive modes,
                // leave it to the existing TargetPolicy/SSRF guard rather
                // than changing that error path into a source-policy error.
                Err(_error)
                    if matches!(self.mode, SourceMode::Any | SourceMode::PreferOfficial)
                        && !host.strip_suffix('.').unwrap_or(host).contains('.') =>
                {
                    Ok(self.assess_parts(false, false, SourceTier::Unknown))
                }
                Err(error) => Err(error),
            },
            // IP literals are valid URL hosts, but cannot receive catalogue
            // authority or match hostname scope. The unchanged TargetPolicy
            // and SSRF checks still get to reject unsafe literals afterward.
            Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
                Ok(self.assess_parts(false, false, SourceTier::Unknown))
            }
            None => Err(SourcePolicyError::InvalidUrl(url.to_string())),
        }
    }

    /// Assess a canonicalizable host without performing any lookup.
    pub fn assessment_for_host(
        &self,
        host: &str,
        catalogue: &SourceCatalogue,
    ) -> Result<SourceAssessment, SourcePolicyError> {
        let host =
            canonicalize_host(host).map_err(|_| SourcePolicyError::InvalidUrl(host.to_string()))?;
        let source_tier = catalogue.classify_host(&host);
        let requested_match = self.allowed.iter().any(|domain| domain.matches_host(&host));
        let excluded = self.denied.iter().any(|domain| domain.matches_host(&host));
        Ok(self.assess_parts(requested_match, excluded, source_tier))
    }

    fn assess_parts(
        &self,
        requested_match: bool,
        excluded: bool,
        source_tier: SourceTier,
    ) -> SourceAssessment {
        let reason = if excluded {
            PolicyReason::Excluded
        } else {
            match self.mode {
                SourceMode::Any | SourceMode::PreferOfficial => PolicyReason::Allowed,
                SourceMode::RequireAllowed
                    if requested_match || source_tier == SourceTier::Secondary =>
                {
                    PolicyReason::Allowed
                }
                SourceMode::RequireAllowed => PolicyReason::NotAllowed,
                SourceMode::OfficialOnly if source_tier != SourceTier::Official => {
                    PolicyReason::NotOfficial
                }
                SourceMode::OfficialOnly if self.allowed.is_empty() || requested_match => {
                    PolicyReason::Allowed
                }
                SourceMode::OfficialOnly => PolicyReason::NotAllowed,
            }
        };
        SourceAssessment {
            requested_match,
            source_tier,
            reason,
        }
    }

    /// Whether a URL is eligible under this policy.
    pub fn evaluate_url(
        &self,
        url: &str,
        catalogue: &SourceCatalogue,
    ) -> Result<bool, SourcePolicyError> {
        Ok(self.assessment_for_url(url, catalogue)?.allowed())
    }

    /// Alias for [`Self::evaluate_url`] with a predicate-oriented name.
    pub fn allows_url(
        &self,
        url: &str,
        catalogue: &SourceCatalogue,
    ) -> Result<bool, SourcePolicyError> {
        self.evaluate_url(url, catalogue)
    }
}

fn compile_list<I, S>(domains: I) -> Result<Vec<NormalizedDomain>, SourcePolicyError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    domains
        .into_iter()
        .map(|domain| NormalizedDomain::parse(domain.as_ref()))
        .collect()
}

/// The result metadata attached to every eligible result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SourceAssessment {
    /// Whether the host matched the caller's requested scope.
    pub requested_match: bool,
    /// Authority independently assigned by the operator catalogue.
    pub source_tier: SourceTier,
    /// The local policy decision explanation.
    pub reason: PolicyReason,
}

impl SourceAssessment {
    /// Whether this assessment admits the source.
    pub fn allowed(&self) -> bool {
        self.reason == PolicyReason::Allowed
    }
}

fn canonicalize_host(host: &str) -> Result<String, SourcePolicyError> {
    let host = host.trim();
    if host.is_empty()
        || host.contains('/')
        || host.contains('?')
        || host.contains('#')
        || host.contains('@')
        || host.contains(':')
        || host.contains('*')
    {
        return Err(SourcePolicyError::invalid_domain(host));
    }
    let url = Url::parse(&format!("http://{host}/"))
        .map_err(|_| SourcePolicyError::invalid_domain(host))?;
    if url.username() != "" || url.password().is_some() || url.port().is_some() {
        return Err(SourcePolicyError::invalid_domain(host));
    }
    let Some(Host::Domain(host)) = url.host() else {
        return Err(SourcePolicyError::invalid_domain(host));
    };
    Ok(NormalizedDomain::from_canonical_host(host)?.into_string())
}

// `psl` embeds Mozilla's Public Suffix List as generated Rust code. Comparing
// the complete suffix with the complete hostname rejects both ICANN and
// private bare suffixes, while retaining the registrable name immediately
// above the suffix. There is no runtime file, network, or DNS lookup.
fn is_public_suffix(host: &str) -> bool {
    psl::suffix_str(host).is_some_and(|suffix| suffix == host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue() -> SourceCatalogue {
        SourceCatalogue::compile(["Example.COM."], ["secondary.example", "bücher.example"]).unwrap()
    }

    #[test]
    fn normalizes_case_terminal_dot_and_idn() {
        assert_eq!(
            NormalizedDomain::parse(" API.Example.COM. ")
                .unwrap()
                .as_str(),
            "api.example.com"
        );
        assert_eq!(
            NormalizedDomain::parse("bücher.example").unwrap().as_str(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn rejects_non_hostname_and_public_suffix_inputs() {
        for domain in [
            "https://example.com",
            "example.com/path",
            "example.com:443",
            "user@example.com",
            "*.example.com",
            "example.*",
            "127.0.0.1",
            "[::1]",
            "com",
            "co.uk",
            "example..com",
            "example.com..",
        ] {
            assert!(
                NormalizedDomain::parse(domain).is_err(),
                "accepted {domain}"
            );
        }
    }

    #[test]
    fn matches_exact_and_dot_boundary_only() {
        let domain = NormalizedDomain::parse("example.com").unwrap();
        assert!(domain.matches_host("API.Example.COM."));
        assert!(domain.matches_host("deep.api.example.com"));
        assert!(!domain.matches_host("official.com.evil"));
        assert!(!domain.matches_host("notexample.com"));
    }

    #[test]
    fn exclusions_win_and_scope_is_independent_from_authority() {
        let policy = SourcePolicy::compile(
            "require-allowed",
            ["docs.example.com", "unknown.example"],
            ["private.docs.example.com"],
        )
        .unwrap();
        let catalogue = catalogue();

        let requested_official = policy
            .assessment_for_url("https://docs.example.com/guide", &catalogue)
            .unwrap();
        assert!(requested_official.allowed());
        assert!(requested_official.requested_match);
        assert_eq!(requested_official.source_tier, SourceTier::Official);
        assert_eq!(requested_official.reason, PolicyReason::Allowed);

        let requested_unknown = policy
            .assessment_for_url("https://unknown.example/page", &catalogue)
            .unwrap();
        assert!(requested_unknown.allowed());
        assert!(requested_unknown.requested_match);
        assert_eq!(requested_unknown.source_tier, SourceTier::Unknown);

        let excluded = policy
            .assessment_for_url("https://private.docs.example.com/page", &catalogue)
            .unwrap();
        assert!(!excluded.allowed());
        assert_eq!(excluded.reason, PolicyReason::Excluded);
    }

    #[test]
    fn mode_matrix_preserves_authority_and_scope() {
        let catalogue = catalogue();
        let cases = [
            (
                "any",
                "https://unknown.example/a",
                true,
                SourceTier::Unknown,
            ),
            (
                "prefer-official",
                "https://unknown.example/a",
                true,
                SourceTier::Unknown,
            ),
            (
                "require-allowed",
                "https://secondary.example/a",
                true,
                SourceTier::Secondary,
            ),
            (
                "official-only",
                "https://secondary.example/a",
                false,
                SourceTier::Secondary,
            ),
        ];
        for (mode, url, expected_allowed, expected_tier) in cases {
            let allowed = if mode == "require-allowed" {
                vec!["unknown.example"]
            } else {
                Vec::new()
            };
            let policy = SourcePolicy::compile(mode, allowed, std::iter::empty::<&str>())
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
            let assessment = policy.assessment_for_url(url, &catalogue).unwrap();
            assert_eq!(assessment.allowed(), expected_allowed, "mode={mode}");
            assert_eq!(assessment.source_tier, expected_tier, "mode={mode}");
            assert!(
                !assessment.requested_match,
                "catalogue authority must not imply request scope"
            );
        }
    }

    #[test]
    fn invalid_modes_and_empty_required_scope_fail_validation() {
        assert!(
            SourcePolicy::compile(
                "operator-only",
                std::iter::empty::<&str>(),
                std::iter::empty::<&str>(),
            )
            .is_err()
        );
        assert!(
            SourcePolicy::compile(
                "require-allowed",
                std::iter::empty::<&str>(),
                std::iter::empty::<&str>(),
            )
            .is_err()
        );
    }

    #[test]
    fn requested_scope_never_grants_official_authority() {
        let policy = SourcePolicy::compile(
            "official-only",
            ["uncatalogued.example"],
            std::iter::empty::<&str>(),
        )
        .unwrap();
        let assessment = policy
            .assessment_for_url(
                "https://uncatalogued.example/page",
                &SourceCatalogue::default(),
            )
            .unwrap();
        assert!(assessment.requested_match);
        assert_eq!(assessment.source_tier, SourceTier::Unknown);
        assert_eq!(assessment.reason, PolicyReason::NotOfficial);
        assert!(!assessment.allowed());
    }

    #[test]
    fn permissive_modes_leave_single_label_hosts_to_the_ssrf_guard() {
        let policy = SourcePolicy::default();
        let assessment = policy
            .assessment_for_url("http://localhost/", &SourceCatalogue::default())
            .unwrap();

        assert!(assessment.allowed());
        assert_eq!(assessment.source_tier, SourceTier::Unknown);
        assert!(!assessment.requested_match);
    }

    #[test]
    fn malformed_or_hostless_urls_are_rejected_locally() {
        let policy = SourcePolicy::default();
        let catalogue = SourceCatalogue::default();
        for url in [
            "not a url",
            "/relative",
            "mailto:user@example.com",
            "https:///missing",
            "https://user:secret@example.com/private",
        ] {
            assert!(
                policy.assessment_for_url(url, &catalogue).is_err(),
                "accepted {url}"
            );
        }
    }

    #[test]
    fn rejects_complete_common_private_and_idn_public_suffixes() {
        for suffix in [
            "blogspot.com",
            "cloudfront.net",
            "github.io",
            "s3.amazonaws.com",
            "公司.cn",
        ] {
            assert!(
                NormalizedDomain::parse(suffix).is_err(),
                "accepted public suffix {suffix}"
            );
        }

        for registrable in [
            "tenant.blogspot.com",
            "tenant.cloudfront.net",
            "tenant.github.io",
            "bucket.s3.amazonaws.com",
            "食狮.公司.cn",
        ] {
            assert!(
                NormalizedDomain::parse(registrable).is_ok(),
                "rejected registrable domain {registrable}"
            );
        }
    }

    #[test]
    fn public_suffix_matching_does_not_overreach_into_bypass_hosts() {
        let catalogue =
            SourceCatalogue::compile(["tenant.github.io"], std::iter::empty::<&str>()).unwrap();
        let policy = SourcePolicy::compile(
            "require-allowed",
            ["tenant.github.io"],
            std::iter::empty::<&str>(),
        )
        .unwrap();

        let assessment = policy
            .assessment_for_url("https://tenant.github.io.evil.example/page", &catalogue)
            .unwrap();
        assert!(!assessment.requested_match);
        assert_eq!(assessment.source_tier, SourceTier::Unknown);
        assert_eq!(assessment.reason, PolicyReason::NotAllowed);
        assert!(!assessment.allowed());
    }
}
