# Explicit opt-in, not part of production maintenance. Only this named project
# is started/stopped. No published ports, legacy restarts or secret import.
[CmdletBinding()]
param([switch]$Run)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = Split-Path -Parent $PSScriptRoot
$project = 'firecrawl-rust-canary'
$compose = @('compose','-p',$project,'-f',(Join-Path $PSScriptRoot 'compose.rust-canary.yaml'))
function Invoke-CanaryDocker {
    param([string[]]$Arguments)
    $output = & docker @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Canary Docker action failed: $($Arguments[0])" }
    return $output
}
$config = (Invoke-CanaryDocker -Arguments ($compose + @('config','--format','json'))) -join [Environment]::NewLine | ConvertFrom-Json
$names = @($config.services.PSObject.Properties.Name | Sort-Object)
if (($names -join ',') -cne 'init,mcp,pdf-ocr,rust-api') { throw 'Unexpected canary services' }
foreach ($service in $config.services.PSObject.Properties.Value) {
    if ($service.ports -or $service.container_name -or $service.privileged -or -not $service.read_only) { throw 'Unsafe canary service configuration' }
}
if ($config.name -cne $project -or $config.networks.production_backend.name -cne 'firecrawl_backend' -or
    -not $config.networks.production_backend.external -or -not $config.networks.private.internal -or
    $config.networks.private.name -cne "${project}_private") { throw 'Unexpected canary network' }
foreach ($name in @('mcp','pdf-ocr')) {
    $nets = @($config.services.$name.networks.PSObject.Properties.Name)
    if ($nets.Count -ne 1 -or $nets[0] -cne 'private') { throw 'Worker/MCP must remain private' }
}
$api = $config.services.'rust-api'
if (@($api.networks.PSObject.Properties.Name).Count -ne 2 -or
    $api.networks.private.aliases -cnotcontains 'api' -or
    $api.networks.production_backend.aliases -contains 'api' -or
    $config.services.init.environment.RESEARCH_CI_FIXTURE -cne '0') { throw 'Unexpected API alias or credential mode' }
foreach ($volume in $config.volumes.PSObject.Properties.Value) {
    if ($volume.external -or $volume.name -notlike "${project}_*") { throw 'Unsafe canary volume' }
}
if (-not $Run) {
    [pscustomobject]@{project=$project;verdict='PLAN';services=$names;production_routing='unchanged';host_ports=0;model='disabled'} | ConvertTo-Json -Compress
    return
}
$dirty = @(& git -C $root status --porcelain)
if ($LASTEXITCODE -ne 0 -or $dirty.Count -ne 0) { throw 'Use a clean reviewed checkout' }
$branch = (& git -C $root branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or $branch -cne 'main') { throw 'Use reviewed main' }
$revision = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $revision -notmatch '^[0-9a-f]{40}$') { throw 'Invalid revision' }
$backendLabel = @(Invoke-CanaryDocker -Arguments @('network','inspect','--format','{{index .Labels "com.docker.compose.project"}}','firecrawl_backend'))
if ($backendLabel.Count -ne 1 -or $backendLabel[0] -cne 'firecrawl') { throw 'Unexpected production backend ownership' }
foreach ($service in @('api','searxng-proxy','playwright-router')) {
    $container = "firecrawl-$service-1"
    $onBackend = @(Invoke-CanaryDocker -Arguments @('inspect','--format','{{.State.Running}} {{if index .NetworkSettings.Networks "firecrawl_backend"}}true{{else}}false{{end}}',$container))
    if ($onBackend.Count -ne 1 -or $onBackend[0] -cne 'true true') { throw "Production sidecar unavailable: $service" }
}
# Refuse a pre-existing canary so this run cannot stop another test.
$existing = @(Invoke-CanaryDocker -Arguments @('ps','-aq','--filter',"label=com.docker.compose.project=$project"))
if ($existing.Count -ne 0) { throw 'Existing canary containers; inspect before another run' }
$driver = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'verify-mcp-load.mjs')
$receipts = @{}
$attempted = $false
try {
    $attempted = $true
    & docker @compose up -d --build
    if ($LASTEXITCODE -ne 0) { throw 'Canary build/start failed' }
    # Inspect only IDs, aliases and network counts, never env or secrets.
    foreach ($service in @('rust-api','mcp','pdf-ocr')) {
        $id = @(Invoke-CanaryDocker -Arguments ($compose + @('ps','-q',$service)))
        if ($id.Count -ne 1 -or $id[0] -notmatch '^[0-9a-f]{12,64}$') { throw "Canary unavailable: $service" }
        $shape = @(Invoke-CanaryDocker -Arguments @('inspect','--format','{{len .NetworkSettings.Networks}} {{len .HostConfig.PortBindings}}',$id[0]))
        $expected = if ($service -eq 'rust-api') { '2 0' } else { '1 0' }
        if ($shape.Count -ne 1 -or $shape[0] -cne $expected) { throw "Unsafe canary runtime topology: $service" }
    }
    # All probes use the existing locked MCP transport; no credential-file reads.
    foreach ($mode in @('--documents','--search','--browser')) {
        $lines = @($driver | & docker @compose exec -T mcp node --input-type=module - candidate $mode)
        $exit = $LASTEXITCODE
        try {
            $receipt = ($lines -join [Environment]::NewLine) | ConvertFrom-Json
            if ($receipt.backend -cne 'candidate' -or $receipt.verdict -notin @('PASS','UNVERIFIED')) { throw 'Invalid receipt' }
        } catch { $receipt = @{backend='candidate';verdict='UNVERIFIED';reason='missing_or_invalid_receipt'} }
        if ($exit -ne 0) { $receipt.verdict = 'UNVERIFIED' }
        $receipts[$mode] = $receipt
    }
    $passed = @($receipts.Values | Where-Object {$_.verdict -ne 'PASS'}).Count -eq 0
    [pscustomobject]@{project=$project;revision=$revision;verdict=$(if($passed){'PASS'}else{'UNVERIFIED'});scope='shared production sidecars, fixed MCP fixtures, no production routing';receipts=$receipts} | ConvertTo-Json -Depth 8
    if (-not $passed) { throw 'Canary acceptance incomplete; inspect receipts' }
} finally {
    if ($attempted) {
        # Retain only the canary token/job volumes. Never remove external backend.
        & docker @compose down --timeout 15
        if ($LASTEXITCODE -ne 0) { Write-Warning 'Canary cleanup failed; inspect named project' }
    }
}
