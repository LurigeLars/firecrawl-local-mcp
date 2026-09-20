---
name: firecrawl-mcp-build
description: Write application code that calls Firecrawl (scrape, search, map, crawl, JSON extraction) via the official SDKs or REST, targeting a self-hosted instance by default. Use when code, not a chat answer, needs web data.
---

# Building with Firecrawl

For live web work during a session, use the `firecrawl-mcp` skill instead. This skill is for product or script code.

## Connection

- Self-hosted base URL: `http://127.0.0.1:3002` (only reachable on the local machine). No API key is required; SDKs that insist on one accept any placeholder.
- Read the URL from configuration (`FIRECRAWL_API_URL`), never hard-code it, so the same code can target hosted Firecrawl (`https://api.firecrawl.dev` + `FIRECRAWL_API_KEY`).
- Never put the public MCP gateway URL (it contains a secret) in code, docs, or commits.

## SDKs

- Python: `pip install firecrawl-py` -> `from firecrawl import Firecrawl; fc = Firecrawl(api_key=os.getenv("FIRECRAWL_API_KEY", "local"), api_url=os.environ["FIRECRAWL_API_URL"])`
- Node: `npm i @mendable/firecrawl-js` -> `new Firecrawl({ apiKey: process.env.FIRECRAWL_API_KEY ?? "local", apiUrl: process.env.FIRECRAWL_API_URL })`
- Pin versions and check the installed SDK's method names before use (`scrape`, `search`, `map`, `crawl`, `start_crawl`/`startCrawl`); they have changed between major versions.

## REST (v2)

- `POST /v2/scrape` `{url, formats: ["markdown"], onlyMainContent: true}`
- `POST /v2/search` `{query, limit}`
- `POST /v2/map` `{url, limit}`
- `POST /v2/crawl` `{url, limit, includePaths}` then `GET /v2/crawl/{id}` until `status == "completed"`
- JSON extraction: `formats: [{type: "json", prompt, schema}]` - runs on a local model, allow 15-90 s per page and set client timeouts above 5 minutes.

## Design rules

- Treat missing fields as missing (`null`), not empty strings or guesses.
- Handle `success: false`, timeouts, and 429 with bounded retries and backoff.
- Unsupported here: agent, interact/browser, monitor, research/developer indexes, branding, screenshot. Fail clearly if code depends on them.
