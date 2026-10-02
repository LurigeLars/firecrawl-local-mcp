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
