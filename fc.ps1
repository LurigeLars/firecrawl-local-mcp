# Manage the local Firecrawl stack.  Usage: .\fc.ps1 up | redeploy | down | status | logs | test | url | recover | repair-postgres-auth | import-gemini
# When public ChatGPT access is configured, also manages the narrow local browser bridge used for Season supplier login.
# The public ChatGPT gateway (compose.public.yaml) is included when public/gateway.env exists; the shared tunnel is managed separately.
param([ValidateSet('up', 'redeploy', 'down', 'status', 'logs', 'test', 'url', 'recover', 'repair-postgres-auth', 'import-gemini')][string]$Action = 'status')

$root = $PSScriptRoot
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is required.' }
$GeminiDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\gemini_api_key.dpapi"
$PostgresDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\postgres_password.dpapi"
$SearxngDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\searxng_secret.dpapi"
$BrowserBridgeDpapiPath = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets\browser_bridge_token.dpapi"
$compose = @('compose', '--project-directory', "$root\firecrawl",
    '-f', "$root\firecrawl\docker-compose.yaml", '-f', "$root\compose.local.yaml")
$public = Test-Path "$root\public\gateway.env"
if ($public) { $compose += @('-f', "$root\compose.public.yaml") }
$compose += @(
    '--env-file', "$root\compose.defaults.env",
    '--env-file', "$root\.env"
)

function Read-BrowserTokenDpapi {
    if (-not (Test-Path -LiteralPath $BrowserBridgeDpapiPath -PathType Leaf)) {
        throw 'Browser bridge DPAPI secret is missing.'
    }
    $encrypted = Get-Content -LiteralPath $BrowserBridgeDpapiPath -Raw
    $secure = ConvertTo-SecureString -String $encrypted
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try {
        $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
        if ([string]::IsNullOrWhiteSpace($plain)) {
            throw 'Browser bridge DPAPI secret decrypted to an empty value.'
        }
        return $plain
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
        $secure = $null
    }
}

function Save-BrowserTokenDpapi([string]$Token) {
    if ([string]::IsNullOrWhiteSpace($Token) -or $Token.Length -lt 32) {
        throw 'Browser bridge token is missing or too short.'
    }
    $secretDir = Split-Path $BrowserBridgeDpapiPath -Parent
    New-Item -ItemType Directory -Path $secretDir -Force | Out-Null
    $secure = ConvertTo-SecureString -String $Token -AsPlainText -Force
    try {
        $encrypted = ConvertFrom-SecureString -SecureString $secure
        [IO.File]::WriteAllText($BrowserBridgeDpapiPath, $encrypted, [Text.UTF8Encoding]::new($false))
    }
    finally {
        $secure = $null
    }
    $roundTrip = Read-BrowserTokenDpapi
    try {
        if ($roundTrip -cne $Token) {
            Remove-Item -LiteralPath $BrowserBridgeDpapiPath -Force -ErrorAction SilentlyContinue
            throw 'Browser bridge DPAPI verification failed.'
        }
    }
    finally {
        $roundTrip = $null
    }
}

function Ensure-BrowserTokenSecret {
    $legacyPath = "$root\public\browser.env"
    $legacyToken = $null
    if (Test-Path -LiteralPath $legacyPath -PathType Leaf) {
        $line = @(Get-Content -LiteralPath $legacyPath | Where-Object { $_ -match '^BROWSER_BRIDGE_TOKEN=' })[0]
        if (-not $line) {
            throw 'Legacy public\\browser.env exists but does not contain BROWSER_BRIDGE_TOKEN.'
        }
        $legacyToken = ($line -replace '^BROWSER_BRIDGE_TOKEN=', '').Trim()
        if ($legacyToken.Length -lt 32) {
            throw 'Legacy BROWSER_BRIDGE_TOKEN is too short.'
        }
    }

    if (-not (Test-Path -LiteralPath $BrowserBridgeDpapiPath -PathType Leaf)) {
        $token = $legacyToken
        if ([string]::IsNullOrWhiteSpace($token)) {
            $bytes = New-Object byte[] 32
            $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
            try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
            $token = [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
        }
        try {
            Save-BrowserTokenDpapi -Token $token
        }
        finally {
            $token = $null
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($legacyToken)) {
        $stored = Read-BrowserTokenDpapi
        try {
            if ($stored -cne $legacyToken) {
                throw 'Browser bridge DPAPI secret does not match legacy browser.env; refusing to remove plaintext.'
            }
        }
        finally {
            $stored = $null
        }
        Remove-Item -LiteralPath $legacyPath -Force
    }
    $legacyToken = $null
}

function Get-BrowserToken {
    Ensure-BrowserTokenSecret
    $token = Read-BrowserTokenDpapi
    if ($token.Length -lt 32) { throw 'BROWSER_BRIDGE_TOKEN is too short' }
    return $token
}

function Get-ConfiguredBrowserPort {
    $line = @(Get-Content "$root\.env" -ErrorAction SilentlyContinue | Where-Object { $_ -match '^BROWSER_BRIDGE_PORT=' })[0]
    if (-not $line) { return $null }
    $value = ($line -replace '^BROWSER_BRIDGE_PORT=', '').Trim()
    $port = 0
    if (-not [int]::TryParse($value, [ref]$port) -or $port -lt 1024 -or $port -gt 65535) {
        throw 'BROWSER_BRIDGE_PORT in .env must be an integer between 1024 and 65535'
    }
    return $port
}

function Get-RuntimeBrowserPort {
    $path = "$root\.runtime\browser-bridge.port"
    if (-not (Test-Path $path)) { return $null }
    $value = (Get-Content $path -Raw).Trim()
    $port = 0
    if (-not [int]::TryParse($value, [ref]$port) -or $port -lt 1024 -or $port -gt 65535) {
        return $null
    }
    return $port
}

function Save-RuntimeBrowserPort([int]$Port) {
    $runtime = "$root\.runtime"
    New-Item -ItemType Directory -Path $runtime -Force | Out-Null
    Set-Content -LiteralPath "$runtime\browser-bridge.port" -Value $Port -Encoding ascii
}

function Test-TcpPortFree([int]$Port) {
    $listener = $null
    try {
        $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
        $listener.Start()
        return $true
    } catch {
        return $false
    } finally {
        if ($listener) {
            try { $listener.Stop() } catch {}
        }
    }
}

function Test-BrowserBridgeAtPort([int]$Port) {
    try {
        $token = Get-BrowserToken
        $h = @{ Authorization = "Bearer $token" }
        $r = Invoke-RestMethod -Method Get -Uri "http://127.0.0.1:$Port/health" -Headers $h -TimeoutSec 1
        return [bool]$r.ok
    } catch {
        return $false
    }
}

function Get-BrowserPort {
    $configured = Get-ConfiguredBrowserPort
    if ($null -ne $configured) { return [int]$configured }
    $runtimePort = Get-RuntimeBrowserPort
    if ($null -ne $runtimePort) { return [int]$runtimePort }
    return $null
}

function Select-BrowserPort {
    $configured = Get-ConfiguredBrowserPort
    if ($null -ne $configured) {
        if (Test-BrowserBridgeAtPort $configured) {
            Save-RuntimeBrowserPort $configured
            return [int]$configured
        }
        if (-not (Test-TcpPortFree $configured)) {
            throw "Configured BROWSER_BRIDGE_PORT $configured is already in use by another process."
        }
        Save-RuntimeBrowserPort $configured
        return [int]$configured
    }

    $runtimePort = Get-RuntimeBrowserPort
    if ($null -ne $runtimePort) {
        if ((Test-BrowserBridgeAtPort $runtimePort) -or (Test-TcpPortFree $runtimePort)) {
            return [int]$runtimePort
        }
    }

    foreach ($candidate in 8765..8799) {
        if (Test-TcpPortFree $candidate) {
            Save-RuntimeBrowserPort $candidate
            return [int]$candidate
        }
    }
    throw 'No free browser bridge port found in 8765-8799. Set BROWSER_BRIDGE_PORT in .env to an available localhost port.'
}

function Test-BrowserBridge {
    $port = Get-BrowserPort
    if ($null -eq $port) { return $false }
    return Test-BrowserBridgeAtPort $port
}

function Set-BrowserComposePort {
    $port = Get-BrowserPort
    if ($null -ne $port) {
        $env:BROWSER_BRIDGE_PORT = [string]$port
    }
}

function Start-BrowserBridge {
    if (-not $public) { return $null }
    Ensure-BrowserTokenSecret

    $existingPort = Get-BrowserPort
    if ($null -ne $existingPort -and (Test-BrowserBridgeAtPort $existingPort)) {
        $env:BROWSER_BRIDGE_PORT = [string]$existingPort
        return [int]$existingPort
    }

    $runtime = "$root\.runtime"
    New-Item -ItemType Directory -Path $runtime -Force | Out-Null
    $node = (Get-Command node -ErrorAction Stop).Source
    $token = Get-BrowserToken
    $port = Select-BrowserPort
    $oldToken = $env:BROWSER_BRIDGE_TOKEN
    $oldPort = $env:BROWSER_BRIDGE_PORT
    try {
        $env:BROWSER_BRIDGE_TOKEN = $token
        $env:BROWSER_BRIDGE_PORT = [string]$port
        $proc = Start-Process -FilePath $node -ArgumentList @("`"$root\public\browser-bridge.mjs`"") -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput "$runtime\browser-bridge.out.log" -RedirectStandardError "$runtime\browser-bridge.err.log"
    } finally {
        $env:BROWSER_BRIDGE_TOKEN = $oldToken
        $env:BROWSER_BRIDGE_PORT = $oldPort
    }
    Set-Content -LiteralPath "$runtime\browser-bridge.pid" -Value $proc.Id -Encoding ascii
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        if (Test-BrowserBridgeAtPort $port) {
            $env:BROWSER_BRIDGE_PORT = [string]$port
            return [int]$port
        }
        if ($proc.HasExited) { break }
    }
    throw "Browser bridge failed to start on port $port. See $runtime\browser-bridge.err.log"
}

function Stop-BrowserBridge {
    $pidFile = "$root\.runtime\browser-bridge.pid"
    if (Test-Path $pidFile) {
        $pidValue = [int](Get-Content $pidFile -Raw)
        $p = Get-CimInstance Win32_Process -Filter "ProcessId=$pidValue" -ErrorAction SilentlyContinue
        if ($p -and [string]$p.CommandLine -like '*browser-bridge.mjs*') {
            Stop-Process -Id $pidValue -Force -ErrorAction SilentlyContinue
        }
        Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
    }
    Remove-Item "$root\.runtime\browser-bridge.port" -Force -ErrorAction SilentlyContinue
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
    Ensure-BrowserTokenSecret
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
        $browser = $null
        try {
            $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET = $postgres
            $env:FIRECRAWL_SEARXNG_SECRET_SECRET = $searxng
            if ($public) {
                $browser = Get-BrowserToken
                $env:FIRECRAWL_BROWSER_BRIDGE_SECRET = $browser
            }
        }
        finally {
            $postgres = $null
            $searxng = $null
            $browser = $null
        }
        return
    }

    $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET = "compose-config-only"
    $env:FIRECRAWL_SEARXNG_SECRET_SECRET = "compose-config-only"
    if ($public) {
        $env:FIRECRAWL_BROWSER_BRIDGE_SECRET = "compose-config-only"
    }
}

function Restore-ComposeServiceSecrets(
    [bool]$PostgresWasSet,
    [AllowNull()][string]$PostgresValue,
    [bool]$SearxngWasSet,
    [AllowNull()][string]$SearxngValue,
    [bool]$BrowserWasSet,
    [AllowNull()][string]$BrowserValue
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

    if ($BrowserWasSet) {
        $env:FIRECRAWL_BROWSER_BRIDGE_SECRET = $BrowserValue
    } else {
        Remove-Item Env:FIRECRAWL_BROWSER_BRIDGE_SECRET -ErrorAction SilentlyContinue
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

function Import-AvailableRuntimeSecrets {
    if (Test-Path -LiteralPath $GeminiDpapiPath -PathType Leaf) {
        Import-GeminiKey
        return
    }
    if (Test-LegacyGeminiKey) {
        throw 'Legacy GEMINI_API_KEY found in secrets.env. Run .\scripts\configure_gemini.ps1 once to migrate it to DPAPI.'
    }
}

$composeActions = @("up", "redeploy", "down", "status", "logs", "recover", "repair-postgres-auth", "import-gemini")
$postgresSecretWasSet = Test-Path Env:FIRECRAWL_POSTGRES_PASSWORD_SECRET
$postgresSecretOriginal = if ($postgresSecretWasSet) { $env:FIRECRAWL_POSTGRES_PASSWORD_SECRET } else { $null }
$searxngSecretWasSet = Test-Path Env:FIRECRAWL_SEARXNG_SECRET_SECRET
$searxngSecretOriginal = if ($searxngSecretWasSet) { $env:FIRECRAWL_SEARXNG_SECRET_SECRET } else { $null }
$browserSecretWasSet = Test-Path Env:FIRECRAWL_BROWSER_BRIDGE_SECRET
$browserSecretOriginal = if ($browserSecretWasSet) { $env:FIRECRAWL_BROWSER_BRIDGE_SECRET } else { $null }

try {
    if ($Action -in $composeActions) {
        Set-RuntimeHookSecrets -UseRealSecrets ($Action -in @("up", "redeploy"))
    }
switch ($Action) {
    'up' {
        if ($public) {
            Stop-BrowserBridge
            $port = Start-BrowserBridge
            $env:BROWSER_BRIDGE_PORT = [string]$port
            "browser bridge port: $port"
        }
        docker @compose up -d --build
        if ($LASTEXITCODE -ne 0) { throw "docker compose up failed with exit code $LASTEXITCODE" }
        Set-RuntimeHookSecrets -UseRealSecrets $false
        Import-AvailableRuntimeSecrets
    }
    'redeploy' {
        if ($public) {
            Stop-BrowserBridge
            $port = Start-BrowserBridge
            $env:BROWSER_BRIDGE_PORT = [string]$port
            "browser bridge port: $port"
        }
        docker @compose up -d --build --force-recreate
        if ($LASTEXITCODE -ne 0) { throw "docker compose redeploy failed with exit code $LASTEXITCODE" }
        Set-RuntimeHookSecrets -UseRealSecrets $false
        Import-AvailableRuntimeSecrets
    }
    'down' {
        if ($public) { Set-BrowserComposePort }
        docker @compose down
        Stop-BrowserBridge
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
        if ($public) { Set-BrowserComposePort }
        docker @compose ps
        if ($public) {
            $port = Get-BrowserPort
            if (Test-BrowserBridge) { "browser bridge: healthy (port $port)" } else { "browser bridge: down" }
        }
    }
    'logs' {
        if ($public) { Set-BrowserComposePort; docker @compose logs -f --tail 50 api gateway }
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
            Set-BrowserComposePort
            $port = Get-BrowserPort
            "browser bridge: healthy=$(Test-BrowserBridge) port=$port"

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
        -SearxngValue $searxngSecretOriginal `
        -BrowserWasSet $browserSecretWasSet `
        -BrowserValue $browserSecretOriginal
}