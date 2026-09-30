import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');

const manifest = JSON.parse(read('public/package.json'));
const dockerfile = read('public/Dockerfile.mcp');
const proxy = read('local-mcp/stdio-proxy.mjs');
const pin = JSON.parse(read('upstream/firecrawl.json'));
const launcher = read('fc.ps1');

test('firecrawl-mcp has one exact manifest version source', () => {
  const version = manifest?.dependencies?.['firecrawl-mcp'];
  assert.match(version, /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/);
  assert.match(dockerfile, /COPY package\.json/);
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
});
