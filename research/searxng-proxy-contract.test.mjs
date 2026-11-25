// Run the existing language proxy against a local synthetic SearXNG server.
// No public search engine, real credentials or production service is involved.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import http from 'node:http';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server.address().port;
}

const close = server => new Promise(resolve => server.close(resolve));

test('existing proxy preserves language, engine fallback, domain filtering and paging', async t => {
  const calls = [];
  const upstream = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://local');
    const p = Object.fromEntries(url.searchParams);
    calls.push(p);
    const results = p.engines === 'google,startpage' ? [
      {url: 'https://www.eia.gov/a', title: 'A', content: 'first'},
      {url: 'https://eia.gov.example.org/b', title: 'off-domain'}
    ] : [
      {url: 'https://www.eia.gov/a', title: 'duplicate'},
      {url: 'https://eia.gov/c', title: 'C', content: 'second'}
    ];
    res.writeHead(200, {'content-type': 'application/json'});
    res.end(JSON.stringify({results}));
  });
  const upstreamPort = await listen(upstream);
  t.after(() => close(upstream));
  const reservation = http.createServer();
  const port = await listen(reservation);
  await close(reservation);
  const child = spawn(process.execPath, [fileURLToPath(new URL('../searxng/lang-proxy.mjs', import.meta.url))], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {...process.env, PORT: String(port), UPSTREAM: 'http://127.0.0.1:' + upstreamPort,
      PRIMARY_ENGINES: 'google,startpage', PRIMARY_LANG: 'all',
      FALLBACK_ENGINES: 'bing', FALLBACK_LANG: 'en-GB', MIN_RESULTS: '5',
      HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', NO_PROXY: '127.0.0.1'}
  });
  let logs = '';
  child.stdout.on('data', b => { logs = (logs + b).slice(-2000); });
  child.stderr.on('data', b => { logs = (logs + b).slice(-2000); });
  const exited = once(child, 'exit');
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill();
    await exited;
  });
  const base = 'http://127.0.0.1:' + port;
  let ready = false;
  for (let i = 0; i < 40; i++) {
    if (child.exitCode !== null) break;
    try {
      if ((await fetch(base + '/healthz', {signal: AbortSignal.timeout(250)})).ok) {
        ready = true; break;
      }
    } catch {}
    await delay(50);
  }
  assert.ok(ready, logs);

  for (const [language, primaryLang, fallbackLang] of [
    ['en', 'all', 'en-GB'], ['sv-SE', 'sv-SE', 'sv-SE']
  ]) {
    const query = new URLSearchParams({q: 'oil & gas site:eia.gov', language,
      pageno: '2', format: 'json', safesearch: '2'});
    const res = await fetch(base + '/search?' + query, {signal: AbortSignal.timeout(3000)});
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.deepEqual(body.results.map(r => r.url), [
      'https://www.eia.gov/a', 'https://eia.gov/c'
    ]);
    const [primary, fallback] = calls.slice(-2);
    assert.equal(primary.engines, 'google,startpage');
    assert.equal(fallback.engines, 'bing');
    assert.equal(primary.language, primaryLang);
    assert.equal(fallback.language, fallbackLang);
    for (const p of [primary, fallback]) {
      assert.equal(p.q, 'oil & gas eia.gov');
      assert.equal(p.pageno, '2');
      assert.equal(p.safesearch, '2');
      assert.equal(p.format, 'json');
    }
  }
  assert.equal(calls.length, 4);
});
