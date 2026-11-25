$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Assert-Order([string]$File,[string]$Protect,[string]$Build,[string]$Check) {
    $Text = Get-Content -LiteralPath (Join-Path $PSScriptRoot $File) -Raw
    $A = $Text.IndexOf($Protect)
    $B = $Text.IndexOf($Build)
    $C = $Text.LastIndexOf($Check)
    if ($A -lt 0 -or $B -le $A -or $C -le $B) { throw "Rollback ordering invalid: $File" }
}
Assert-Order 'refresh-public-gateway.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments build gateway' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-node-helpers.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments build $Service' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-node-runtime.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments --profile public-proxy build' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-ephemeral.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments build redis' 'Assert-FirecrawlRollbackImage'
Assert-Order 'refresh-searxng.ps1' '$Rollback = Save-FirecrawlRollbackImage' '& docker @ComposeArguments pull $Service' 'Assert-FirecrawlRollbackImage'

. (Join-Path $PSScriptRoot 'refresh-baseline.ps1')
. (Join-Path $PSScriptRoot 'refresh-public-gateway.ps1')
$script:Baseline = 'sha256:' + ('a' * 64)
$script:Running = $script:Baseline
$script:Image = $script:Baseline
$script:Present = $true
$script:ListFails = $false
$script:TagCalls = 0
function docker {
    $global:LASTEXITCODE=0
    if ($args[0] -eq 'inspect') { return $script:Running }
    if ($args[0] -eq 'image' -and $args[1] -eq 'ls') {
        if ($script:ListFails) { $global:LASTEXITCODE=1; return }
        if ($script:Present) { return $script:Baseline }
        return
    }
    if ($args[0] -eq 'image' -and $args[1] -eq 'inspect') { return $script:Image }
    if ($args[0] -eq 'tag') {
        if (-not $script:Present) { $global:LASTEXITCODE=1; return }
        $script:TagCalls++
        return
    }
    throw 'Unexpected Docker invocation.'
}
$Tag = Save-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -Repository 'firecrawl-gateway' -AllowMissing
if ($Tag -cne ('firecrawl-gateway:rollback-' + ('a' * 12)) -or $script:TagCalls -ne 1) { throw 'Available rollback must be retained.' }
Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag $Tag -RollbackId $script:Baseline
$script:Image = 'sha256:' + ('b' * 64)
$Rejected = $false
try { Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag $Tag -RollbackId $script:Baseline } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Changed retained image was accepted.' }
$script:Present = $false
$Tag = Save-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -Repository 'firecrawl-gateway' -AllowMissing
if ($Tag -cne 'unavailable' -or $script:TagCalls -ne 1) { throw 'Missing stateless image should warn, not build or tag.' }
Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag $Tag -RollbackId $script:Baseline
$Rejected = $false
try { Save-FirecrawlRollbackImage -Container 'firecrawl-redis-1' -BaselineId $script:Baseline -Repository 'firecrawl-redis' } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Stateful rollback failure must remain blocking.' }
$script:ListFails=$true
$Rejected=$false
try { Save-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -Repository 'firecrawl-gateway' -AllowMissing } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Docker inventory failure must not be treated as missing image.' }
$script:ListFails=$false
$script:Running='sha256:' + ('d' * 64)
$Rejected=$false
try { Assert-FirecrawlRollbackImage -Container 'firecrawl-gateway-1' -BaselineId $script:Baseline -RollbackTag 'unavailable' -RollbackId $script:Baseline } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Concurrent replacement must remain blocking.' }
$Windows = ConvertTo-FirecrawlGatewayBindPath -Path 'C:\ClaudeCode\firecrawl-local\public\gateway'
$Linux = ConvertTo-FirecrawlGatewayBindPath -Path '/run/desktop/mnt/host/c/ClaudeCode/firecrawl-local/public/gateway'
if ($Windows -cne $Linux) { throw 'Equivalent Docker Desktop path rejected.' }
$Other = ConvertTo-FirecrawlGatewayBindPath -Path '/run/desktop/mnt/host/c/ClaudeCode/firecrawl-local-other/public/gateway'
if ($Other -ceq $Windows) { throw 'Different source path accepted.' }
$Rejected=$false
try { ConvertTo-FirecrawlGatewayBindPath -Path '/run/desktop/mnt/host/c/ClaudeCode/firecrawl-local/public/../gateway' } catch { $Rejected=$true }
if (-not $Rejected) { throw 'Path traversal accepted.' }
Write-Host 'ROLLBACK_ORDER_POLICY_AND_BIND_PATH_TESTS_PASS'
