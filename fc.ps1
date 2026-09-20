# Manage the local Firecrawl stack.  Usage: .\fc.ps1 up | down | status | logs | test | url
# The public ChatGPT part (compose.public.yaml) is included once the tunnel is configured.
param([ValidateSet('up', 'down', 'status', 'logs', 'test', 'url')][string]$Action = 'status')

$root = $PSScriptRoot
$compose = @('compose', '--project-directory', "$root\firecrawl",
    '-f', "$root\firecrawl\docker-compose.yaml", '-f', "$root\compose.local.yaml")
$public = Test-Path "$root\.cloudflared\config.yml"
if ($public) { $compose += @('-f', "$root\compose.public.yaml") }
$compose += @('--env-file', "$root\.env")
if (Test-Path "$root\secrets.env") { $compose += @('--env-file', "$root\secrets.env") }

function Get-PublicUrl {
    $secret = ((Get-Content "$root\public\gateway.env") -match '^GATEWAY_SECRET=')[0] -replace '^GATEWAY_SECRET=', ''
    $hostname = ((Get-Content "$root\.cloudflared\config.yml") -match 'hostname:')[0] -replace '.*hostname:\s*', ''
    $aud = ((Get-Content "$root\.env") -match '^ACCESS_AUD=')[0] -replace '^ACCESS_AUD=', ''
    if ($aud) { "https://$hostname/mcp" } else { "https://$hostname/$secret/mcp" }
}

switch ($Action) {
    'up'     { docker @compose up -d --build }
    'down'   { docker @compose down }
    'status' { docker @compose ps }
    'logs'   { if ($public) { docker @compose logs -f --tail 50 api gateway cloudflared } else { docker @compose logs -f --tail 100 api } }
    'url'    { if ($public) { Get-PublicUrl } else { 'Public access is not configured.' } }
    'test'   {
        $body = @{ url = 'https://www.iana.org/help/example-domains'; formats = @('markdown') } | ConvertTo-Json
        $r = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:3002/v2/scrape' -ContentType 'application/json' -Body $body -TimeoutSec 120
        "local engine: success=$($r.success)"
        if ($public) {
            $init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fc-test","version":"1"}}}'
            $h = @{ Accept = 'application/json, text/event-stream' }
            $resp = Invoke-WebRequest -Method Post -Uri (Get-PublicUrl) -ContentType 'application/json' -Headers $h -Body $init -TimeoutSec 60
            "public MCP endpoint: HTTP $($resp.StatusCode)"
        }
    }
}
