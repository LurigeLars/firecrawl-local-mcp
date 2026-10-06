import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');

const manifest = JSON.parse(read('public/package.json'));
const lock = JSON.parse(read('public/package-lock.json'));
const dockerfile = read('public/Dockerfile.mcp');
const proxy = read('local-mcp/stdio-proxy.mjs');
const pin = JSON.parse(read('upstream/firecrawl.json'));
const launcher = read('fc.ps1');
const composeLocal = read('compose.local.yaml');
const firecrawlApiHotfix = read('security/firecrawl-api-runtime-hotfix.Dockerfile');

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
  assert.match(firecrawlApiHotfix, /go get golang\.org\/x\/net@v0\.58\.0/);
  assert.match(firecrawlApiHotfix, /go list -m golang\.org\/x\/net \| grep -Fx 'golang\.org\/x\/net v0\.58\.0'/);
  assert.match(firecrawlApiHotfix, /go version \| grep -F 'go1\.25\.14'/);
  assert.match(firecrawlApiHotfix, /go version -m libhtml-to-markdown\.so[\s\S]*golang\.org\/x\/net[\s\S]*v0\.58\.0/);
  assert.match(firecrawlApiHotfix, /FROM runtime-base AS build-base/);
  assert.match(firecrawlApiHotfix, /FROM build-base AS build/);
  assert.match(firecrawlApiHotfix, /FROM runtime-base AS runtime/);
  assert.match(firecrawlApiHotfix, /FROM runtime AS verify-go-runtime/);
  assert.match(firecrawlApiHotfix, /\/usr\/local\/go\/bin\/go version -m "\$f"/);
  assert.match(firecrawlApiHotfix, /grep -v -F 'v0\.58\.0'/);
  assert.match(firecrawlApiHotfix, /runtime contains Go binaries built with toolchains vulnerable to CVE-2026-39821/);
  assert.match(firecrawlApiHotfix, /minor === 25 && patch >= 13/);
  assert.match(firecrawlApiHotfix, /minor === 26 && patch >= 6/);
  assert.match(firecrawlApiHotfix, /FROM runtime AS final/);
  assert.match(firecrawlApiHotfix, /GO_RUNTIME_MODULES\.txt/);
  assert.match(firecrawlApiHotfix, /FROM node:22\.23\.3-slim@sha256:[0-9a-f]{64} AS runtime-base/);
  assert.match(firecrawlApiHotfix, /pnpm-workspace\.yaml/);
  assert.match(firecrawlApiHotfix, /proxy-addr: \"2\.0\.8\"/);
  assert.match(firecrawlApiHotfix, /source-map-js: \"1\.2\.2\"/);
  assert.match(firecrawlApiHotfix, /undici: \"7\.29\.1\"/);
  assert.match(firecrawlApiHotfix, /smol-toml@>=1\.0\.0 <1\.9\.0.*1\.9\.0/);
  assert.match(firecrawlApiHotfix, /shell-quote@<1\.11\.0.*1\.11\.0/);
  assert.match(firecrawlApiHotfix, /\"undici\": \"7\.29\.1\"/);
  assert.match(firecrawlApiHotfix, /source-map-js@1\\\.2\\\.2/);
  assert.match(firecrawlApiHotfix, /! grep -q '\^  source-map-js@1\\\.2\\\.1:' pnpm-lock\.yaml/);
  assert.match(firecrawlApiHotfix, /! grep -q '\^  proxy-addr@2\\\.0\\\.7:' pnpm-lock\.yaml/);
  assert.match(firecrawlApiHotfix, /smol-toml@1\\\.9\\\.0/);
  assert.match(firecrawlApiHotfix, /! grep -q '\^  smol-toml@1\\\.8\\\.0:' pnpm-lock\.yaml/);
  assert.match(firecrawlApiHotfix, /shell-quote@1\\\.11\\\.0/);
  assert.match(firecrawlApiHotfix, /! grep -q '\^  shell-quote@1\\\.10\\\.0:' pnpm-lock\.yaml/);
  assert.match(firecrawlApiHotfix, /undici@7\\\.29\\\.1/);
  assert.match(firecrawlApiHotfix, /! grep -q '\^  undici@7\\\.30\\\.0:' pnpm-lock\.yaml/);
  assert.match(firecrawlApiHotfix, /--mount=type=cache,target=\/app\/native\/target[\s\S]*pnpm install --no-frozen-lockfile/);
  assert.match(firecrawlApiHotfix, /CVE-2026-59873 is fixed in tar >= 7\.5\.19/);
  assert.match(firecrawlApiHotfix, /vulnerable node-tar/);
  assert.match(firecrawlApiHotfix, /TypeScript 7 is only used by development\/watch commands/);
  assert.match(firecrawlApiHotfix, /node_modules\/\.pnpm\/@typescript\+typescript-linux-x64@7\.0\.2/);
  assert.match(firecrawlApiHotfix, /test ! -e node_modules\/typescript-7/);
  assert.doesNotMatch(firecrawlApiHotfix, /rm -rf \/usr\/local\/lib\/node_modules\/npm/);
});
