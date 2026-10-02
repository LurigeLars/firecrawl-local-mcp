import test from 'node:test';
import assert from 'node:assert/strict';
import { BROWSER_TOOL_DEFINITIONS, BROWSER_TOOL_NAMES, appendBrowserTools, callBrowserTool, isBrowserTool } from './browser-tools.mjs';

test('browser tool names are unique and discoverable', () => {
  assert.equal(BROWSER_TOOL_NAMES.length, new Set(BROWSER_TOOL_NAMES).size);
  assert(BROWSER_TOOL_NAMES.includes('firecrawl_browser_network'));
  assert(BROWSER_TOOL_NAMES.includes('firecrawl_browser_close'));
  assert(BROWSER_TOOL_DEFINITIONS.every(tool => tool.inputSchema?.type === 'object'));
  assert.equal(isBrowserTool('firecrawl_browser_snapshot'), true);
  assert.equal(isBrowserTool('firecrawl_scrape'), false);
});

test('appendBrowserTools respects the configured allowlist and avoids duplicates', () => {
  const allowed = new Set(['firecrawl_scrape', 'firecrawl_browser_network']);
  const msg = { result: { tools: [{ name: 'firecrawl_scrape', inputSchema: { type: 'object' } }] } };
  appendBrowserTools(msg, allowed);
  assert.deepEqual(msg.result.tools.map(tool => tool.name), ['firecrawl_scrape', 'firecrawl_browser_network']);
  appendBrowserTools(msg, allowed);
  assert.deepEqual(msg.result.tools.map(tool => tool.name), ['firecrawl_scrape', 'firecrawl_browser_network']);
});

test('browser adapter maps a network read to the local session service', async t => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let seen;
  globalThis.fetch = async (url, init) => {
    seen = { url: String(url), init };
    return new Response(JSON.stringify({ sessionId: 'browser_a1', entries: [] }), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  const result = await callBrowserTool('http://127.0.0.1:3010', 'firecrawl_browser_network', {
    sessionId: 'browser_a1', tabId: 'tab_b2', limit: 999,
  });
  assert.match(seen.url, /\/sessions\/browser_a1\/network\?tabId=tab_b2&limit=200$/);
  assert.equal(seen.init.method, 'GET');
  assert.equal(result.isError, undefined);
});

test('browser adapter returns screenshot as MCP image content', async t => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  globalThis.fetch = async () => new Response(JSON.stringify({
    sessionId: 'browser_a1', tabId: 'tab_b2', url: 'https://example.com/', mimeType: 'image/png', data: 'aGVsbG8=',
  }), { status: 200, headers: { 'content-type': 'application/json' } });
  const result = await callBrowserTool('http://browser:3010', 'firecrawl_browser_screenshot', { sessionId: 'browser_a1' });
  assert.equal(result.content[1].type, 'image');
  assert.equal(result.content[1].mimeType, 'image/png');
  assert.equal(result.content[1].data, 'aGVsbG8=');
});

test('browser adapter fails closed for malformed session IDs before network access', async t => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let called = false;
  globalThis.fetch = async () => { called = true; throw new Error('should not run'); };
  const result = await callBrowserTool('http://browser:3010', 'firecrawl_browser_tabs', { sessionId: '../bad' });
  assert.equal(called, false);
  assert.equal(result.isError, true);
  assert.match(result.content[0].text, /invalid browser id/);
});
