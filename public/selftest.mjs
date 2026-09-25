// Exercises the gateway end to end. Usage: node selftest.mjs <base-url-with-secret-path>
// e.g. http://gateway:8080/<secret>/mcp  or  https://firecrawl.example.com/<secret>/mcp
const url = process.argv[2];
// This operator-run diagnostic intentionally probes the explicitly supplied gateway URL.
// It is not part of the gateway request path and must be able to test local deployments.
const origin = new URL(url).origin;
const headers = { 'content-type': 'application/json', accept: 'application/json, text/event-stream' };

async function rpc(method, params, id = 1) {
  const r = await fetch(url, { method: 'POST', headers, body: JSON.stringify({ jsonrpc: '2.0', id, method, params }) });
  const text = await r.text();
  const data = text.split('\n').find(l => l.startsWith('data:'))?.slice(5) ?? text;
  let json; try { json = JSON.parse(data); } catch { json = text.slice(0, 200); }
  return { status: r.status, json };
}

const results = [];
const check = (name, ok, detail = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ' - ' + detail : ''}`);

// With Cloudflare Access enforced, unauthenticated calls stop at the edge; that is the expected result.
const probe = await fetch(url, { method: 'POST', headers, body: '{}' });
if (probe.status === 401 && /resource_metadata/.test(probe.headers.get('www-authenticate') ?? '')) {
  console.log('PASS Cloudflare Access enforced - unauthenticated request got 401 with OAuth metadata');
  const meta = await (await fetch(`${origin}/.well-known/oauth-protected-resource`)).json().catch(() => ({}));
  console.log(`${meta.authorization_servers?.length ? 'PASS' : 'FAIL'} OAuth protected-resource metadata - ${JSON.stringify(meta)}`);
  console.log('INFO tool checks need a logged-in client (ChatGPT); run node public/access-test.mjs for the gateway JWT checks');
  process.exit(meta.authorization_servers?.length ? 0 : 1);
}

const bad = await fetch(`${origin}/wrong-secret/mcp`, { method: 'POST', headers, body: '{}' });
check('wrong secret -> 404', bad.status === 404, `got ${bad.status}`);
const root = await fetch(`${origin}/`);
check('root -> 404', root.status === 404, `got ${root.status}`);

const init = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'selftest', version: '1' } });
check('initialize', init.status === 200 && !!init.json?.result, `status ${init.status}`);
const instr = init.json?.result?.instructions ?? '';
check('custom instructions served', instr.includes('self-hosted Firecrawl') && instr.includes('queryOptions'), `${instr.length} chars`);

const list = await rpc('tools/list', {}, 2);
const names = list.json?.result?.tools?.map(t => t.name) ?? [];
check('tools/list filtered', names.length > 0 && !names.includes('firecrawl_parse'), names.join(','));

const parse = await rpc('tools/call', { name: 'firecrawl_parse', arguments: { filePath: '/etc/passwd' } }, 3);
check('firecrawl_parse blocked', !!parse.json?.error, JSON.stringify(parse.json).slice(0, 120));

const shot = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url: 'https://www.iana.org/', formats: ['screenshot'] } }, 6);
check('screenshot refused fast', shot.json?.result?.isError === true && JSON.stringify(shot.json).includes('not available'), JSON.stringify(shot.json).slice(0, 100));

const scrape = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url: 'https://www.iana.org/help/example-domains', formats: ['markdown'] } }, 4);
const text = scrape.json?.result?.content?.[0]?.text ?? '';
check('scrape works', !scrape.json?.result?.isError && text.includes('Example Domains'), `${text.length} chars`);

const lan = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url: 'http://10.0.0.1/' } }, 5);
check('LAN scrape refused', !!lan.json?.result?.isError || !!lan.json?.error, JSON.stringify(lan.json).slice(0, 100));

console.log(results.join('\n'));
process.exit(results.some(r => r.startsWith('FAIL')) ? 1 : 0);
