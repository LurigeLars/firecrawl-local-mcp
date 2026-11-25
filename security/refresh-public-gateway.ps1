# Recover only the documented orphaned gateway image from November 25.
# The deployment audit recorded its original image ID and source revision.
# A reconstructed fallback is NOT an exact original. Never commit/export the
# container: its configuration contains runtime authentication values.
function Get-FirecrawlGatewayRollback {
    param([string]$Container,[string]$BaselineId,[string]$ProjectRoot)
    $Present = [string](& docker image inspect --format '{{.Id}}' $BaselineId 2>$null)
    if ($LASTEXITCODE -eq 0 -and $Present -ceq $BaselineId) {
        $Tag = Save-FirecrawlRollbackImage -Container $Container -BaselineId $BaselineId -Repository 'firecrawl-gateway'
        return @{ Tag=$Tag; Id=$BaselineId; Reconstructed=$false }
    }

    $KnownMissing = 'sha256:7edfc616ff5f469baeae19f5bc0bdf511239d97c83f010761d31b02b7e43e9bc'
    $Revision = 'b7dbfba9860c1742022dfe309c34c80705a0c036'
    if ($Container -cne 'firecrawl-gateway-1' -or $BaselineId -cne $KnownMissing) {
        throw 'Baseline image unavailable and no reviewed reconstruction exists; deployment aborted.'
    }
    $Dirty = @(& git -C $ProjectRoot status --porcelain)
    if ($LASTEXITCODE -ne 0 -or $Dirty.Count -ne 0) { throw 'Recovery requires a clean reviewed checkout.' }
    & git -C $ProjectRoot merge-base --is-ancestor $Revision HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Recorded baseline revision is not an ancestor.' }
    $Recipe = @(& git -C $ProjectRoot show "$($Revision):public/Dockerfile.gateway")
    if ($LASTEXITCODE -ne 0 -or $Recipe.Count -eq 0) { throw 'Historical gateway recipe unavailable.' }
    if (($Recipe -join [Environment]::NewLine) -match '(?im)^\s*(COPY|ADD|ENV|ARG)\s') {
        throw 'Historical recipe is not a self-contained gateway base.'
    }
    $Stage = Join-Path ([IO.Path]::GetTempPath()) ('firecrawl-gateway-recovery-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Stage -ErrorAction Stop | Out-Null
    try {
        $Dockerfile = Join-Path $Stage 'Dockerfile'
        [IO.File]::WriteAllText($Dockerfile,(($Recipe -join [Environment]::NewLine) + [Environment]::NewLine),[Text.UTF8Encoding]::new($false))
        $Version = Get-FirecrawlPinnedNodeVersion -Dockerfile $Dockerfile
        $Tag = "firecrawl-gateway:reconstructed-$($Revision.Substring(0,12))"
        & docker build --tag $Tag --label "org.opencontainers.image.source=https://github.com/LurigeLars/firecrawl-local-mcp" --label "org.opencontainers.image.revision=$Revision" --label "io.firecrawl.recovery.original-image=$BaselineId" --label 'io.firecrawl.recovery.kind=reconstructed' $Stage | Out-Host
        if ($LASTEXITCODE -ne 0) { throw 'Reconstruction build failed; live gateway untouched.' }
        $Recovered = [string](& docker image inspect --format '{{.Id}}' $Tag)
        if ($LASTEXITCODE -ne 0 -or $Recovered -cnotmatch '^sha256:[a-f0-9]{64}$') {
            throw 'Reconstructed fallback image unavailable.'
        }
        & docker scout cves --only-severity critical,high --only-fixed --exit-code --vex-location (Join-Path $PSScriptRoot 'vex/firecrawl-mcp.openvex.json') --vex-author 'firecrawl-local-mcp maintainers' ("local://" + $Recovered) | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Scout rejected reconstructed fallback; live gateway untouched.' }
        # No secret mounts, network, host paths, or gateway environment are passed.
        $ActualVersion = [string](& docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges --entrypoint node $Recovered --version)
        if ($LASTEXITCODE -ne 0 -or $ActualVersion -cne $Version) {
            throw 'Reconstructed gateway Node smoke failed.'
        }
        Assert-FirecrawlRollbackImage -Container $Container -BaselineId $BaselineId -RollbackTag $Tag -RollbackId $Recovered
        Write-Host "GATEWAY_BASELINE_RECONSTRUCTED source=$Revision image=$Recovered original=$BaselineId exact_original=false"
        return @{ Tag=$Tag; Id=$Recovered; Reconstructed=$true }
    } finally {
        Remove-Item -LiteralPath $Stage -Recurse -Force
    }
}

# Secure one-service promotion for the public Firecrawl gateway.
# Secrets stay in Docker's env_file and Cloudflare Access; never print their values.
function Invoke-FirecrawlPublicGatewayRefresh {
    param(
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Container = 'firecrawl-gateway-1'
    $TargetImage = 'firecrawl-gateway:local'
    $Unrelated = [ordered]@{}
    foreach ($Name in @('firecrawl-api-1','firecrawl-redis-1','firecrawl-rabbitmq-1',
                        'firecrawl-nuq-postgres-1','firecrawl-searxng-1','firecrawl-searxng-proxy-1',
                        'firecrawl-mcp-1')) {
        $Id = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Id -cnotmatch '^[a-f0-9]{64}$') {
            throw "Unrelated service unavailable: $Name"
        }
        $Unrelated[$Name] = [string]$Id
    }
    $OldId = Get-FirecrawlRunningImage -Container $Container
    $null = Get-FirecrawlPinnedNodeVersion -Dockerfile (Join-Path $ProjectRoot 'public\Dockerfile.gateway')
    # Compare effective gateway environment against the live gateway *locally*.
    # Do not emit effective Compose config: it contains ignored OAuth/Access values.
    $EffectiveJson = (& docker @ComposeArguments config --format json | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve reviewed gateway Compose config.' }
    $Effective = $EffectiveJson | ConvertFrom-Json -Depth 80
    $GatewayCfg = $Effective.services.gateway
    if ($null -eq $GatewayCfg -or
        $null -eq $GatewayCfg.environment -or
        $null -eq $GatewayCfg.networks.edge) {
        throw 'Expected public gateway env and edge networking missing from Compose config.'
    }
    $ExistingJson = (& docker inspect --format '{{json .Config.Env}}' $Container | Out-String)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect live public gateway environment.' }
    $RunningEnvItems = ConvertFrom-Json -InputObject $ExistingJson -Depth 10
    $RunningEnv = @{}
    foreach ($Item in @($RunningEnvItems)) {
        $Parts = ([string]$Item).Split('=',2)
        if ($Parts.Count -eq 2) { $RunningEnv[[string]$Parts[0]] = [string]$Parts[1] }
    }
    $EnvProperties = @($GatewayCfg.environment.PSObject.Properties)
    if ($EnvProperties.Count -lt 5) { throw 'Expected gateway env configuration missing.' }
    foreach ($Property in $EnvProperties) {
        $Key = [string]$Property.Name
        # The sole authorized change is the addition of the metadata-only report.
        # Preserve all other tools, credential values, upstream and policy settings.
        if ($Key -ceq 'ALLOWED_TOOLS' -and $RunningEnv.ContainsKey($Key)) {
            $Before = @(([string]$RunningEnv[$Key]).Split(','))
            $After = @(([string]$Property.Value).Split(','))
            if ('tool_usage' -cnotin $Before -and
                $After.Count -eq ($Before.Count + 1) -and
                @($After | Where-Object { $_ -ceq 'tool_usage' }).Count -eq 1 -and
                ((@($After | Where-Object { $_ -cne 'tool_usage' }) -join ',') -ceq ($Before -join ','))) {
                continue
            }
        }
        if (-not $RunningEnv.ContainsKey($Key) -or
            [string]$RunningEnv[$Key] -cne [string]$Property.Value) {
            # Key names only. Neither secret values nor JSON contents are logged.
            throw "Gateway environment is not unchanged at key $Key. Refusing deployment."
        }
    }
    $ExistingBind = (& docker inspect --format '{{range .Mounts}}{{if eq .Destination "/app"}}{{.Source}}{{end}}{{end}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$ExistingBind -notlike '*firecrawl-local*public*gateway') {
        throw 'Existing gateway source bind does not match the reviewed location.'
    }
    $Protected = Get-FirecrawlGatewayRollback -Container $Container -BaselineId $OldId -ProjectRoot $ProjectRoot
    $Rollback = $Protected.Tag
    & docker @ComposeArguments build gateway
    if ($LASTEXITCODE -ne 0) { throw 'Gateway image build failed; existing gateway remains untouched.' }
    $CandidateId = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$CandidateId -cnotmatch '^sha256:[a-f0-9]{64}$') {
        throw 'Cannot inspect candidate gateway image.'
    }
    & docker scout cves --only-severity critical,high --only-fixed --exit-code --vex-location (Join-Path $PSScriptRoot 'vex/firecrawl-mcp.openvex.json') --vex-author 'firecrawl-local-mcp maintainers' ("local://" + $CandidateId) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Scout rejected candidate gateway; old runtime untouched.' }
    $AfterScan = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne [string]$CandidateId) {
        throw 'Gateway candidate identity changed after scan.'
    }
    Assert-FirecrawlRollbackImage -Container $Container -BaselineId $OldId -RollbackTag $Rollback -RollbackId $Protected.Id
    & docker @ComposeArguments up -d --no-deps --no-build --force-recreate gateway
    if ($LASTEXITCODE -ne 0) { throw 'Public gateway scoped deploy failed.' }
    $NewImage = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$NewImage -cne [string]$CandidateId) {
        throw 'Public gateway did not start reviewed candidate.'
    }
    $ActualBind = (& docker inspect --format '{{range .Mounts}}{{if eq .Destination "/app"}}{{.Source}}{{end}}{{end}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$ActualBind -cne [string]$ExistingBind) {
        throw 'Public gateway source mount changed during update.'
    }
    foreach ($Name in $Unrelated.Keys) {
        $AfterId = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$AfterId -cne [string]$Unrelated[$Name]) {
            throw "Unrelated container was recreated: $Name"
        }
    }
    # Authenticate is intentionally NOT exercised: Cloudflare Access must still
    # reject anonymous requests and expose its normal public discovery metadata.
    $SmokePassed = $false
    for ($Attempt = 0; $Attempt -lt 12; $Attempt++) {
        & node (Join-Path $ProjectRoot 'public\selftest.mjs') (Get-PublicUrl)
        if ($LASTEXITCODE -eq 0) { $SmokePassed = $true; break }
        Start-Sleep -Seconds 3
    }
    if (-not $SmokePassed) { throw 'Public gateway Access/OAuth self-test failed.' }
    "PUBLIC_GATEWAY_REFRESH_OK image=$CandidateId rollback=$Rollback reconstructed=$($Protected.Reconstructed) cloudflare_access_selftest=pass unrelated_container_ids_unchanged=true"
}
