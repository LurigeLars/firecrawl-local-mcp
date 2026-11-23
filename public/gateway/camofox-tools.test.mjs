import test from 'node:test';
import assert from 'node:assert/strict';
import { CAMOFOX_TOOL_DEFINITION, appendCamofoxTool, callCamofoxTool } from './camofox-tools.mjs';

function stub(t, directStatus, directText, proxyText = null) {
  const old = globalThis.fetch, requests = [];
  t.after(() => { globalThis.fetch = old; });
  globalThis.fetch = async (url, opts) => {
    const u = new URL(url);
    const isProxy = u.hostname === 'camofox-proxy';
    requests.push({ url: String(url), method: opts.method, auth: opts.headers?.authorization, body: opts.body });
    if (opts.method === 'POST') return new Response(JSON.stringify({ tabId: 'tab_abc', httpStatus: isProxy ? 200 : directStatus }), { status: 200 });
    if (opts.method === 'DELETE') return new Response('{}', { status: 200 });
    return new Response(JSON.stringify({ snapshot: isProxy ? proxyText : directText }), { status: 200 });
  };
  return requests;
}
const setup = { accessKey: 'a'.repeat(64), directUrl: 'http://camofox-direct:9377',
  proxyUrl: 'http://camofox-proxy:9377', wait: async () => {} };

test('tool exposed exactly once only for allowlisted callers', () => {
  const m = { result: { tools: [] } };
  appendCamofoxTool(m, new Set());
  assert.equal(m.result.tools.length, 0);
  appendCamofoxTool(m, new Set([CAMOFOX_TOOL_DEFINITION.name]));
  appendCamofoxTool(m, new Set([CAMOFOX_TOOL_DEFINITION.name]));
  assert.equal(m.result.tools.length, 1);
});

test('returns a direct read, cleans session, never touches proxy for successful pages', async t => {
  const calls = stub(t, 200, 'A complete public page with main content.');
  const result = await callCamofoxTool({ url: 'https://5e.tools/adventure.html#kftgv', maxChars: 1000 }, setup);
  const payload = JSON.parse(result.content[0].text);
  assert.equal(payload.engine, 'camofox-direct');
  assert.equal(payload.proxyAttempted, false);
  assert.equal(payload.snapshot, 'A complete public page with main content.');
  assert.equal(calls.length, 3);
  assert.equal(calls[0].auth, 'Bearer ' + 'a'.repeat(64));
  assert.match(calls[2].url, /\/sessions\/firecrawl-read-/);
});

test('a 403 on 5e.tools takes a fresh isolated proxy session', async t => {
  const calls = stub(t, 403, 'Access denied', 'Golden Vault content loaded.');
  const result = await callCamofoxTool({ url: 'https://5e.tools/adventure.html#kftgv' }, setup);
  const payload = JSON.parse(result.content[0].text);
  assert.equal(payload.engine, 'camofox-webshare');
  assert.equal(payload.directBlocked, true);
  assert.equal(payload.snapshot, 'Golden Vault content loaded.');
  assert.equal(calls.filter(c => c.method === 'POST').length, 2);
  assert.equal(calls.filter(c => c.method === 'DELETE').length, 2);
});

test('a Cloudflare HTTP 200 challenge can trigger allowlisted fallback', async t => {
  stub(t, 200, 'Just a moment... checking your browser', 'Actual page');
  const result = await callCamofoxTool({ url: 'https://5e.tools/adventure.html#kftgv' }, setup);
  assert.equal(JSON.parse(result.content[0].text).engine, 'camofox-webshare');
});

test('a block on unrelated hosts does not leak through rotating proxy', async t => {
  const calls = stub(t, 403, 'Access denied');
  const result = await callCamofoxTool({ url: 'https://example.com' }, setup);
  assert.equal(JSON.parse(result.content[0].text).proxyAttempted, false);
  assert.equal(calls.filter(c => c.method === 'POST').length, 1);
});

test('invalid URL is rejected before any HTTP request', async t => {
  const old = globalThis.fetch, calls = [];
  t.after(() => { globalThis.fetch = old; });
  globalThis.fetch = async (...args) => { calls.push(args); throw Error('unexpected'); };
  const result = await callCamofoxTool({ url: 'file:///etc/passwd' }, setup);
  assert.equal(result.isError, true);
  assert.equal(calls.length, 0);
});
