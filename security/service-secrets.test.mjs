import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';

const compose = readFileSync(new URL('../compose.local.yaml', import.meta.url), 'utf8');
const launcher = readFileSync(new URL('../fc.ps1', import.meta.url), 'utf8');
const bootstrap = readFileSync(new URL('../scripts/configure_service_secrets.ps1', import.meta.url), 'utf8');

test('service secrets are injected into tmpfs by post_start hooks', () => {
  assert.doesNotMatch(compose, /^secrets:\s*$/m);

  assert.match(compose, /post_start:/);
  assert.match(compose, /\/run\/firecrawl-secrets:rw,nosuid,nodev,noexec,size=64k,mode=0777/);
  assert.match(compose, /printf '%s' "\$\$FIRECRAWL_POSTGRES_PASSWORD_SECRET" > \/run\/firecrawl-secrets\/postgres_password/);
  assert.match(compose, /printf '%s' "\$\$FIRECRAWL_SEARXNG_SECRET_SECRET" > \/run\/firecrawl-secrets\/searxng_secret/);
  assert.match(compose, /POSTGRES_PASSWORD_FILE:\s*\/run\/firecrawl-secrets\/postgres_password/);
});

test('main processes wait for tmpfs secrets instead of receiving secret values in container config', () => {
  assert.match(compose, /POSTGRES_PASSWORD:\s*""/);
  assert.match(compose, /SEARXNG_SECRET:\s*""/);
  assert.match(compose, /while \[ ! -s \/run\/firecrawl-secrets\/postgres_password \]/);
  assert.match(compose, /while \[ ! -s \/run\/firecrawl-secrets\/searxng_secret \]/);
  assert.doesNotMatch(compose, /POSTGRES_PASSWORD:\s*\$\{/);
  assert.doesNotMatch(compose, /SEARXNG_SECRET:\s*\$\{/);
});

test('launcher no longer loads legacy secrets.env and supplies hook variables only for up', () => {
  assert.doesNotMatch(launcher, /--env-file['",\s]+\$root\\secrets\.env/);
  assert.match(launcher, /postgres_password\.dpapi/);
  assert.match(launcher, /searxng_secret\.dpapi/);
  assert.match(launcher, /Set-RuntimeHookSecrets/);
  assert.match(launcher, /-UseRealSecrets \(\$Action -eq "up"\)/);
});

test('runtime recovery restores tmpfs secrets without rebuilding the stack', () => {
  assert.match(launcher, /ValidateSet\('up', 'down', 'status', 'logs', 'test', 'url', 'recover', 'import-gemini'\)/);
  assert.match(launcher, /function Import-ServiceRuntimeSecrets/);
  assert.match(launcher, /docker @compose exec -T \$service sh -c 'umask 077; cat > \/run\/firecrawl-secrets\/postgres_password'/);
  assert.match(launcher, /docker @compose exec -T searxng sh -c 'umask 077; cat > \/run\/firecrawl-secrets\/searxng_secret'/);
  assert.match(launcher, /'recover' \{\s*Import-ServiceRuntimeSecrets\s*Import-AvailableRuntimeSecrets/s);
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
