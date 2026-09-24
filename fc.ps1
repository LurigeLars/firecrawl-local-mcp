# Manage the local Firecrawl stack.  Usage: .\fc.ps1 up | down | status | logs | test | url
# When public ChatGPT access is configured, also manages the narrow local browser bridge used for Season supplier login.
# The public ChatGPT part (compose.public.yaml) is included once the tunnel is configured.
param([ValidateSet('up', 'down', 'status', 'logs', 'test', 'url')][string]$Action = 'status')

$root = $PSScriptRoot
$compose = @('compose', '--project-directory', "$root\firecrawl",
    '-f', "$root\firecrawl\docker-compose.yaml", '-f', "$root\compose.local.yaml")
$public = Test-Path "$root\.cloudflared\config.yml"
if ($public) { $compose += @('-f', "$root\compose.public.yaml") }
$compose += @('--env-file', "$root\.env")
if (Test-Path "$root\secrets.env") { $compose += @('--env-file', "$root\secrets.env") }

function Ensure-BrowserEnv {
    $path = "$root\public\browser.env"
    if (Test-Path $path) { return }
    $bytes = New-Object byte[] 32
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($bytes) } finally { $rng.Dispose() }
    $token = [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
    Set-Content -LiteralPath $path -Value "BROWSER_BRIDGE_TOKEN=$token" -Encoding ascii
}

function Get-BrowserToken {
    Ensure-BrowserEnv
    $line = @(Get-Content "$root\public\browser.env" | Where-Object { $_ -match '^BROWSER_BRIDGE_TOKEN=' })[0]
    if (-not $line) { throw 'BROWSER_BRIDGE_TOKEN missing from public\\browser.env' }
    $token = $line -replace '^BROWSER_BRIDGE_TOKEN=', ''
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
    Ensure-BrowserEnv

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

if ($public) { Ensure-BrowserEnv }

function Get-PublicUrl {
    $secret = ((Get-Content "$root\public\gateway.env") -match '^GATEWAY_SECRET=')[0] -replace '^GATEWAY_SECRET=', ''
    $hostname = ((Get-Content "$root\.cloudflared\config.yml") -match 'hostname:')[0] -replace '.*hostname:\s*', ''
    $aud = ((Get-Content "$root\.env") -match '^ACCESS_AUD=')[0] -replace '^ACCESS_AUD=', ''
    if ($aud) { "https://$hostname/mcp" } else { "https://$hostname/$secret/mcp" }
}

switch ($Action) {
    'up' {
        if ($public) {
            $port = Start-BrowserBridge
            $env:BROWSER_BRIDGE_PORT = [string]$port
            "browser bridge port: $port"
        }
        docker @compose up -d --build
    }
    'down' {
        if ($public) { Set-BrowserComposePort }
        docker @compose down
        Stop-BrowserBridge
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
        if ($public) { Set-BrowserComposePort; docker @compose logs -f --tail 50 api gateway cloudflared }
        else { docker @compose logs -f --tail 100 api }
    }
    'url' {
        if ($public) { Get-PublicUrl } else { 'Public access is not configured.' }
    }
    'test' {
        $body = @{ url = 'https://www.iana.org/help/example-domains'; formats = @('markdown') } | ConvertTo-Json
        $r = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:3002/v2/scrape' -ContentType 'application/json' -Body $body -TimeoutSec 120
        "local engine: success=$($r.success)"
        if ($public) {
            Set-BrowserComposePort
            $port = Get-BrowserPort
            "browser bridge: healthy=$(Test-BrowserBridge) port=$port"
            $init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fc-test","version":"1"}}}'
            $h = @{ Accept = 'application/json, text/event-stream' }
            $resp = Invoke-WebRequest -Method Post -Uri (Get-PublicUrl) -ContentType 'application/json' -Headers $h -Body $init -TimeoutSec 60
            "public MCP endpoint: HTTP $($resp.StatusCode)"
        }
    }
}
