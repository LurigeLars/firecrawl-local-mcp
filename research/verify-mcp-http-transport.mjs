// The actual locked MCP image is running its default entrypoint on an
// isolated network, with a synthetic credential file mounted read-only.
import assert from 'node:assert/strict';

const MCP = 'http://mcp:3000/mcp';
const API = 'http://api:3002/health';
const token = 'synthetic-ci-token-not-a-real-secret-0123456789';
let session;
let version = '2025-06-18';
let nextId = 1;

async function rpc(method, params, notification = false) {
  const id = nextId++;
  const headers = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    ...(session ? {'mcp-session-id': session} : {}),
    'mcp-protocol-version': version,
  };
  const res = await fetch(MCP, {
    method: 'POST', headers, redirect: 'error',
    body: JSON.stringify({jsonrpc: '2.0', method, params, ...(!notification ? {id} : {})}),
    signal: AbortSignal.timeout(45000),
  });
  session = res.headers.get('mcp-session-id') ?? session;
  if (notification) {
    assert.ok([200, 202, 204].includes(res.status), 'MCP notification failed');
    await res.body?.cancel();
    return;
  }
  assert.equal(res.status, 200, 'MCP HTTP request failed');
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  const sse = res.headers.get('content-type')?.includes('text/event-stream');
  let raw = '', bytes = 0;
  try {
    while (true) {
      const {done, value} = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      assert.ok(bytes <= 2 * 1024 * 1024, 'MCP response exceeds fixture cap');
      raw += decoder.decode(value, {stream: true});
      if (sse) {
        const events = raw.split(/\r?\n\r?\n/);
        raw = events.pop();
        for (const event of events) {
          const data = event.split(/\r?\n/).filter(l => l.startsWith('data:')).map(l => l.slice(5).trimStart()).join('\n');
          if (!data) continue;
          const json = JSON.parse(data);
          if (json.id === id) return json;
        }
      }
    }
    if (!sse) {
      const json = JSON.parse(raw + decoder.decode());
      assert.equal(json.id, id);
      return json;
    }
    throw new Error('MCP response ended without a matching JSON-RPC message');
  } finally {
    await reader.cancel().catch(() => {});
  }
}

const unauthorized = await fetch(API, {redirect:'error', signal:AbortSignal.timeout(5000)});
assert.equal(unauthorized.status, 401);
await unauthorized.body?.cancel();
const authorized = await fetch(API, {headers:{authorization:'Bearer '+token},redirect:'error',signal:AbortSignal.timeout(5000)});
assert.equal(authorized.status, 200);
await authorized.body?.cancel();

let init;
for (let i = 0; i < 30; i++) {
  try {
    init = await rpc('initialize', {protocolVersion:version,capabilities:{},clientInfo:{name:'isolated-http-acceptance',version:'1'}});
    break;
  } catch (error) {
    if (error.cause?.code !== 'ECONNREFUSED' && error.cause?.code !== 'ENOTFOUND') throw error;
    await new Promise(resolve => setTimeout(resolve, 1000));
  }
}
assert.ok(init?.result?.serverInfo, 'MCP HTTP initialization unavailable');
version = init.result.protocolVersion ?? version;
await rpc('notifications/initialized', {}, true);
const list = await rpc('tools/list', {});
assert.ok(list.result?.tools?.some(tool=>tool.name==='firecrawl_scrape'));

const blocked = await rpc('tools/call',{name:'firecrawl_scrape',arguments:{url:'https://127.0.0.1/',formats:['markdown']}});
assert.ok(blocked.error||blocked.result?.isError, 'private destination must fail');
console.log('MCP_HTTP_FILE_CREDENTIAL_POLICY_PASS');

const started = performance.now();
const result = await rpc('tools/call',{name:'firecrawl_scrape',arguments:{url:'https://www.iana.org/help/example-domains',formats:['markdown']}});
assert.ok(result.result&&!result.result.isError&&!result.error, 'actual MCP scrape failed');
const visible = JSON.stringify(result.result).replace(/\\n|\\r|\\t/g,' ').replace(/\s+/g,' ').toLowerCase();
assert.ok(['Example Domains','RFC 2606','RFC 6761'].every(marker=>visible.includes(marker.toLowerCase())), 'MCP lost source content');
console.log('MCP_HTTP_FILE_CREDENTIAL_LIVE_PASS',JSON.stringify({markers:3,elapsed_ms:Math.round(performance.now()-started)}));
