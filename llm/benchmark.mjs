// Scores local extraction models through the real Firecrawl pipeline.
// Usage: node benchmark.mjs <model-label>   (the API must already run with MODEL_NAME set to that model)
// Ground truth was read from each page's own content on 2026-09-16.
const apiUrl = new URL(process.env.FIRECRAWL_API_URL ?? 'http://127.0.0.1:3002');
if (apiUrl.protocol !== 'http:' || !new Set(['127.0.0.1', 'localhost', 'host.docker.internal', 'api']).has(apiUrl.hostname)) {
  throw new Error('FIRECRAWL_API_URL must point to the local Firecrawl API');
}
const API = apiUrl.origin;
const company = {
  type: 'object',
  properties: {
    name: { type: 'string' }, founded: { type: 'string' },
    founders: { type: 'array', items: { type: 'string' } }, headquarters: { type: 'string' },
  },
};
const has = (v, s) => JSON.stringify(v ?? '').toLowerCase().includes(s.toLowerCase());
const cases = [
  { url: 'https://en.wikipedia.org/wiki/Klarna', prompt: 'Company facts', schema: company,
    checks: j => [has(j.founded, '2005'), has(j.founders, 'Siemiatkowski'), has(j.founders, 'Adalberth'), has(j.founders, 'Jacobsson'), has(j.headquarters, 'Stockholm'), (j.founders ?? []).length === 3] },
  { url: 'https://en.wikipedia.org/wiki/Spotify', prompt: 'Company facts', schema: company,
    checks: j => [has(j.founded, '2006'), has(j.founders, 'Ek'), has(j.founders, 'Lorentzon'), has(j.headquarters, 'Stockholm'), (j.founders ?? []).length === 2] },
  { url: 'https://en.wikipedia.org/wiki/Ericsson', prompt: 'Company facts', schema: company,
    checks: j => [has(j.founded, '1876'), has(j.founders, 'Lars Magnus Ericsson'), has(j.headquarters, 'Kista') || has(j.headquarters, 'Stockholm'), !has(j, 'math.')] },
  { url: 'https://www.iana.org/help/example-domains', prompt: 'List the RFC numbers mentioned on the page',
    schema: { type: 'object', properties: { rfcs: { type: 'array', items: { type: 'string' } } }, required: ['rfcs'] },
    checks: j => { const r = (j.rfcs ?? []).map(x => String(x).replace(/\D/g, '')); return [r.includes('2606'), r.includes('6761'), r.length === 2]; } },
];

const label = process.argv[2] ?? 'model';
let pass = 0, total = 0;
for (const c of cases) {
  const t0 = Date.now();
  let json, err;
  try {
    const r = await fetch(`${API}/v2/scrape`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ url: c.url, onlyMainContent: true, maxAge: 0, formats: [{ type: 'json', prompt: c.prompt, schema: c.schema }] }),
      signal: AbortSignal.timeout(400_000),
    });
    const body = await r.json();
    json = body.data?.json; err = body.success ? undefined : body.error;
  } catch (e) { err = e.message; }
  const secs = ((Date.now() - t0) / 1000).toFixed(0);
  const results = json ? c.checks(json) : [];
  const ok = results.filter(Boolean).length;
  const n = c.checks({}).length;
  pass += ok; total += n;
  console.log(`${label} | ${c.url.split('/').pop()} | ${ok}/${n} | ${secs}s | ${err ?? JSON.stringify(json).slice(0, 220)}`);
}
console.log(`${label} | TOTAL | ${pass}/${total}`);
