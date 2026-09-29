import assert from 'node:assert/strict';
import test from 'node:test';
import { urlSafetyReason } from './policy.mjs';

const params = url => ({ name: 'firecrawl_scrape', arguments: { url } });

for (const url of [
  'http://127.0.0.1/',
  'http://10.0.0.1/',
  'http://100.64.0.1/',
  'http://169.254.169.254/latest/meta-data/',
  'http://172.16.0.1/',
  'http://192.168.1.1/',
  'http://[::1]/',
  'http://[fe80::1]/',
  'http://[fc00::1]/',
  'http://[::ffff:10.0.0.1]/',
]) {
  test(`blocks non-public destination ${url}`, async () => {
    assert.match(await urlSafetyReason(params(url)), /Private or local/);
  });
}

test('allows a public IPv4 literal', async () => {
  assert.equal(await urlSafetyReason(params('https://93.184.216.34/')), null);
});

test('rejects non-HTTP protocols', async () => {
  assert.match(await urlSafetyReason(params('file:///etc/passwd')), /HTTP/);
});

test('rejects URL credentials', async () => {
  assert.match(await urlSafetyReason(params('https://user:pass@93.184.216.34/')), /credentials/);
});

test('does not apply URL policy to search-only calls', async () => {
  assert.equal(await urlSafetyReason({ name: 'firecrawl_search', arguments: { query: 'example' } }), null);
});
