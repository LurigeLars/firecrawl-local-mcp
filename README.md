# firecrawl-local-mcp

## Current deployment and security posture

This repository wraps a self-hosted Firecrawl stack with local policy, secret isolation, search routing, and a bounded MCP surface.

- Firecrawl API, MCP, Playwright, gateway, SearXNG proxy, and LLM proxy run non-root; SearXNG runs under its dedicated UID/GID.
- Node-facing services use dropped Linux capabilities and `no-new-privileges` where compatible.
- Redis, RabbitMQ, and PostgreSQL retain their official root-entrypoint-to-service-user startup model; `no-new-privileges` is applied where compatible instead of forcing an unsafe `user:` override.
- PostgreSQL, SearXNG, Gemini, and browser-bridge secrets are host-protected with DPAPI and injected into service-specific tmpfs at runtime. Secret tmpfs directories use service-owned or root-owned mode `0700`.
- SearXNG is internal-only, has explicit pinned configuration, and the local proxy forwards internal client identity without enabling public rate limiting.
- Public MCP access is mediated by Cloudflare Access and explicit tool/request policy. The supplier-browser bridge is intentionally narrow and is not generic browser automation.
- Machine-specific paths, IPs, identities, Cloudflare values, supplier sessions, and credentials must remain outside Git.

## Repository status

This is an original deployment-wrapper project, **not a fork of Firecrawl**. It layers a self-hosted MCP deployment around an upstream Firecrawl checkout without vendoring or rewriting the Firecrawl source tree.

The repository-specific layer includes:

- Local and public MCP proxies/gateways with explicit tool allowlists and model-context compaction.
- Cloudflare Access integration using a shared-tunnel deployment model.
- SearXNG routing, search normalization, and optional local/Gemini LLM adapters.
- DPAPI-backed host secrets, Docker/network isolation, and hardened runtime configuration.
- A narrowly scoped local browser-session bridge for explicitly configured supplier workflows.
- Deployment scripts, tests, benchmarks, and adapted workflow skills.

Upstream Firecrawl remains a separate checkout and retains its own license and release lifecycle.

This public reference snapshot contains the deployment overrides, MCP gateway/proxy code, search/LLM adapters, tests, and adapted workflow skills used with that upstream checkout.

Upstream projects:
- Firecrawl: https://github.com/firecrawl/firecrawl — AGPL-3.0
- Firecrawl MCP server: https://github.com/firecrawl/firecrawl-mcp-server — MIT
- Firecrawl workflow skills: adapted portions are ISC-licensed; see `marketplace/plugins/firecrawl-mcp/THIRD_PARTY_NOTICES.md`.

Original wrapper code in this repository is licensed under the MIT License; see `LICENSE`. Third-party components and adapted material retain their upstream licenses; see `NOTICE.md` and `LICENSES/`.

> **Public-snapshot note:** hostnames, identities, app IDs, tunnel IDs, local paths, and LAN details are examples/placeholders. Real deployment secrets and machine-specific configuration are intentionally excluded.

- `firecrawl/` — untouched upstream checkout. The reviewed tag/commit is machine-readable in `upstream/firecrawl.json`; `fc.ps1 up/redeploy` verifies origin, commit and tracked cleanliness before building.
- `compose.local.yaml` — local overrides (FoundationDB off, auto-restart).
- `.env` — settings. API bound to `127.0.0.1:3002` only; it has **no authentication**, never expose it.
- `fc.ps1` — `up` / `down` / `status` / `logs` / `test` / `import-gemini`.

## Restore on a new machine

This repo holds only the local additions. Secrets and machine-specific files are gitignored.

1. Read `upstream/firecrawl.json`, then clone the exact reviewed tag from `https://github.com/firecrawl/firecrawl.git` into `firecrawl` and verify that `HEAD` equals the recorded commit.
2. Copy `.env.example` to `.env` and `public/gateway.env.example` to `public/gateway.env`; replace deployment placeholders locally and keep the real files out of Git.
3. Run `.\scripts\configure_service_secrets.ps1` once. PostgreSQL and SearXNG secrets are stored with Windows DPAPI under `%LOCALAPPDATA%\FirecrawlLocal\secrets` and injected into per-container tmpfs only at runtime.
4. If Gemini should be enabled, run `.\scripts\configure_gemini.ps1`. The API key uses the same DPAPI namespace and is injected into the LLM proxy's tmpfs runtime secret.
5. The supplier-browser bridge token is generated automatically on first `.\fc.ps1 up`, protected with Windows DPAPI at `%LOCALAPPDATA%\FirecrawlLocal\secrets\browser_bridge_token.dpapi`, and injected into the gateway tmpfs only at runtime.
6. Configure the shared Cloudflare route and Access application with your hostname only in local deployment configuration; do not commit the real hostname.
7. Install the required local model/runtime dependencies described below.
8. Run `.\fc.ps1 up`, then `.\fc.ps1 test`.

## Local MCP proxy (Claude desktop, Claude Code, Codex)

`local-mcp/stdio-proxy.mjs` wraps the exact `firecrawl-mcp` version in `public/package.json` over stdio and applies the same rules as the public gateway
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

`compose.public.yaml` adds two containers (none publishes a host port):
`mcp` (the exact `firecrawl-mcp` version declared in `public/package.json`, in HTTP mode) → `gateway` (`public/gateway/gateway.mjs` + `instructions.md`).
The host-level `mcp-cloudflared` container provides the shared tunnel and reaches this stack through the
`firecrawl-gateway:8080` alias. `fc.ps1` includes the public compose overlay when `public/gateway.env` exists.

- Login: configure a Cloudflare Access application for the MCP endpoint with **Managed OAuth** (DCR; redirect URIs
  `https://chatgpt.com/connector_platform_oauth_redirect` and `https://chatgpt.com/connector/oauth/*`; grant 1 month,
  access token 10 min). Unauthenticated requests get 401 + OAuth metadata at the edge. The gateway also verifies the
  Access JWT (`ACCESS_TEAM_DOMAIN`, `ACCESS_AUD`, `ACCESS_ALLOWED_EMAILS` in `.env`; offline test:
  `node public/access-test.mjs file:///<repo-path>/public/gateway/gateway.mjs`).
  The gateway refuses to start when the Access team, audience, or allowed identity is missing. Cloudflare Access is the
  only public authentication path; the old secret-link fallback has been retired.
- Address: the public endpoint is supplied only through deployment-local configuration; `.\fc.ps1 url` prints the configured endpoint.
  Anything else returns 404.
- Deployment identifiers are local-only and must stay out of Git. `fc.ps1` removes any legacy `GATEWAY_SECRET` entry from the ignored `public/gateway.env` during startup.
- Gateway: the Firecrawl surface is limited to scrape/map/search/crawl/check_crawl_status; `firecrawl_parse` is blocked because it can read arbitrary local files in this mode. The public deployment also exposes the separate, allowlisted supplier-browser tools documented below. Scrape/crawl requests asking for screenshot, branding, audio, or browser `actions` are rejected immediately because this self-hosted instance cannot serve them. The gateway defaults to 120 requests/min per caller IP (`RATE_PER_MIN`) with a 256 KB request cap.
- Server instructions: the gateway replaces the MCP `initialize` instructions with `public/gateway/instructions.md`
  (read on every connect, no restart needed). ChatGPT picks them up when the connector is refreshed ("Uppdatera").
  Keep it in line with the `firecrawl-mcp` core skill.
- Verify end to end: `node public/selftest.mjs (.\fc.ps1 url)`; watch traffic: `.\fc.ps1 logs`.
- Per-service public kill switch: stop the Firecrawl gateway container. Do not stop the shared `mcp-cloudflared`
  container unless you intend to disconnect all MCP gateways that use it.
- Docker Desktop must be running for any of this; enable "Start Docker Desktop when you sign in".

### Local interactive supplier browser bridge

For the Season Hotel pilot, the public ChatGPT connector exposes a separate, narrowly scoped supplier-browser surface:

- `browser_session_open` and `browser_session_status` manage the two approved sessions: `season-spendrups` and `season-ms`.
- `browser_product_open` navigates an approved session only to a direct numeric product-detail path; it does not accept arbitrary URLs or search terms.
- `browser_category_open` is restricted to the fixed Spendrups category flow and a bounded page number.
- `browser_snapshot` and `browser_network_log` return bounded, redacted read-only views.
- `browser_product_probe` and `browser_category_probe` return sanitized data from already observed Spendrups requests.

`fc.ps1 up` starts `public/browser-bridge.mjs` on the Windows host and creates a random internal token if needed. The token is persisted only as a Windows DPAPI blob under `%LOCALAPPDATA%\FirecrawlLocal\secrets`; an older gitignored `public/browser.env` is verified, migrated, and deleted automatically. During gateway startup the token is decrypted only long enough for a Compose `post_start` hook to write it into `/run/firecrawl-gateway-secrets/browser_bridge_token` on tmpfs. It is absent from the gateway service `.Config.Env`. Unless `BROWSER_BRIDGE_PORT` is configured explicitly, the launcher reuses its previous valid port or selects the first free loopback port in `8765-8799`, records it under `.runtime`, and passes that port to the Docker gateway. Supplier Chrome DevTools Protocol endpoints remain loopback-only on `127.0.0.1:9440` and `127.0.0.1:9441`.

Manual-auth workflow:
1. Call `browser_session_open` for the supplier. A visible Chrome window opens on the Windows desktop.
2. The user logs in manually in that Chrome window. Never send credentials through chat or browser automation.
3. Use the bounded snapshot, probe, category/product-open, and network-log tools as needed. Network capture begins when the bridge attaches, so open the session before logging in or navigating.
4. Browser profiles stay under `%LOCALAPPDATA%\FirecrawlLocal\browser-profiles` and remain outside Git/Drive. `fc.ps1 down` stops the bridge process but does not delete the profiles.

The bridge is intentionally not a generic browser automation service. It does not expose arbitrary URLs, credential entry, cookie/storage export, request headers, or request bodies. Expanding suppliers or navigation capabilities requires an explicit code change and review.

## Search backend (SearXNG)

`searxng` (image `searxng/searxng:2026.9.10-931fd9787`, service in `compose.local.yaml`, settings in
`searxng/settings.yml`) is Firecrawl's search backend (reached through `searxng-proxy`, see below). It is only
reachable on the internal Docker network. `SEARXNG_SECRET` is DPAPI-protected on the Windows host and injected into the container's runtime tmpfs by a `post_start` hook; the container wrapper exports it only into the SearXNG process at startup. Limiter off (single internal
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
keeps only `num_ctx/2` tokens, silently dropping the instructions and the top of the page (infoboxes, intros),
which produced confident but wrong answers. The proxy cuts the page from the end to `LLM_MAX_INPUT_TOKENS` (default
7000, ~3 chars/token) so the head always survives. Facts deep inside very long pages can therefore be missed.

`MODEL_NAME=qwen2.5-16k` is `qwen2.5:7b` with a 16k context (`ollama create qwen2.5-16k` from a Modelfile with
`PARAMETER num_ctx 16384`). The user env var `OLLAMA_IGPU_ENABLE=1` lets Ollama use the Arc 140V iGPU: a long
Wikipedia extraction went from >5 min (CPU; firecrawl-mcp times out at 300 s) to ~65 s.
No Ollama account is involved.

### Local service secrets

`POSTGRES_PASSWORD` and `SEARXNG_SECRET` use Windows DPAPI on the host plus per-container tmpfs at runtime. Run `.\scripts\configure_service_secrets.ps1` once. On an existing installation the script reads the current values from the legacy gitignored `secrets.env`, stores verified DPAPI blobs as `postgres_password.dpapi` and `searxng_secret.dpapi`, and then removes the plaintext entries. If no unrelated entries remain, the legacy `secrets.env` file is deleted.

`fc.ps1` no longer loads `secrets.env`. During `up`, it decrypts the two DPAPI blobs into temporary host-process variables used only by Compose `post_start` hooks. Each affected container starts with its own `/run/firecrawl-secrets` tmpfs; its main process waits until the hook writes the secret there with `umask 077`. Immediately after `docker compose up` returns, the host-process values are replaced with inert placeholders and restored/removed when `fc.ps1` exits. The real values are therefore absent from container `.Config.Env` and are not copied into the container writable layer.

PostgreSQL reads `/run/firecrawl-secrets/postgres_password` through its standard `POSTGRES_PASSWORD_FILE` convention. Firecrawl's API still expects `POSTGRES_PASSWORD`, so its startup wrapper reads the tmpfs file, exports it only inside the running process, then execs the normal harness. SearXNG uses the same wrapper pattern with `/run/firecrawl-secrets/searxng_secret` before executing its pinned image entrypoint. The CI runtime smoke test also asserts that the injected value is absent from `.Config.Env` and from `docker diff`.

Optional Gemini uses a Windows DPAPI secret instead of an environment variable. Run `.\scripts\configure_gemini.ps1` once; it stores the encrypted blob at `%LOCALAPPDATA%\FirecrawlLocal\secrets\gemini_api_key.dpapi`. If an older `secrets.env` contains a non-empty `GEMINI_API_KEY`, the script migrates that value to DPAPI, verifies it can be decrypted by the current Windows user, and only then removes the plaintext line.

On `.\fc.ps1 up`, the host decrypts the DPAPI blob in memory and streams the key into the `llm-proxy` container at `/run/firecrawl-secrets/gemini_api_key`. The key value is tmpfs-backed, is not a Docker environment variable, is absent from container `.Config.Env`, and disappears with the container; Docker metadata can still reveal the tmpfs mount path itself. Use `.\fc.ps1 import-gemini` to refresh an already-running proxy after rotating the key. The proxy reads the secret file on demand, so no process restart is needed after import.

When the runtime secret is present, `llm-proxy` sends `/v1/responses` calls (query, json and summary all use them) to Gemini's OpenAI-compatible endpoint (`GEMINI_MODEL`, default `gemini-3.5-flash-lite`), with up to 60k characters of page text. Any Gemini failure (rate limit, quota, rejected schema, timeout) falls back to the local model; after a 429 Gemini is skipped for 60 s. Page text and prompts then go to Google. Setting Gemini through Firecrawl's own `GOOGLE_GENERATIVE_AI_API_KEY` does not work here because its query path uses a different provider/model flow. `docker logs firecrawl-llm-proxy-1` shows `gemini ok` or `gemini failed, using local model` without logging the API key.

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
- `POSTGRES_PASSWORD` and `SEARXNG_SECRET` are stored as DPAPI blobs under `%LOCALAPPDATA%\FirecrawlLocal\secrets`. `fc.ps1 up` decrypts them only long enough for service-specific `post_start` hooks to write them into per-container tmpfs. The values are absent from container `.Config.Env` and the writable layer. PostgreSQL receives the password through `POSTGRES_PASSWORD_FILE`; the Firecrawl API and SearXNG wrappers read their tmpfs files at process startup. The migration preserves the existing PostgreSQL password, so the existing database volume remains valid. If an older installation has a DPAPI value that no longer matches the password stored in the existing PostgreSQL volume, `fc.ps1 recover` fails closed and tells you to run the one-time `fc.ps1 repair-postgres-auth` action. That action uses local socket authentication inside the database container and rotates only the `postgres` role to the already-stored DPAPI value.
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