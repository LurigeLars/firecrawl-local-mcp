import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');

const manifest = JSON.parse(read('public/package.json'));
const lock = JSON.parse(read('public/package-lock.json'));
const dockerfile = read('public/Dockerfile.mcp');
const gatewayDockerfile = read('public/Dockerfile.gateway');
const publicCompose = read('compose.public.yaml');
const proxy = read('local-mcp/stdio-proxy.mjs');
const pin = JSON.parse(read('upstream/firecrawl.json'));
const launcher = read('fc.ps1');
const composeLocal = read('compose.local.yaml');
const redisDockerfile = read('security/Dockerfile.redis-security');
const firecrawlApiHotfix = read('security/firecrawl-api-runtime-hotfix.Dockerfile');

test('public gateway and MCP runtime strip unused vulnerable npm CLI and pin patched Alpine image', () => {
  for (const source of [dockerfile, gatewayDockerfile]) {
    assert.match(source, /^FROM node:26\.12\.1-alpine@sha256:40cd011b55415bc894e45b02fdd5a5dfde056921b7d23a64c74f80497af48797$/m);
    assert.match(source, /apk upgrade --no-cache/);
    assert.match(source, /rm -rf \/usr\/local\/lib\/node_modules\/npm/);
    assert.match(source, /test ! -e \/usr\/local\/lib\/node_modules\/npm/);
  }
  assert.match(publicCompose, /gateway:[\s\S]*image: firecrawl-gateway:local\n\s+build:[\s\S]*dockerfile: Dockerfile\.gateway/);
});

test('Redis runtime uses patched zlib without exposing wrapper secrets to Docker build', () => {
  assert.match(redisDockerfile, /^FROM redis:8\.10\.2-alpine@sha256:[0-9a-f]{64}$/m);
  assert.match(redisDockerfile, /apk upgrade --no-cache zlib/);
  assert.match(redisDockerfile, /grep -qvFx 'zlib-1\.3\.2-r0'/);
  assert.match(composeLocal, /redis:[\s\S]*image: firecrawl-redis:patched\n\s+build:[\s\S]*context: \.\.\/security/);
});

test('MCP and HTTP transports are locked above disclosed redirect, SSRF and isolation CVEs', () => {
  const fixed = {
    '@modelcontextprotocol/client': '2.3.0',
    '@modelcontextprotocol/core': '2.3.0',
    '@modelcontextprotocol/sdk': '1.32.0',
    '@modelcontextprotocol/server': '2.3.0',
    'follow-redirects': '1.16.1',
    'axios': '1.20.0',
  };
  for (const [name, version] of Object.entries(fixed)) {
    assert.equal(manifest.overrides?.[name], version, `${name} must be constrained in the manifest`);
    const resolved = lock.packages?.[`node_modules/${name}`];
    if (!resolved) {
      // firecrawl-mcp 3.36.x no longer brings in these optional MCP packages.
      // Keep the patched override for any future reintroduction, while requiring
      // core/server and all HTTP transport packages to be installed and checked.
      assert.ok(['@modelcontextprotocol/client', '@modelcontextprotocol/sdk'].includes(name),
        `${name} must resolve to the patched version if used`);
      continue;
    }
    assert.equal(resolved.version, version, `${name} must resolve to the patched version`);
    assert.match(resolved.integrity ?? '', /^sha512-/);
  }
});

test('firecrawl-mcp has one exact manifest version source and reproducible public install', () => {
  const version = manifest?.dependencies?.['firecrawl-mcp'];
  assert.match(version, /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/);
  assert.equal(lock.lockfileVersion, 3);
  assert.equal(lock?.packages?.['']?.dependencies?.['firecrawl-mcp'], version);
  assert.equal(lock?.packages?.['node_modules/firecrawl-mcp']?.version, version);
  assert.equal(
    lock?.packages?.['node_modules/firecrawl-mcp']?.resolved,
    `https://registry.npmjs.org/firecrawl-mcp/-/firecrawl-mcp-${version}.tgz`,
  );
  assert.match(lock?.packages?.['node_modules/firecrawl-mcp']?.integrity ?? '', /^sha512-/);
  assert.match(dockerfile, /COPY package\.json package-lock\.json \.\//);
  assert.match(dockerfile, /npm ci --ignore-scripts --omit=dev --no-audit --no-fund/);
  assert.doesNotMatch(dockerfile, /npm install/);
  assert.doesNotMatch(dockerfile, /npm ci(?![^\n]*--ignore-scripts)/);
  assert.doesNotMatch(dockerfile, /firecrawl-mcp@\d/);
  assert.match(proxy, /public\/package\.json/);
  assert.doesNotMatch(proxy, /firecrawl-mcp@\d/);
});

test('upstream Firecrawl pin is machine-readable and enforced before builds', () => {
  assert.equal(pin.repository, 'firecrawl/firecrawl');
  assert.equal(pin.tag, 'v2.11.642');
  assert.equal(pin.commit, '7e6bd701febaacc3bb95c0fb7eee2c509a4813c5');
  assert.match(pin.tag, /^v\d+\.\d+\.\d+$/);
  assert.match(pin.commit, /^[0-9a-f]{40}$/);
  assert.match(launcher, /upstream\\firecrawl\.json/);
  assert.match(launcher, /Assert-UpstreamFirecrawlPin/);
  assert.match(launcher, /Unexpected upstream Firecrawl origin/);
  assert.match(launcher, /safe\.directory=\$safeCheckout/);
  assert.match(launcher, /Get-Command git\.exe/);
  assert.match(launcher, /\$originExit = \$LASTEXITCODE/);
  assert.match(launcher, /\$statusExit = \$LASTEXITCODE/);
  assert.match(launcher, /sync-upstream/);
  assert.match(launcher, /Assert-UpstreamFirecrawlPin -AllowHeadMismatch/);
  assert.match(launcher, /fetch --force origin "refs\/tags\/\$\(\$pin\.tag\):refs\/tags\/\$\(\$pin\.tag\)"/);
  assert.match(launcher, /rev-parse "\$\(\$pin\.tag\)\^\{\}"/);
  assert.match(launcher, /checkout --detach \(\[string\]\$pin\.commit\)/);
  assert.doesNotMatch(launcher, /reset --hard/);
});


test('temporary Firecrawl API security build is narrow and pinned', () => {
  assert.match(composeLocal, /context:\s+apps\/api/);
  assert.match(composeLocal, /firecrawl-api-runtime-hotfix\.Dockerfile/);
  assert.match(firecrawlApiHotfix, /FROM golang:1\.25\.14@sha256:[0-9a-f]{64} AS go-build/);
  assert.match(firecrawlApiHotfix, /ENV GOTOOLCHAIN=local/);
  assert.match(firecrawlApiHotfix, /go1\.26\.10\.linux-/);
  assert.match(firecrawlApiHotfix, /ccfe7ae822d2c2f41f4fe7c956199967a4dac22d0ee0d2b2bfdb440aab3b20c5/);
  assert.match(firecrawlApiHotfix, /sha256sum -c -/);
  assert.match(firecrawlApiHotfix, /go get golang\.org\/x\/net@v0\.60\.0/);
  assert.match(firecrawlApiHotfix, /go list -m golang\.org\/x\/net \| grep -Fx 'golang\.org\/x\/net v0\.60\.0'/);
  assert.match(firecrawlApiHotfix, /go version \| grep -F 'go1\.26\.10'/);
  assert.match(firecrawlApiHotfix, /go version -m libhtml-to-markdown\.so[\s\S]*golang\.org\/x\/net[\s\S]*v0\.60\.0/);
  assert.match(firecrawlApiHotfix, /FROM runtime-base AS build-base/);
  assert.match(firecrawlApiHotfix, /FROM build-base AS build/);
  assert.match(firecrawlApiHotfix, /FROM runtime-base AS runtime/);
  assert.match(firecrawlApiHotfix, /FROM runtime AS verify-go-runtime/);
  assert.match(firecrawlApiHotfix, /\/usr\/local\/go\/bin\/go version -m "\$f"/);
  assert.match(firecrawlApiHotfix, /grep -v -F 'v0\.60\.0'/);
  assert.match(firecrawlApiHotfix, /runtime contains Go binaries built with toolchains vulnerable to CVE-2026-39821/);
  assert.match(firecrawlApiHotfix, /minor === 26 && patch >= 10/);
  assert.match(firecrawlApiHotfix, /FROM runtime AS final/);
  assert.match(firecrawlApiHotfix, /GO_RUNTIME_MODULES\.txt/);
  assert.match(firecrawlApiHotfix, /FROM node:22\.23\.4-slim@sha256:[0-9a-f]{64} AS runtime-base/);
  assert.match(firecrawlApiHotfix, /pnpm install --frozen-lockfile/);
  assert.match(firecrawlApiHotfix, /proxy-addr@>=2\.0\.0 <2\.0\.8/);
  assert.match(firecrawlApiHotfix, /source-map-js@>=1\.0\.0 <1\.2\.2/);
  assert.match(firecrawlApiHotfix, /smol-toml@>=1\.0\.0 <1\.9\.0/);
  assert.match(firecrawlApiHotfix, /shell-quote@<1\.11\.0/);
  assert.match(firecrawlApiHotfix, /undici: "7\.30\.1"/);
  assert.match(firecrawlApiHotfix, /"undici": "7\.30\.1"/);
  assert.doesNotMatch(firecrawlApiHotfix, /pnpm install --no-frozen-lockfile/);
  assert.doesNotMatch(firecrawlApiHotfix, /undici: "7\.29\.1"/);
  assert.match(firecrawlApiHotfix, /CVE-2026-59873 is fixed in tar >= 7\.5\.19/);
  assert.match(firecrawlApiHotfix, /vulnerable node-tar/);
  assert.match(firecrawlApiHotfix, /TypeScript 7 is only used by development\/watch commands/);
  assert.match(firecrawlApiHotfix, /test ! -e node_modules\/typescript-7/);
  assert.doesNotMatch(firecrawlApiHotfix, /rm -rf \/usr\/local\/lib\/node_modules\/npm/);
});
