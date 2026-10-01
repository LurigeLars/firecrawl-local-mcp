import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';

const compose = readFileSync(new URL('../compose.local.yaml', import.meta.url), 'utf8');
const launcher = readFileSync(new URL('../fc.ps1', import.meta.url), 'utf8');
const bootstrap = readFileSync(new URL('../scripts/configure_service_secrets.ps1', import.meta.url), 'utf8');
const publicProxyBootstrap = readFileSync(new URL('../scripts/configure_public_proxy.ps1', import.meta.url), 'utf8');
const composeDefaults = readFileSync(new URL('../compose.defaults.env', import.meta.url), 'utf8');
const publicCompose = readFileSync(new URL('../compose.public.yaml', import.meta.url), 'utf8');
const gateway = readFileSync(new URL('../public/gateway/gateway.mjs', import.meta.url), 'utf8');

test('service secrets are injected into tmpfs by post_start hooks', () => {
  assert.doesNotMatch(compose, /^secrets:\s*$/m);

  assert.match(compose, /post_start:/);
  assert.doesNotMatch(compose, /mode=0777/);
  assert.match(compose, /\/run\/firecrawl-secrets:rw,nosuid,nodev,noexec,size=64k,uid=1000,gid=1000,mode=0700/);
  assert.match(compose, /\/run\/firecrawl-secrets:rw,nosuid,nodev,noexec,size=64k,uid=0,gid=0,mode=0700/);
  assert.match(compose, /\/run\/firecrawl-secrets:rw,nosuid,nodev,noexec,size=64k,uid=977,gid=977,mode=0700/);
  assert.match(compose, /printf '%s' "\$\$FIRECRAWL_POSTGRES_PASSWORD_SECRET" > \/run\/firecrawl-secrets\/postgres_password/);
  assert.match(compose, /printf '%s' "\$\$FIRECRAWL_SEARXNG_SECRET_SECRET" > \/run\/firecrawl-secrets\/searxng_secret/);
  assert.match(compose, /POSTGRES_PASSWORD_FILE:\s*\/run\/firecrawl-secrets\/postgres_password/);
});

test('internal proxies and broker/cache services use compatible privilege hardening', () => {
  assert.match(compose, /searxng-proxy:[\s\S]*?user:\s+node[\s\S]*?cap_drop:\s*\n\s+- ALL[\s\S]*?no-new-privileges:true/);
  assert.match(compose, /llm-proxy:[\s\S]*?user:\s+node[\s\S]*?cap_drop:\s*\n\s+- ALL[\s\S]*?no-new-privileges:true/);
  assert.match(compose, /redis:[\s\S]*?no-new-privileges:true/);
  assert.match(compose, /rabbitmq:[\s\S]*?no-new-privileges:true/);
});

test('API and SearXNG run non-root with bounded Linux privileges', () => {
  assert.match(compose, /api:\n\s+user:\s+node/);
  assert.match(compose, /api:[\s\S]*?cap_drop:\s*\n\s+- ALL[\s\S]*?no-new-privileges:true/);
  assert.match(compose, /searxng:[\s\S]*?user:\s+"977:977"/);
  assert.match(compose, /searxng:[\s\S]*?cap_drop:\s*\n\s+- ALL[\s\S]*?no-new-privileges:true/);
  assert.match(compose, /FORCE_OWNERSHIP:\s*"false"/);
});

test('main processes wait for tmpfs secrets instead of receiving secret values in container config', () => {
  assert.match(compose, /POSTGRES_PASSWORD:\s*""/);
  assert.match(compose, /SEARXNG_SECRET:\s*""/);
  assert.match(compose, /while \[ ! -s \/run\/firecrawl-secrets\/postgres_password \]/);
  assert.match(compose, /while \[ ! -s \/run\/firecrawl-secrets\/searxng_secret \]/);
  assert.doesNotMatch(compose, /POSTGRES_PASSWORD:\s*\$\{/);
  assert.doesNotMatch(compose, /SEARXNG_SECRET:\s*\$\{/);
});

test('launcher no longer loads legacy secrets.env and supplies hook variables only for secret-bearing start actions', () => {
  assert.doesNotMatch(launcher, /--env-file['",\s]+\$root\\secrets\.env/);
  assert.match(launcher, /postgres_password\.dpapi/);
  assert.match(launcher, /searxng_secret\.dpapi/);
  assert.match(launcher, /Set-RuntimeHookSecrets/);
  assert.match(launcher, /-UseRealSecrets \(\$Action -in @\("up", "redeploy"\)\)/);
});

test('runtime recovery restores tmpfs secrets without rebuilding the stack', () => {
  assert.match(launcher, /ValidateSet\('up', 'redeploy', 'down', 'status', 'logs', 'test', 'url', 'recover', 'repair-postgres-auth', 'import-gemini'\)/);
  assert.match(launcher, /'redeploy' \{/);
  assert.match(launcher, /docker @compose up -d --build --force-recreate/);
  assert.match(launcher, /function Import-ServiceRuntimeSecrets/);
  assert.match(launcher, /function Invoke-DockerWithExactStdin/);
  assert.match(launcher, /StandardInput\.Write\(\$InputText\)/);
  assert.match(launcher, /Invoke-DockerWithExactStdin -InputText \$postgres/);
  assert.match(launcher, /Invoke-DockerWithExactStdin -InputText \$searxng/);
  assert.match(launcher, /'recover' \{\s*Import-ServiceRuntimeSecrets\s*Import-AvailableRuntimeSecrets\s*if \(-not \(Test-PostgresRuntimePassword\)\)/s);
});

test('exact stdin helper does not append a host newline to runtime secrets', () => {
  assert.match(launcher, /RedirectStandardInput = \$true/);
  assert.match(launcher, /StandardInput\.Write\(\$InputText\)/);
  assert.doesNotMatch(launcher, /\$postgres\s*\|\s*& docker @compose exec/);
  assert.doesNotMatch(launcher, /\$searxng\s*\|\s*& docker @compose exec/);
  assert.doesNotMatch(launcher, /\$plain\s*\|\s*& docker @compose exec/);
});

test('runtime recovery refuses a mismatched persisted PostgreSQL password and exposes an explicit one-time repair action', () => {
  assert.match(launcher, /repair-postgres-auth/);
  assert.match(launcher, /function Test-PostgresRuntimePassword/);
  assert.match(launcher, /function Repair-PostgresRuntimePassword/);
  assert.match(launcher, /hostname -i/);
  assert.match(launcher, /PGPASSWORD="\$\(cat \/run\/firecrawl-secrets\/postgres_password\)"/);
  assert.match(launcher, /\\password postgres/);
  assert.match(launcher, /Run \.\\fc\.ps1 repair-postgres-auth once/);
});

test('migration removes only known legacy service-secret entries after DPAPI storage', () => {
  assert.match(bootstrap, /POSTGRES_PASSWORD/);
  assert.match(bootstrap, /SEARXNG_SECRET/);
  assert.match(bootstrap, /ConvertFrom-SecureString/);
  assert.match(bootstrap, /ConvertTo-SecureString/);
  assert.match(bootstrap, /LEGACY_SECRETS_ENV_REMOVED/);
});

test('public proxy credentials are DPAPI-backed and isolated from the normal Firecrawl path', () => {
  assert.match(publicProxyBootstrap, /Read-Host .*Webshare API key.*-AsSecureString/);
  assert.match(publicProxyBootstrap, /proxy\.webshare\.io\/api\/v2\/proxy\/list\/\?mode=direct/);
  assert.match(publicProxyBootstrap, /Authorization = "Token \$apiKey"/);
  assert.match(publicProxyBootstrap, /ipv4\.webshare\.io/);
  assert.match(publicProxyBootstrap, /API key.*sparas inte/);
  assert.match(publicProxyBootstrap, /public_proxy_server\.dpapi/);
  assert.match(publicProxyBootstrap, /public_proxy_username\.dpapi/);
  assert.match(publicProxyBootstrap, /public_proxy_password\.dpapi/);
  assert.match(publicProxyBootstrap, /ConvertFrom-SecureString/);
  assert.doesNotMatch(publicProxyBootstrap, /webshare_api.*dpapi/i);

  assert.match(compose, /playwright-public-proxy-service:/);
  assert.match(compose, /profiles:\s*\["public-proxy"\]/);
  assert.match(compose, /PROXY_SERVER:\s*""/);
  assert.match(compose, /PROXY_USERNAME:\s*""/);
  assert.match(compose, /PROXY_PASSWORD:\s*""/);
  assert.match(compose, /\/run\/firecrawl-secrets\/proxy_server/);
  assert.match(compose, /\/run\/firecrawl-secrets\/proxy_username/);
  assert.match(compose, /\/run\/firecrawl-secrets\/proxy_password/);
  assert.match(compose, /playwright-router:/);
  assert.match(compose, /PUBLIC_PROXY_HOSTS:/);

  assert.match(launcher, /public_proxy_server\.dpapi/);
  assert.match(launcher, /public_proxy_username\.dpapi/);
  assert.match(launcher, /public_proxy_password\.dpapi/);
  assert.match(launcher, /--profile', 'public-proxy'/);
  assert.match(launcher, /PLAYWRIGHT_MICROSERVICE_URL = "http:\/\/playwright-router:3000\/scrape"/);
  assert.match(launcher, /Assert-NoGlobalProxyConfiguration/);
  assert.match(launcher, /must remain empty in \.env/);
});

test('plaintext service-secret template is retired', () => {
  assert.equal(existsSync(new URL('../secrets.env.example', import.meta.url)), false);
});



test('public gateway is Cloudflare Access only with no secret-path fallback', () => {
  assert.doesNotMatch(gateway, /GATEWAY_SECRET/);
  assert.doesNotMatch(gateway, /ALLOW_SECRET_PATH/);
  assert.doesNotMatch(publicCompose, /ALLOW_SECRET_PATH/);
  assert.match(gateway, /ACCESS_AUD is required/);
  assert.match(gateway, /split\('\?'\)\[0\]==='\/mcp'/);
  assert.match(launcher, /Remove-LegacyGatewaySecret/);
  assert.match(launcher, /Where-Object \{ \$_ -notmatch '\^\\s\*GATEWAY_SECRET='/);
  assert.equal(existsSync(new URL('../public/browser.env.example', import.meta.url)), false);
});

test('optional Compose defaults suppress warnings without supplying values', () => {
  const dataLines = composeDefaults
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith('#'));

  assert.ok(dataLines.length > 0);
  for (const line of dataLines) {
    assert.match(line, /^[A-Z0-9_]+=$/);
  }

  assert.match(
    launcher,
    /'--env-file', "\$root\\compose\.defaults\.env",[\s\S]*'--env-file', "\$root\\\.env"/,
  );
});
