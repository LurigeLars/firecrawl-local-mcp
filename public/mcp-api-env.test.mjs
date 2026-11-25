import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { candidateApiEnvironment } from './mcp-api-env.mjs';

const local = 'http://127.0.0.1:3002';
const internal = 'http://api:3002';
const key = 'synthetic-test-only-key-not-a-real-credential-0123456789';

test('default legacy environment unchanged except existing local URL default', () => {
  assert.deepEqual(candidateApiEnvironment({ X: 'y' }, local, local),
    { X: 'y', FIRECRAWL_API_URL: local });
  const external = { FIRECRAWL_API_URL: 'https://example.com', FIRECRAWL_API_KEY: 'legacy' };
  assert.deepEqual(candidateApiEnvironment(external, local, local), external);
});

test('file credential is scoped to exact operator-configured local or internal API', () => {
  const dir = mkdtempSync(join(tmpdir(), 'fc-key-'));
  try {
    const path = join(dir, 'token');
    writeFileSync(path, key + '\r\n');
    const original = { FIRECRAWL_API_KEY_FILE: path };
    for (const url of [local, internal]) {
      const next = candidateApiEnvironment(original, url, url);
      assert.equal(next.FIRECRAWL_API_KEY, key);
      assert.equal(next.FIRECRAWL_API_URL, url);
      assert.equal(next.FIRECRAWL_API_KEY_FILE, undefined);
    }
    assert.deepEqual(original, { FIRECRAWL_API_KEY_FILE: path });
    for (const url of ['https://example.com', 'http://api:3002@evil.example', internal, local + '/v2']) {
      assert.throws(() => candidateApiEnvironment({ ...original, FIRECRAWL_API_URL: url }, local, local),
        /Invalid candidate credential configuration/);
    }
    assert.throws(() => candidateApiEnvironment({ ...original, FIRECRAWL_API_KEY: key }, local, local),
      /Invalid candidate credential configuration/);
    for (const bad of ['', 'x'.repeat(31), 'x'.repeat(257), 'x'.repeat(1025), key + '\nsecret', key + '\u0000']) {
      writeFileSync(path, bad);
      assert.throws(() => candidateApiEnvironment(original, local, local),
        /Candidate credential file unavailable or invalid/);
    }
    assert.throws(() => candidateApiEnvironment({ FIRECRAWL_API_KEY_FILE: dir }, local, local),
      /Candidate credential file unavailable or invalid/);
    assert.throws(() => candidateApiEnvironment({ FIRECRAWL_API_KEY_FILE: join(dir, 'missing') }, local, local),
      /Candidate credential file unavailable or invalid/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
