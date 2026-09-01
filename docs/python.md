# Python bindings reference

`phrona` Python package built from `crates/phrona-python` with pyo3
0.29. Python 3.9-3.14.

## Building the wheel

```bash
uv build                       # needs [build-system] setuptools-rust in pyproject.toml
uv pip install dist/phrona-*.whl --python <python3.12 venv>
```

The wheel tags the platform (`cp312-cp312-linux_x86_64` etc.). On other
platforms use `cargo build --release -p phrona-python` and copy the
cdylib as `phrona.so` into a venv site-packages.

## API

```python
import phrona

phrona.version()                # "0.2.0"

phrona.engines("web")           # {'web': ["duckduckgo","google",...]}
phrona.engines()                # {'web': [...], 'images': [...], 'news': [...],
                                    #  'videos': [...], 'books': [...]}

phrona.search("rust", engines=["bing", "brave"], max_results=10)
# {'query': 'rust', 'category': 'web', 'page': 1, 'total': 8,
#  'results': [{'type': 'web', 'title': ..., 'url': ..., 'description': ...,
#               'score': ..., 'position': ..., 'engines': [...]}],
#  'suggestions': [...], 'answer': None,
#  'engines': [{'name': 'bing', 'status': 'ok', 'results': 10}],
#  'elapsed_ms': 1200}

phrona.suggest("rus", source="bing")
# {'query': 'rus', 'source': 'bing', 'suggestions': ['rust', 'rustup', ...]}

phrona.suggest("rus")           # source=None: per-source map
# {'query': 'rus', 'suggestions': {'bing': [...], 'google': [...], ...}}

phrona.extract("https://example.com", max_chars=5000, query="hello")
# {'title': ..., 'description': ..., 'text': ..., 'images': [...]}
```

### Client class

```python
client = phrona.Client(profile="chrome", timeout=20)
client.search("rust", category="web", engines=None, page=1, max_results=20,
              safesearch="moderate", region=None, language=None,
               time_range=None, filters=None, source_policy_mode="any",
               allowed_domains=None, excluded_domains=None)
client.suggest("rus", source=None, region="us-en")
# {'query': 'rus', 'suggestions': {'bing': [...], 'google': [...], ...}}
client.extract("https://example.com", max_chars=8000, query=None)
client.engines("news")
```

All result values are plain Python dicts/lists/str/int/float - no wrapper
objects, JSON-serializable by construction. `extract` uses manual
serialization of `ExtractedPage` (pyo3 class specialization).

`search` keyword arguments mirror `SearchOptions`; source policy arguments use
the same mode/domain semantics as REST and MCP. `engines=None` means all
engines of the category. `profile` accepts "chrome", "firefox", "edge",
"safari", "opera", "okhttp" (or a numeric profile). `timeout` is seconds.
Invalid enum values (`safesearch`, `time_range`, `category`, `profile`)
raise `ValueError`.
