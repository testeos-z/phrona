# MCP server reference

`cargo run -p phrona-mcp`

A Model Context Protocol server over stdio (JSON-RPC 2.0, protocol version
2025-11-25). Compatible with any MCP client - Claude Desktop, claude-code,
Cursor, VS Code Copilot, Continue, n8n, and anything speaking the stdio MCP
transport.

## Wiring up a client

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "phrona": {
      "command": "/path/to/phrona/target/release/phrona-mcp"
    }
  }
}
```

claude-code:

```bash
claude mcp add phrona -- /path/to/phrona/target/release/phrona-mcp
```

Any client that can run a command. The binary speaks MCP on stdin/stdout -
no ports, no config file, no API keys.

## Tools

Nine tools, each with JSON Schema parameters (generated with schemars):

| Tool | Description |
| --- | --- |
| `web_search` | search web results (query, page, max_results, safesearch, region, language, time_range, filters, engines, source_policy) |
| `image_search` | image results (query, max_results, safesearch, region, filters, engines) |
| `news_search` | news results with date/source (query, max_results, region, time_range, engines) |
| `video_search` | videos with duration/views/uploader (query, max_results, safesearch, region, engines) |
| `book_search` | books with author/publisher (query, max_results, region, engines) |
| `suggest` | query completions (query, source, region) |
| `fetch_page` | extract a page: title, description, text (max_chars, query bias, source_policy) |
| `search_grounded` | RAG: search + pick best page + return verbatim excerpt and ranked sources (same `source_policy` as search) |
| `list_engines` | available engines per category (invalid categories return an error envelope) |

Tool call results are `text/plain` content containing JSON.

`source_policy` is optional and has the shared shape
`{"mode":"any|prefer-official|require-allowed|official-only", "allowed_domains": [], "excluded_domains": []}`.
It defaults to `any`. Results retain additive `source_policy_mode`,
`requested_match`, `source_tier` and `policy_reason` metadata. The operator's
configured catalogue is the only authority source; policy evaluation is local
and adds no network lookups, waits or retries. Fetches enforce the policy on
every redirect in addition to existing SSRF protections.

## search_grounded

For AI RAG workflows: performs a web search, returns a synthesized answer
(the library answer engine's text verbatim when present, otherwise an
extractive summary) plus ranked sources with content:

```json
{
  "query": "...",
  "answer": "Extractive summary for \"...\": ...",
  "sources": [{"title": "...", "url": "...", "content": "...", "score": 0.92}]
}
```

so the model can answer with actual cited content instead of paraphrasing.

## Raw JSON-RPC (for debugging without a client)

```bash
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n' | ./target/release/phrona-mcp
```

Each message must be one line (newline-delimited JSON). `tools/call` uses
`params: {"name": "web_search", "arguments": {"query": "rust"}}`.

## Options

Only the HTTP options of the core client are in effect (Chrome profile,
20s timeout); there is no config surface - keep it simple for agents.
