# Isolated patch for the final two Firecrawl Node 26.10 runtime containers.
# The router is refreshed first, then the LLM proxy with DPAPI Gemini-key restore.
function Invoke-FirecrawlNodeRuntimeRefresh {
    param(
        [Parameter(Mandatory)][ValidateSet('playwright-router','llm-proxy')][string]$Service,
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Container = "firecrawl-$Service-1"
    $Image = 'firecrawl-node-runtime:local'
    $Unrelated = [ordered]@{}
    foreach ($Name in @('firecrawl-api-1','firecrawl-nuq-postgres-1',
        'firecrawl-redis-1','firecrawl-rabbitmq-1','firecrawl-searxng-1',
        'firecrawl-searxng-proxy-1','firecrawl-gateway-1',
        'firecrawl-playwright-service-1','firecrawl-playwright-public-proxy-service-1',
        'firecrawl-browser-session-service-1','firecrawl-mcp-1')) {
        $Id = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Id -cnotmatch '^[0-9a-f]{64}$') {
            throw "Unrelated Firecrawl container unavailable: $Name"
        }
        $Unrelated[$Name] = [string]$Id
    }
    $Old = Get-FirecrawlRunningImage -Container $Container
    $ExpectedVersion = Get-FirecrawlPinnedNodeVersion -Dockerfile (Join-Path $ProjectRoot 'security\Dockerfile.node-runtime')

    # Preserve the optional Gemini runtime key BEFORE recreating llm-proxy's
    # tmpfs. Import-GeminiKey is the existing DPAPI-backed function in fc.ps1.
    $GeminiConfigured = $false
    if ($Service -eq 'llm-proxy') {
        $KeyPath = Join-Path $env:LOCALAPPDATA 'FirecrawlLocal\secrets\gemini_api_key.dpapi'
        $GeminiConfigured = Test-Path -LiteralPath $KeyPath -PathType Leaf
        & docker exec $Container sh -c 'test -s /run/firecrawl-secrets/gemini_api_key' 2>$null | Out-Null
        $KeyWasPresent = ($LASTEXITCODE -eq 0)
        if ($KeyWasPresent -and -not $GeminiConfigured) {
            throw 'Gemini runtime key exists, but DPAPI restore source is missing; not restarting LLM proxy.'
        }
        if (-not $GeminiConfigured -and (Test-LegacyGeminiKey)) {
            throw 'Legacy Gemini secret is not DPAPI-migrated; not restarting LLM proxy.'
        }
    }
    $Rollback = Save-FirecrawlRollbackImage -Container $Container -BaselineId $Old -Repository 'firecrawl-node-runtime' -AllowMissing
    if ($Service -eq 'playwright-router') {
        & docker @ComposeArguments --profile public-proxy build playwright-router
        if ($LASTEXITCODE -ne 0) { throw 'Pinned Node image build failed; old router untouched.' }
    }
    $Candidate = (& docker image inspect --format '{{.Id}}' $Image)
    if ($LASTEXITCODE -ne 0 -or [string]$Candidate -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Pinned Node helper image missing; run router refresh first.'
    }
    & docker scout cves --only-severity critical,high --only-fixed --exit-code --vex-location (Join-Path $PSScriptRoot 'vex/firecrawl-mcp.openvex.json') --vex-author 'firecrawl-local-mcp maintainers' ("local://" + $Candidate) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Scout rejected the $Service image; old container untouched." }
    $AfterScan = (& docker image inspect --format '{{.Id}}' $Image)
    if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne [string]$Candidate) {
        throw 'Reviewed Node helper image changed during scan.'
    }
    Assert-FirecrawlRollbackImage -Container $Container -BaselineId $Old -RollbackTag $Rollback -RollbackId $Old
    if ($Service -eq 'playwright-router') {
        & docker @ComposeArguments --profile public-proxy up -d --no-deps --no-build --force-recreate $Service
    } else {
        & docker @ComposeArguments up -d --no-deps --no-build --force-recreate $Service
    }
    if ($LASTEXITCODE -ne 0) { throw "Scoped $Service container recreation failed." }

    $ActualImage = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$ActualImage -cne [string]$Candidate) {
        throw "New $Service does not use the reviewed Node runtime image."
    }
    $ActualVersion = (& docker exec $Container node --version)
    if ($LASTEXITCODE -ne 0 -or ([string]$ActualVersion).Trim() -cne $ExpectedVersion) {
        throw "New $Service runtime version is not Node $ExpectedVersion."
    }
    if ($Service -eq 'llm-proxy' -and $GeminiConfigured) {
        Import-GeminiKey
        & docker exec $Container sh -c 'test -s /run/firecrawl-secrets/gemini_api_key' | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Gemini DPAPI key not restored after LLM proxy recreation.' }
    }

    # Router has a real GET /health. LLM is a pure transparent proxy:
    # a TCP handshake proves Node is listening without sending prompts or keys.
    $Ready = $false
    for ($Attempt = 0; $Attempt -lt 30; $Attempt++) {
        if ($Service -eq 'playwright-router') {
            & docker exec $Container node -e 'fetch("http://127.0.0.1:3000/health").then(r=>process.exit(r.status===200?0:1)).catch(()=>process.exit(1))' 2>$null | Out-Null
        } else {
            & docker exec $Container node -e 'const n=require("node:net");const s=n.connect(11435,"127.0.0.1");s.setTimeout(2000);s.on("connect",()=>{s.destroy();process.exit(0)});s.on("error",()=>process.exit(1));s.on("timeout",()=>process.exit(1))' 2>$null | Out-Null
        }
        $Ready = ($LASTEXITCODE -eq 0)
        if ($Ready) { break }
        Start-Sleep -Seconds 2
    }
    if (-not $Ready) { throw "Scoped $Service did not pass its local readiness probe." }
    foreach ($Name in $Unrelated.Keys) {
        $After = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$After -cne [string]$Unrelated[$Name]) {
            throw "Unrelated container changed unexpectedly: $Name"
        }
    }
    "NODE_SUPPORT_REFRESH_OK service=$Service node=$ActualVersion image=$Candidate rollback=$Rollback gemini_restored=$GeminiConfigured unrelated_containers_unchanged=true"
}
