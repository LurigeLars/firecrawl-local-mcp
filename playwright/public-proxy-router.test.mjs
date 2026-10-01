import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';

import { createServer } from './public-proxy-router.mjs';

function listen(server) {
  return new Promise(resolve => {
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolve(`http://127.0.0.1:${address.port}/scrape`);
    });
  });
}

function fakePlaywright(handler) {
  return http.createServer((req, res) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      const response = handler(body);
      const payload = Buffer.from(JSON.stringify(response));
      res.writeHead(200, {
        'content-type': 'application/json',
        'content-length': String(payload.length),
      });
      res.end(payload);
    });
  });
}

async function get(url) {
  const response = await fetch(url, {
    headers: { connection: 'close' },
  });
  return {
    status: response.status,
    headers: response.headers,
    body: await response.json(),
  };
}

function tempMetricsPath(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'firecrawl-proxy-metrics-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return path.join(dir, 'metrics.json');
}

async function post(url, body) {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', connection: 'close' },
    body: JSON.stringify(body),
  });
  return {
    status: response.status,
    headers: response.headers,
    body: await response.json(),
  };
}

test('non-allowlisted hosts always stay on direct Playwright', async t => {
  let directHits = 0;
  let proxyHits = 0;
  const direct = fakePlaywright(body => {
    directHits += 1;
    return { content: '<html>direct</html>', pageStatusCode: 429, url: body.url };
  });
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    return { content: '<html>proxy</html>', pageStatusCode: 200, url: body.url };
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://example.com/' });
  assert.equal(result.body.content, '<html>direct</html>');
  assert.equal(directHits, 1);
  assert.equal(proxyHits, 0);
  assert.equal(result.headers.get('x-firecrawl-public-proxy'), 'direct');
});

test('allowlisted public host retries through proxy only after direct 403/429', async t => {
  let directHits = 0;
  let proxyHits = 0;
  const direct = fakePlaywright(body => {
    directHits += 1;
    return { content: '<html>blocked</html>', pageStatusCode: 429, url: body.url };
  });
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    return { content: '<html>proxied</html>', pageStatusCode: 200, url: body.url };
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://www.curemydisease.com/upcoming-results' });
  assert.equal(result.body.content, '<html>proxied</html>');
  assert.equal(directHits, 1);
  assert.equal(proxyHits, 1);
  assert.equal(result.headers.get('x-firecrawl-public-proxy'), 'used');
});

test('allowlisted public host retries blocked proxy exits up to the bounded attempt count', async t => {
  let directHits = 0;
  let proxyHits = 0;
  const direct = fakePlaywright(body => {
    directHits += 1;
    return { content: '<html>blocked-direct</html>', pageStatusCode: 429, url: body.url };
  });
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    if (proxyHits < 3) {
      return { content: '<html>blocked-proxy</html>', pageStatusCode: 429, url: body.url };
    }
    return { content: '<html>rotated-ok</html>', pageStatusCode: 200, url: body.url };
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
    proxyAttempts: 3,
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://curemydisease.com/upcoming-results' });
  assert.equal(result.body.content, '<html>rotated-ok</html>');
  assert.equal(directHits, 1);
  assert.equal(proxyHits, 3);
  assert.equal(result.headers.get('x-firecrawl-public-proxy'), 'used');
  assert.equal(result.headers.get('x-firecrawl-public-proxy-attempts'), '3');
});

test('bounded proxy fallback returns the final blocked response after max attempts', async t => {
  let proxyHits = 0;
  const direct = fakePlaywright(body => ({
    content: '<html>blocked-direct</html>',
    pageStatusCode: 429,
    url: body.url,
  }));
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    return { content: '<html>still-blocked</html>', pageStatusCode: 429, url: body.url };
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
    proxyAttempts: 2,
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://curemydisease.com/' });
  assert.equal(result.body.pageStatusCode, 429);
  assert.equal(proxyHits, 2);
  assert.equal(result.headers.get('x-firecrawl-public-proxy-attempts'), '2');
});

test('aggregate metrics persist across router restart without URLs or credentials', async t => {
  const metricsPath = tempMetricsPath(t);
  let proxyHits = 0;

  const direct = fakePlaywright(body => {
    if (body.url.includes('blocked')) {
      return { content: '<html>blocked</html>', pageStatusCode: 429, url: body.url };
    }
    return { content: '<html>direct-ok</html>', pageStatusCode: 200, url: body.url };
  });
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    return { content: '<html>proxy-ok</html>', pageStatusCode: 200, url: body.url };
  });

  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const first = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
    metricsPath,
  });
  const firstUrl = await listen(first);

  await post(firstUrl, { url: 'https://example.com/' });
  await post(firstUrl, { url: 'https://curemydisease.com/blocked' });
  await post(firstUrl, {
    url: 'https://curemydisease.com/',
    headers: { Cookie: 'dummy=1' },
  });

  const metricsUrl = firstUrl.replace(/\/scrape$/, '/metrics');
  const beforeRestart = await get(metricsUrl);
  assert.equal(beforeRestart.status, 200);
  assert.equal(beforeRestart.body.requests_total, 3);
  assert.equal(beforeRestart.body.direct_success, 1);
  assert.equal(beforeRestart.body.direct_blocked, 1);
  assert.equal(beforeRestart.body.proxy_attempts, 1);
  assert.equal(beforeRestart.body.proxy_success, 1);
  assert.equal(beforeRestart.body.proxy_exhausted, 0);
  assert.equal(beforeRestart.body.sensitive_rejected, 1);
  assert.equal(proxyHits, 1);
  assert.ok(beforeRestart.body.total_latency_ms >= 0);
  assert.ok(beforeRestart.body.avg_latency_ms >= 0);

  const persisted = fs.readFileSync(metricsPath, 'utf8');
  assert.doesNotMatch(persisted, /example\.com|curemydisease|dummy=1|Cookie|Authorization/i);

  await new Promise(r => first.close(r));

  const second = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
    metricsPath,
  });
  const secondUrl = await listen(second);
  const afterRestart = await get(secondUrl.replace(/\/scrape$/, '/metrics'));
  assert.equal(afterRestart.body.requests_total, 3);
  assert.equal(afterRestart.body.proxy_success, 1);
  assert.equal(afterRestart.body.sensitive_rejected, 1);

  t.after(() => Promise.all([
    new Promise(r => second.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));
});

test('allowlisted host does not spend proxy bandwidth when direct succeeds', async t => {
  let proxyHits = 0;
  const direct = fakePlaywright(body => ({
    content: '<html>direct-ok</html>',
    pageStatusCode: 200,
    url: body.url,
  }));
  const proxy = fakePlaywright(body => {
    proxyHits += 1;
    return { content: '<html>proxy</html>', pageStatusCode: 200, url: body.url };
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://curemydisease.com/' });
  assert.equal(result.body.content, '<html>direct-ok</html>');
  assert.equal(proxyHits, 0);
  assert.equal(result.headers.get('x-firecrawl-public-proxy'), 'direct');
});

test('proxy-eligible request with session/auth headers fails closed', async t => {
  const direct = fakePlaywright(body => ({
    content: '<html>direct</html>',
    pageStatusCode: 429,
    url: body.url,
  }));
  const proxy = fakePlaywright(body => ({
    content: '<html>proxy</html>',
    pageStatusCode: 200,
    url: body.url,
  }));
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, {
    url: 'https://curemydisease.com/',
    headers: { Cookie: 'session=sensitive' },
  });
  assert.equal(result.status, 400);
  assert.match(result.body.error, /authentication\/session headers/);
});

test('URL credentials are refused before any upstream request', async t => {
  const direct = fakePlaywright(() => {
    throw new Error('must not be called');
  });
  const proxy = fakePlaywright(() => {
    throw new Error('must not be called');
  });
  const directUrl = await listen(direct);
  const proxyUrl = await listen(proxy);
  const router = createServer({
    directUpstream: directUrl,
    proxyUpstream: proxyUrl,
    proxyHosts: new Set(['curemydisease.com']),
  });
  const routerUrl = await listen(router);
  t.after(() => Promise.all([
    new Promise(r => router.close(r)),
    new Promise(r => direct.close(r)),
    new Promise(r => proxy.close(r)),
  ]));

  const result = await post(routerUrl, { url: 'https://user:pass@curemydisease.com/' });
  assert.equal(result.status, 400);
  assert.match(result.body.error, /URL credentials/);
});
