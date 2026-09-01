# REST API reference

`cargo run -p phrona-api` (or `phrona serve`, or `phrona serve --no-mcp` for
the REST server only).

Serves the web frontend at `/`: a single page with a Search tab
(full-parameter search) and a Tools tab (suggest, extract, ground,
engines, test - every capability from the browser). JSON API at
`/v1/*`. CORS is permissive (access-control-allow-origin: *), so the API
can be called from any browser origin. Responses are JSON, all timings in
milliseconds.

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `PHRONA_ADDR` | `127.0.0.1:8080` | bind address |
| `PHRONA_API_KEY` | unset | if set, protected `/v1/*` data routes and `/search` require a key |
| `RUST_LOG` | `info` | tracing level (use `debug` for detail) |

When a key is set, clients authenticate protected GET routes with either
`x-api-key: <key>` or `Authorization: Bearer <key>`. A GET query-string
credential such as `api_key=...` is rejected with HTTP 400 and never
authenticates. JSON-body `api_key` credentials are supported only on the POST
routes listed below; those routes also accept either authentication header.

| Route/method | Authentication |
| --- | --- |
| `GET /v1/search`, `/v1/suggest`, `/v1/test`, `/v1/extract`, `/v1/grounding` | `x-api-key` or `Authorization: Bearer`; query-string `api_key` returns HTTP 400 |
| `POST /search`, `/v1/tavily`, `/v1/extract`, `/v1/grounding` | JSON-body `api_key` or either authentication header |
| `GET /health`, `GET /v1/engines`, `GET /metrics`, frontend routes | public; no authentication |

## GET /

The web frontend (single page: Search + Tools tabs). See
[docs/frontend.md](frontend.md).

## GET /health

```json
{"status":"ok","version":"0.2.0","engines":{"web":12,"images":6,"news":4,"videos":3,"books":1},"uptime_s":42,"auth":false}
```

No auth required.

## GET /v1/engines

Optional `category` query parameter (`web | images | news | videos |
books`; default: all categories). Returns a map of category to engine
names, in priority order:

```json
{
  "web": ["duckduckgo", "google", "bing", "brave", "mojeek", "yahoo", "yandex", "startpage", "qwant", "marginalia", "wikipedia", "grokipedia"],
  "images": ["duckduckgo_images", "bing_images", "brave_images", "startpage_images", "mojeek_images", "google_images"],
  "news": ["duckduckgo_news", "bing_news", "yahoo_news", "brave_news"],
  "videos": ["duckduckgo_videos", "bing_videos", "brave_videos"],
  "books": ["annas_archive"]
}
```

Public - no auth required (same as `/health`; the frontend uses it
without a key).

## GET /v1/search

Query parameters:

| Param | Type | Default | Meaning |
| --- | --- | --- | --- |
| `q` | string | required | the query |
| `category` | string | `web` | `web`, `images`, `news`, `videos`, `books` |
| `engines` | string | all of category | comma-separated engine names |
| `page` | uint | 1 | result page |
| `max_results` | uint | 20 | max merged results to return (1-100) |
| `safesearch` | string | `moderate` | `off`, `moderate`, `strict` |
| `region` | string | unset | e.g. `us-en`, `de-de` |
| `language` | string | unset | e.g. `en` |
| `time_range` | string | unset | `day`, `week`, `month`, `year` |
| `filters` | string | unset | engine filter string (e.g. `site:github.com`) |
| `source_policy_mode` | string | `any` | `any`, `prefer-official`, `require-allowed`, or `official-only` |
| `allowed_domains` | CSV | empty | caller-requested hostname scope |
| `excluded_domains` | CSV | empty | caller-excluded hostnames; exclusions win |
| `api_key` | string | rejected | GET query-string credentials return HTTP 400; use an authentication header |

Response:

```json
{
  "query": "rust",
  "category": "web",
  "page": 1,
  "total": 8,
  "results": [
    {
      "type": "web",
      "title": "Rust Programming Language",
      "url": "https://www.rust-lang.org/",
      "description": "A language empowering everyone ...",
      "score": 1.0,
      "position": 1,
      "engines": ["bing", "brave"]
    }
  ],
  "suggestions": ["rust tutorial", "rust programming language"],
  "answer": null,
  "engines": [
    {"name": "bing", "status": "ok", "results": 10},
    {"name": "google", "status": "error", "error": "rate limited [scope=Provider, engine=google, status=429]", "scope": "Provider", "kind": "RateLimited { retry_after: Some(30s) }"}
  ],
  "elapsed_ms": 1200
}
```

Each result also includes additive `source_policy_mode`, `requested_match`,
`source_tier` (`official`, `secondary`, or `unknown`) and `policy_reason`.
Authority comes only from the operator `sources` catalogue; requested domains
never self-certify. Checks are local and happen before aggregation and
fetches, so strict modes may return fewer results without extra DNS, retries,
waits, or deadline extensions. Existing SSRF and redirect checks remain active.
Hostname policy inputs are checked against the pinned `psl` dependency
(`2.1.226`), which bundles a Mozilla Public Suffix List snapshot with ICANN
and PRIVATE rules. Updating that local snapshot is a dependency upgrade, not
a runtime lookup; single-label extraction URLs remain delegated to the existing
TargetPolicy/SSRF guard for compatibility.

Result fields by type:

- web: title, url, description, score, position, engines
- image: title, url, image_url, thumbnail_url, width, height, score, position, engines
- news: title, url, description, published, source, score, position, engines
- video: title, url, description, thumbnail_url, duration, views, uploader, score, position, engines
- book: title, url, description, author, publisher, score, position, engines

Errors: HTTP 400 (bad params, e.g. unknown category or engine, or an error
with `Query` scope), 401 (auth required / wrong key), 429 (rate limited),
500 (internal), 502/503 (egress/schema/provider failures, incl. all engines
failed, JSON `{"error": "..."}`).

## GET /v1/suggest

| Param | Type | Default | Meaning |
| --- | --- | --- | --- |
| `q` | string | required | query prefix |
| `source` | string | all sources | duckduckgo, google, bing, brave, startpage, qwant, wikipedia |
| `region` | string | `us-en` | locale |

```json
{"query":"rus","source":"bing","suggestions":["rust","rustup","russian"]}
```

Without `source`, returns a map of every source to its list.

## GET|POST /v1/extract

Readable-text extraction of a page (the same feature as `phrona extract` and
the library's `extract`). Query params (GET) or JSON body (POST):

| Field | Default | Meaning |
| --- | --- | --- |
| `url` | required | page to fetch and extract |
| `max_chars` | `5000` | max characters of extracted text (1-100000) |
| `query` | unset | bias the excerpt toward this query |
| `source_policy_mode` | `any` | `any`, `prefer-official`, `require-allowed`, or `official-only` |
| `allowed_domains` | empty | CSV caller-requested hostname scope |
| `excluded_domains` | empty | CSV caller-excluded hostnames; exclusions win |

The GET form sends the three policy fields as query parameters. The POST JSON
form uses the same field names, with `allowed_domains` and `excluded_domains`
as arrays:

```json
{
  "url": "https://docs.example.com/guide",
  "max_chars": 5000,
  "source_policy_mode": "require-allowed",
  "allowed_domains": ["docs.example.com"],
  "excluded_domains": ["private.docs.example.com"]
}
```

Response is the `ExtractedPage` shape:

```json
{
  "url": "https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html",
  "title": "What Is Ownership? - The Rust Programming Language",
  "description": "...",
  "text": "...",
  "images": ["https://..."]
}
```

## GET /v1/test

Availability probe across every category (the same feature as
`phrona test`): runs a real search per category with
`SearchOptions::probe_all`, so **every** engine runs to completion and
reports status, result counts and errors - even for categories where
every engine failed.

| Param | Default | Meaning |
| --- | --- | --- |
| `query` | `rust programming` | probe query |
| `category` | all categories | `web`, `images`, `news`, `videos`, `books` |
| `max_results` | `5` | merged results per category (1-10) |

Response: an array of per-category reports.

```json
[
  {
    "category": "web",
    "total": 8,
    "elapsed_ms": 1200,
    "answer": "...",
    "engines": [
      {"name": "bing", "status": "ok", "results": 10},
      {"name": "google", "status": "error", "error": "http ... 429"}
    ]
  }
]
```

## POST /search and POST /v1/tavily

Tavily-compatible endpoint for drop-in replacement in Tavily clients.
The JSON-body `api_key` shown below is POST-only. `x-api-key` and
`Authorization: Bearer` headers are supported alternatives.

```json
{
  "query": "rust",
  "api_key": "...",
  "search_depth": "basic",
  "topic": "general",
  "days": 7,
  "max_results": 8,
  "include_images": false,
  "include_answer": false,
  "include_raw_content": false,
  "include_domains": ["example.com"],
  "exclude_domains": ["spam.net"],
  "source_policy": {
    "mode": "require-allowed",
    "allowed_domains": ["example.com"],
    "excluded_domains": []
  }
}
```

| Field | Meaning |
| --- | --- |
| `query` | required |
| `api_key` | auth (any value works when `PHRONA_API_KEY` is unset) |
| `search_depth` | `basic` restricts to bing + duckduckgo (plus grokipedia when `include_answer`); `advanced` uses all web engines |
| `topic` | `general` or `news` (news: category=news, time_range=week) |
| `days` | news recency window |
| `max_results` | default 5, cap 20 |
| `include_images` | adds `images` field |
| `include_answer` | adds `answer` field; the grokipedia answer engine is queried for this |
| `include_raw_content` | adds `raw_content` (full extracted page text, capped) |
| `include_domains` / `exclude_domains` | legacy host filters, enforced locally and also translated to provider hints |
| `source_policy` | additive object with `mode`, `allowed_domains`, `excluded_domains` |

Response is the Tavily shape. Each result has additive `source_metadata` with
the same mode, requested-match, authority-tier and policy-reason fields; the
legacy result fields remain unchanged. Raw-content fetches apply the same
policy to the initial URL and every redirect.

```json
{
  "query": "rust",
  "follow_up_questions": [],
  "answer": "",
  "images": [],
  "results": [
    {"title": "...", "url": "...", "content": "...", "score": 0.9, "raw_content": "..."}
  ],
  "response_time": 1.2
}
```

## GET|POST /v1/grounding

AI grounding for RAG: returns a synthesized extractive answer plus ranked
sources with content, all with citation-ready attribution. The library
answer (from the grokipedia answer engine) is used verbatim when present;
otherwise the strongest snippets are stitched into an extractive summary.

For `GET`, authentication uses headers only: `x-api-key` or
`Authorization: Bearer`. A query-string `api_key` returns HTTP 400 and never
authenticates. For `POST`, the POST-body-only `api_key` may be supplied in the
JSON body, with either header accepted as an alternative.

Query params (GET) or JSON body (POST):

```json
{
  "query": "serde json",
  "max_results": 8,
  "category": "web",
  "time_range": "week",
  "engines": "bing,duckduckgo",
  "region": "us-en",
  "language": "en",
  "safesearch": "moderate",
  "filters": null,
  "source_policy_mode": "prefer-official",
  "allowed_domains": "docs.rs,serde.rs",
  "excluded_domains": "private.docs.rs"
}
```

The GET and POST grounding forms use these same policy fields as strings
(domain lists are comma-separated CSV). Omitted policy fields select `any`.

Response:

```json
{
  "query": "serde json",
  "answer": "Extractive summary for \"serde json\":\nSource 1 (https://serde.rs/json.html): ...",
  "sources": [
    {"title": "JSON Format - serde", "url": "https://serde.rs/json.html", "content": "...", "score": 1.0,
     "source_policy_mode": "prefer-official", "requested_match": true,
     "source_tier": "official", "policy_reason": "allowed"}
  ],
  "response_time": 1.1
}
```

`max_results` clamps to 1-50 (default 8, aligned with `phrona ground`
and the web UI). All other parameters mirror `/v1/search` semantics;
unknown engine names in `engines` are rejected with a 400. Source
scores are positional (`phrona::rank::positional_score`: 1.0 decaying
by 0.05 per position, floored at 0.05) - identical to the Tavily
endpoint and MCP `search_grounded`.

Grounding sources include additive `source_policy_mode`, `requested_match`,
`source_tier`, and `policy_reason` fields, matching native search metadata.

## Frontend

The static app lives in `crates/phrona-api/assets/` and is served
without embedding: edit `assets/index.html`, `assets/style.css`,
`frontend/app.js` and restart the server - no rebuild needed. The fallback
serves index.html for any other path.
