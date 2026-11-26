# Local, digest-reviewed promotion of the two isolated Camofox research
# sidecars. Called through fc.ps1 with its launcher lock and DPAPI hook.
function Get-FirecrawlCamofoxState {
    param([Parameter(Mandatory)][ValidateSet('camofox-research','camofox-research-proxy')][string]$Service)
    $Lines = @(& docker container inspect --format '{{json .}}' "firecrawl-$Service-1" 2>$null)
    if ($LASTEXITCODE -ne 0 -or $Lines.Count -ne 1) { throw "Cannot inspect $Service." }
    $State = $Lines[0] | ConvertFrom-Json -ErrorAction Stop
    if ($State.Config.Labels.'com.docker.compose.project' -cne 'firecrawl' -or
        $State.Config.Labels.'com.docker.compose.service' -cne $Service -or
        $State.State.Running -ne $true -or [string]$State.Id -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$State.Image -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw "Unexpected or stopped $Service container."
    }
    return $State
}

function Assert-FirecrawlCamofoxImage {
    param([Parameter(Mandatory)]$State, [Parameter(Mandatory)][string]$ImageId)
    $Descriptor = $State.ImageManifestDescriptor
    if ($null -eq $Descriptor) {
        if ([string]$State.Image -cne $ImageId) { throw 'Camofox image identity mismatch.' }
        return
    }
    $Platform = "$($Descriptor.platform.os)/$($Descriptor.platform.architecture)"
    if ($Descriptor.platform.variant) { $Platform += "/$($Descriptor.platform.variant)" }
    if ($Platform -cnotmatch '^linux/[a-z0-9]+(/[a-z0-9]+)?$' -or
        [string]$Descriptor.digest -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Camofox platform identity unavailable.'
    }
    $PlatformId = [string](& docker image inspect --platform $Platform --format '{{.Id}}' $ImageId)
    if ($LASTEXITCODE -ne 0 -or $PlatformId -cne [string]$Descriptor.digest) {
        throw 'Camofox platform manifest mismatch.'
    }
}

function Test-FirecrawlCamofoxReady {
    param([ValidateSet('camofox-research','camofox-research-proxy')][string]$Service, [switch]$RequireIdle)
    $Name = "firecrawl-$Service-1"
    $SecretCheck = 'test -s /run/camofox-research-secrets/access_key'
    if ($Service -ceq 'camofox-research-proxy') {
        $SecretCheck += ' && test -s /run/firecrawl-proxy-secrets/proxy_username && test -s /run/firecrawl-proxy-secrets/proxy_password'
    }
    & docker exec $Name sh -ec $SecretCheck 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) { return $false }
    $Probe = 'fetch("http://127.0.0.1:9377/health",{signal:AbortSignal.timeout(2000)}).then(async r=>{const h=await r.json();process.exit(r.ok&&h.ok===true&&(!process.argv.includes("idle")||(h.activeTabs===0&&h.activeSessions===0))?0:1)}).catch(()=>process.exit(1))'
    $ProbeArgs = @('exec',$Name,'node','-e',$Probe)
    if ($RequireIdle) { $ProbeArgs += 'idle' }
    & docker @ProbeArgs 2>$null | Out-Null
    return ($LASTEXITCODE -eq 0)
}

function Wait-FirecrawlCamofoxReady {
    param([string]$Service, [string]$ImageId)
    for ($Attempt=0; $Attempt -lt 30; $Attempt++) {
        if (Test-FirecrawlCamofoxReady -Service $Service) {
            Assert-FirecrawlCamofoxImage -State (Get-FirecrawlCamofoxState -Service $Service) -ImageId $ImageId
            return
        }
        Start-Sleep -Seconds 2
    }
    throw "$Service did not become ready."
}

function Invoke-FirecrawlCamofoxRefresh {
    param(
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ReviewedDigest
    )
    if ($ReviewedDigest -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Supply the exact independently reviewed Camofox image digest.' }
    $Services = @('camofox-research','camofox-research-proxy')
    $Tag = 'influencerresearch-camofox:latest'
    $Candidate = [string](& docker image inspect --format '{{.Id}}' $Tag)
    if ($LASTEXITCODE -ne 0 -or $Candidate -cne $ReviewedDigest) { throw 'Reviewed Camofox image is not the current local tag.' }
    $Before = @{}
    foreach ($Service in $Services) {
        $Before[$Service] = Get-FirecrawlCamofoxState -Service $Service
        if (-not (Test-FirecrawlCamofoxReady -Service $Service -RequireIdle)) {
            throw "$Service is busy or unhealthy; no promotion attempted."
        }
    }
    $Old = [string]$Before[$Services[0]].Image
    if ($Before[$Services[1]].Image -cne $Old) { throw 'Research sidecars use different baseline images; no promotion attempted.' }
    foreach ($Service in $Services) { Assert-FirecrawlCamofoxImage -State $Before[$Service] -ImageId $Old }
    if ($Old -ceq $Candidate) { Write-Output 'CAMOFOX_RESEARCH_ALREADY_CURRENT'; return }
    # Keep an exact rollback image without mutating the shared IR image tag.
    $Rollback = Save-FirecrawlRollbackImage -Container 'firecrawl-camofox-research-1' -BaselineId $Old -Repository 'influencerresearch-camofox'
    $UnrelatedBefore = @(& docker ps -a --filter 'label=com.docker.compose.project=firecrawl' --format '{{.Names}}|{{.ID}}' --no-trunc)
    if ($LASTEXITCODE -ne 0 -or $UnrelatedBefore.Count -lt 3) { throw 'Cannot snapshot Firecrawl container identities.' }
    $UnrelatedBefore = @($UnrelatedBefore | Where-Object { $_ -notmatch '^firecrawl-camofox-research(-proxy)?-1\|' } | Sort-Object)
    foreach ($Service in $Services) {
        $Current = Get-FirecrawlCamofoxState -Service $Service
        if ($Current.Id -cne $Before[$Service].Id -or -not (Test-FirecrawlCamofoxReady -Service $Service -RequireIdle)) {
            throw 'Camofox changed or became busy during preflight.'
        }
    }
    Assert-FirecrawlRollbackImage -Container 'firecrawl-camofox-research-1' -BaselineId $Old -RollbackTag $Rollback -RollbackId $Old
    if ([string](& docker image inspect --format '{{.Id}}' $Tag) -cne $Candidate -or $LASTEXITCODE -ne 0) {
        throw 'Shared image tag changed during preflight.'
    }

    $Up = @('up','-d','--no-deps','--no-build','--pull','never','--force-recreate') + $Services
    try {
        # The existing DPAPI-backed post_start hook restores proxy credentials;
        # the named access-key volume and the secrets-init service are untouched.
        & docker @ComposeArguments @Up
        if ($LASTEXITCODE -ne 0) { throw 'Scoped Camofox recreation failed.' }
        foreach ($Service in $Services) { Wait-FirecrawlCamofoxReady -Service $Service -ImageId $Candidate }
        $UnrelatedAfter = @(& docker ps -a --filter 'label=com.docker.compose.project=firecrawl' --format '{{.Names}}|{{.ID}}' --no-trunc)
        if ($LASTEXITCODE -ne 0) { throw 'Cannot verify unrelated Firecrawl containers.' }
        $UnrelatedAfter = @($UnrelatedAfter | Where-Object { $_ -notmatch '^firecrawl-camofox-research(-proxy)?-1\|' } | Sort-Object)
        if (($UnrelatedAfter -join [Environment]::NewLine) -cne ($UnrelatedBefore -join [Environment]::NewLine)) {
            throw 'Unrelated Firecrawl container identities changed.'
        }
    } catch {
        # Do not retag IR's current image. An explicit local override retains the
        # baseline for only the research pair; a later successful refresh
        # restores the normal Compose image reference.
        $Override = Join-Path $env:LOCALAPPDATA ("FirecrawlLocal\camofox-research-rollback-" + $Old.Substring(7,12) + '.yaml')
        $OverrideText = @('services:', '  camofox-research:', "    image: $Rollback", '  camofox-research-proxy:', "    image: $Rollback", '') -join [Environment]::NewLine
        [IO.File]::WriteAllText($Override, $OverrideText)
        $RollbackArgs = $ComposeArguments + @('-f',$Override)
        try {
            & docker @RollbackArgs @Up
            if ($LASTEXITCODE -ne 0) { throw 'Rollback recreation failed.' }
            foreach ($Service in $Services) { Wait-FirecrawlCamofoxReady -Service $Service -ImageId $Old }
        } catch { throw 'Camofox promotion and rollback failed; inspect the two research sidecars locally.' }
        throw 'Camofox promotion failed; baseline restored with a local rollback override. Shared IR tag unchanged.'
    }
    Write-Output "CAMOFOX_RESEARCH_REFRESH_OK image=$Candidate rollback=$Rollback unrelated_containers_unchanged=true"
}
