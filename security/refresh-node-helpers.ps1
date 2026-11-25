# Security-gated, single-service upgrade of internal SearXNG language proxy.
# Never accesses public gateway credentials, DPAPI secrets, or other services.
function Invoke-FirecrawlNodeHelperRefresh {
    param(
        [Parameter(Mandatory)][ValidateSet('searxng-proxy')][string]$Service,
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Container = 'firecrawl-searxng-proxy-1'
    $TargetImage = 'firecrawl-searxng-proxy:local'
    $Required = [ordered]@{}
    foreach ($Name in @('firecrawl-api-1','firecrawl-redis-1','firecrawl-rabbitmq-1',
                        'firecrawl-nuq-postgres-1','firecrawl-searxng-1','firecrawl-gateway-1')) {
        $Id = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Id -notmatch '^[a-f0-9]{64}$') {
            throw "Unrelated container unavailable: $Name"
        }
        $Required[$Name] = [string]$Id
    }
    $BaselineId = Get-FirecrawlRunningImage -Container $Container
    $null = Get-FirecrawlPinnedNodeVersion -Dockerfile (Join-Path $ProjectRoot 'security\Dockerfile.searxng-proxy')
    $Rollback = Save-FirecrawlRollbackImage -Container $Container -BaselineId $BaselineId -Repository 'firecrawl-searxng-proxy'
    # Build stages only; the old container stays online until scan passes.
    & docker @ComposeArguments build $Service
    if ($LASTEXITCODE -ne 0) { throw 'SearXNG proxy build failed; old runtime remains active.' }
    $CandidateId = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$CandidateId -cnotmatch '^sha256:[a-f0-9]{64}$') {
        throw 'Candidate SearXNG proxy image identity unavailable.'
    }
    & docker scout cves --only-severity critical,high --only-fixed --exit-code --vex-location (Join-Path $PSScriptRoot 'vex/firecrawl-mcp.openvex.json') --vex-author 'firecrawl-local-mcp maintainers' ("local://" + $CandidateId) | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw 'Scout rejected candidate SearXNG proxy (fixable critical/high). Old runtime remains active.'
    }
    $AfterScan = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne [string]$CandidateId) {
        throw 'SearXNG proxy image identity changed after security scan.'
    }
    Assert-FirecrawlRollbackImage -Container $Container -BaselineId $BaselineId -RollbackTag $Rollback -RollbackId $BaselineId
    & docker @ComposeArguments up -d --no-deps --no-build --force-recreate $Service
    if ($LASTEXITCODE -ne 0) { throw 'Scoped SearXNG proxy deployment failed.' }
    $Actual = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$Actual -cne [string]$CandidateId) {
        throw 'SearXNG proxy not running the reviewed candidate.'
    }
    $Ready = $false
    for ($Attempt=0; $Attempt -lt 35; $Attempt++) {
        & docker exec $Container node -e 'fetch("http://searxng:8080/").then(r=>process.exit(r.status === 200?0:1)).catch(()=>process.exit(1))' 2>$null | Out-Null
        $Ready = ($LASTEXITCODE -eq 0)
        if ($Ready) { break }
        Start-Sleep -Seconds 2
    }
    if (-not $Ready) { throw 'SearXNG proxy failed its internal search backend check.' }
    foreach ($Name in $Required.Keys) {
        $AfterId = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$AfterId -cne [string]$Required[$Name]) {
            throw "Unrelated container was recreated: $Name"
        }
    }
    "SEARCH_PROXY_REFRESH_OK image=$CandidateId rollback=$Rollback internal_http=200 unrelated_container_ids_unchanged=true"
}
