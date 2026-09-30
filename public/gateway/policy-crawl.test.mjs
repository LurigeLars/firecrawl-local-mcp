import test from 'node:test';
import assert from 'node:assert/strict';

import { parseAllowedTools, rememberCrawlRequest, rewriteResponse } from './policy.mjs';

const allowedTools = parseAllowedTools();

function rewriteCall(request, payload, structuredContent = payload) {
  const crawlRequests = new Map();
  assert.equal(rememberCrawlRequest(request, crawlRequests), true);
  const message = {
    jsonrpc: '2.0',
    id: request.id,
    result: {
      content: [{ type: 'text', text: JSON.stringify(payload) }],
      structuredContent: structuredContent ? structuredClone(structuredContent) : undefined,
    },
  };
  const out = rewriteResponse(message, {
    allowedTools,
    compactIds: new Set(),
    crawlRequests,
  });
  assert.equal(crawlRequests.size, 0);
  return out;
}

test('crawl text carries bounded-job evidence without claiming site completeness', () => {
  const request = {
    method: 'tools/call',
    id: 41,
    params: {
      name: 'firecrawl_crawl',
      arguments: { url: 'https://example.com/docs', limit: 10, maxConcurrency: 2 },
    },
  };
  const payload = {
    id: 'crawl-1',
    status: 'completed',
    completed: 10,
    total: 10,
    data: [{ metadata: { statusCode: 200 } }],
  };
  const out = rewriteCall(request, payload);
  const textPayload = JSON.parse(out.result.content[0].text);
  const evidence = textPayload.localCrawlEvidence;

  assert.equal(evidence.semantics, 'bounded-firecrawl-job');
  assert.equal(evidence.siteCoverage, 'NOT_PROVEN');
  assert.equal(evidence.boundedJobCompleted, true);
  assert.equal(evidence.jobCountsReconciled, true);
  assert.equal(evidence.requestedLimit, 10);
  assert.equal(evidence.requestedLimitBoundaryReached, true);
  assert.equal(evidence.requestedMaxConcurrency, 2);
  assert.equal(evidence.requestedDelaySeconds, null);
  assert.equal(evidence.resultPageHasMore, false);
  assert.deepEqual(evidence.returnedDataHttpSignals, {
    returnedDocuments: 1,
    rateLimited429: 0,
    forbidden403: 0,
    server5xx: 0,
    documentErrors: 0,
  });

  assert.equal(out.result.structuredContent.localCrawlEvidence, undefined);
});

test('crawl evidence exposes pressure signals only from the returned data page', () => {
  const request = {
    method: 'tools/call',
    id: 'crawl-pressure',
    params: {
      name: 'firecrawl_crawl',
      arguments: { url: 'https://example.com', limit: 25, maxConcurrency: 1, delay: 2 },
    },
  };
  const payload = {
    status: 'completed',
    completed: 4,
    total: 5,
    next: 'https://firecrawl.local/result-page-2',
    data: [
      { metadata: { statusCode: 429 } },
      { metadata: { statusCode: 403 } },
      { metadata: { statusCode: 503 }, error: 'upstream unavailable' },
      { metadata: { statusCode: 200 } },
    ],
  };
  const evidence = JSON.parse(rewriteCall(request, payload).result.content[0].text).localCrawlEvidence;

  assert.equal(evidence.jobCountsReconciled, false);
  assert.equal(evidence.requestedLimitBoundaryReached, false);
  assert.equal(evidence.requestedDelaySeconds, 2);
  assert.equal(evidence.resultPageHasMore, true);
  assert.deepEqual(evidence.returnedDataHttpSignals, {
    returnedDocuments: 4,
    rateLimited429: 1,
    forbidden403: 1,
    server5xx: 1,
    documentErrors: 1,
  });
});

test('crawl status gets evidence without inventing original request bounds', () => {
  const request = {
    method: 'tools/call',
    id: 9,
    params: {
      name: 'firecrawl_check_crawl_status',
      arguments: { id: 'crawl-1' },
    },
  };
  const payload = { status: 'scraping', completed: 3, total: 7, data: [] };
  const evidence = JSON.parse(rewriteCall(request, payload).result.content[0].text).localCrawlEvidence;

  assert.equal(evidence.siteCoverage, 'NOT_PROVEN');
  assert.equal(evidence.boundedJobCompleted, false);
  assert.equal(evidence.requestedLimit, null);
  assert.equal(evidence.requestedLimitBoundaryReached, null);
  assert.equal(evidence.requestedMaxConcurrency, null);
});

test('non-crawl calls are not tracked', () => {
  const requests = new Map();
  assert.equal(rememberCrawlRequest({
    method: 'tools/call',
    id: 1,
    params: { name: 'firecrawl_scrape', arguments: { url: 'https://example.com' } },
  }, requests), false);
  assert.equal(requests.size, 0);
});
