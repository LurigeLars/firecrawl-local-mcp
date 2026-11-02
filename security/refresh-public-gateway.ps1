# Secure one-service promotion for the public Firecrawl gateway.
# Secrets stay in Docker's env_file and Cloudflare Access; never print their values.
function Invoke-FirecrawlPublicGatewayRefresh {
    param(
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Container = 'firecrawl-gateway-1'
    $TargetImage = 'firecrawl-gateway:local'
    $OldId = 'sha256:0b36e8c136b94cd4fcf02188228e76c31ad5872eef3fec8cbd2eee500cfd9e80'
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
    $CurrentImageId = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$CurrentImageId -cne $OldId) {
        throw 'Unexpected existing gateway image; refuse implicit upgrade.'
    }
    $Source = Get-Content -LiteralPath (Join-Path $ProjectRoot 'public\Dockerfile.gateway') -Raw
    if ($Source -notmatch 'node:26[.]13[.]0-alpine@sha256:c6ad9e9c1d80f6eb607c26b4b0a132ff7a2344fad1c78ea3baa2a7d4c8de6d94' -or
        $Source -notmatch 'rm -rf /usr/local/lib/node_modules/npm') {
        throw 'Reviewed pinned npm-free public gateway Dockerfile changed.'
    }
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
    & docker @ComposeArguments build gateway
    if ($LASTEXITCODE -ne 0) { throw 'Gateway image build failed; existing gateway remains untouched.' }
    $CandidateId = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$CandidateId -cnotmatch '^sha256:[a-f0-9]{64}$') {
        throw 'Cannot inspect candidate gateway image.'
    }
    & docker scout cves --only-severity critical,high --only-fixed --exit-code ("local://" + $CandidateId) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Scout rejected candidate gateway; old runtime untouched.' }
    $AfterScan = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne [string]$CandidateId) {
        throw 'Gateway candidate identity changed after scan.'
    }
    $StillOld = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$StillOld -cne $OldId) {
        throw 'Gateway changed during staging; deployment aborted.'
    }
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
    "PUBLIC_GATEWAY_REFRESH_OK image=$CandidateId cloudflare_access_selftest=pass unrelated_container_ids_unchanged=true"
}
