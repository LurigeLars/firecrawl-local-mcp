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
