# Explicit operator-only transition. Sourced by fc.ps1; no top-level mutation.
# The existing launcher supplies reviewed Compose files, DPAPI runtime hooks,
# sanitized Docker command helper and rollback secret restoration functions.
function Get-TransitionPath([string]$Name) {
    return Join-Path (Join-Path $env:LOCALAPPDATA 'FirecrawlLocal') $Name
}

function Get-TransitionImageId([string]$Image) {
    $id = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$Image)
    if ($id -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Invalid transition image identity.' }
    return $id
}

function Get-AcceptedCandidateImage([string]$Image,[string]$Digest) {
    if ($Digest -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Explicit accepted image digest required.' }
    $raw = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}|{{json .RepoDigests}}',$Image)
    $parts = $raw.Split('|',2)
    if ($parts.Count -ne 2 -or $parts[0] -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Candidate image identity invalid.' }
    $digests = @($parts[1] | ConvertFrom-Json)
    $repo = $Image -replace ':latest$',''
    if ($parts[0] -cne $Digest -and $digests -cnotcontains "$repo@$Digest") { throw 'Candidate differs from accepted image.' }
    return $parts[0]
}

function Assert-TransitionSource([string]$ProjectRoot,[string]$Revision) {
    if ($Revision -cnotmatch '^[0-9a-f]{40}$') { throw 'Explicit accepted source revision required.' }
    $dirty = @(& git -C $ProjectRoot status --porcelain)
    if ($LASTEXITCODE -ne 0 -or $dirty.Count -ne 0) { throw 'Use a clean reviewed checkout.' }
    $branch = & git -C $ProjectRoot branch --show-current
    if ($LASTEXITCODE -ne 0 -or $branch -cne 'main') { throw 'Use reviewed main.' }
    & git -C $ProjectRoot merge-base --is-ancestor $Revision HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Accepted revision is not an ancestor of main.' }
    # All files copied into either tested release image must match the
    # accepted revision. Other scripts/docs/config may have advanced.
    & git -C $ProjectRoot diff --quiet $Revision HEAD -- research/Dockerfile.rust-table-ci ':(glob)research/*.rs' ':(glob)research/*.patch' research/job-store public/Dockerfile.mcp public/package.json public/package-lock.json public/mcp-api-env.mjs public/mcp-api-bootstrap.mjs
    if ($LASTEXITCODE -ne 0) { throw 'Release runtime inputs differ from accepted revision; re-test required.' }
}

function Set-TransitionTag([string]$Id,[string]$Tag) {
    $null = Invoke-McpDocker -Arguments @('image','tag',$Id,$Tag)
    if ((Get-TransitionImageId $Tag) -cne $Id) { throw 'Transition image tag verification failed.' }
}

function Get-TransitionService([string]$Service) {
    if ($Service -cnotin @('api','mcp','gateway','redis','rabbitmq','nuq-postgres','rust-init','pdf-ocr')) { throw 'Unreviewed transition service.' }
    $name = "firecrawl-$Service-1"
    $found = @(Invoke-McpDocker -Arguments @('container','ls','--all','--filter',"name=^/$name$",'--format','{{.Names}}'))
    if ($found.Count -eq 0 -or ($found.Count -eq 1 -and -not $found[0])) { return $null }
    if ($found.Count -ne 1 -or $found[0] -cne $name) { throw 'Ambiguous transition service.' }
    $format = '{{.Id}}|{{.Image}}|{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}|{{.State.Running}}'
    $parts = (Invoke-McpDocker -Arguments @('inspect','--format',$format,$name)).Split('|')
    if ($parts.Count -ne 5 -or $parts[0] -cnotmatch '^[0-9a-f]{64}$' -or $parts[1] -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        $parts[2] -cne 'firecrawl' -or $parts[3] -cne $Service -or $parts[4] -cnotin @('true','false')) { throw 'Unreviewed production service identity.' }
    return @{id=$parts[0];image=$parts[1];running=($parts[4] -ceq 'true')}
}

function Stop-TransitionService([string]$Service) {
    $state = Get-TransitionService $Service
    if ($null -ne $state -and $state.running) {
        $null = Invoke-McpDocker -Arguments @('stop','--time','90',$state.id) -TimeoutSeconds 105
        if ((Get-TransitionService $Service).running) { throw 'Transition stop failed.' }
    }
}

function Start-OriginalTransitionService([string]$Service,[string]$Id,[string]$Image) {
    $state = Get-TransitionService $Service
    if ($null -eq $state -or $state.id -cne $Id -or $state.image -cne $Image) { throw 'Original rollback container missing or replaced.' }
    if (-not $state.running) { $null = Invoke-McpDocker -Arguments @('start',$Id) }
    if (-not (Get-TransitionService $Service).running) { throw 'Original rollback container did not start.' }
}

function Write-TransitionMarker([string]$Value,[switch]$New) {
    if ($Value -cnotin @('rust-pending-v1','rust-active-v1')) { throw 'Invalid transition state.' }
    $path = Get-TransitionPath 'rust-transition.lock'
    $tmp = "$path.$([guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($tmp,$Value,[Text.UTF8Encoding]::new($false))
        [IO.File]::Move($tmp,$path,(-not $New))
    } finally {
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
    }
}

function Get-RustRollbackJournal {
    $marker = Get-TransitionPath 'rust-transition.lock'
    $path = Get-TransitionPath 'rust-transition.json'
    if (-not (Test-Path -LiteralPath $marker -PathType Leaf) -or -not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'Reviewed transition journal unavailable.' }
    if ((Get-Content -LiteralPath $marker -Raw).Trim() -cnotin @('rust-pending-v1','rust-active-v1')) { throw 'Invalid transition marker.' }
    $j = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json -AsHashtable
    if ($j.schema -ne 1 -or $j.revision -cnotmatch '^[0-9a-f]{40}$') { throw 'Invalid rollback journal.' }
    foreach ($service in @('api','mcp','redis','rabbitmq','nuq-postgres','gateway')) {
        if ($j.legacy[$service].image -cnotmatch '^sha256:[0-9a-f]{64}$' -or $j.legacy[$service].id -cnotmatch '^[0-9a-f]{64}$') { throw 'Invalid legacy rollback identity.' }
    }
    foreach ($service in @('api','mcp','pdf-ocr')) {
        if ($j.candidate[$service] -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Invalid candidate rollback identity.' }
    }
    return $j
}

function Get-RustStoppedQueueCount {
    # Only the stopped production Rust database, no network and no payload.
    # The pinned Node image is already used by rust-init. Read-only SQLite
    # connection and volume; do not acquire the normal worker's write lock.
    $code = "const {DatabaseSync}=require('node:sqlite'),fs=require('node:fs'); if(!fs.existsSync('/state/jobs.sqlite')){if(fs.existsSync('/state/jobs.sqlite-wal')||fs.existsSync('/state/jobs.sqlite-shm'))process.exit(1); console.log('MISSING');process.exit(0);} const db=new DatabaseSync('/state/jobs.sqlite',{readOnly:true}); const n=db.prepare(""SELECT count(*) AS n FROM jobs WHERE status='scraping'"").get().n; if(!Number.isSafeInteger(n)||n<0)process.exit(1); console.log(n); db.close();"
    $count = Invoke-McpDocker -Arguments @('run','--rm','--pull','never','--network','none','--read-only','--cap-drop','ALL','--security-opt','no-new-privileges:true','--user','65532:65532','--mount','type=volume,source=firecrawl_rust-jobs,target=/state,readonly','node:26.13.1-alpine@sha256:6bd9ab43c0ec0190ca7b2d193906cb8b7edf6e7f4afa43ddeccd6452710bffb5','node','--no-warnings','-e',$code) -TimeoutSeconds 30
    if ($count -ceq 'MISSING' -and (Get-Content -LiteralPath (Get-TransitionPath 'rust-transition.lock') -Raw).Trim() -ceq 'rust-pending-v1') { return [int64]0 }
    if ($count -cnotmatch '^[0-9]{1,18}$') { throw 'Invalid Rust queue count.' }
    return [int64]$count
}

function Invoke-TransitionMcpProbe([string]$ProjectRoot,[string]$Backend,[string]$Mode) {
    $source = Get-Content -LiteralPath (Join-Path $ProjectRoot 'research/verify-mcp-load.mjs') -Raw
    $params = @('exec','-i','firecrawl-mcp-1','node','--input-type=module','-',$Backend)
    if ($Mode) { $params += $Mode }
    $lines = @($source | & docker @params 2>$null)
    if ($LASTEXITCODE -ne 0) { throw 'Transition MCP probe failed.' }
    try { $receipt = ($lines -join [Environment]::NewLine) | ConvertFrom-Json }
    catch { throw 'Invalid transition MCP receipt.' }
    if ($receipt.verdict -cne 'PASS' -or $receipt.backend -cne $Backend) { throw 'Transition MCP acceptance failed.' }
}

function Wait-LegacyTransitionHealth {
    for ($i=0;$i -lt 45;$i++) {
        try {
            $r = Invoke-WebRequest -Uri 'http://127.0.0.1:3002/v0/health/liveness' -TimeoutSec 2 -MaximumRedirection 0 -SkipHttpErrorCheck
            if ($r.StatusCode -eq 200) { return }
        } catch {}
        Start-Sleep -Seconds 2
    }
    throw 'Legacy API did not recover.'
}

function Restore-LegacyTransition([string[]]$LegacyCompose,[string]$ProjectRoot,$Journal) {
    $legacy=$Journal.legacy
    $candidate=$Journal.candidate
    $mcpBefore=Get-TransitionService mcp
    $gatewayBefore=Get-TransitionService gateway
    $active=(Get-Content -LiteralPath (Get-TransitionPath 'rust-transition.lock') -Raw).Trim() -ceq 'rust-active-v1'
    if ($null -eq $gatewayBefore) { throw 'Gateway identity unavailable; rollback refused.' }
    if (-not $active -and ($gatewayBefore.id -cne $legacy.gateway.id -or $gatewayBefore.image -cne $legacy.gateway.image)) { throw 'Original gateway changed during promotion.' }
    # Gateway is shared, and reviewed maintenance may have replaced it
    # after promotion. Retain the exact instance observed at rollback.
    Stop-TransitionService gateway
    Stop-TransitionService mcp
    $api=Get-TransitionService api
    if ($null -ne $api -and $api.image -cne $legacy.api.image -and $api.image -cne $candidate.api) { throw 'Unknown API image; rollback refused.' }
    if ($null -ne $api -and $api.image -ceq $candidate.api) {
        Stop-TransitionService api
        $idle = $false
        try { $idle = (Get-RustStoppedQueueCount) -eq 0 } catch { $idle = $false }
        if (-not $idle) {
            # Preserve durable jobs and let the same engine finish. Never
            # remove its volume or pretend the other engine can read its jobs.
            if ($active) {
                $null = Invoke-McpDocker -Arguments @('start',$api.id)
                $mcp=Get-TransitionService mcp
                if ($null -eq $mcpBefore -or $null -eq $mcp -or $mcp.id -cne $mcpBefore.id -or $mcp.image -cne $mcpBefore.image) { throw 'Rust MCP identity unavailable.' }
                $null = Invoke-McpDocker -Arguments @('start',$mcp.id)
                Start-OriginalTransitionService -Service gateway -Id $gatewayBefore.id -Image $gatewayBefore.image
            }
            throw 'Rust durable jobs are not verified idle; rollback refused.'
        }
    }
    Stop-TransitionService pdf-ocr
    foreach ($service in @('redis','rabbitmq','nuq-postgres')) {
        Start-OriginalTransitionService -Service $service -Id $legacy[$service].id -Image $legacy[$service].image
    }
    foreach ($service in @('api','mcp')) {
        $tag="firecrawl-$($service):rust-transition-legacy"
        if ((Get-TransitionImageId $tag) -cne $legacy[$service].image) { throw 'Preserved rollback image differs.' }
        Set-TransitionTag -Id $legacy[$service].image -Tag "firecrawl-$($service):latest"
    }
    # Use the original legacy definitions, never the Rust overlay. Named
    # stateful containers are restarted, not recreated or volume-migrated.
    if ($null -ne $api -and $api.image -ceq $legacy.api.image) {
        Start-OriginalTransitionService -Service api -Id $api.id -Image $legacy.api.image
    } else {
        $null=Invoke-McpDocker -Arguments ($LegacyCompose + @('up','-d','--no-deps','--no-build','--pull','never','--force-recreate','api')) -TimeoutSeconds 180
    }
    # Caller uses the original fc.ps1 Compose arguments and DPAPI functions.
    Import-ServiceRuntimeSecrets
    for ($i=0;$i -lt 30;$i++) {
        if (Test-PostgresRuntimePassword) { break }
        Start-Sleep -Seconds 2
    }
    if ($i -eq 30) { throw 'Legacy PostgreSQL runtime authentication failed.' }
    Wait-LegacyTransitionHealth
    $null=Invoke-McpDocker -Arguments ($LegacyCompose + @('up','-d','--no-deps','--no-build','--pull','never','--force-recreate','mcp')) -TimeoutSeconds 180
    foreach ($service in @('api','mcp')) {
        $state=Get-TransitionService $service
        if ($null -eq $state -or -not $state.running -or $state.image -cne $legacy[$service].image) { throw 'Legacy image recovery not verified.' }
    }
    Invoke-TransitionMcpProbe -ProjectRoot $ProjectRoot -Backend legacy -Mode ''
    Start-OriginalTransitionService -Service gateway -Id $gatewayBefore.id -Image $gatewayBefore.image
    # Clear marker only after successful original engine/MCP verification.
    $journalPath=Get-TransitionPath 'rust-transition.json'
    $archive=Get-TransitionPath ('rust-transition-last-'+[guid]::NewGuid().ToString('N')+'.json')
    [IO.File]::Copy($journalPath,$archive,$false)
    Remove-Item -LiteralPath (Get-TransitionPath 'rust-transition.lock') -Force -ErrorAction Stop
    Remove-Item -LiteralPath $journalPath -Force -ErrorAction Stop
    'RUST_ROLLBACK_LEGACY_VERIFIED'
}

function Invoke-RustRollback([string[]]$LegacyCompose,[string]$ProjectRoot) {
    $ErrorActionPreference = 'Stop'
    $journal=Get-RustRollbackJournal
    Restore-LegacyTransition -LegacyCompose $LegacyCompose -ProjectRoot $ProjectRoot -Journal $journal
}

function Invoke-RustPromotion {
    param([string[]]$LegacyCompose,[string]$ProjectRoot,[string]$Revision,[string]$ApiDigest,[string]$DocumentDigest,[string]$McpDigest)
    $ErrorActionPreference='Stop'
    $marker=Get-TransitionPath 'rust-transition.lock'
    $journalPath=Get-TransitionPath 'rust-transition.json'
    if ((Test-Path -LiteralPath $marker) -or (Test-Path -LiteralPath $journalPath)) { throw 'Existing transition state; do not start a second transition.' }
    Assert-TransitionSource -ProjectRoot $ProjectRoot -Revision $Revision
    $rustCompose=$LegacyCompose + @('-f',(Join-Path $ProjectRoot 'research/compose.rust-production-plan.yaml'))
    $null=Invoke-McpDocker -Arguments ($rustCompose + @('config','--quiet'))
    . (Join-Path $ProjectRoot 'research/rust-transition-preflight.ps1') -Library
    $originalImages=Get-LegacyInventoryReceipt
    $queue=Get-LegacyQueueReceipt
    if (@($queue.Values | Where-Object {$_ -ne 0}).Count) { throw 'Legacy jobs are not idle.' }
    $legacy=[ordered]@{}
    foreach ($service in @('api','mcp','redis','rabbitmq','nuq-postgres','gateway')) {
        $state=Get-TransitionService $service
        if ($null -eq $state -or -not $state.running -or ($service -ne 'gateway' -and $state.image -cne $originalImages[$service])) { throw 'Legacy inventory changed.' }
        $legacy[$service]=$state
    }
    $candidate=[ordered]@{
        api=(Get-AcceptedCandidateImage 'firecrawl-rust-canary-rust-api:latest' $ApiDigest)
        'pdf-ocr'=(Get-AcceptedCandidateImage 'firecrawl-rust-canary-pdf-ocr:latest' $DocumentDigest)
        mcp=(Get-AcceptedCandidateImage 'firecrawl-rust-canary-mcp:latest' $McpDigest)
    }
    # Scan exact accepted images before touching production. Existing MCP VEX
    # scope is restricted to reviewed package versions and author.
    foreach ($service in @('api','pdf-ocr','mcp')) {
        $scan=@('scout','cves','--only-severity','critical,high','--only-fixed','--exit-code')
        if ($service -eq 'mcp') { $scan+=@('--vex-location',(Join-Path $ProjectRoot 'security/vex/firecrawl-mcp.openvex.json'),'--vex-author','firecrawl-local-mcp maintainers') }
        $null=Invoke-McpDocker -Arguments ($scan+@("local://$($candidate[$service])")) -TimeoutSeconds 300
    }
    Set-TransitionTag -Id $candidate.api -Tag 'firecrawl-rust-api:local'
    Set-TransitionTag -Id $candidate['pdf-ocr'] -Tag 'firecrawl-rust-document:local'
    foreach ($service in @('api','mcp')) { Set-TransitionTag -Id $legacy[$service].image -Tag "firecrawl-$($service):rust-transition-legacy" }
    $j=[ordered]@{schema=1;revision=$Revision;legacy=$legacy;candidate=$candidate}
    $null=New-Item -ItemType Directory -Path (Split-Path -Parent $journalPath) -Force
    $bytes=[Text.Encoding]::UTF8.GetBytes(($j | ConvertTo-Json -Depth 8))
    $stream=[IO.File]::Open($journalPath,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
    try { $stream.Write($bytes,0,$bytes.Length);$stream.Flush($true) } finally { $stream.Dispose() }
    Write-TransitionMarker -Value 'rust-pending-v1' -New
    try {
        Stop-TransitionService gateway
        Stop-TransitionService mcp
        Stop-TransitionService api
        # The API is stopped: no new direct or MCP requests can enqueue.
        $queue=Get-LegacyQueueReceipt
        if (@($queue.Values | Where-Object {$_ -ne 0}).Count) { throw 'Legacy jobs remain after ingress stopped.' }
        foreach ($service in @('rabbitmq','nuq-postgres','redis')) { Stop-TransitionService $service }
        $null=Invoke-McpDocker -Arguments ($rustCompose+@('up','-d','--no-deps','--no-build','--pull','never','rust-init')) -TimeoutSeconds 120
        $exit=Invoke-McpDocker -Arguments @('wait','firecrawl-rust-init-1') -TimeoutSeconds 60
        if ($exit -cne '0') { throw 'Rust credential/job initialization failed.' }
        $null=Invoke-McpDocker -Arguments ($rustCompose+@('up','-d','--no-deps','--no-build','--pull','never','--force-recreate','pdf-ocr','api')) -TimeoutSeconds 180
        foreach ($service in @('api','pdf-ocr')) {
            $state=Get-TransitionService $service
            if ($null -eq $state -or -not $state.running -or $state.image -cne $candidate[$service]) { throw 'Candidate runtime image differs.' }
        }
        # Token is captured by the dedicated local DPAPI script, never
        # passed as an argument, written to stdout or copied into the repo.
        & (Join-Path $ProjectRoot 'local-mcp/rust-client.ps1') -Mode Import | Out-Null
        $authReady=$false
        for ($i=0;$i -lt 15;$i++) {
            try {
                $unauth=Invoke-WebRequest -Uri 'http://127.0.0.1:3002/health' -TimeoutSec 3 -MaximumRedirection 0 -SkipHttpErrorCheck
                if ($unauth.StatusCode -eq 401) { $authReady=$true;break }
                if ($unauth.StatusCode -eq 200) { throw 'Unexpected unauthenticated health success.' }
            } catch {
                if ($_.Exception.Message -ceq 'Unexpected unauthenticated health success.') { throw }
            }
            Start-Sleep -Seconds 1
        }
        if (-not $authReady) { throw 'Rust loopback authentication check failed.' }
        & node (Join-Path $ProjectRoot 'local-mcp/verify-rust-client.mjs')
        if ($LASTEXITCODE -ne 0) { throw 'Actual local Rust client test failed.' }
        Set-TransitionTag -Id $candidate.mcp -Tag 'firecrawl-mcp:latest'
        $null=Invoke-McpDocker -Arguments ($rustCompose+@('up','-d','--no-deps','--no-build','--pull','never','--force-recreate','mcp')) -TimeoutSeconds 180
        $state=Get-TransitionService mcp
        if ($null -eq $state -or -not $state.running -or $state.image -cne $candidate.mcp) { throw 'Rust MCP image differs.' }
        Invoke-TransitionMcpProbe -ProjectRoot $ProjectRoot -Backend candidate -Mode '--search'
        $modelSource=Get-Content -LiteralPath (Join-Path $ProjectRoot 'research/verify-live-model.mjs') -Raw
        $modelResult=@($modelSource | & docker exec -i firecrawl-mcp-1 node --input-type=module - 2>$null)
        if ($LASTEXITCODE -ne 0 -or ($modelResult -join '') -cnotmatch '^RUST_LIVE_MODEL_PASS provider=(gemini|ollama)$') { throw 'Configured model route not verified.' }
        $modelResult
        Write-TransitionMarker -Value 'rust-active-v1'
        Start-OriginalTransitionService -Service gateway -Id $legacy.gateway.id -Image $legacy.gateway.image
        'RUST_PROMOTION_LOCAL_VERIFIED'
    } catch {
        # All failures before gateway reopening use the same reviewed rollback.
        # Do not include native errors/response bodies in the returned receipt.
        try { Restore-LegacyTransition -LegacyCompose $LegacyCompose -ProjectRoot $ProjectRoot -Journal $j }
        catch { throw 'Rust promotion and automatic rollback failed; marker retained. Inspect local services before further changes.' }
        throw 'Rust promotion failed; verified Legacy restored.'
    }
}
