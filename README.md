# firecrawl-local-mcp

[![CI](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/fork-ci.yml/badge.svg)](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/fork-ci.yml)
[![CodeQL](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/codeql.yml/badge.svg)](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/codeql.yml)
[![Static analysis](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/static-analysis.yml/badge.svg)](https://github.com/LurigeLars/firecrawl-local-mcp/actions/workflows/static-analysis.yml)
![License](https://img.shields.io/badge/wrapper%20license-MIT-green)

A local-first deployment and MCP policy layer around a **self-hosted Firecrawl stack**.

This repository exists to run Firecrawl as infrastructure you control locally, while
presenting a smaller, reviewed MCP surface to AI clients. It combines the upstream
Firecrawl runtime with local search routing, optional LLM adapters, an ephemeral research
browser, secret isolation and a protected remote gateway.

It is **not a fork of Firecrawl** and does not vendor the Firecrawl source tree.

## Why this project exists

Upstream Firecrawl is a general-purpose web crawling/scraping system. In a local agent
stack, simply exposing the whole service directly creates several practical problems:

- the upstream MCP surface is broader than many agent workflows need.
- some operations are unsafe or unnecessarily expensive to expose remotely.
- self-hosted search needs reliable routing when public engines block a residential IP.
- local browser state needs stronger SSRF and credential boundaries than a generic browser.
- secrets for databases, search, proxies and optional LLM providers should not live in Git.
- remote MCP clients need authentication and request policy without publishing the raw
  Firecrawl API.

This repository is the integration layer that solves those deployment problems while
leaving the upstream Firecrawl checkout independently updateable.

## What is in the stack

| Component | Purpose |
|---|---|
| Upstream Firecrawl checkout | Crawl, scrape, map and search engine |
| Local MCP proxy | Bounded stdio surface for local MCP clients |
| Public MCP gateway | Reviewed Cloudflare Access-protected remote surface |
| SearXNG + routing proxy | Search backend and language/engine routing |
| Playwright services | Firecrawl rendering plus isolated browser paths |
| Ephemeral browser sidecar | Stateful public-web research without persistent credentials |
| Optional LLM proxy | Bounded local Ollama / optional Gemini-compatible extraction path |
| Secret bootstrap | DPAPI-backed host secret storage and runtime injection |
| `fc.ps1` | Local lifecycle, status, logs, tests and redeploy operations |

Normal Firecrawl and Playwright traffic stays direct. Optional public-proxy fallback is
isolated to explicitly allowlisted public research hosts.

## High-level architecture

```text
Local MCP client
      |
      v
local stdio policy proxy
      |
      v
self-hosted Firecrawl MCP/API
      |
      +--> Firecrawl workers / Playwright
      |
      +--> SearXNG routing
      |
      +--> optional LLM adapter
      |
      +--> isolated ephemeral browser
```

For a remote client:

```text
ChatGPT / remote MCP client
      |
      v
Cloudflare Access
      |
      v
reviewed MCP gateway
      |
      +--> bounded Firecrawl MCP surface
      |
      +--> isolated ephemeral browser tools
```

The raw Firecrawl API remains loopback-only in the maintained deployment and has no
authentication of its own. Do not expose it directly to a LAN or the Internet.

## MCP scope

The local/public policy layer intentionally exposes a bounded set of Firecrawl
capabilities rather than every upstream feature.

Core Firecrawl operations include:

- scrape.
- map.
- search.
- crawl.
- crawl-status checks.

Unsupported/high-risk formats and operations are rejected by policy. In particular,
local-file parse behavior and unrestricted upstream browser actions are not part of the
reviewed surface.

The repository also provides a narrow ephemeral-browser tool family for public-web
research. It supports opening/navigating pages, tabs, snapshots, network/console
inspection, screenshots and cleanup, but deliberately omits arbitrary JavaScript
execution and interactive credential entry.

## Security model

The deployment assumes that scraped web content is untrusted and that remote MCP access
must not imply access to the host or private network.

Key boundaries:

- Firecrawl API, MCP, Playwright, gateway and proxies run with reduced container
  privileges where compatible.
- PostgreSQL, RabbitMQ and Redis keep their official service-user startup model rather
  than unsafe forced user overrides.
- PostgreSQL, SearXNG, optional proxy and Gemini secrets are protected on the Windows
  host with DPAPI and injected only into runtime boundaries.
- SearXNG remains internal-only.
- The ephemeral browser accepts public HTTP(S) destinations and rejects local/private
  address space and credential-bearing URLs.
- Browser network output excludes headers, cookies and bodies; credential-like query
  parameters are redacted.
- Authenticated supplier/browser behavior does **not** belong in this generic browser
  layer.
- Public MCP access is authenticated through Cloudflare Access and an explicit tool /
  request policy.
- Machine paths, identities, LAN addresses, tunnel identifiers and credentials must
  remain outside Git.

## Upstream relationship and licensing

This repository is an original deployment-wrapper project.

The upstream runtime is a separate checkout described by the machine-readable metadata
under `upstream/`; lifecycle commands verify the reviewed origin/commit before building.

Relevant upstream projects:

- [Firecrawl](https://github.com/firecrawl/firecrawl) — AGPL-3.0
- [Firecrawl MCP server](https://github.com/firecrawl/firecrawl-mcp-server) — MIT
- Firecrawl workflow skills — adapted portions retain their ISC notices

Original wrapper code in this repository is MIT-licensed. Third-party material retains
its upstream license. See [NOTICE.md](NOTICE.md) and [LICENSES/](LICENSES/) for provenance.

> **Public repository:** committed configuration contains placeholders/examples only.
> Real hostnames, identities, app/tunnel IDs, machine paths and secrets belong in ignored
> local configuration.

## Quick start

This repository contains the local additions, not the upstream Firecrawl source itself.

The basic lifecycle on an already configured machine is:

```powershell
.\fc.ps1 up
.\fc.ps1 status
.\fc.ps1 test
```

For a clean-machine restore, follow the next section in order; it pins the reviewed
upstream checkout and recreates local secret/configuration boundaries.

## Restore on a new machine

This repo holds only the local additions. Secrets and machine-specific files are gitignored.

1. Read `upstream/firecrawl.json`, then clone the exact reviewed tag from `https://github.com/firecrawl/firecrawl.git` into `firecrawl` and verify that `HEAD` equals the recorded commit.
2. Copy `.env.example` to `.env` and `public/gateway.env.example` to `public/gateway.env`; replace deployment placeholders locally and keep the real files out of Git.
3. Run `.\scripts\configure_service_secrets.ps1` once. PostgreSQL and SearXNG secrets are stored with Windows DPAPI under `%LOCALAPPDATA%\FirecrawlLocal\secrets` and injected into per-container tmpfs only at runtime.
4. If the isolated public-proxy fallback is wanted, create/copy a Webshare API key and run `.\scripts\configure_public_proxy.ps1`; the API key is used once and not stored, while the selected proxy credentials use the same DPAPI namespace and never enter the direct Firecrawl path.
5. If Gemini should be enabled, run `.\scripts\configure_gemini.ps1`. The API key uses the same DPAPI namespace and is injected into the LLM proxy's tmpfs runtime secret.
6. Configure the shared Cloudflare route and Access application with your hostname only in local deployment configuration; do not commit the real hostname.
7. Install the required local model/runtime dependencies described below.
8. Run `.\fc.ps1 up`, then `.\fc.ps1 test`.

## Isolated public-proxy fallback

The normal Firecrawl API and Playwright service stay **direct**. Optional third-party proxy credentials
are used only by a separate `public-proxy` profile:

- `playwright-router` receives browser-engine requests from the Firecrawl API.
- Non-allowlisted hosts always use the normal direct Playwright service.
- Allowlisted public hosts are tried direct first. On target HTTP 403/429 the router makes up to
  three bounded attempts through `playwright-public-proxy-service`.
- Webshare runtime traffic uses the Backbone endpoint `p.webshare.io:80` with the documented
  `-rotate` username parameter, so each retry can receive a different exit IP from the plan's pool.
- Requests carrying authentication/session headers fail closed before proxy routing.
- Proxy credentials are never stored in Git, `.env`, normal container environment, or the normal
  Firecrawl/Playwright path. They are stored with Windows DPAPI and injected into tmpfs for the
  isolated proxy browser only.

Configure once:

```powershell
.\scripts\configure_public_proxy.ps1
.\fc.ps1 redeploy
```

The configuration script accepts a **Webshare API key** via a secure prompt. It uses the key only
for the bootstrap call to Webshare's direct Proxy List API, tests the returned valid proxies against
Webshare's IP endpoint, and stores only proxy credentials under
`%LOCALAPPDATA%\FirecrawlLocal\secrets`. At runtime the launcher reuses those credentials with
Webshare's rotating Backbone endpoint; the API key itself is discarded and never persisted. This is
intentional because Webshare API keys have full account access.

The initial proxy hostname allowlist defaults to `curemydisease.com`. Extend
`FIRECRAWL_PUBLIC_PROXY_HOSTS` locally only for other **public research sites** that need the same
fallback. Never add authenticated supplier or financial-service hosts.

Global upstream `PROXY_SERVER`, `PROXY_USERNAME` and `PROXY_PASSWORD` values in `.env` are
explicitly rejected by the launcher so the direct stack cannot accidentally start routing all traffic
through a third party.


### Public-proxy observability

The router keeps only aggregate counters in the Docker named volume `public-proxy-metrics`. It never
stores request URLs, page content, headers, cookies or proxy credentials. The internal-only
`GET /metrics` response reports:

- `requests_total`
- `direct_success`
- `direct_blocked` (target HTTP 403/429)
- `proxy_attempts`
- `proxy_success`
- `proxy_exhausted`
- `sensitive_rejected`
- `upstream_errors`
- `total_latency_ms` and derived `avg_latency_ms`

The counters persist across router/container restarts so proxy usefulness can be measured over weeks
without retaining browsing history. The router itself is not published on a host/public port.

Every `.\fc.ps1 up` / `.\fc.ps1 redeploy` with the public-proxy profile also runs a one-shot
live fail-closed smoke test inside the Docker backend network. It sends a dummy Cookie header to an
allowlisted public URL and requires the router to reject it before either direct or proxy browsing.
The smoke also verifies that `sensitive_rejected` and `requests_total` increment while
`proxy_attempts` does not. The one-shot container is removed after the test.

## Local MCP proxy (Claude desktop, Claude Code, Codex)

`local-mcp/stdio-proxy.mjs` wraps the exact `firecrawl-mcp` version in `public/package.json` over stdio and applies the same rules as the public gateway
(`public/gateway/policy.mjs`). It exposes the five bounded Firecrawl tools plus nine local ephemeral-browser tools; `parse` and unsupported Firecrawl formats are refused, the server instructions come from
`public/gateway/instructions.md`, and scrapes that only ask for `query`/`json`/`summary` drop the page metadata
(a query answer is ~60 tokens instead of ~570). Markdown/html/links results keep their metadata.
Test: `node local-mcp/test-proxy.mjs` (stack must be running).

Registered in: Claude desktop (`%APPDATA%\Claude\claude_desktop_config.json`, key `firecrawl-mcp`), Claude Code
(user scope, below) and Codex (`[mcp_servers.firecrawl_mcp]`, command `node`).

MCP registration (user scope, done once):

```
claude mcp add-json firecrawl-mcp -s user '{"type":"stdio","command":"node","args":["<repo-path>/local-mcp/stdio-proxy.mjs"],"env":{"FIRECRAWL_API_URL":"http://127.0.0.1:3002"}}'
```

## Ephemeral stateful research browser

`browser-session-service` is a separate local Playwright/Chromium sidecar for tasks that need page state or request-level debugging rather than one-shot scraping. It reuses the reviewed Playwright image from the pinned upstream Firecrawl checkout but does not modify or vendor upstream Firecrawl source.

The browser surface is intentionally narrow: `firecrawl_browser_open`, `firecrawl_browser_list`, `firecrawl_browser_navigate`, `firecrawl_browser_tabs`, `firecrawl_browser_snapshot`, `firecrawl_browser_network`, `firecrawl_browser_console`, `firecrawl_browser_screenshot`, and `firecrawl_browser_close`. There is no arbitrary JavaScript/code-evaluation tool and no click/type/form-login API in this first version.

Security boundary:

- sessions are ephemeral browser contexts with no persistent profile and no imported credentials.
- only public HTTP(S) destinations are accepted; local/private IP space and credential-bearing URLs fail closed.
- subresource requests are checked again in the browser service, and the container is isolated from the Firecrawl backend on its own `browser_runtime` network.
- network output never includes headers, cookies, or request/response bodies; credential-like URL query values are redacted.
- downloads are cancelled, permissions are cleared, and session/tab counts plus TTL/inactivity limits are bounded.
- the service publishes only a loopback host port (`127.0.0.1:3010` by default) for local stdio clients. ChatGPT reaches it only through the Access-protected gateway over an internal `browser_edge` network.

This generic browser must not absorb authenticated supplier behavior. Supplier-specific sessions, URL rules, robots/pacing decisions, customer credentials and customer-price collection remain in the owning application (currently Inköpsplattformen).

## Public access for ChatGPT

`compose.public.yaml` adds the `mcp` and `gateway` containers and attaches the existing local `browser-session-service` to a dedicated internal gateway network. The browser service's only host publication comes from `compose.local.yaml` and is loopback-only.
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
- Gateway: the upstream Firecrawl surface is limited to scrape/map/search/crawl/check_crawl_status; `firecrawl_parse` is blocked because it can read arbitrary local files in this mode. The gateway additionally injects the bounded local `firecrawl_browser_*` tools and dispatches them only to the isolated browser sidecar. Firecrawl scrape/crawl requests asking for screenshot, branding, audio, or upstream browser `actions` are still rejected. The gateway defaults to 120 requests/min per caller IP (`RATE_PER_MIN`) with a 256 KB request cap.
- Server instructions: the gateway replaces the MCP `initialize` instructions with `public/gateway/instructions.md`
  (read on every connect, no restart needed). ChatGPT picks them up when the connector is refreshed ("Uppdatera").
  Keep it in line with the `firecrawl-mcp` core skill.
- Verify end to end: `node public/selftest.mjs (.\fc.ps1 url)`; watch traffic: `.\fc.ps1 logs`.
- Per-service public kill switch: stop the Firecrawl gateway container. Do not stop the shared `mcp-cloudflared`
  container unless you intend to disconnect all MCP gateways that use it.
- Docker Desktop must be running for any of this; enable "Start Docker Desktop when you sign in".

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
- Codex: `[mcp_servers.firecrawl_mcp]` in `~/.codex/config.toml` (stdio via `local-mcp/stdio-proxy.mjs`, bounded Firecrawl + browser tools, auto-approve), added by hand
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
- Upstream Firecrawl agent/interact remains unavailable in this self-hosted stack, but the wrapper now provides its own bounded ephemeral browser sidecar for stateful navigation, snapshots, sanitized network/console inspection and screenshots. Monitor still needs Firecrawl's account database; research/developer indexes remain unavailable; Firecrawl branding/screenshot scrape formats still need fire-engine. `firecrawl_extract` is deprecated in the MCP; use scrape with the json format.
- Upgrade: tag the current images for rollback (`docker tag firecrawl-api:latest firecrawl-api:<old>-backup`, same for
  `firecrawl-playwright-service` and `firecrawl-nuq-postgres`), then
  `git -C firecrawl fetch --depth 1 origin tag vX.Y.Z && git -C firecrawl checkout vX.Y.Z` and `.\fc.ps1 up`.
  Verify with `.\fc.ps1 test`, `node public/selftest.mjs (.\fc.ps1 url)` and `node llm/benchmark.mjs <model>`.
  Firecrawl publishes frequent patch tags (`v2.11.N`); GitHub "releases" lag far behind.
- Rollback to v2.11.0: `git -C firecrawl fetch --depth 1 origin tag v2.11.0 && git -C firecrawl checkout v2.11.0`, then
  `.\fc.ps1 up` (rebuilds from the old source; the `:v2.11.0-backup` images were deleted on 2026-09-16).
- Models kept (2026-09-16): `qwen2.5-16k` (active) and `gemma3-12b-16k` (reserve: same accuracy, slower; switch with
  `MODEL_NAME=gemma3-12b-16k` in `.env` + `.\fc.ps1 up`). `qwen2.5:14b` was removed after failing the benchmark.