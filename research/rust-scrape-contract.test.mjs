// Experimental contract only. Not imported by either live gateway.
// Pinned engine: firecrawl/firecrawl 283bc7c662cf7b14801eb8b6f82b1b26533d8008.
import assert from 'node:assert/strict';
import test from 'node:test';

const keys = new Set(['url', 'formats', 'onlyMainContent', 'maxAge', 'storeInCache', 'timeout']);

// A positive allowlist, not a permissive translation of unsupported options.
// The existing public-URL/DNS policy MUST still run before dispatch. This
// predicate is not an SSRF filter and must never authorize a destination.
function candidateRequest(body) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return null;
  if (Object.keys(body).some(key => !keys.has(key))) return null;
  if (typeof body.url !== 'string' || body.url.length > 8192) return null;
  let url;
  try { url = new URL(body.url); } catch { return null; }
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.port) return null;
  if (!Array.isArray(body.formats) || body.formats.length !== 1 || body.formats[0] !== 'markdown') return null;
  // Explicit uncached fetch only. Do not silently replace default cache behavior.
  if (body.maxAge !== 0 || body.storeInCache !== false) return null;
  if (body.onlyMainContent !== true) return null;
  if (!Number.isInteger(body.timeout) || body.timeout < 1 || body.timeout > 45000) return null;
  return { url: body.url, formats: ['markdown'], onlyMainContent: true, timeout: body.timeout };
}

// Transport-level success is necessary, NOT evidence of complete content.
// A reviewed source/quality check and total request deadline are separate gates.
function acceptableResponse(payload) {
  const data = payload?.data;
  return payload?.success === true && data?.source === 'local' &&
    data?.blocked !== true && !data?.warning && !data?.error &&
    data?.metadata?.statusCode === 200 &&
    typeof data?.markdown === 'string' && data.markdown.trim().length > 0;
}

const basic = Object.freeze({ url: 'https://www.iana.org/help/example-domains', formats: ['markdown'], onlyMainContent: true, maxAge: 0, storeInCache: false, timeout: 30000 });

test('only the measured static uncached contract is translated', () => {
  assert.deepEqual(candidateRequest(basic), { url: basic.url, formats: ['markdown'], onlyMainContent: true, timeout: 30000 });
  assert.equal(candidateRequest({ ...basic, timeout: 45000 })?.timeout, 45000);
  assert.equal(basic.maxAge, 0);
});

test('all advanced, sensitive and unreviewed parameters stay on existing implementation', () => {
  for (const patch of [
    { formats: ['json'] }, { formats: ['markdown', 'links'] }, { formats: ['query'] }, { formats: ['summary'] },
    { formats: [{ type: 'markdown' }] }, { maxAge: 60000 }, { maxAge: undefined }, { storeInCache: true },
    { storeInCache: undefined }, { onlyMainContent: false }, { onlyMainContent: undefined },
    { timeout: undefined }, { timeout: 0 }, { timeout: 45001 }, { timeout: '30000' },
    { headers: { Authorization: 'dummy' } }, { headers: {} }, { proxy: 'basic' }, { waitFor: 0 },
    { actions: [] }, { parsers: ['pdf'] }, { pdfOptions: { maxPages: 1 } }, { includeTags: [] },
    { excludeTags: [] }, { mobile: false }, { skipTlsVerification: false }, { profile: {} },
    { zeroDataRetention: true }, { redactPII: true }, { lockdown: false }, { requestId: 'dummy' },
    { alexandria: {} }, { unknownFutureOption: true }, { url: 'https://example.com/?token=dummy' },
    { url: 'https://dummy@example.com/' }, { url: 'http://example.com/' }, { url: 'https://example.com:8443/' },
  ]) assert.equal(candidateRequest({ ...basic, ...patch }), null, JSON.stringify(patch));
});

test('does not accept blocks, partial warnings, cloud responses or missing target status', () => {
  const good = { success: true, data: { source: 'local', markdown: 'Example Domains', metadata: { statusCode: 200 } } };
  assert.equal(acceptableResponse(good), true);
  for (const patch of [
    { source: 'firecrawl-cloud' }, { source: undefined }, { blocked: true }, { warning: 'partial' },
    { error: 'failed' }, { markdown: '' }, { markdown: '  ' }, { markdown: null },
    { metadata: {} }, { metadata: { statusCode: 403 } }, { metadata: { statusCode: 429 } },
  ]) assert.equal(acceptableResponse({ ...good, data: { ...good.data, ...patch } }), false);
  assert.equal(acceptableResponse({ ...good, success: false }), false);
});
