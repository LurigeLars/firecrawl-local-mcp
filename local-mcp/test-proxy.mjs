// End-to-end check of stdio-proxy.mjs against the local Firecrawl (needs the stack running).
// Usage: node local-mcp/test-proxy.mjs
import { spawn } from 'node:child_process';
import readline from 'node:readline';

const proxy = spawn(process.execPath, [new URL('./stdio-proxy.mjs', import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')],
  { env: { ...process.env, FIRECRAWL_API_URL: 'http://127.0.0.1:3002' }, stdio: ['pipe', 'pipe', 'inherit'] });
const pending = new Map();
let nextId = 1;
readline.createInterface({ input: proxy.stdout }).on('line', line => {
  const m = JSON.parse(line);
  pending.get(m.id)?.(m);
});
const rpc = (method, params) => new Promise(resolve => {
  const id = nextId++;
  pending.set(id, resolve);
  proxy.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
});
const results = [];
const check = (name, ok, detail = '') => results.push(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ' - ' + detail : ''}`);
const tokens = s => Math.round(s.length / 3.8);
const url = 'https://www.iana.org/help/example-domains';

const init = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'test', version: '1' } });
proxy.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
check('custom instructions', (init.result?.instructions ?? '').includes('self-hosted Firecrawl'));

const list = await rpc('tools/list', {});
const names = (list.result?.tools ?? []).map(t => t.name);
check('only 5 tools listed', names.length === 5 && !names.includes('firecrawl_parse'), `${names.length}: ~${tokens(JSON.stringify(list.result.tools))} tokens`);

const parse = await rpc('tools/call', { name: 'firecrawl_parse', arguments: { filePath: 'C:/example/blocked.txt' } });
check('parse blocked', !!parse.error, parse.error?.message);

const shot = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url, formats: ['screenshot'] } });
check('screenshot refused', shot.result?.isError === true);

const sq = await rpc('tools/call', { name: 'firecrawl_search', arguments: { query: 'iana example domains', limit: 6, scrapeOptions: { formats: ['query'], queryOptions: { prompt: 'x' } } } });
check('search with local-model formats refused above 5 results', sq.result?.isError === true);
const sq2 = await rpc('tools/call', { name: 'firecrawl_search', arguments: { query: 'iana example domains', limit: 2, scrapeOptions: { formats: ['query'], queryOptions: { prompt: 'Which RFCs reserve example domains?' } } } });
const sq2Text = sq2.result?.content?.[0]?.text ?? '';
check('search with query on 2 results allowed', sq2.result && !sq2.result.isError && sq2Text.includes('answer'), `~${tokens(sq2Text)} tokens`);

const q = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url, formats: ['query'], queryOptions: { prompt: 'Which RFC numbers are mentioned?' } } });
const qText = q.result?.content?.[0]?.text ?? '';
const qData = JSON.parse(qText || '{}');
check('query compact', typeof qData.answer === 'string' && qData.source?.url && !qData.metadata, `~${tokens(qText)} tokens: ${qText.slice(0, 160)}`);

const search = await rpc('tools/call', { name: 'firecrawl_search', arguments: { query: 'Riksbanken styrränta', limit: 5 } });
const searchText = search.result?.content?.[0]?.text ?? '';
const searchData = JSON.parse(searchText || '{}');
const hits = (searchData.data?.web ?? searchData.web ?? []).length;
check('search returns results (SearXNG)', hits > 0, `${hits} hits, ~${tokens(searchText)} tokens`);

const md = await rpc('tools/call', { name: 'firecrawl_scrape', arguments: { url, formats: ['markdown'] } });
const mdData = JSON.parse(md.result?.content?.[0]?.text ?? '{}');
check('markdown keeps metadata', !!mdData.markdown && !!mdData.metadata?.title);

console.log(results.join('\n'));
proxy.kill();
process.exit(results.some(r => r.startsWith('FAIL')) ? 1 : 0);
