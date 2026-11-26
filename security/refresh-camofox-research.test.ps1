# Synthetic Docker fixture: no host runtime, secrets or browser network access.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'refresh-baseline.ps1')
. (Join-Path $PSScriptRoot 'refresh-camofox-research.ps1')
$Old = 'sha256:' + ('a' * 64)
$Candidate = 'sha256:' + ('b' * 64)
$OldManifest = 'sha256:' + ('c' * 64)
$CandidateManifest = 'sha256:' + ('d' * 64)
$script:Running = $Old
$script:Busy = $false
$script:Tag = ''
$script:UpCount = 0
$script:FailNextUp = $false
$script:RollbackCount = 0
$script:Calls = @()
function docker {
    $global:LASTEXITCODE = 0
    $script:Calls += ($args -join ' ')
    if ($args[0] -eq 'container' -and $args[1] -eq 'inspect') {
        $service = $args[-1] -replace '^firecrawl-' -replace '-1$'
        $manifest = if ($script:Running -eq $Old) { $OldManifest } else { $CandidateManifest }
        return (@{
            Id = if ($service -eq 'camofox-research') { '1' * 64 } else { '2' * 64 }
            Image = $script:Running
            ImageManifestDescriptor = @{ digest=$manifest; platform=@{os='linux'; architecture='amd64'} }
            Config = @{ Labels=@{'com.docker.compose.project'='firecrawl';'com.docker.compose.service'=$service} }
            State = @{ Running=$true }
        } | ConvertTo-Json -Depth 8 -Compress)
    }
    if ($args[0] -eq 'inspect' -and $args[2] -eq '{{.Image}}') { return $script:Running }
    if ($args[0] -eq 'image' -and $args[1] -eq 'inspect') {
        if ($args -contains '--platform') {
            if ($args[-1] -eq $Old) { return $OldManifest }
            if ($args[-1] -eq $Candidate) { return $CandidateManifest }
        }
        if ($args[-1] -eq 'influencerresearch-camofox:latest') { return $Candidate }
        if ($args[-1] -eq $script:Tag) { return $Old }
    }
    if ($args[0] -eq 'tag' -and $args[1] -eq $Old -and $args[2] -like 'influencerresearch-camofox:rollback-*') {
        $script:Tag = $args[2]; return
    }
    if ($args[0] -eq 'exec') {
        if ($script:Busy -and $args -contains 'idle') { $global:LASTEXITCODE = 1 }
        return
    }
    if ($args[0] -eq 'ps') {
        return @("firecrawl-api-1|$('3' * 64)", "firecrawl-camofox-research-1|$('1' * 64)", "firecrawl-camofox-research-proxy-1|$('2' * 64)")
    }
    if ($args[0] -eq 'compose') {
        if ($args.Count -gt 3 -and $args[3] -eq '-f') {
            if (($args[5..($args.Count-1)] -join ' ') -cne 'up -d --no-deps --no-build --pull never --force-recreate camofox-research camofox-research-proxy' -or
                ((Get-Content -LiteralPath $args[4] -Raw) -notmatch [regex]::Escape($script:Tag))) {
                throw 'Rollback scope or image mismatch.'
            }
            $script:RollbackCount++
            $script:Running = $Old
            return
        }
        if (($args -join ' ') -cne 'compose -f fixture.yaml up -d --no-deps --no-build --pull never --force-recreate camofox-research camofox-research-proxy') {
            throw 'Unexpected Compose mutation scope.'
        }
        $script:UpCount++
        if ($script:FailNextUp) { $script:FailNextUp=$false; $global:LASTEXITCODE=1; return }
        $script:Running = $Candidate
        return
    }
    throw "Unexpected Docker command: $($args[0])"
}
function Assert-Refused([string]$Digest, [string]$Expected) {
    $Rejected = $false
    try { Invoke-FirecrawlCamofoxRefresh -ComposeArguments @('compose','-f','fixture.yaml') -ReviewedDigest $Digest | Out-Null }
    catch {
        if ($_.Exception.Message -notlike $Expected) { throw }
        $Rejected=$true
    }
    if (-not $Rejected -or $script:UpCount -ne 0) { throw 'Preflight did not fail closed.' }
}
Assert-Refused ('sha256:' + ('e' * 64)) '*not the current local tag*'
$script:Busy = $true
Assert-Refused $Candidate '*busy or unhealthy*'
$script:Busy = $false
$Result = Invoke-FirecrawlCamofoxRefresh -ComposeArguments @('compose','-f','fixture.yaml') -ReviewedDigest $Candidate
if ($script:UpCount -ne 1 -or $script:Running -cne $Candidate -or
    $script:Tag -cne ('influencerresearch-camofox:rollback-' + ('a' * 12)) -or
    ($Result -join ' ') -notmatch 'CAMOFOX_RESEARCH_REFRESH_OK') {
    throw 'Scoped promotion fixture failed.'
}
if (@($script:Calls | Where-Object { $_ -match '^exec firecrawl-camofox-research-proxy-1 sh -ec .*proxy_username.*proxy_password' }).Count -lt 2) {
    throw 'Proxy secret readiness check was not exercised.'
}
# Exercise the exact rollback path in a synthetic local state directory.
$Temp = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$OriginalLocalAppData = $env:LOCALAPPDATA
$null = New-Item -ItemType Directory -Path (Join-Path $Temp 'FirecrawlLocal') -Force
$env:LOCALAPPDATA = $Temp
try {
    $script:Running = $Old
    $script:FailNextUp = $true
    $script:RollbackCount = 0
    $Rejected = $false
    try { Invoke-FirecrawlCamofoxRefresh -ComposeArguments @('compose','-f','fixture.yaml') -ReviewedDigest $Candidate | Out-Null }
    catch {
        if ($_.Exception.Message -notlike '*baseline restored*') { throw }
        $Rejected=$true
    }
    if (-not $Rejected -or $script:RollbackCount -ne 1 -or $script:Running -cne $Old) { throw 'Scoped rollback fixture failed.' }
} finally {
    $env:LOCALAPPDATA = $OriginalLocalAppData
    Remove-Item -LiteralPath $Temp -Recurse -Force
}
Write-Output 'CAMOFOX_RESEARCH_REFRESH_FIXTURE_OK'
