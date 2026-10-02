import assert from 'node:assert/strict';
import test from 'node:test';
import { urlSafetyReason, unsupportedReason, rewriteResponse, compactToolDefinition, MAX_CRAWL_LIMIT, MAX_CRAWL_CONCURRENCY, MAX_INLINE_TOOL_RESULT_BYTES } from './policy.mjs';

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


test('oversized tool text is replaced by a structured response-budget descriptor', () => {
  const huge = {
    markdown: 'x'.repeat(MAX_INLINE_TOOL_RESULT_BYTES + 1024),
    metadata: { title: 'Large page', url: 'https://example.com/large', statusCode: 200 },
  };
  const message = {
    id: 77,
    result: {
      content: [{ type: 'text', text: JSON.stringify(huge) }],
      structuredContent: structuredClone(huge),
    },
  };
  const result = rewriteResponse(message, {
    allowedTools: new Set(),
    compactIds: new Set(),
    crawlRequests: new Map(),
  });
  const payload = JSON.parse(result.result.content[0].text);

  assert.equal(payload.truncated, true);
  assert.equal(payload.warning, 'RESULT_EXCEEDS_INLINE_BUDGET');
  assert.ok(payload.original_bytes > MAX_INLINE_TOOL_RESULT_BYTES);
  assert.equal(payload.max_inline_bytes, MAX_INLINE_TOOL_RESULT_BYTES);
  assert.equal(payload.source.url, 'https://example.com/large');
  assert.ok(payload.omitted_fields.includes('markdown'));
  assert.equal(result.result.structuredContent, undefined);
});

test('small tool text remains unchanged by the response-budget guard', () => {
  const text = JSON.stringify({ answer: 'small' });
  const message = { id: 78, result: { content: [{ type: 'text', text }] } };
  const result = rewriteResponse(message, {
    allowedTools: new Set(),
    compactIds: new Set(),
    crawlRequests: new Map(),
  });
  assert.equal(result.result.content[0].text, text);
});


test('tools/list compacts descriptions without changing schema semantics', () => {
  const original = {
    name: 'firecrawl_scrape',
    description: 'Very long upstream description '.repeat(100),
    inputSchema: {
      type: 'object',
      required: ['url'],
      properties: {
        url: {
          type: 'string',
          format: 'uri',
          description: 'A very long URL field description '.repeat(20),
        },
        formats: {
          type: 'array',
          items: { type: 'string', enum: ['markdown', 'html', 'query', 'json'] },
          description: 'A very long format field description '.repeat(20),
        },
      },
    },
  };
  const compact = compactToolDefinition(original);

  assert.ok(compact.description.length < 300);
  assert.ok(compact.inputSchema.properties.url.description.length <= 120);
  assert.deepEqual(compact.inputSchema.required, ['url']);
  assert.equal(compact.inputSchema.properties.url.format, 'uri');
  assert.deepEqual(
    compact.inputSchema.properties.formats.items.enum,
    ['markdown', 'html', 'query', 'json'],
  );
  assert.equal(original.description.startsWith('Very long upstream'), true);
});
