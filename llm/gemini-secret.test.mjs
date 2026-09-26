import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { GEMINI_SECRET_PATH, readGeminiApiKey } from './trim-proxy.mjs';

test('Gemini API key is read only from the runtime secret file', () => {
  const dir = mkdtempSync(join(tmpdir(), 'firecrawl-gemini-secret-'));
  const path = join(dir, 'gemini_api_key');
  const old = process.env.GEMINI_API_KEY;
  process.env.GEMINI_API_KEY = 'SHOULD_NOT_BE_USED';
  try {
    assert.equal(readGeminiApiKey(path), '');
    writeFileSync(path, 'test-secret\n', { mode: 0o600 });
    assert.equal(readGeminiApiKey(path), 'test-secret');
  } finally {
    if (old === undefined) delete process.env.GEMINI_API_KEY;
    else process.env.GEMINI_API_KEY = old;
    rmSync(dir, { recursive: true, force: true });
  }
});

test('deployment does not expose Gemini API key through compose or env template', () => {
  const compose = readFileSync(new URL('../compose.local.yaml', import.meta.url), 'utf8');
  const envExample = readFileSync(new URL('../secrets.env.example', import.meta.url), 'utf8');
  assert.equal(GEMINI_SECRET_PATH, '/run/firecrawl-secrets/gemini_api_key');
  assert.doesNotMatch(compose, /GEMINI_API_KEY\s*:/);
  assert.match(compose, /\/run\/firecrawl-secrets/);
  assert.doesNotMatch(envExample, /GEMINI_API_KEY/);
});
