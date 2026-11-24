# Rust migration spike (#231)

Status: investigation only. No Compose, public routes, or production dependencies changed.

## Evidence

Operator benchmark at upstream 283bc7c662cf7b14801eb8b6f82b1b26533d8008 (v2.11.851), compared with deployed v2.11.793, three NUQ workers:
- 21/21 HTML requests per engine; serial median 181 vs 628 ms, parallel median 305 vs 1399 ms.
- Idle engine cgroup memory 20.3 vs 2094.1 MiB; these are not equivalent feature sets. Rust is additive until the legacy stack can actually be retired.
- W3C PDF returned HTTP 403 on Rust vs 200 on existing engine. PDF parser remains unverified.
- Cloud-off, private-literal rejection and unauthorized API request guards passed. Cross-engine output hashes differed. No JS rendering or sustained crawl parity established.

## Architecture

Keep the pinned upstream Rust library/binary as the engine, rather than copying its scraping implementation. Keep the existing public gateway, local stdio policy and installed firecrawl-mcp package. Their client-facing tool schema, authorization, DNS policy, result-size caps, crawl evidence and browser dispatch must stay intact. The public MCP service currently targets api:3002; local stdio and CLI use loopback port 3002. Design the eventual API compatibility layer at that shared API boundary rather than duplicating routing in both MCP gateways. Reuse existing SearXNG, Playwright/router, Camofox/browser sidecars and llm-proxy interfaces. Avoid rewriting working sidecars just to change languages.

## Compatibility findings from reviewed pinned code

| Capability | Current wrapper | Rust v2.11.851 | Migration gate |
| --- | --- | --- | --- |
| Markdown scrape | existing full engine | works on sampled public HTML | source-specific content quality, bounded deadline, explicit uncached mode |
| HTML/links/PDF | supported with current engine | format support; PDF fetch blocked in trial | verify formats and PDF on reachable known-text fixture |
| Search | SearXNG/lang-proxy | cloud-only route | local SearXNG adapter preserving response/options, no cloud |
| Query/JSON/summary | llm-proxy Gemini/Ollama | unsupported/skipped | preserve extraction schema, prompt and quota/fallback behavior |
| JS/proxy | Playwright router + Camofox tools | native Chrome path; static test only | reuse existing services, keep authenticated/session traffic out of public proxy |
| Crawl/map | bounded job and status tools | local map; crawl/batch job map in RAM, 1h TTL | result semantics, cancellation, pagination, restart/recovery, concurrency and site-coverage evidence |
| Cache/freshness, idempotency, redaction, advanced flags | existing contract | ScrapeOptions silently ignores unknown fields | positive option allowlist; never drop meaning |
| Crawl delay | seconds (v2 schema) | milliseconds (CrawlOptions) | explicit unit translation, tests, preserve robots and caps |
| DNS/redirect | wrapper refuses any private DNS answer | guarded resolver filters private answers; redirect guard and 32 MiB fetch cap | retain wrapper policy; test mixed DNS answers, redirect and metadata rejection, resource bounds |

Source anchors (exact reviewed revisions):
- https://github.com/firecrawl/firecrawl/blob/283bc7c662cf7b14801eb8b6f82b1b26533d8008/apps/local/src/scrape.rs
- https://github.com/firecrawl/firecrawl/blob/283bc7c662cf7b14801eb8b6f82b1b26533d8008/apps/local/src/serve.rs
- https://github.com/firecrawl/firecrawl/blob/283bc7c662cf7b14801eb8b6f82b1b26533d8008/apps/local/src/crawl.rs
- https://github.com/firecrawl/firecrawl/blob/283bc7c662cf7b14801eb8b6f82b1b26533d8008/apps/local/src/fetch.rs
- https://github.com/firecrawl/firecrawl/blob/v2.11.793/apps/api/src/controllers/v2/types.ts

## Sequencing and stop/go criteria

1. This spike establishes a conservative input/output contract for the sampled static markdown case. Run the offline prototype tests with node --test research/rust-scrape-contract.test.mjs. No runtime imports.
2. Representative paired tests (actual public news/official-report/documentation pages, known text PDF, JS-required pages, redirects, bounded concurrency). Measure accepted content and end-to-end latency including fallback. No automatic promotion from an HTTP 200 or text length alone. Keep FIRECRAWL_CLOUD=off, no credentials, no private access. Test safe rejection separately from transient failures; never retry a policy-denied destination on another engine.
3. Only after parity for the supported subset: opt-in API compatibility spike, legacy default, explicit disable/rollback; one total timeout, bounded concurrency and response bytes. Do not allow arbitrary downstream endpoints. Unknown fields stay on the existing path and are still subject to existing policy. No retry on rate-limit or policy refusal. No permanent new service without measured benefit on actual workload.
4. Extend the Rust API using existing sidecar HTTP interfaces for search, browser/proxy and model formats. Retain the pinned upstream implementation and minimize custom patches. Assess whether upstream support removes the need for each adapter.
5. Investigate actual durability requirements before selecting storage. An in-memory crawl job map is not equivalent to Redis/RabbitMQ/Postgres; no deletion, migration or queue replacement without durable job/status/recovery tests, backup, drain and rollback. Do not introduce a new database solely because it is Rust-compatible.

Scope limit: no live routing, no Rust service deployment, no queue changes in this spike. The tests are offline policy examples, not runtime compatibility evidence. Revisit after upstream upgrade #230 and security review; repeat tests against whichever pinned version is actually promoted.

## Search adapter spike (2026-11-24)

Reuse the existing internal `searxng-proxy:8081/search` endpoint (see compose.local.yaml and searxng/lang-proxy.mjs). The current v2 SearXNG adapter sends q, language, format=json, pageno and optional safesearch=2; it budgets 20 results per page. Do not duplicate the existing proxy's language/engine fallback or site-operator parsing. The Rust research CLI uses the already locked reqwest/serde/tokio/url dependencies and returns a bounded data.web array with explicit research counts. It never follows redirects, accepts a caller-controlled backend, or invokes Firecrawl Cloud/Alexandria. Result URLs are inert; literal private targets and credentials are rejected and every future fetch must still pass existing DNS/redirect guards.

Supported subset: plain web search, query, limit (1..100), lang, safe, timeout (single overall budget up to 60 s), mutually exclusive ASCII include/exclude domains. Source-specific options, categories, country/location, tbs/filter, scrapeOptions, integration/credit/id contract and advanced sources are not implemented. Unknown fields are rejected; a future dispatcher must keep unsupported requests on the legacy path. Response cap 2 MiB per proxy page; max five pages, URL dedup, bounded fields, explicit error on invalid upstream response. Existing proxy fallback triggers on insufficient results from a successful primary call, not on all possible upstream errors. Do not silently reinterpret partial results as comprehensive coverage.

Tests: Rust local mock HTTP server exercises parameter serialization, pagination, domain boundary and private-URL filtering, duplicate results, redirect refusal, invalid/oversized body and timeout. Node test runs the actual existing language proxy with a synthetic loopback upstream to verify default/explicit language, primary/fallback routing, safe search, pagination and domain filtering. Both are wired into existing public regression workflow; no production API, Compose or gateway changes. End-to-end Rust -> actual proxy -> live SearXNG, full API contract and local latency remain separate gates. The narrow adapter was reviewed using Ponytail and Trail of Bits Rust security guidance (upstream default-branch skill blob f32c03a536c2f45f5b8b81af67dc2499c3554e65); this is not a full repository security audit.

## Model transport spike (2026-11-24)

The existing internal llm-proxy:11435/v1/responses endpoint owns Gemini credentials, cooldown, context trimming and Ollama fallback. The Rust probe uses the configured MODEL_NAME, fixed proxy endpoint, existing locked dependencies, no redirects or proxy environment overrides, one 180 s client deadline and bounded request/response sizes. Query, summary and schema-bearing JSON are transport candidates only, using separate instructions and untrusted document input. It refuses oversized text rather than silently truncating it and refuses incomplete/error/refusal responses and invalid JSON.

Important promotion gates: a valid JSON parse does not establish JSON-schema compliance or source correctness. Reuse or preserve the legacy schema validation/normalization contract (AJV, structured output, limits); direct-quote query and advanced extraction semantics remain on the legacy path. The current Gemini chatToResponses mapping sets completed without checking finish_reason, and local proxy trimming is independently configurable and not surfaced as structured response metadata. A future production path must preserve incomplete/refusal status and explicitly communicate any truncation; the research receipt marks provider completion, schema and source verification false. Prompt instructions are mitigation, not a security boundary. No document or credentials were sent to a real model in CI. The existing proxy's synthetic mapping tests and Rust mock HTTP tests are wired into the public regression workflow; live Gemini/Ollama, provider fallback, quality and latency remain unverified. No changes to runtime proxy, configuration, secret mounts, gateway or production routing.
