import test from 'node:test';
import assert from 'node:assert/strict';
import { publicUrlReason, redactUrl, sanitizeText } from './safety.mjs';

const publicLookup = async () => [{ address: '93.184.216.34', family: 4 }];
const privateLookup = async () => [{ address: '127.0.0.1', family: 4 }];

test('public URL safety rejects local and credential-bearing targets', async () => {
  assert.match(await publicUrlReason('http://127.0.0.1/'), /Private or local/);
  assert.match(await publicUrlReason('http://localhost/'), /Private or local/);
  assert.match(await publicUrlReason('https://user:pass@example.com/', { lookupFn: publicLookup }), /credentials/);
  assert.match(await publicUrlReason('file:///etc/passwd'), /HTTP/);
});

test('public URL safety rejects DNS rebinding toward private space', async () => {
  assert.match(await publicUrlReason('https://example.com/', { lookupFn: privateLookup }), /Private or local/);
  assert.equal(await publicUrlReason('https://example.com/', { lookupFn: publicLookup }), null);
});

test('redactUrl removes credential-like query values but retains useful routing data', () => {
  const value = redactUrl('https://example.com/path?q=books&token=abc123&signature=sig');
  assert.match(value, /^https:\/\/example\.com\/path\?/);
  assert.match(value, /q=books/);
  assert.doesNotMatch(value, /abc123|signature=sig/);
  assert.match(value, /%5BREDACTED%5D/);
});

test('sanitizeText redacts common bearer and JWT-shaped secrets', () => {
  const input = 'Bearer abcdefghijklmnopqrstuvwxyz token=clear eyJabcdefghij.abcdefghij.abcdefghij';
  const out = sanitizeText(input);
  assert.doesNotMatch(out, /abcdefghijklmnopqrstuvwxyz/);
  assert.doesNotMatch(out, /eyJabcdefghij\.abcdefghij\.abcdefghij/);
  assert.match(out, /REDACTED/);
});


test('public URL safety retries one transient DNS failure before allowing a public target', async () => {
  let calls = 0;
  const flakyLookup = async () => {
    calls += 1;
    if (calls === 1) {
      const error = new Error('temporary resolver failure');
      error.code = 'EAI_AGAIN';
      throw error;
    }
    return [{ address: '93.184.216.34', family: 4 }];
  };
  assert.equal(
    await publicUrlReason('https://example.com/', { lookupFn: flakyLookup, dnsRetryDelayMs: 0 }),
    null,
  );
  assert.equal(calls, 2);
});

test('public URL safety still fails closed after repeated transient DNS failures', async () => {
  let calls = 0;
  const failingLookup = async () => {
    calls += 1;
    const error = new Error('temporary resolver failure');
    error.code = 'EAI_AGAIN';
    throw error;
  };
  assert.match(
    await publicUrlReason('https://example.com/', { lookupFn: failingLookup, dnsRetryDelayMs: 0 }),
    /could not be safely resolved/,
  );
  assert.equal(calls, 2);
});

test('public URL safety does not retry permanent DNS failures', async () => {
  let calls = 0;
  const missingLookup = async () => {
    calls += 1;
    const error = new Error('not found');
    error.code = 'ENOTFOUND';
    throw error;
  };
  assert.match(
    await publicUrlReason('https://example.com/', { lookupFn: missingLookup, dnsRetryDelayMs: 0 }),
    /could not be safely resolved/,
  );
  assert.equal(calls, 1);
});
