$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Assert-Order([string]$File,[string]$Protect,[string]$Build,[string]$Check) {
    $Text = Get-Content -LiteralPath (Join-Path $PSScriptRoot $File) -Raw
    $A = $Text.IndexOf($Protect)
    $B = $Text.IndexOf($Build)
    $C = $Text.LastIndexOf($Check)
    if ($A -lt 0 -or $B -le $A -or $C -le $B) { throw "Rollback ordering invalid: $File" }
}
Assert-Order 'refresh-public-gateway.ps1' '$Protected = Get-FirecrawlGatewayRollback' '& docker @ComposeArguments build gateway' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-node-helpers.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments build $Service' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-node-runtime.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments --profile public-proxy build' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-ephemeral.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments build redis' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-searxng.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments pull $Service' 'Assert-FirecrawlRollbackImage'

. (Join-Path $PSScriptRoot 'refresh-baseline.ps1')
. (Join-Path $PSScriptRoot 'refresh-public-gateway.ps1')
$script:Baseline = 'sha256:' + ('a' * 64)
$script:Image = $script:Baseline
$script:TagCalls = 0
function docker {
    if ($args[0] -eq 'inspect') { $global:LASTEXITCODE=0; return $script:Baseline }
    if ($args[0] -eq 'image' -and $args[1] -eq 'inspect') {
        $global:LASTEXITCODE=0
        return $script:Image
    }
    if ($args[0] -eq 'tag') { $script:TagCalls++; $global:LASTEXITCODE=0; return }
    throw 'Unexpected Docker invocation in regression.'
}
$Protected = Get-FirecrawlGatewayRollback -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -ProjectRoot '.'
if ($Protected.Reconstructed -or $Protected.Id -cne $script:Baseline -or $script:TagCalls -ne 1) {
    throw 'Existing exact image must be tagged normally.'
}
Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag $Protected.Tag -RollbackId $Protected.Id
$script:Image = 'sha256:' + ('b' * 64)
$Rejected = $false
try { Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag $Protected.Tag -RollbackId $Protected.Id } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Changed rollback image was accepted.' }
$Rejected = $false
try { Get-FirecrawlGatewayRollback -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -ProjectRoot '.' } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Unknown missing image was accepted.' }
Write-Host 'ROLLBACK_ORDER_AND_IDENTITY_TESTS_PASS'


# Simulate the one missing baseline with fixed git and Docker test doubles.
# No actual build, registry, secrets, or container state is used in this test.
$script:Baseline = 'sha256:7edfc616ff5f469baeae19f5bc0bdf511239d97c83f010761d31b02b7e43e9bc'
$script:Recovered = 'sha256:' + ('c' * 64)
$script:BuildSeen = $false
$script:ScanSeen = $false
$script:SmokeSeen = $false
$script:BuildDirectory = ''
function git {
    $global:LASTEXITCODE=0
    if ($args[2] -eq 'status' -or $args[2] -eq 'merge-base') { return }
    if ($args[2] -eq 'show' -and $args[3] -ceq 'b7dbfba9860c1742022dfe309c34c80705a0c036:public/Dockerfile.gateway') {
        return @('FROM node:26.13.1-alpine@sha256:6bd9ab43c0ec0190ca7b2d193906cb8b7edf6e7f4afa43ddeccd6452710bffb5','RUN rm -rf /usr/local/lib/node_modules/npm','USER node','WORKDIR /app')
    }
    throw 'Unexpected git recovery invocation.'
}
function docker {
    $global:LASTEXITCODE=0
    switch ($args[0]) {
        'inspect' { return $script:Baseline }
        'image' {
            if ($args[-1] -ceq $script:Baseline) { $global:LASTEXITCODE=1; return }
            if ($args[-1] -ceq 'firecrawl-gateway:reconstructed-b7dbfba9860c' -and $script:BuildSeen) { return $script:Recovered }
            throw 'Unexpected recovery image inspection.'
        }
        'build' {
            $script:BuildDirectory = $args[-1]
            $Files = @(Get-ChildItem -LiteralPath $script:BuildDirectory -Force)
            if ($Files.Count -ne 1 -or $Files[0].Name -cne 'Dockerfile') { throw 'Recovery context was not empty except for the recipe.' }
            if ($args[2] -cne 'firecrawl-gateway:reconstructed-b7dbfba9860c') { throw 'Recovery attempted to replace the live image tag.' }
            $script:BuildSeen=$true
            return
        }
        'scout' {
            if (-not $script:BuildSeen -or $args[-1] -cne ('local://' + $script:Recovered)) { throw 'Recovery scan did not target the rebuilt image.' }
            $script:ScanSeen=$true
            return
        }
        'run' {
            if (-not $script:ScanSeen -or '--read-only' -notin $args -or '--network' -notin $args -or 'none' -notin $args -or '--mount' -in $args -or '-e' -in $args -or $script:Recovered -notin $args) { throw 'Recovery smoke was not isolated.' }
            $script:SmokeSeen=$true
            return 'v26.13.1'
        }
        default { throw 'Unexpected Docker recovery invocation.' }
    }
}
$Protected = Get-FirecrawlGatewayRollback -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -ProjectRoot '.'
if (-not $Protected.Reconstructed -or $Protected.Id -cne $script:Recovered -or -not $script:SmokeSeen -or (Test-Path -LiteralPath $script:BuildDirectory)) {
    throw 'Reconstructed fallback did not pass identity, smoke or cleanup checks.'
}
Write-Host 'FIXED_ORPHAN_GATEWAY_RECOVERY_TEST_PASS'
