import assert from 'node:assert/strict';
import test from 'node:test';
import { urlSafetyReason, unsupportedReason, rewriteResponse, MAX_CRAWL_LIMIT, MAX_CRAWL_CONCURRENCY } from './policy.mjs';

const params = url => ({ name: 'firecrawl_scrape', arguments: { url } });

for (const url of [
  'http://127.0.0.1/',
  'http://10.0.0.1/',
  'http://100.64.0.1/',
  'http://169.254.169.254/latest/meta-data/',
  'http://172.16.0.1/',
  'http://192.168.1.1/',
  'http://[::1]/',
  'http://[fe80::1]/',
  'http://[fc00::1]/',
  'http://[::ffff:10.0.0.1]/',
]) {
  test(`blocks non-public destination ${url}`, async () => {
    assert.match(await urlSafetyReason(params(url)), /Private or local/);
  });
}

test('allows a public IPv4 literal', async () => {
  assert.equal(await urlSafetyReason(params('https://93.184.216.34/')), null);
});

test('rejects non-HTTP protocols', async () => {
  assert.match(await urlSafetyReason(params('file:///etc/passwd')), /HTTP/);
});

test('rejects URL credentials', async () => {
  assert.match(await urlSafetyReason(params('https://user:pass@93.184.216.34/')), /credentials/);
});

test('does not apply URL policy to search-only calls', async () => {
  assert.equal(await urlSafetyReason({ name: 'firecrawl_search', arguments: { query: 'example' } }), null);
});


test('requires explicit bounded crawl size and concurrency', () => {
  assert.match(unsupportedReason({ name: 'firecrawl_crawl', arguments: { url: 'https://example.com' } }), /explicit integer limit/);
  assert.match(unsupportedReason({ name: 'firecrawl_crawl', arguments: { url: 'https://example.com', limit: MAX_CRAWL_LIMIT + 1, maxConcurrency: 1 } }), /explicit integer limit/);
  assert.match(unsupportedReason({ name: 'firecrawl_crawl', arguments: { url: 'https://example.com', limit: 25, maxConcurrency: MAX_CRAWL_CONCURRENCY + 1 } }), /maxConcurrency/);
  assert.equal(unsupportedReason({ name: 'firecrawl_crawl', arguments: { url: 'https://example.com', limit: 25, maxConcurrency: 4 } }), null);
});

test('advertises the local crawl ceilings in tools/list', () => {
  const message = {
    result: {
      tools: [{
        name: 'firecrawl_crawl',
        inputSchema: {
          type: 'object',
          properties: {
            limit: { type: 'integer' },
            maxConcurrency: { type: 'integer' },
          },
        },
      }],
    },
  };
  const result = rewriteResponse(message, {
    allowedTools: new Set(['firecrawl_crawl']),
    compactIds: new Set(),
  });
  const schema = result.result.tools[0].inputSchema;
  assert.equal(schema.properties.limit.maximum, MAX_CRAWL_LIMIT);
  assert.equal(schema.properties.maxConcurrency.maximum, MAX_CRAWL_CONCURRENCY);
  assert.ok(schema.required.includes('limit'));
  assert.ok(schema.required.includes('maxConcurrency'));
});
