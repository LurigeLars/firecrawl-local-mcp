import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';

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
