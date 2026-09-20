# firecrawl-local-mcp

Public reference snapshot of a self-hosted Firecrawl deployment wrapper. This repository is **not a fork of Firecrawl** and does not vendor the Firecrawl source tree. It contains deployment overrides, MCP gateway/proxy code, search/LLM adapters, tests, and adapted workflow skills that are used with an upstream Firecrawl checkout.

Upstream projects:
- Firecrawl: https://github.com/firecrawl/firecrawl — AGPL-3.0
- Firecrawl MCP server: https://github.com/firecrawl/firecrawl-mcp-server — MIT
- Firecrawl workflow skills: adapted portions are ISC-licensed; see `marketplace/plugins/firecrawl-mcp/THIRD_PARTY_NOTICES.md`.

Original wrapper code in this repository is licensed under the MIT License; see `LICENSE`. Third-party components and adapted material retain their upstream licenses; see `NOTICE.md` and `LICENSES/`.

> **Public-snapshot note:** hostnames, identities, app IDs, tunnel IDs, local paths, and LAN details are examples/placeholders. Real deployment secrets and machine-specific configuration are intentionally excluded.

- `firecrawl/` — untouched upstream checkout, pinned to tag `v2.11.343` (upgraded from v2.11.0 on 2026-09-16), images built locally.
- `compose.local.yaml` — local overrides (FoundationDB off, auto-restart).
- `.env` — settings. API bound to `127.0.0.1:3002` only; it has **no authentication**, never expose it.
- `fc.ps1` — `up` / `down` / `status` / `logs` / `test`.

## Restore on a new machine

This repo holds only the local additions. Secrets and machine-specific files are gitignored.

1. `git clone --depth 1 --branch v2.11.343 https://github.com/firecrawl/firecrawl.git firecrawl`
2. Copy `.env.example` → `.env`, `secrets.env.example` → `secrets.env`, `public/gateway.env.example` → `public/gateway.env`,
   and fill in the placeholders (random values for the secrets; Cloudflare Access values from the dashboard).
3. Cloudflare Tunnel: `cloudflared tunnel login`, `cloudflared tunnel create firecrawl`, then copy
   `cloudflared.example/config.yml` → `.cloudflared/config.yml` with the tunnel id (credentials JSON goes next to it).
4. `secrets.env` also needs `SEARXNG_SECRET` (random hex).
5. Ollama: `ollama pull qwen2.5:7b`, create `qwen2.5-16k` (see "Local AI model"), set user env `OLLAMA_IGPU_ENABLE=1`.
6. `.\fc.ps1 up`, then `.\fc.ps1 test`.

## Local MCP proxy (Claude desktop, Claude Code, Codex)

`local-mcp/stdio-proxy.mjs` wraps `firecrawl-mcp@3.24.0` over stdio and applies the same rules as the public gateway
(`public/gateway/policy.mjs`): only the 5 working tools are listed/callable (tool definitions ~3.9k instead of ~10.6k
tokens), `parse` and unsupported formats are refused, the server instructions come from
`public/gateway/instructions.md`, and scrapes that only ask for `query`/`json`/`summary` drop the page metadata
(a query answer is ~60 tokens instead of ~570). Markdown/html/links results keep their metadata.
Test: `node local-mcp/test-proxy.mjs` (stack must be running).

Registered in: Claude desktop (`%APPDATA%\Claude\claude_desktop_config.json`, key `firecrawl-mcp`), Claude Code
(user scope, below) and Codex (`[mcp_servers.firecrawl_mcp]`, command `node`).

MCP registration (user scope, done once):

```
claude mcp add-json firecrawl-mcp -s user '{"type":"stdio","command":"node","args":["<repo-path>/local-mcp/stdio-proxy.mjs"],"env":{"FIRECRAWL_API_URL":"http://127.0.0.1:3002"}}'
```

## Public access for ChatGPT

`compose.public.yaml` adds three containers (none publishes a host port):
`mcp` (firecrawl-mcp@3.24.0 in HTTP mode) → `gateway` (`public/gateway/gateway.mjs` + `instructions.md`) → `cloudflared` (tunnel configuration in `.cloudflared/`). They're included automatically by `fc.ps1` once `.cloudflared/config.yml` exists.

- Login: configure a Cloudflare Access application for the MCP endpoint with **Managed OAuth** (DCR; redirect URIs
  `https://chatgpt.com/connector_platform_oauth_redirect` and `https://chatgpt.com/connector/oauth/*`; grant 1 month,
  access token 10 min). Unauthenticated requests get 401 + OAuth metadata at the edge. The gateway also verifies the
  Access JWT (`ACCESS_TEAM_DOMAIN`, `ACCESS_AUD`, `ACCESS_ALLOWED_EMAILS` in `.env`; offline test:
  `node public/access-test.mjs file:///<repo-path>/public/gateway/gateway.mjs`).
  The gateway refuses to start when `ACCESS_AUD` is empty, so an accidentally blank value cannot silently leave only
  the secret link in place (since 2026-09-17). Emergency fallback to the secret link: set `ACCESS_AUD=` **and**
  `ALLOW_SECRET_PATH=1` in `.env`, run `.\fc.ps1 up`, and disable the Access application.
- Address: `.\fc.ps1 url` prints the configured `https://<hostname>/mcp` endpoint (or the secret-path URL when Access is off).
  Anything else returns 404.
- Secret: `public/gateway.env` (`GATEWAY_SECRET`). **Keep `public/gateway.env` and `.cloudflared/` local and out of git.**
- Rotate the secret (e.g. if the link leaks): replace the value in `public/gateway.env`, run `.\fc.ps1 up`, update the ChatGPT connector URL.
- Gateway: only scrape/map/search/crawl/check_crawl_status are listed and callable (`firecrawl_parse` reads arbitrary
  local files in this mode and is blocked); scrape/crawl requests asking for screenshot, branding, audio or browser
  `actions` are answered at once with a tool error (the instance cannot serve them); 120 requests/min per caller IP
  (`RATE_PER_MIN`); 256 KB request cap.
- Server instructions: the gateway replaces the MCP `initialize` instructions with `public/gateway/instructions.md`
  (read on every connect, no restart needed). ChatGPT picks them up when the connector is refreshed ("Uppdatera").
  Keep it in line with the `firecrawl-mcp` core skill.
- Verify end to end: `node public/selftest.mjs (.\fc.ps1 url)`; watch traffic: `.\fc.ps1 logs`.
- Kill switch: `docker stop firecrawl-cloudflared-1` (local use keeps working). Remove entirely: delete the tunnel in the
  Cloudflare dashboard (Zero Trust → Networks → Tunnels) and the `firecrawl` DNS record.
- Docker Desktop must be running for any of this; enable "Start Docker Desktop when you sign in".

## Search backend (SearXNG)

`searxng` (image `searxng/searxng:2026.9.10-931fd9787`, service in `compose.local.yaml`, settings in
`searxng/settings.yml`) is Firecrawl's search backend (reached through `searxng-proxy`, see below). It is only
reachable on the internal Docker network; its `SEARXNG_SECRET` lives in `secrets.env`. Limiter off (single internal
client), JSON output on.

Why: without it Firecrawl searched DuckDuckGo directly, which answers this home IP with CAPTCHAs, so searches often came
back empty (for example "Spotify Premium Family pris Sverige"). Engines in use: Google, Startpage and Bing (see routing
below); Mojeek (JavaScript challenge), DuckDuckGo (CAPTCHA) and Yahoo (parsing errors) are off. SearXNG suspends an
engine for a while when it gets blocked (Google: CAPTCHA, suspended for an hour), which is why there is a fallback. Verified 2026-09-16: two bursts of 15 searches,
30/30 with results, average ~1 s. Firecrawl still falls back to DuckDuckGo when SearXNG returns nothing, but a SearXNG
error makes the search return empty, so check `docker logs firecrawl-searxng-1` if searches stop working.
Search routing and relevance: `searxng/lang-proxy.mjs` (service `searxng-proxy`, `SEARXNG_ENDPOINT=http://searxng-proxy:8081`)
sits in front of SearXNG. Firecrawl sends one language (default `en`) for all engines, but tests 2026-09-16 (7 mixed
Swedish/English queries per engine and language) showed the engines need different ones: Google and Startpage are on
topic with `all` and return UK junk with `en-GB`; Bing returns unrelated pages with `en`, is mostly on topic with
`en-GB`, and matches only part of the query with `all`/`sv-SE`. So the proxy asks Google + Startpage with `all` first
and adds Bing with `en-GB` only when that gives fewer than 5 hits (env `PRIMARY_ENGINES`, `PRIMARY_LANG`,
`FALLBACK_ENGINES`, `FALLBACK_LANG`, `MIN_RESULTS`). A language other than `en`/`en-US` is passed through unchanged.
Afterwards 8/8 mixed queries through Firecrawl were on topic, and the fallback was checked with the primary engines failing.
The proxy also handles `site:` filters: Firecrawl turns `includeDomains`/`excludeDomains` into `site:x`/`-site:x`, and
the engines ignore the `site:` operator via SearXNG and return unrelated pages (a ChatGPT query `site:elgiganten.se Acer laptop`
+ `includeDomains` returned only dictionary pages). The proxy replaces `site:x` with the plain word `x`, drops
`-site:x`, and filters the results by host, so a domain-limited search can return fewer hits than `limit`.
`firecrawl_map` with `search` does not use SearXNG.
Engine health: `docker exec firecrawl-api-1 node -e "fetch('http://searxng:8080/search?q=test&format=json').then(r=>r.json()).then(j=>console.log(j.unresponsive_engines))"`.

## Local AI model (JSON / summary formats)

`.env` points Firecrawl at Ollama on the host through `llm-proxy` (`llm/trim-proxy.mjs`, service in
`compose.local.yaml`): `OLLAMA_BASE_URL=http://llm-proxy:11435/api` and, because some code paths always use the OpenAI
provider, `OPENAI_BASE_URL=http://llm-proxy:11435/v1` with a dummy `OPENAI_API_KEY`.

Why the proxy: Firecrawl's scrape JSON extraction sends the whole page (it assumes a 128k-token model). Ollama then
keeps only the last `num_ctx/2` tokens, silently dropping the instructions and the top of the page (infoboxes, intros),
which produced confident but wrong answers. The proxy cuts the page from the end to `LLM_MAX_INPUT_TOKENS` (default
7000, ~3 chars/token) so the head always survives. Facts deep inside very long pages can therefore be missed.

`MODEL_NAME=qwen2.5-16k` is `qwen2.5:7b` with a 16k context (`ollama create qwen2.5-16k` from a Modelfile with
`PARAMETER num_ctx 16384`). The user env var `OLLAMA_IGPU_ENABLE=1` lets Ollama use the Arc 140V iGPU: a long
Wikipedia extraction went from >5 min (CPU; firecrawl-mcp times out at 300 s) to ~65 s.
No Ollama account is involved.

Optional Gemini: with `GEMINI_API_KEY` in `secrets.env` (Google AI Studio key; restart with `.\fc.ps1 up`), `llm-proxy`
sends `/v1/responses` calls (query, json and summary all use them) to Gemini's OpenAI-compatible endpoint
(`GEMINI_MODEL`, default `gemini-3.5-flash-lite`; `gemini-2.5-flash-lite` returned 404 "no longer available to new users" on 2026-09-16), with up to 60k characters of page text (`GEMINI_MAX_CHARS`; the free tier allows 15 requests and 250k input tokens per minute and 500 requests per day (AI Studio, 2026-09-16; the day resets at midnight Pacific time, 09:00 Swedish summer time), and 9 full Wikipedia pages used 111k tokens), and converts the answer
back. Any failure (rate limit, quota, rejected schema, 60 s timeout) falls back to the local model; after a 429 Gemini
is skipped for 60 s. Page text and prompts then go to Google, whose free tier may use them to improve its products.
Setting Gemini through Firecrawl's own `GOOGLE_GENERATIVE_AI_API_KEY` does not work here: its query code asks for that retired model, and `MODEL_NAME` overrides the
model name for every provider. Tested 2026-09-16 with a mock endpoint and Firecrawl's AI SDK (text, JSON schema, 429
fallback, pause); `docker logs firecrawl-llm-proxy-1` shows `gemini ok` or `gemini failed ..., using local model`.

Benchmark: `llm/run-benchmark.ps1` (switches `MODEL_NAME`, scores 4 pages against ground truth, restores the setting).

## Plugin (skills) for Claude Code, Codex and ChatGPT

`marketplace/` is a local marketplace (`firecrawl-local`) with one plugin, `firecrawl-mcp`: 10 skills (all named
`firecrawl-mcp*` to avoid clashing with the official `firecrawl` plugins), several adapted from Firecrawl's
ISC-licensed workflow skills. The local plugin is **skills only**; each host gets the tools separately:

- Claude Code: user-scope MCP server `firecrawl-mcp` (stdio, local). Plugin: `claude plugin install firecrawl-mcp@firecrawl-local`.
- Codex: `[mcp_servers.firecrawl_mcp]` in `~/.codex/config.toml` (stdio via `local-mcp/stdio-proxy.mjs`, 5 tools, auto-approve), added by hand
  (`codex mcp add` from Git Bash mangles `/d /s /c` and rewrites the file). Plugin: `codex plugin add firecrawl-mcp@firecrawl-local`.
  Neither host goes through Cloudflare or needs a login.
- ChatGPT web: tools come from the developer app "Local-Firecrawl" (OAuth). Skills are uploaded as a zip under
  Anpassa → Skills → + (the plugin folder zipped as-is works; `marketplace/build-chatgpt-zip.ps1 -SkillsOnly` also
  builds one). Do not use @plugin-creator: it leads to public plugin submission (verified identity + card).
  Keep any host-specific setup notes outside the public repository.
- Claude desktop chats: upload one zip per skill under Settings → Capabilities → Skills.
- After editing skills: bump the version in the three `plugin.json` files, then `claude plugin marketplace update firecrawl-local`,
  `claude plugin update firecrawl-mcp@firecrawl-local`, `codex plugin add firecrawl-mcp@firecrawl-local`, and rebuild the ChatGPT zip.
- Terminal agents also get the `firecrawl` CLI (`firecrawl-cli@1.23.3`, user env `FIRECRAWL_API_URL=http://127.0.0.1:3002`).

## Network isolation

Containers run inside Docker Desktop's WSL2 VM with no writable host mounts. Their outbound traffic leaves Windows
through `com.docker.backend.exe`. The reference deployment adds outbound firewall rules that block the local/private LAN while preserving DNS and required internet access. Reproduce that isolation for your own LAN CIDRs and verify from the Playwright/API containers that private network targets are unreachable. Ollama access through `host.docker.internal` and the Cloudflare tunnel should remain explicitly allowed only when needed.
Chromium in the Playwright service runs with `--no-sandbox` (upstream default); keep Firecrawl updated.

Notes
- `secrets.env` (loaded by `fc.ps1` as a second `--env-file`) holds a random `POSTGRES_PASSWORD`, as the self-host guide
  recommends. Never commit or publish it. The Postgres image keeps its data in an anonymous volume that survives container
  recreation, so changing the password in `secrets.env` alone breaks the API (`28P01 auth_failed`); also run
  `ALTER USER postgres WITH PASSWORD '…'` via `docker exec -i firecrawl-nuq-postgres-1 psql -U postgres`.
- Browser viewport: `compose.local.yaml` patches the Playwright service at start-up so its viewport height comes from
  `PLAYWRIGHT_VIEWPORT_HEIGHT` (default 4000; upstream hard-codes 800). Virtualized pages can then render more rows before scrolling is required. The container logs a WARNING if an upgrade changes the patched line.
- Model variants in Ollama carry `num_ctx 16384` and `num_predict 1536` (stops runaway generations that otherwise hit
  the 300 s MCP timeout).
- Docker Desktop must be running; containers restart automatically with it.
- Verified not available self-hosted: agent ("Agent beta is not enabled"), interact/browser (needs a closed
  `BROWSER_SERVICE_URL`), monitor (needs Firecrawl's account database), research/developer indexes (404), branding and
  screenshot (need fire-engine). `firecrawl_extract` is deprecated in the MCP; use scrape with the json format.
- Upgrade: tag the current images for rollback (`docker tag firecrawl-api:latest firecrawl-api:<old>-backup`, same for
  `firecrawl-playwright-service` and `firecrawl-nuq-postgres`), then
  `git -C firecrawl fetch --depth 1 origin tag vX.Y.Z && git -C firecrawl checkout vX.Y.Z` and `.\fc.ps1 up`.
  Verify with `.\fc.ps1 test`, `node public/selftest.mjs (.\fc.ps1 url)` and `node llm/benchmark.mjs <model>`.
  Firecrawl publishes frequent patch tags (`v2.11.N`); GitHub "releases" lag far behind.
- Rollback to v2.11.0: `git -C firecrawl fetch --depth 1 origin tag v2.11.0 && git -C firecrawl checkout v2.11.0`, then
  `.\fc.ps1 up` (rebuilds from the old source; the `:v2.11.0-backup` images were deleted on 2026-09-16).
- Models kept (2026-09-16): `qwen2.5-16k` (active) and `gemma3-12b-16k` (reserve: same accuracy, slower; switch with
  `MODEL_NAME=gemma3-12b-16k` in `.env` + `.\fc.ps1 up`). `qwen2.5:14b` was removed after failing the benchmark.
