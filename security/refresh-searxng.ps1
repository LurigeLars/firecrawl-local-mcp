# One-service patch of SearXNG. No full-stack Compose actions.
# Caller fc.ps1 injects DPAPI-derived secrets solely for the existing post_start hook.
function Invoke-FirecrawlSearchBackendRefresh {
    param([Parameter(Mandatory)][string[]]$ComposeArguments)
    $Service = 'searxng'
    $Container = 'firecrawl-searxng-1'
    $OldId = 'sha256:2fb0fa85096fe6df5c3ab98ecb4d6e0ee2ef66b8fb96ce6fce0f75b51c4bd90a'
    $TargetImage = 'searxng/searxng:2026.10.30-ffa88e3ba'
    $TargetId = 'sha256:f4aa660c54d062dedb268be84e821ea24e39f48c7a496af8dd02cc9eedb93847'
    $Unrelated = [ordered]@{}
    foreach ($Name in @('firecrawl-api-1','firecrawl-redis-1','firecrawl-rabbitmq-1','firecrawl-nuq-postgres-1')) {
        $Id = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Id -cnotmatch '^[a-f0-9]{64}$') {
            throw "Unrelated service unavailable: $Name"
        }
        $Unrelated[$Name] = [string]$Id
    }
    $CurrentImageId = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$CurrentImageId -cnotin @($OldId,$TargetId)) {
        throw 'Unexpected SearXNG runtime image; refusing blind deployment.'
    }
    $CandidateId = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$CandidateId -cne $TargetId) {
        throw 'Reviewed SearXNG image is not present at its pinned digest.'
    }
    if ([string]$CurrentImageId -cne $TargetId) {
        # Exact local image SHA is checked before and after fail-closed Scout scan.
        & docker scout cves --only-severity critical,high --only-fixed --exit-code ("local://" + $TargetId) | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Scout rejected SearXNG candidate: running container untouched.' }
        $AfterScan = (& docker image inspect --format '{{.Id}}' $TargetImage)
        if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne $TargetId) {
            throw 'SearXNG image identity changed after scan.'
        }
        & docker @ComposeArguments up -d --no-deps --no-build --force-recreate $Service
        if ($LASTEXITCODE -ne 0) { throw 'Scoped SearXNG recreation failed.' }
    }
    $NewId = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$NewId -cne $TargetId) {
        throw 'Scoped SearXNG deployment did not use expected image.'
    }
    $Ready = $false
    for ($Attempt = 0; $Attempt -lt 40; $Attempt++) {
        # Probe via the existing search proxy's Docker network, not a host port.
        & docker exec firecrawl-searxng-proxy-1 node -e 'fetch("http://searxng:8080/").then(r=>process.exit(r.status === 200 ? 0 : 1)).catch(()=>process.exit(1))' 2>$null | Out-Null
        $Ready = ($LASTEXITCODE -eq 0)
        if ($Ready) { break }
        Start-Sleep -Seconds 3
    }
    if (-not $Ready) { throw 'Updated SearXNG did not respond through the internal search proxy.' }
    foreach ($Name in $Unrelated.Keys) {
        $AfterId = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$AfterId -cne [string]$Unrelated[$Name]) {
            throw "Unexpected unrelated service recreation: $Name"
        }
    }
    "SEARXNG_REFRESH_OK image=$TargetId source=2026.10.30-ffa88e3ba internal_http=200 unrelated_containers_unchanged=true"
}
