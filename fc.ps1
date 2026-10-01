# Manage the local Firecrawl stack.  Usage: .\fc.ps1 up | redeploy | down | status | logs | test | url | recover | repair-postgres-auth | import-gemini
# The public ChatGPT gateway (compose.public.yaml) is included when public/gateway.env exists; the shared tunnel is managed separately.
param([ValidateSet('up', 'redeploy', 'down', 'status', 'logs', 'test', 'url', 'recover', 'repair-postgres-auth', 'import-gemini')][string]$Action = 'status')

$root = $PSScriptRoot
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is required.' }
$GeminiDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\gemini_api_key.dpapi"
$PostgresDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\postgres_password.dpapi"
$SearxngDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\searxng_secret.dpapi"
$PublicProxyServerDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\public_proxy_server.dpapi"
$PublicProxyUsernameDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\public_proxy_username.dpapi"
$PublicProxyPasswordDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\public_proxy_password.dpapi"
$publicProxySecretPaths = @($PublicProxyServerDpapiPath, $PublicProxyUsernameDpapiPath, $PublicProxyPasswordDpapiPath)
$publicProxySecretCount = @($publicProxySecretPaths | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }).Count
if ($publicProxySecretCount -notin @(0, 3)) {
    throw "Public proxy DPAPI configuration is incomplete. Run .\scripts\configure_public_proxy.ps1 again."
}
$PublicProxyConfigured = ($publicProxySecretCount -eq 3)

$compose = @('compose', '--project-directory', "$root\firecrawl",
    '-f', "$root\firecrawl\docker-compose.yaml", '-f', "$root\compose.local.yaml")
if ($PublicProxyConfigured) { $compose += @('--profile', 'public-proxy') }
$public = Test-Path "$root\public\gateway.env"
if ($public) { $compose += @('-f', "$root\compose.public.yaml") }
$compose += @(
    '--env-file', "$root\compose.defaults.env",
    '--env-file', "$root\.env"
)

function Assert-UpstreamFirecrawlPin {
    $pinPath = Join-Path $root "upstream\firecrawl.json"
    $checkout = Join-Path $root "firecrawl"
    $safeCheckout = $checkout.Replace('\', '/')
    if (-not (Test-Path -LiteralPath $pinPath -PathType Leaf)) {
        throw "Upstream Firecrawl pin is missing: $pinPath"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $checkout ".git"))) {
        throw "Upstream Firecrawl checkout is missing: $checkout"
    }

    $pin = Get-Content -LiteralPath $pinPath -Raw | ConvertFrom-Json
    if ($pin.repository -ne "firecrawl/firecrawl" -or $pin.commit -notmatch '^[0-9a-f]{40}$') {
        throw "Upstream Firecrawl pin metadata is invalid."
    }

    $gitCommand = Get-Command git.exe -ErrorAction SilentlyContinue
    if (-not $gitCommand) {
        $gitCommand = Get-Command git -ErrorAction Stop
    }
    $gitExe = $gitCommand.Source

    # Capture the complete native-process output before inspecting it. Piping a
    # native command directly into Select-Object can obscure its exit status in
    # non-interactive maintenance-runner sessions.
    $originOutput = @(& $gitExe -c "safe.directory=$safeCheckout" -C $checkout remote get-url origin 2>&1)
    $originExit = $LASTEXITCODE
    if ($originExit -ne 0 -or $originOutput.Count -lt 1) {
        throw "Unable to read upstream Firecrawl origin (git exit $originExit)."
    }
    $origin = [string]$originOutput[0]
    if ([string]::IsNullOrWhiteSpace($origin)) {
        throw "Upstream Firecrawl origin was empty."
    }
    $origin = $origin.Trim()
    if ($origin -notin @("https://github.com/firecrawl/firecrawl.git", "git@github.com:firecrawl/firecrawl.git")) {
        throw "Unexpected upstream Firecrawl origin: $origin"
    }

    $headOutput = @(& $gitExe -c "safe.directory=$safeCheckout" -C $checkout rev-parse HEAD 2>&1)
    $headExit = $LASTEXITCODE
    $head = if ($headOutput.Count -ge 1) { [string]$headOutput[0] } else { "" }
    if ($headExit -ne 0 -or [string]::IsNullOrWhiteSpace($head) -or $head.Trim() -ne [string]$pin.commit) {
        throw "Upstream Firecrawl checkout does not match the reviewed pin $($pin.tag) / $($pin.commit)."
    }

    $statusOutput = @(& $gitExe -c "safe.directory=$safeCheckout" -C $checkout status --porcelain --untracked-files=no 2>&1)
    $statusExit = $LASTEXITCODE
    if ($statusExit -ne 0) {
        throw "Unable to verify upstream Firecrawl working tree (git exit $statusExit)."
    }
    if ($statusOutput.Count -ne 0) {
        throw "Upstream Firecrawl checkout contains tracked local modifications."
    }
}

function Assert-NoGlobalProxyConfiguration {
    $path = "$root\.env"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return }
    foreach ($name in @("PROXY_SERVER", "PROXY_USERNAME", "PROXY_PASSWORD")) {
        $pattern = "^\s*" + [regex]::Escape($name) + "="
        $line = @(Get-Content -LiteralPath $path | Where-Object { $_ -match $pattern })[0]
        if (-not $line) { continue }
        $value = ($line -split "=", 2)[1].Trim()
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            throw "$name must remain empty in .env. Use the isolated public-proxy DPAPI route instead."
        }
    }
}

function Remove-LegacyGatewaySecret {
    $path = "$root\public\gateway.env"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return }
    $lines = @(Get-Content -LiteralPath $path)
    $kept = @($lines | Where-Object { $_ -notmatch '^\s*GATEWAY_SECRET=' })
    if ($kept.Count -ne $lines.Count) {
        [IO.File]::WriteAllLines($path, $kept, [Text.UTF8Encoding]::new($false))
    }
}

if ($public) {
    Remove-LegacyGatewaySecret
}

function Get-PublicUrl {
    $hostnameLine = @(Get-Content "$root\public\gateway.env" | Where-Object { $_ -match '^PUBLIC_HOSTNAME=' })[0]
    if (-not $hostnameLine) { throw 'PUBLIC_HOSTNAME missing from public\gateway.env' }
    $hostname = ($hostnameLine -replace '^PUBLIC_HOSTNAME=', '').Trim()
    if ([string]::IsNullOrWhiteSpace($hostname)) { throw 'PUBLIC_HOSTNAME is empty in public\gateway.env' }
    "https://$hostname/mcp"
}


function Get-DpapiSecretValue([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label DPAPI secret is missing."
    }

    $encrypted = Get-Content -LiteralPath $Path -Raw
    $secure = ConvertTo-SecureString -String $encrypted
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try {
        $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
        if ([string]::IsNullOrWhiteSpace($plain)) {
            throw "$Label DPAPI secret decrypted to an empty value."
        }
        return $plain
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
        $secure = $null
    }
}

function Get-PublicProxyRuntimeValues {
    if (-not $PublicProxyConfigured) {
        throw "Public proxy DPAPI configuration is not complete."
    }

    # Webshare's Backbone connection accepts the same proxy username/password and
    # supports a "-rotate" username parameter to select a new exit IP per request.
    # Keep the originally selected direct proxy only as bootstrap proof/metadata;
    # runtime traffic uses the bounded rotating Backbone endpoint.
    $storedServer = Get-DpapiSecretValue -Path $PublicProxyServerDpapiPath -Label "Public proxy server"
    $baseUsername = Get-DpapiSecretValue -Path $PublicProxyUsernameDpapiPath -Label "Public proxy username"
    $password = Get-DpapiSecretValue -Path $PublicProxyPasswordDpapiPath -Label "Public proxy password"
    try {
        if ([string]::IsNullOrWhiteSpace($storedServer)) {
            throw "Stored public proxy server is empty."
        }
        $runtimeUsername = if ($baseUsername.EndsWith("-rotate", [StringComparison]::OrdinalIgnoreCase)) {
            $baseUsername
        }
        else {
            "$baseUsername-rotate"
        }

        return [ordered]@{
            proxy_server = "http://p.webshare.io:80"
            proxy_username = $runtimeUsername
            proxy_password = $password
        }
    }
    finally {
        $storedServer = $null
        $baseUsername = $null
        $runtimeUsername = $null
        $password = $null
    }
}

function Invoke-DockerWithExactStdin {
    param(
        [Parameter(Mandatory)][string]$InputText,
        [Parameter(Mandatory)][string[]]$Arguments
    )

    $dockerCommand = Get-Command docker.exe -ErrorAction SilentlyContinue
    if (-not $dockerCommand) {
        $dockerCommand = Get-Command docker -ErrorAction Stop
    }

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $dockerCommand.Source
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.CreateNoWindow = $true

    foreach ($argument in $Arguments) {
        [void]$psi.ArgumentList.Add([string]$argument)
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $psi

    try {
        [void]$process.Start()
        $process.StandardInput.Write($InputText)
        $process.StandardInput.Close()
        $process.WaitForExit()
        return [int]$process.ExitCode
    }
    finally {
        if (-not $process.HasExited) {
            try { $process.Kill($true) } catch {}
        }
        $process.Dispose()
    }
}

function Test-LegacyServiceSecrets {
    $path = "$root\secrets.env"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }

    foreach ($name in @("POSTGRES_PASSWORD", "SEARXNG_SECRET")) {
        $pattern = "^\s*" + [regex]::Escape($name) + "="
        $line = @(Get-Content -LiteralPath $path | Where-Object { $_ -match $pattern })[0]
        if ($line) {
            $value = ($line -split "=", 2)[1].Trim()
            if (-not [string]::IsNullOrWhiteSpace($value)) { return $true }
        }
    }
    return $false
}

function Set-RuntimeHookSecrets([bool]$UseRealSecrets) {
    if ($UseRealSecrets) {
        Assert-NoGlobalProxyConfiguration
        if (
            -not (Test-Path -LiteralPath $PostgresDpapiPath -PathType Leaf) -or
            -not (Test-Path -LiteralPath $SearxngDpapiPath -PathType Leaf)
        ) {
            if (Test-LegacyServiceSecrets) {
                throw "Legacy POSTGRES_PASSWORD/SEARXNG_SECRET found in secrets.env. Run .\scripts\configure_service_secrets.ps1 once to migrate them to DPAPI."
            }
            throw "Firecrawl service DPAPI secrets are missing. Run .\scripts\configure_service_secrets.ps1."
        }

        $postgres = Get-DpapiSecretValue -Path $PostgresDpapiPath -Label "PostgreSQL"
        $searxng = Get-DpapiSecretValue -Path $SearxngDpapiPath -Label "SearXNG"
        try {
            $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET = $postgres
            $env:FIRECRAWL_SEARXNG_SECRET_SECRET = $searxng
        }
        finally {
            $postgres = $null
            $searxng = $null
        }
        if ($PublicProxyConfigured) {
            $proxyValues = Get-PublicProxyRuntimeValues
            try {
                $env:FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET = [string]$proxyValues.proxy_server
                $env:FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET = [string]$proxyValues.proxy_username
                $env:FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET = [string]$proxyValues.proxy_password
                $env:PLAYWRIGHT_MICROSERVICE_URL = "http://playwright-router:3000/scrape"
            }
            finally {
                foreach ($key in @($proxyValues.Keys)) { $proxyValues[$key] = $null }
                $proxyValues = $null
            }
        }
        return
    }

    $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET = "compose-config-only"
    $env:FIRECRAWL_SEARXNG_SECRET_SECRET = "compose-config-only"
    if ($PublicProxyConfigured) {
        $env:FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET = "compose-config-only"
        $env:FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET = "compose-config-only"
        $env:FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET = "compose-config-only"
    }
}

function Restore-ComposeServiceSecrets(
    [bool]$PostgresWasSet,
    [AllowNull()][string]$PostgresValue,
    [bool]$SearxngWasSet,
    [AllowNull()][string]$SearxngValue
) {
    if ($PostgresWasSet) {
        $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET = $PostgresValue
    } else {
        Remove-Item Env:FIRECRAWL_POSTGRES_PASSWORD_SECRET -ErrorAction SilentlyContinue
    }

    if ($SearxngWasSet) {
        $env:FIRECRAWL_SEARXNG_SECRET_SECRET = $SearxngValue
    } else {
        Remove-Item Env:FIRECRAWL_SEARXNG_SECRET_SECRET -ErrorAction SilentlyContinue
    }

}
function Test-LegacyGeminiKey {
    $path = "$root\secrets.env"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    $line = @(Get-Content -LiteralPath $path | Where-Object { $_ -match '^\s*GEMINI_API_KEY=' })[0]
    if (-not $line) { return $false }
    $value = ($line -split '=', 2)[1].Trim()
    return -not [string]::IsNullOrWhiteSpace($value)
}

function Import-ServiceRuntimeSecrets {
    $postgres = Get-DpapiSecretValue -Path $PostgresDpapiPath -Label "PostgreSQL"
    $searxng = Get-DpapiSecretValue -Path $SearxngDpapiPath -Label "SearXNG"

    try {
        foreach ($service in @("api", "nuq-postgres")) {
            $serviceId = (& docker @compose ps -q $service).Trim()
            if (-not $serviceId) {
                throw "$service is not running; cannot restore PostgreSQL runtime secret."
            }

            $secretArgs = $compose + @(
                "exec", "-T", $service,
                "sh", "-c",
                "umask 077; cat > /run/firecrawl-secrets/postgres_password"
            )
            $secretExit = Invoke-DockerWithExactStdin -InputText $postgres -Arguments $secretArgs
            if ($secretExit -ne 0) {
                throw "PostgreSQL runtime secret import failed for $service."
            }
        }

        $searxngId = (& docker @compose ps -q searxng).Trim()
        if (-not $searxngId) {
            throw 'searxng is not running; cannot restore SearXNG runtime secret.'
        }

        $searxArgs = $compose + @(
            "exec", "-T", "searxng",
            "sh", "-c",
            "umask 077; cat > /run/firecrawl-secrets/searxng_secret"
        )
        $searxExit = Invoke-DockerWithExactStdin -InputText $searxng -Arguments $searxArgs
        if ($searxExit -ne 0) {
            throw 'SearXNG runtime secret import failed.'
        }
    }
    finally {
        $postgres = $null
        $searxng = $null
    }

    foreach ($service in @("api", "nuq-postgres")) {
        & docker @compose exec -T $service sh -c 'test -s /run/firecrawl-secrets/postgres_password'
        if ($LASTEXITCODE -ne 0) {
            throw "PostgreSQL runtime secret verification failed for $service."
        }
    }

    & docker @compose exec -T searxng sh -c 'test -s /run/firecrawl-secrets/searxng_secret'
    if ($LASTEXITCODE -ne 0) {
        throw 'SearXNG runtime secret verification failed.'
    }
}

function Test-PostgresRuntimePassword {
    $serviceId = (& docker @compose ps -q nuq-postgres).Trim()
    if (-not $serviceId) {
        throw 'nuq-postgres is not running.'
    }

    & docker @compose exec -T nuq-postgres sh -c 'test -s /run/firecrawl-secrets/postgres_password'
    if ($LASTEXITCODE -ne 0) {
        return $false
    }

    & docker @compose exec -T nuq-postgres sh -c 'dbhost="$(hostname -i | awk ''{print $1}'')"; test -n "$dbhost"; PGPASSWORD="$(cat /run/firecrawl-secrets/postgres_password)" psql -h "$dbhost" -U postgres -d postgres -Atqc "SELECT 1" >/dev/null 2>&1'
    return ($LASTEXITCODE -eq 0)
}

function Repair-PostgresRuntimePassword {
    $serviceId = (& docker @compose ps -q nuq-postgres).Trim()
    if (-not $serviceId) {
        throw 'nuq-postgres is not running.'
    }

    & docker @compose exec -T nuq-postgres sh -c 'test -s /run/firecrawl-secrets/postgres_password'
    if ($LASTEXITCODE -ne 0) {
        throw 'PostgreSQL runtime secret is missing from tmpfs. Run .\fc.ps1 recover first.'
    }

    # Authenticate locally over the Unix socket. The password itself never appears
    # in Docker config or the host command line; psql reads it from tmpfs inside
    # the container and rotates the persisted postgres role to match the DPAPI secret.
    & docker @compose exec -T nuq-postgres sh -c 'psql -U postgres -d postgres -Atqc "SELECT 1" >/dev/null 2>&1'
    if ($LASTEXITCODE -ne 0) {
        throw 'Local PostgreSQL socket authentication failed; refusing to modify the postgres role.'
    }

    & docker @compose exec -T nuq-postgres sh -c 'pw="$(cat /run/firecrawl-secrets/postgres_password)"; printf "%s\n%s\n" "$pw" "$pw" | psql -U postgres -d postgres -v ON_ERROR_STOP=1 -c "\password postgres" >/dev/null'
    if ($LASTEXITCODE -ne 0) {
        throw 'PostgreSQL password reconciliation failed.'
    }

    if (-not (Test-PostgresRuntimePassword)) {
        throw 'PostgreSQL password reconciliation completed but TCP authentication still fails.'
    }

    'postgres runtime password: reconciled'
}

function Import-GeminiKey {
    if (-not (Test-Path -LiteralPath $GeminiDpapiPath -PathType Leaf)) {
        throw 'Gemini DPAPI secret is missing. Run .\scripts\configure_gemini.ps1 first.'
    }

    $serviceId = (& docker @compose ps -q llm-proxy).Trim()
    if (-not $serviceId) {
        throw 'llm-proxy is not running. Run .\fc.ps1 up first.'
    }

    $plain = Get-DpapiSecretValue -Path $GeminiDpapiPath -Label "Gemini"
    try {
        $geminiArgs = $compose + @(
            "exec", "-T", "llm-proxy",
            "sh", "-c",
            "umask 077; cat > /run/firecrawl-secrets/gemini_api_key"
        )
        $geminiExit = Invoke-DockerWithExactStdin -InputText $plain -Arguments $geminiArgs
        if ($geminiExit -ne 0) { throw 'Gemini key import failed.' }
    }
    finally {
        $plain = $null
    }

    & docker @compose exec -T llm-proxy sh -c 'test -s /run/firecrawl-secrets/gemini_api_key && printf "GEMINI_KEY_IMPORTED\n"'
    if ($LASTEXITCODE -ne 0) { throw 'Gemini key verification failed.' }
}

function Import-PublicProxyRuntimeSecrets {
    if (-not $PublicProxyConfigured) { return }

    $serviceId = (& docker @compose ps -q playwright-public-proxy-service).Trim()
    if (-not $serviceId) {
        throw 'playwright-public-proxy-service is not running.'
    }

    $values = Get-PublicProxyRuntimeValues
    try {
        foreach ($entry in $values.GetEnumerator()) {
            $args = $compose + @(
                "exec", "-T", "playwright-public-proxy-service",
                "sh", "-c",
                "umask 077; cat > /run/firecrawl-secrets/$($entry.Key)"
            )
            $exitCode = Invoke-DockerWithExactStdin -InputText ([string]$entry.Value) -Arguments $args
            if ($exitCode -ne 0) {
                throw "Public proxy runtime secret import failed for $($entry.Key)."
            }
        }
    }
    finally {
        foreach ($key in @($values.Keys)) { $values[$key] = $null }
    }

    & docker @compose exec -T playwright-public-proxy-service sh -c 'test -s /run/firecrawl-secrets/proxy_server && test -s /run/firecrawl-secrets/proxy_username && test -s /run/firecrawl-secrets/proxy_password'
    if ($LASTEXITCODE -ne 0) { throw 'Public proxy runtime secret verification failed.' }
}

function Remove-PublicProxyInitContainer {
    if (-not $PublicProxyConfigured) { return }

    & docker @compose rm -f public-proxy-metrics-init | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Could not remove completed public-proxy-metrics-init container."
    }
}

function Invoke-PublicProxySmokeTest {
    if (-not $PublicProxyConfigured) { return }

    $args = $compose + @(
        "--profile", "smoke",
        "run", "--rm", "--no-deps",
        "public-proxy-smoke"
    )
    & docker @args
    if ($LASTEXITCODE -ne 0) {
        throw "Public proxy live fail-closed smoke test failed."
    }
}

function Import-AvailableRuntimeSecrets {
    if (Test-Path -LiteralPath $GeminiDpapiPath -PathType Leaf) {
        Import-GeminiKey
    }
    elseif (Test-LegacyGeminiKey) {
        throw 'Legacy GEMINI_API_KEY found in secrets.env. Run .\scripts\configure_gemini.ps1 once to migrate it to DPAPI.'
    }

    if ($PublicProxyConfigured) {
        Import-PublicProxyRuntimeSecrets
    }
}

$composeActions = @("up", "redeploy", "down", "status", "logs", "recover", "repair-postgres-auth", "import-gemini")
$postgresSecretWasSet = Test-Path Env:FIRECRAWL_POSTGRES_PASSWORD_SECRET
$postgresSecretOriginal = if ($postgresSecretWasSet) { $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET } else { $null }
$searxngSecretWasSet = Test-Path Env:FIRECRAWL_SEARXNG_SECRET_SECRET
$searxngSecretOriginal = if ($searxngSecretWasSet) { $env:FIRECRAWL_SEARXNG_SECRET_SECRET } else { $null }
$proxyServerWasSet = Test-Path Env:FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET
$proxyServerOriginal = if ($proxyServerWasSet) { $env:FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET } else { $null }
$proxyUsernameWasSet = Test-Path Env:FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET
$proxyUsernameOriginal = if ($proxyUsernameWasSet) { $env:FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET } else { $null }
$proxyPasswordWasSet = Test-Path Env:FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET
$proxyPasswordOriginal = if ($proxyPasswordWasSet) { $env:FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET } else { $null }
$playwrightUrlWasSet = Test-Path Env:PLAYWRIGHT_MICROSERVICE_URL
$playwrightUrlOriginal = if ($playwrightUrlWasSet) { $env:PLAYWRIGHT_MICROSERVICE_URL } else { $null }
try {
    if ($Action -in $composeActions) {
        Set-RuntimeHookSecrets -UseRealSecrets ($Action -in @("up", "redeploy"))
    }
switch ($Action) {
    'up' {
        Assert-UpstreamFirecrawlPin
        docker @compose up -d --build
        if ($LASTEXITCODE -ne 0) { throw "docker compose up failed with exit code $LASTEXITCODE" }
        Set-RuntimeHookSecrets -UseRealSecrets $false
        Import-AvailableRuntimeSecrets
        Invoke-PublicProxySmokeTest
        Remove-PublicProxyInitContainer
    }
    'redeploy' {
        Assert-UpstreamFirecrawlPin
        docker @compose up -d --build --force-recreate
        if ($LASTEXITCODE -ne 0) { throw "docker compose redeploy failed with exit code $LASTEXITCODE" }
        Set-RuntimeHookSecrets -UseRealSecrets $false
        Import-AvailableRuntimeSecrets
        Invoke-PublicProxySmokeTest
        Remove-PublicProxyInitContainer
    }
    'down' {
        docker @compose down
    }
    'recover' {
        Import-ServiceRuntimeSecrets
        Import-AvailableRuntimeSecrets
        if (-not (Test-PostgresRuntimePassword)) {
            throw 'PostgreSQL DPAPI runtime secret does not match the persisted postgres role password. Run .\fc.ps1 repair-postgres-auth once.'
        }
        'runtime secrets: restored'
    }
    'repair-postgres-auth' {
        Import-ServiceRuntimeSecrets
        Repair-PostgresRuntimePassword
    }
    'import-gemini' {
        Import-GeminiKey
    }
    'status' {
        docker @compose ps
    }
    'logs' {
        if ($public) { docker @compose logs -f --tail 50 api gateway }
        else { docker @compose logs -f --tail 100 api }
    }
    'url' {
        if ($public) { Get-PublicUrl } else { 'Public access is not configured.' }
    }
    'test' {
        $body = @{ url = 'https://www.iana.org/help/example-domains'; formats = @('markdown') } | ConvertTo-Json
        $localOk = $false
        $lastLocalError = $null
        for ($attempt = 1; $attempt -le 15; $attempt++) {
            try {
                $r = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:3002/v2/scrape' -ContentType 'application/json' -Body $body -TimeoutSec 30
                if ($r.success) {
                    $localOk = $true
                    break
                }
                $lastLocalError = 'Firecrawl returned success=false'
            } catch {
                $lastLocalError = $_.Exception.Message
            }
            if ($attempt -lt 15) { Start-Sleep -Seconds 2 }
        }
        if (-not $localOk) { throw "local engine test failed after 15 attempts: $lastLocalError" }
        "local engine: success=True"

        if ($public) {
            # selftest.mjs understands both supported public modes:
            # - Cloudflare Access: unauthenticated 401 + OAuth metadata is a PASS
            # - secret-path fallback: run the full unauthenticated gateway checks
            & node "$root\public\selftest.mjs" (Get-PublicUrl)
            if ($LASTEXITCODE -ne 0) { throw "public MCP self-test failed with exit code $LASTEXITCODE" }
        }
    }
}
}
finally {
    Restore-ComposeServiceSecrets `
        -PostgresWasSet $postgresSecretWasSet `
        -PostgresValue $postgresSecretOriginal `
        -SearxngWasSet $searxngSecretWasSet `
        -SearxngValue $searxngSecretOriginal

    foreach ($item in @(
        @{ Name = "FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET"; WasSet = $proxyServerWasSet; Value = $proxyServerOriginal },
        @{ Name = "FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET"; WasSet = $proxyUsernameWasSet; Value = $proxyUsernameOriginal },
        @{ Name = "FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET"; WasSet = $proxyPasswordWasSet; Value = $proxyPasswordOriginal },
        @{ Name = "PLAYWRIGHT_MICROSERVICE_URL"; WasSet = $playwrightUrlWasSet; Value = $playwrightUrlOriginal }
    )) {
        if ($item.WasSet) {
            Set-Item -Path ("Env:" + $item.Name) -Value $item.Value
        } else {
            Remove-Item -Path ("Env:" + $item.Name) -ErrorAction SilentlyContinue
        }
    }
}