//! Python bindings for the phrona library.
//!
//! ```python
//! import phrona
//! phrona.search("rust programming", engines=["bing", "brave"])
//! phrona.suggest("rus")
//! phrona.extract("https://doc.rust-lang.org/book/")
//! ```

#![warn(missing_docs)]

use std::sync::LazyLock;
use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use phrona_core::{Category, PhronaConfig, Profile, SearchClient, SearchOptions, SourcePolicy};

/// Dedicated multi-threaded runtime for all blocking calls. Network I/O runs
/// on this runtime with the Python GIL released, so Python threads and
/// asyncio loops are never blocked.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
});

/// Resolve an impersonation profile by name via the core library's table
/// (single source of truth; drifts never happen).
fn parse_profile(s: &str) -> PyResult<Profile> {
    Profile::from_name(s).ok_or_else(|| {
        PyValueError::new_err(format!(
            "unknown profile '{s}', expected one of: chrome, chrome100, chrome120, chrome131, chrome140, chrome149, firefox, firefox139, firefox148, safari, edge, opera, opera131, okhttp, random"
        ))
    })
}

fn parse_category(s: &str) -> PyResult<Category> {
    s.parse::<Category>().map_err(|_| {
        PyValueError::new_err("category must be one of: web, images, news, videos, books")
    })
}

fn parse_source_policy(
    mode: &str,
    allowed: Option<Vec<String>>,
    denied: Option<Vec<String>>,
) -> PyResult<SourcePolicy> {
    SourcePolicy::compile(
        mode,
        allowed.unwrap_or_default(),
        denied.unwrap_or_default(),
    )
    .map_err(|e| PyValueError::new_err(format!("invalid source policy: {e}")))
}

#[cfg(test)]
mod source_policy_tests {
    use super::*;

    #[test]
    fn python_policy_arguments_use_core_validation() {
        let policy = parse_source_policy(
            "require-allowed",
            Some(vec!["Docs.Example".into()]),
            Some(vec!["private.docs.example".into()]),
        )
        .unwrap();
        assert_eq!(policy.mode(), phrona_core::SourceMode::RequireAllowed);
        assert_eq!(policy.allowed()[0].as_str(), "docs.example");
        assert_eq!(policy.denied()[0].as_str(), "private.docs.example");
    }

    #[test]
    fn omitted_python_policy_is_any() {
        assert_eq!(
            parse_source_policy("any", None, None).unwrap().mode(),
            phrona_core::SourceMode::Any
        );
    }
}

fn to_py(py: Python<'_>, v: &impl serde::Serialize) -> PyResult<Py<PyAny>> {
    let j =
        serde_json::to_value(v).map_err(|e| PyValueError::new_err(format!("serialize: {e}")))?;
    json_to_py(py, &j)
}

/// Convert a serde_json::Value into the matching Python object.
fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<Py<PyAny>> {
    let o: Py<PyAny> = match v {
        serde_json::Value::Null => py.None(),
        serde_json::Value::Bool(b) => (*b).into_pyobject(py)?.to_owned().into_any().unbind(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any().unbind()
            } else {
                n.as_f64()
                    .unwrap_or(0.0)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind()
            }
        }
        serde_json::Value::String(s) => s.into_pyobject(py)?.into_any().unbind(),
        serde_json::Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any().unbind()
        }
        serde_json::Value::Object(map) => {
            let d = PyDict::new(py);
            for (k, val) in map {
                d.set_item(k, json_to_py(py, val)?)?;
            }
            d.into_any().unbind()
        }
    };
    Ok(o)
}

/// A metasearch client. Safe to share across threads.
#[pyclass]
struct Client {
    client: SearchClient,
}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (profile="chrome", timeout=15.0))]
    fn new(profile: &str, timeout: f64) -> PyResult<Self> {
        let cfg = PhronaConfig::load().map_err(|e| PyValueError::new_err(e.to_string()))?;
        let catalogue = cfg
            .source_catalogue()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let client = SearchClient::with_options(
            parse_profile(profile)?,
            Some(Duration::from_secs_f64(timeout.max(1.0))),
            None,
            phrona_core::TargetPolicy::from_security(&cfg.security),
        )
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(Self {
            client: client.with_source_catalogue(catalogue),
        })
    }

    /// Search all engines for a query. Returns a dict with results, engines
    /// report, suggestions and elapsed time.
    #[pyo3(signature = (query, category="web", engines=None, page=1, max_results=20,
                        safesearch="moderate", region=None, language=None,
                        time_range=None, filters=None, source_policy_mode="any",
                        allowed_domains=None, excluded_domains=None))]
    #[allow(clippy::too_many_arguments)]
    fn search(
        &self,
        py: Python<'_>,
        query: &str,
        category: &str,
        engines: Option<Vec<String>>,
        page: u32,
        max_results: usize,
        safesearch: &str,
        region: Option<String>,
        language: Option<String>,
        time_range: Option<String>,
        filters: Option<String>,
        source_policy_mode: &str,
        allowed_domains: Option<Vec<String>>,
        excluded_domains: Option<Vec<String>>,
    ) -> PyResult<Py<PyAny>> {
        let mut opts = SearchOptions::new(query);
        opts.category = parse_category(category)?;
        opts.engines = engines.unwrap_or_default();
        opts.page = page.max(1);
        opts.max_results = max_results.clamp(1, 200);
        opts.safesearch = safesearch.parse::<phrona_core::SafeSearch>().map_err(|_| {
            PyValueError::new_err("safesearch must be one of: off, moderate, strict")
        })?;
        opts.region = region;
        opts.language = language;
        opts.time_range = time_range
            .map(|t| {
                t.parse::<phrona_core::TimeRange>().map_err(|_| {
                    PyValueError::new_err("time_range must be one of: day, week, month, year")
                })
            })
            .transpose()?;
        opts.filters = filters;
        opts.source_policy =
            parse_source_policy(source_policy_mode, allowed_domains, excluded_domains)?;
        let resp = py
            .detach(|| {
                RUNTIME
                    .block_on(self.client.search(opts))
                    .map_err(|e| e.to_string())
            })
            .map_err(PyValueError::new_err)?;
        to_py(py, &resp)
    }

    /// Query suggestions. source: duckduckgo, google, bing, brave, startpage,
    /// qwant or wikipedia. None returns all sources.
    #[pyo3(signature = (query, source=None, region="us-en"))]
    fn suggest(
        &self,
        py: Python<'_>,
        query: &str,
        source: Option<String>,
        region: &str,
    ) -> PyResult<Py<PyAny>> {
        let http = self.client.http();
        let value = py
            .detach(|| -> Result<serde_json::Value, String> {
                match source {
                    Some(name) => {
                        let s = phrona_core::SuggestSource::from_name(&name).ok_or_else(|| {
                            format!(
                                "unknown source '{name}', expected one of: {}",
                                phrona_core::SuggestSource::ALL
                                    .iter()
                                    .map(|s| s.name())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        })?;
                        let list = RUNTIME
                            .block_on(phrona_core::suggest(http, s, query, region))
                            .map_err(|e| e.to_string())?;
                        Ok(serde_json::json!({"query": query, "source": name, "suggestions": list}))
                    }
                    None => {
                        let all = RUNTIME.block_on(phrona_core::suggest_all(http, query, region));
                        let map: serde_json::Map<String, serde_json::Value> = all
                            .into_iter()
                            .map(|(s, list)| (s.name().to_string(), serde_json::json!(list)))
                            .collect();
                        Ok(serde_json::json!({"query": query, "suggestions": map}))
                    }
                }
            })
            .map_err(PyValueError::new_err)?;
        to_py(py, &value)
    }

    /// Fetch a URL and extract its readable main content (AI grounding).
    // Keep the additive Python keyword arguments in the public method
    // signature rather than hiding them behind a breaking wrapper.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (url, max_chars=8000, query=None, source_policy_mode="any",
                        allowed_domains=None, excluded_domains=None))]
    fn extract(
        &self,
        py: Python<'_>,
        url: &str,
        max_chars: usize,
        query: Option<&str>,
        source_policy_mode: &str,
        allowed_domains: Option<Vec<String>>,
        excluded_domains: Option<Vec<String>>,
    ) -> PyResult<Py<PyAny>> {
        let policy = parse_source_policy(source_policy_mode, allowed_domains, excluded_domains)?;
        let page = py
            .detach(|| {
                RUNTIME
                    .block_on(phrona_core::extract_with_policy(
                        self.client.http(),
                        &policy,
                        self.client.source_catalogue(),
                        url,
                        max_chars,
                        query,
                    ))
                    .map_err(|e| e.to_string())
            })
            .map_err(PyValueError::new_err)?;
        to_py(py, &page)
    }

    /// List available engines per category.
    #[pyo3(signature = (category=None))]
    fn engines(&self, py: Python<'_>, category: Option<String>) -> PyResult<Py<PyAny>> {
        let out = py.detach(|| {
            RUNTIME.block_on(async {
                let mut out = serde_json::Map::new();
                let cats: Vec<Category> = match category {
                    Some(c) => vec![parse_category(&c)?],
                    None => Category::ALL.to_vec(),
                };
                for cat in cats {
                    let names: Vec<String> = phrona_core::available_engines(cat)
                        .iter()
                        .map(|e| e.name.clone())
                        .collect();
                    out.insert(cat.as_str().to_string(), serde_json::json!(names));
                }
                Ok::<_, PyErr>(serde_json::Value::Object(out))
            })
        });
        to_py(py, &out?)
    }
}

fn build_client(profile: &str, timeout: f64) -> PyResult<Client> {
    Client::new(profile, timeout)
}

/// One-shot search with a default client. Same parameters as Client.search.
#[pyfunction]
#[pyo3(signature = (query, category="web", engines=None, page=1, max_results=20,
                     safesearch="moderate", region=None, language=None,
                     time_range=None, filters=None, profile="chrome", timeout=15.0,
                     source_policy_mode="any", allowed_domains=None, excluded_domains=None))]
#[allow(clippy::too_many_arguments)]
fn search(
    py: Python<'_>,
    query: &str,
    category: &str,
    engines: Option<Vec<String>>,
    page: u32,
    max_results: usize,
    safesearch: &str,
    region: Option<String>,
    language: Option<String>,
    time_range: Option<String>,
    filters: Option<String>,
    profile: &str,
    timeout: f64,
    source_policy_mode: &str,
    allowed_domains: Option<Vec<String>>,
    excluded_domains: Option<Vec<String>>,
) -> PyResult<Py<PyAny>> {
    let client = build_client(profile, timeout)?;
    client.search(
        py,
        query,
        category,
        engines,
        page,
        max_results,
        safesearch,
        region,
        language,
        time_range,
        filters,
        source_policy_mode,
        allowed_domains,
        excluded_domains,
    )
}

/// One-shot suggestions with a default client.
#[pyfunction]
#[pyo3(signature = (query, source=None, region="us-en"))]
fn suggest(
    py: Python<'_>,
    query: &str,
    source: Option<String>,
    region: &str,
) -> PyResult<Py<PyAny>> {
    build_client("chrome", 15.0)?.suggest(py, query, source, region)
}

/// One-shot page extraction with a default client.
#[pyfunction]
#[pyo3(signature = (url, max_chars=8000, query=None, source_policy_mode="any",
                    allowed_domains=None, excluded_domains=None))]
fn extract(
    py: Python<'_>,
    url: &str,
    max_chars: usize,
    query: Option<&str>,
    source_policy_mode: &str,
    allowed_domains: Option<Vec<String>>,
    excluded_domains: Option<Vec<String>>,
) -> PyResult<Py<PyAny>> {
    build_client("chrome", 15.0)?.extract(
        py,
        url,
        max_chars,
        query,
        source_policy_mode,
        allowed_domains,
        excluded_domains,
    )
}

/// One-shot engines listing with a default client.
#[pyfunction]
#[pyo3(signature = (category=None))]
fn engines(py: Python<'_>, category: Option<String>) -> PyResult<Py<PyAny>> {
    build_client("chrome", 15.0)?.engines(py, category)
}

#[pyfunction]
fn version() -> String {
    phrona_core::version().to_string()
}

#[pymodule]
fn phrona(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    m.add_function(wrap_pyfunction!(search, m)?)?;
    m.add_function(wrap_pyfunction!(suggest, m)?)?;
    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(engines, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
