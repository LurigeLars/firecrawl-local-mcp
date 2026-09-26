import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';

const compose = readFileSync(new URL('../compose.local.yaml', import.meta.url), 'utf8');
const launcher = readFileSync(new URL('../fc.ps1', import.meta.url), 'utf8');
const bootstrap = readFileSync(new URL('../scripts/configure_service_secrets.ps1', import.meta.url), 'utf8');

test('service secrets use Compose secret files, not container environment values', () => {
  assert.match(compose, /postgres_password:\s*\n\s*environment:\s*FIRECRAWL_POSTGRES_PASSWORD_SECRET/);
  assert.match(compose, /searxng_secret:\s*\n\s*environment:\s*FIRECRAWL_SEARXNG_SECRET_SECRET/);
  assert.match(compose, /POSTGRES_PASSWORD_FILE:\s*\/run\/secrets\/postgres_password/);
  assert.match(compose, /cat \/run\/secrets\/postgres_password/);
  assert.match(compose, /cat \/run\/secrets\/searxng_secret/);
  assert.doesNotMatch(compose, /POSTGRES_PASSWORD:\s*\$\{/);
  assert.doesNotMatch(compose, /SEARXNG_SECRET:\s*\$\{/);
});

test('launcher no longer loads legacy secrets.env', () => {
  assert.doesNotMatch(launcher, /--env-file['",\s]+\$root\\secrets\.env/);
  assert.match(launcher, /postgres_password\.dpapi/);
  assert.match(launcher, /searxng_secret\.dpapi/);
  assert.match(launcher, /Set-ComposeServiceSecrets/);
});

test('migration removes only known legacy service-secret entries after DPAPI storage', () => {
  assert.match(bootstrap, /POSTGRES_PASSWORD/);
  assert.match(bootstrap, /SEARXNG_SECRET/);
  assert.match(bootstrap, /ConvertFrom-SecureString/);
  assert.match(bootstrap, /ConvertTo-SecureString/);
  assert.match(bootstrap, /LEGACY_SECRETS_ENV_REMOVED/);
});

test('plaintext service-secret template is retired', () => {
  assert.equal(existsSync(new URL('../secrets.env.example', import.meta.url)), false);
});
