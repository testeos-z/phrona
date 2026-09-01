# CLI reference

`cargo run -p phrona-cli -- ...` (binary name `phrona`). A single entry
point to every library feature plus the full server: search, suggestions,
extraction, grounding, engine listing, availability probing, the REST API
and the MCP server.

## Global options

| Option | Meaning |
| --- | --- |
| `--json` | machine-readable output (all commands) |
| `--profile <name>` | browser impersonation: chrome, firefox, safari, edge, opera, okhttp, random |
| `--proxy <url>` | proxy URL, repeatable (only the first is used today) |
| `--timeout <sec>` | request timeout (default 20) |
| `-h`, `-V` | help, version |

## Commands

### phrona search <query>

Full search with every option:

```bash
phrona search "rust ownership" --max-results 10 --engines bing,brave,wikipedia
phrona search "rust" --category news --time-range week --region us-en --json
phrona search "rust" --category images --safesearch strict --max-results 20
```

Options: `--category web|images|news|videos|books`, `--engines <csv>`,
`--max-results`, `--safesearch off|moderate|strict`, `--region`, `--language`,
`--time-range day|week|month|year`, `--filters`, `--page`,
`--source-policy-mode any|prefer-official|require-allowed|official-only`,
repeatable `--allowed-domain` and `--excluded-domain`.

Text output shows the query summary, answer, suggestions, ranked results
with engine provenance, and the per-engine report (status, result count,
error). `--json` emits the same shape as `GET /v1/search`.

### phrona suggest <query>

```bash
phrona suggest rus --source bing,wikipedia
phrona suggest rus --json                # all 7 sources
```

### phrona extract <url> [url...]

One or more URLs, fetched and extracted in parallel. `--query` biases
the excerpt; `--max-chars` caps the text. Extraction also accepts the source
policy flags, which are checked on every redirect.

```bash
phrona extract https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html \
  --max-chars 3000 --query ownership
phrona extract https://example.com https://example.org --max-chars 800
```

### phrona ground <query>

Grounded output for RAG: the answer (library answer engine verbatim when
present, otherwise an extractive summary) followed by ranked cited sources.
Accepts the full search option set: `--category`, `--engines`,
`--max-results`, `--region`, `--language`, `--time-range`, `--safesearch`,
`--filters`, `--page`, `--source-policy-mode`, `--allowed-domain`,
`--excluded-domain`.

```bash
phrona ground "rust ownership"
phrona ground "rust" --category news --time-range week --region us-en --max-results 5
```

### phrona engines

```bash
phrona engines                      # all categories
phrona engines --category videos
```

### phrona test

Availability probe across every category (or one, with `--category`).
Runs a real search per category and prints an availability matrix plus
per-engine status, result counts and errors. Useful for smoke-testing a
network, a proxy setup or a profile choice.

```bash
phrona test --query "rust programming"
phrona test --category web --max-results 8
phrona test --category web --json
```

### phrona serve

The full server in one process:

```bash
phrona serve                                   # REST on 127.0.0.1:8080 + MCP on tcp 127.0.0.1:8081
phrona serve --addr 0.0.0.0:9090 --mcp-addr 0.0.0.0:9091 --api-key secret
phrona serve --no-mcp                          # REST only
phrona serve --no-rest                         # MCP-over-TCP only
```

- REST API: identical to `phrona-api` (frontend at `/`, `/health`,
  `/v1/*`, Tavily-compatible `/search`). See [docs/api.md](api.md).
- MCP over TCP: the same nine tools as the stdio server, framed as
  newline-delimited JSON-RPC 2.0 over a raw TCP socket. Clients that
  cannot use stdio (remote agents, containers) connect with any MCP
  client configured for a TCP transport.
- `--api-key` / `PHRONA_API_KEY` guards the REST API; the MCP listener is
  unauthenticated (bind it to localhost or a private network).

### phrona mcp

Serve MCP over stdio only (the same contract as `phrona-mcp`):
```bash
phrona mcp
```

### phrona completions <shell>

```bash
phrona completions bash > ~/.bash_completion.d/phrona
phrona completions zsh > "$fpath[1]/_phrona"
```

## Shell completions

Generated from the clap definition via `clap_complete` (bash, zsh, fish,
powershell, elvish).

## Exit codes

0 on success, 1 on search/extraction failure (e.g. all engines blocked),
2 on argument errors (clap default).

## JSON shapes

`search --json` and `ground --json` mirror `GET /v1/search`; `suggest
--json` mirrors `GET /v1/suggest` (all sources when no `--source`);
`engines --json` mirrors `GET /v1/engines`; `test --json` is a list of
per-category `{category, total, elapsed_ms, engines[]}` objects. All
shapes are documented in [docs/api.md](api.md).
