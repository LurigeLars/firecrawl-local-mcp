# Requires an explicitly requested local benchmark. No production service changes.
[CmdletBinding()]
param([switch]$Run)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if (-not $Run) {
    throw 'Pass -Run to build a disposable release stack and run the bounded comparison.'
}
$root = Split-Path -Parent $PSScriptRoot
$project = 'fc-rust-bench-' + ([Guid]::NewGuid().ToString('N').Substring(0,12))
if ($project -notmatch '^fc-rust-bench-[0-9a-f]{12}$') { throw 'Unsafe project name.' }
$compose = @('compose','-p',$project,'-f',(Join-Path $PSScriptRoot 'compose.rust-research.yaml'),'--profile','search')
$load = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'verify-mcp-load.mjs')

function Invoke-Docker {
    param([string[]]$Arguments)
    $out = & docker @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Docker command failed: $($Arguments[0])" }
    return $out
}

function Get-MemoryBytes {
    param([string]$Usage)
    $first = ($Usage -split '/')[0].Trim()
    if ($first -notmatch '^([0-9]+(?:\.[0-9]+)?)\s*(B|KiB|MiB|GiB|TiB|kB|MB|GB|TB)$') {
        throw "Unsupported Docker memory unit: $first"
    }
    $number = [double]::Parse($Matches[1],[Globalization.CultureInfo]::InvariantCulture)
    $scale = switch -CaseSensitive ($Matches[2]) {
        'B' { 1 }
        'KiB' { 1024 }
        'MiB' { 1048576 }
        'GiB' { 1073741824 }
        'TiB' { 1099511627776 }
        'kB' { 1000 }
        'MB' { 1000000 }
        'GB' { 1000000000 }
        'TB' { 1000000000000 }
    }
    return [long]($number * $scale)
}

function Get-ContainerForService {
    param([string]$ProjectName,[string]$Service)
    $ids = @(Invoke-Docker -Arguments @('ps','-q','--no-trunc','--filter',"label=com.docker.compose.project=$ProjectName",'--filter',"label=com.docker.compose.service=$Service"))
    if ($ids.Count -ne 1 -or $ids[0] -notmatch '^[0-9a-f]{12,64}$') {
        throw "Expected one running container: $ProjectName/$Service"
    }
    return $ids[0]
}

function Get-Sample {
    param([hashtable]$Containers,[string[]]$Replacement)
    $ids = @($Containers.Values)
    $lines = @(Invoke-Docker -Arguments (@('stats','--no-stream','--format','{{json .}}') + $ids))
    $rows = @{}
    foreach ($line in $lines) {
        $v = $line | ConvertFrom-Json
        $service = @($Containers.Keys | Where-Object {
            $id = [string]$Containers[$_]
            $seen = [string]$v.ID
            $id -eq $v.Container -or ($seen -match '^[0-9a-f]{12,64}$' -and $id.StartsWith($seen,[StringComparison]::Ordinal))
        })
        if ($service.Count -ne 1) { throw 'Unexpected Docker stats container.' }
        $rows[$service[0]] = @{memory_bytes=(Get-MemoryBytes $v.MemUsage);cpu=$v.CPUPerc;pids=$v.PIDs}
    }
    if ($rows.Count -ne $Containers.Count) { throw 'Incomplete Docker stats snapshot.' }
    $sum = [long]0
    foreach ($service in $Replacement) { $sum += $rows[$service].memory_bytes }
    return [pscustomobject]@{utc=[DateTimeOffset]::UtcNow.ToString('o');replacement_bytes=$sum;services=$rows}
}

function Measure-Backend {
    param([string]$Backend,[hashtable]$Containers,[string[]]$Replacement)
    $samples = [Collections.Generic.List[object]]::new()
    $samples.Add((Get-Sample $Containers $Replacement))
    $job = Start-Job -ArgumentList $Containers.mcp,$Backend,$load -ScriptBlock {
        param($Container,$Mode,$Source)
        $ErrorActionPreference = 'Stop'
        $result = $Source | & docker exec -i $Container node --input-type=module - $Mode
        if ($LASTEXITCODE -ne 0) { throw 'Bounded MCP workload failed.' }
        return ($result -join "`n")
    }
    try {
        $end = [DateTimeOffset]::UtcNow.AddSeconds(150)
        while ($job.State -in @('NotStarted','Running') -and [DateTimeOffset]::UtcNow -lt $end) {
            $samples.Add((Get-Sample $Containers $Replacement))
            Start-Sleep -Milliseconds 500
        }
        if ($job.State -ne 'Completed') { throw "Benchmark job did not complete: $Backend/$($job.State)" }
        $receipt = (Receive-Job -Job $job -ErrorAction Stop) | ConvertFrom-Json
        if ($receipt.verdict -ne 'PASS' -or $receipt.requests -ne 10 -or $receipt.markers -ne 30) {
            throw "Incomplete content verification: $Backend"
        }
        $samples.Add((Get-Sample $Containers $Replacement))
        return [pscustomobject]@{
            backend=$Backend;receipt=$receipt;sample_count=$samples.Count
            before_bytes=$samples[0].replacement_bytes
            after_bytes=$samples[$samples.Count-1].replacement_bytes
            sampled_peak_bytes=($samples | Measure-Object -Property replacement_bytes -Maximum).Maximum
            samples=$samples
        }
    } finally {
        if ($job.State -in @('NotStarted','Running')) { Stop-Job -Job $job -ErrorAction SilentlyContinue }
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
}

# The explicit project prefix, no external mounts/networks, and no published
# host ports are prerequisites. Only this unique project can be torn down.
$legacy = @{}
foreach ($service in @('api','rabbitmq','nuq-postgres','redis','mcp','searxng','searxng-proxy')) {
    $legacy[$service] = Get-ContainerForService 'firecrawl' $service
}
$revision = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $revision -notmatch '^[0-9a-f]{40}$') { throw 'Invalid repository revision.' }
$dirty = @(& git -C $root status --porcelain)
if ($LASTEXITCODE -ne 0 -or $dirty.Count -ne 0) { throw 'Use a clean reviewed checkout.' }
$config = (Invoke-Docker -Arguments ($compose + @('config','--format','json'))) -join "`n" | ConvertFrom-Json
foreach ($service in $config.services.PSObject.Properties) {
    if ($service.Value.ports -or $service.Value.container_name) { throw 'Research project must not publish host ports or fixed container names.' }
}
foreach ($network in $config.networks.PSObject.Properties) {
    if ($network.Value.external -or $network.Value.name -notlike "$project*") { throw 'Unexpected research network.' }
}
foreach ($volume in $config.volumes.PSObject.Properties) {
    if ($volume.Value.external -or $volume.Value.name -notlike "$project*") { throw 'Unexpected research volume.' }
}

$attempted = $false
$oldFixtureMode = [Environment]::GetEnvironmentVariable('RESEARCH_CI_FIXTURE','Process')
try {
    $attempted = $true
    # Local mode always uses a random credential, not the public CI fixture.
    $env:RESEARCH_CI_FIXTURE = '0'
    & docker @compose up -d --build
    if ($LASTEXITCODE -ne 0) { throw 'Disposable release build/start failed.' }
    $candidate = @{}
    foreach ($service in @('api','pdf-ocr','mcp','searxng','searxng-proxy')) {
        $candidate[$service] = Get-ContainerForService $project $service
    }
    Start-Sleep -Seconds 5
    $old = Measure-Backend 'legacy' $legacy @('api','rabbitmq','nuq-postgres','redis')
    $new = Measure-Backend 'candidate' $candidate @('api','pdf-ocr')
    if ($old.sampled_peak_bytes -le 0) { throw 'Invalid legacy sample.' }
    # Same fixed query through the actual MCP SDK in both backends. No
    # credential-file access in the local benchmark; engine variability
    # remains separate from HTML memory/latency and document quality.
    $searchEvidence = @{}
    foreach ($entry in @(@{mode='legacy';id=$legacy.mcp},@{mode='candidate';id=$candidate.mcp})) {
        $lines = @($load | & docker exec -i $entry.id node --input-type=module - $entry.mode --search)
        $exit = $LASTEXITCODE
        try {
            $receipt = ($lines -join "`n") | ConvertFrom-Json
            if ($receipt.backend -ne $entry.mode -or $receipt.verdict -notin @('PASS','UNVERIFIED')) {
                throw 'Invalid search receipt'
            }
            $searchEvidence[$entry.mode] = $receipt
        } catch {
            $searchEvidence[$entry.mode] = @{backend=$entry.mode;verdict='UNVERIFIED';reason='missing_or_invalid_receipt'}
        }
        if ($exit -ne 0) { $searchEvidence[$entry.mode].verdict = 'UNVERIFIED' }
    }
    # Separate document evidence: a legacy extraction failure is reported,
    # not mistaken for a speedup or allowed to erase the HTML measurements.
    $documentEvidence = @{}
    foreach ($entry in @(@{mode='legacy';id=$legacy.mcp},@{mode='candidate';id=$candidate.mcp})) {
        $lines = @($load | & docker exec -i $entry.id node --input-type=module - $entry.mode --documents)
        $exit = $LASTEXITCODE
        try {
            $receipt = ($lines -join "`n") | ConvertFrom-Json
            if ($receipt.backend -ne $entry.mode -or @($receipt.results).Count -ne 2 -or $receipt.verdict -notin @('PASS','UNVERIFIED')) {
                throw 'Invalid document receipt'
            }
            $documentEvidence[$entry.mode] = $receipt
        } catch {
            $documentEvidence[$entry.mode] = @{backend=$entry.mode;verdict='UNVERIFIED';reason='missing_or_invalid_receipt'}
        }
        if ($exit -ne 0) { $documentEvidence[$entry.mode].verdict = 'UNVERIFIED' }
    }
    [pscustomobject]@{
        revision=$revision;project=$project
        scope='same host, actual MCP HTTP, bounded HTML workload, replaceable subsystem only; production remains active'
        limits='sampled Docker CLI memory, not exact peak; concurrent production traffic and other services may confound; not full feature parity'
        sampled_peak_reduction_pct=[Math]::Round(100*(1-$new.sampled_peak_bytes/$old.sampled_peak_bytes),2)
        legacy=$old;candidate=$new;search=$searchEvidence;documents=$documentEvidence
    } | ConvertTo-Json -Depth 12
} finally {
    if ($attempted -and $project -match '^fc-rust-bench-[0-9a-f]{12}$') {
        & docker @compose down --volumes --timeout 10
        if ($LASTEXITCODE -ne 0) { Write-Warning "Disposable project cleanup failed: $project" }
    }
    [Environment]::SetEnvironmentVariable('RESEARCH_CI_FIXTURE',$oldFixtureMode,'Process')
}
