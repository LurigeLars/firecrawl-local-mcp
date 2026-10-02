import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const localCompose = readFileSync(new URL('../compose.local.yaml', import.meta.url), 'utf8');
const publicCompose = readFileSync(new URL('../compose.public.yaml', import.meta.url), 'utf8');
const service = readFileSync(new URL('./service.mjs', import.meta.url), 'utf8');
const tools = readFileSync(new URL('../public/gateway/browser-tools.mjs', import.meta.url), 'utf8');

function serviceBlock(source, name) {
  const lines = source.split('\n');
  const start = lines.findIndex(line => line === `  ${name}:`);
  assert.notEqual(start, -1, `missing service ${name}`);
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[i])) { end = i; break; }
  }
  return lines.slice(start, end).join('\n');
}

test('browser sidecar is loopback-only and isolated from Firecrawl backend', () => {
  const block = serviceBlock(localCompose, 'browser-session-service');
  assert.match(block, /127\.0\.0\.1:\$\{BROWSER_SESSION_HOST_PORT:-3010\}:3010/);
  assert.match(block, /networks:\s*\[browser_dualstack\]/);
  assert.doesNotMatch(block, /networks:\s*\[backend\]/);
  assert.match(block, /read_only:\s*true/);
  assert.match(block, /cap_drop:\s*\n\s+- ALL/);
  assert.match(block, /no-new-privileges:true/);
  assert.match(localCompose, /browser_dualstack:\s*\n\s+driver:\s+bridge\s*\n\s+enable_ipv6:\s*true/);
});

test('ordinary Firecrawl Playwright shares the hardened dual-stack browser egress network', () => {
  const block = serviceBlock(localCompose, 'playwright-service');
  assert.match(block, /networks:\s*\[backend, browser_dualstack\]/);
});

test('browser services use Cloudflare Gateway resolvers for filtered dual-stack DNS', () => {
  for (const name of ['browser-session-service', 'playwright-service']) {
    const block = serviceBlock(localCompose, name);
    assert.match(block, /dns:\s*\n\s+- 172\.64\.36\.1\s*\n\s+- 172\.64\.36\.2/);
  }
});

test('public gateway reaches browser only through dedicated internal edge network', () => {
  const browser = serviceBlock(publicCompose, 'browser-session-service');
  const gateway = serviceBlock(publicCompose, 'gateway');
  assert.match(browser, /networks:\s*\[browser_dualstack, browser_edge\]/);
  assert.match(gateway, /browser_edge:\s*\{\}/);
  assert.match(gateway, /BROWSER_SERVICE_URL:\s*http:\/\/browser-session-service:3010/);
  assert.match(gateway, /firecrawl_browser_network/);
  assert.match(publicCompose, /browser_edge:[\s\S]*?internal:\s*true/);
});

test('browser implementation keeps network evidence metadata-only', () => {
  assert.doesNotMatch(service, /request\.headers\s*\(/);
  assert.doesNotMatch(service, /response\.headers\s*\(/);
  assert.doesNotMatch(service, /postData|request\.body|response\.body/);
  assert.match(service, /redactUrl\(request\.url\(\)\)/);
  assert.match(service, /acceptDownloads:\s*false/);
  assert.match(service, /clearPermissions/);
  assert.match(service, /MAX_URL_CHARS = 8192/);
  assert.match(service, /MAX_SCREENSHOT_BYTES = 4 \* 1024 \* 1024/);
  assert.match(service, /fullPage: false/);
});

test('generic browser contains no supplier-specific session policy', () => {
  const combined = `${service}\n${tools}`.toLowerCase();
  for (const forbidden of ['season-ms', 'season-spendrups', 'martinservera', 'spendrups', 'menigo']) {
    assert.equal(combined.includes(forbidden), false, `generic browser contains supplier-specific term: ${forbidden}`);
  }
});

test('first browser surface has no arbitrary code or form-interaction tool', () => {
  for (const forbidden of ['firecrawl_browser_evaluate', 'firecrawl_browser_click', 'firecrawl_browser_type', 'firecrawl_browser_upload', 'fullPage']) {
    assert.equal(tools.includes(forbidden), false);
  }
  assert.match(tools, /firecrawl_browser_snapshot/);
  assert.match(tools, /firecrawl_browser_network/);
  assert.match(tools, /firecrawl_browser_screenshot/);
});


test('positive DNS safety verdicts are never cached', () => {
  assert.match(service, /if \(reason\) hostVerdicts\.set\(host,/);
  assert.match(service, /else hostVerdicts\.delete\(host\)/);
  assert.doesNotMatch(service, /reason \? 60_000 : 5_000/);
});
