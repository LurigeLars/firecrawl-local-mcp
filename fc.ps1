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
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
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

function Test-BrowserBridge {
    try {
        $token = Get-BrowserToken
        $h = @{ Authorization = "Bearer $token" }
        $r = Invoke-RestMethod -Method Get -Uri 'http://127.0.0.1:8765/health' -Headers $h -TimeoutSec 1
        return [bool]$r.ok
    } catch { return $false }
}

function Start-BrowserBridge {
    if (-not $public) { return }
    Ensure-BrowserEnv
    if (Test-BrowserBridge) { return }
    $runtime = "$root\.runtime"
    New-Item -ItemType Directory -Path $runtime -Force | Out-Null
    $node = (Get-Command node -ErrorAction Stop).Source
    $token = Get-BrowserToken
    $oldToken = $env:BROWSER_BRIDGE_TOKEN
    try {
        $env:BROWSER_BRIDGE_TOKEN = $token
        $proc = Start-Process -FilePath $node -ArgumentList @("`"$root\public\browser-bridge.mjs`"") -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput "$runtime\browser-bridge.out.log" -RedirectStandardError "$runtime\browser-bridge.err.log"
    } finally {
        $env:BROWSER_BRIDGE_TOKEN = $oldToken
    }
    Set-Content -LiteralPath "$runtime\browser-bridge.pid" -Value $proc.Id -Encoding ascii
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        if (Test-BrowserBridge) { return }
        if ($proc.HasExited) { break }
    }
    throw "Browser bridge failed to start. See $runtime\browser-bridge.err.log"
}

function Stop-BrowserBridge {
    $pidFile = "$root\.runtime\browser-bridge.pid"
    if (-not (Test-Path $pidFile)) { return }
    $pidValue = [int](Get-Content $pidFile -Raw)
    $p = Get-CimInstance Win32_Process -Filter "ProcessId=$pidValue" -ErrorAction SilentlyContinue
    if ($p -and [string]$p.CommandLine -like '*browser-bridge.mjs*') {
        Stop-Process -Id $pidValue -Force -ErrorAction SilentlyContinue
    }
    Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
}

function Get-PublicUrl {
    $secret = ((Get-Content "$root\public\gateway.env") -match '^GATEWAY_SECRET=')[0] -replace '^GATEWAY_SECRET=', ''
    $hostname = ((Get-Content "$root\.cloudflared\config.yml") -match 'hostname:')[0] -replace '.*hostname:\s*', ''
    $aud = ((Get-Content "$root\.env") -match '^ACCESS_AUD=')[0] -replace '^ACCESS_AUD=', ''
    if ($aud) { "https://$hostname/mcp" } else { "https://$hostname/$secret/mcp" }
}

switch ($Action) {
    'up'     { if ($public) { Start-BrowserBridge }; docker @compose up -d --build }
    'down'   { docker @compose down; Stop-BrowserBridge }
    'status' { docker @compose ps; if ($public) { if (Test-BrowserBridge) { 'browser bridge: healthy' } else { 'browser bridge: down' } } }
    'logs'   { if ($public) { docker @compose logs -f --tail 50 api gateway cloudflared } else { docker @compose logs -f --tail 100 api } }
    'url'    { if ($public) { Get-PublicUrl } else { 'Public access is not configured.' } }
    'test'   {
        $body = @{ url = 'https://www.iana.org/help/example-domains'; formats = @('markdown') } | ConvertTo-Json
        $r = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:3002/v2/scrape' -ContentType 'application/json' -Body $body -TimeoutSec 120
        "local engine: success=$($r.success)"
        if ($public) {
            "browser bridge: healthy=$(Test-BrowserBridge)"
            $init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fc-test","version":"1"}}}'
            $h = @{ Accept = 'application/json, text/event-stream' }
            $resp = Invoke-WebRequest -Method Post -Uri (Get-PublicUrl) -ContentType 'application/json' -Headers $h -Body $init -TimeoutSec 60
            "public MCP endpoint: HTTP $($resp.StatusCode)"
        }
    }
}