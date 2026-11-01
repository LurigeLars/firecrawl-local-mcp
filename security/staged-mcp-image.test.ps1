# Windows-only behavior test, using reviewed fixed functions and a fake Docker.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Repo = Split-Path $PSScriptRoot -Parent
$Source = Join-Path $Repo 'security/staged-mcp-image.ps1'
$Tokens = $null
$Errors = $null
$Ast = [System.Management.Automation.Language.Parser]::ParseFile($Source, [ref]$Tokens, [ref]$Errors)
if ($Errors.Count -gt 0) { throw 'Stage gate PowerShell syntax invalid.' }

$TempRoot = Join-Path $env:RUNNER_TEMP ('mcp-stage-' + [guid]::NewGuid().ToString('N'))
$OldLocal = $env:LOCALAPPDATA
try {
    New-Item -ItemType Directory -Path $TempRoot -Force | Out-Null
    $env:LOCALAPPDATA = $TempRoot
    . $Source

    $Files = @(
        'fc.ps1','security/staged-mcp-image.ps1','upstream/firecrawl.json',
        'compose.local.yaml','compose.public.yaml','public/Dockerfile.mcp',
        'public/package.json','public/package-lock.json'
    )
    foreach ($File in $Files) {
        $Path = Join-Path $TempRoot $File
        New-Item -ItemType Directory -Path (Split-Path $Path -Parent) -Force | Out-Null
        Set-Content -LiteralPath $Path -Value ('fixture:' + $File)
    }

    $script:ImageId = 'sha256:' + ('a' * 64)
    $script:PreviousId = 'sha256:' + ('b' * 64)
    $script:ActiveId = $script:PreviousId
    $script:Running = 'true'
    $script:ScanFails = $false
    $script:DeployCount = 0
    $script:BuildCount = 0
    $script:RollbackAvailable = $false
    $script:RollbackPinned = $false
    function Get-McpImageId { return $script:ImageId }
    function Invoke-McpDocker {
        param([string[]]$Arguments, [int]$TimeoutSeconds = 1800)
        if ($Arguments[0] -eq 'scout' -and $script:ScanFails) {
            throw 'Simulated CVE or scanner failure'
        }
        if ($Arguments[0] -eq 'image' -and $Arguments[1] -eq 'ls') {
            if ($script:RollbackAvailable) { return $script:PreviousId }
            return ''
        }
        if ($Arguments[0] -eq 'image' -and $Arguments[1] -eq 'tag') {
            if (-not $script:RollbackAvailable) { throw 'Simulated missing previous image' }
            $script:RollbackPinned = $true
            return ''
        }
        if ($Arguments[0] -eq 'image' -and $Arguments[1] -eq 'inspect') {
            $Target = [string]$Arguments[-1]
            if ($Target -ceq $script:PreviousId) {
                if (-not $script:RollbackAvailable) {
                    throw 'Reviewed Docker operation inspect-image failed with exit code 1.'
                }
                return $script:PreviousId
            }
            if (-not $script:RollbackPinned) { throw 'Simulated missing rollback tag' }
            return $script:PreviousId
        }
        if ($Arguments -contains 'build') { $script:BuildCount += 1 }
        if ($Arguments[0] -eq 'inspect') {
            if ($Arguments -contains '{{.State.Running}}') { return $script:Running }
            if ($Arguments -contains '{{.Image}}') { return $script:ActiveId }
        }
        if ($Arguments -contains '--force-recreate') {
            $script:DeployCount += 1
            $script:ActiveId = $script:ImageId
        }
        return ''
    }

    $Args = @('compose','-f','reviewed-compose.yml')
    $Receipt = Join-Path $TempRoot 'FirecrawlLocal/staging/mcp-stage.json'
    # Fail closed without any staged build.
    try {
        Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
        throw 'Deploy unexpectedly accepted without receipt'
    }
    catch {
        if ($_.Exception.Message -notmatch 'Missing successful') { throw }
    }
    # Missing old Docker image must neither block build nor pollute the
    # host maintenance runner's strict PowerShell ErrorRecord history.
    $Error.Clear()
    Invoke-McpStageSecurityCheck -Action build -ComposeArguments $Args -ProjectRoot $TempRoot
    if ($Error.Count -ne 0) { throw 'Successful build left handled errors in PowerShell $Error.' }
    if ($script:BuildCount -ne 1 -or $script:RollbackPinned) {
        throw 'Expected a build without preserved rollback image'
    }
    if (-not (Test-Path -LiteralPath $Receipt)) { throw 'Successful scan receipt not recorded' }
    if ($script:DeployCount -ne 0) { throw 'Build restarted the service' }
    $InitialReceipt = Get-Content -LiteralPath $Receipt -Raw | ConvertFrom-Json
    if ($InitialReceipt.rollback_available -ne $false) {
        throw 'Missing Docker image was incorrectly reported as recoverable'
    }

    # Changed source, image mismatch, stale evidence or scanner failure must
    # independently block release without attempting container changes.
    $Original = Get-Content -LiteralPath (Join-Path $TempRoot 'public/package-lock.json') -Raw
    Set-Content -LiteralPath (Join-Path $TempRoot 'public/package-lock.json') -Value 'tampered'
    try {
        Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
        throw 'Tampered source accepted'
    }
    catch { if ($_.Exception.Message -notmatch 'source changed after scan') { throw } }
    Set-Content -LiteralPath (Join-Path $TempRoot 'public/package-lock.json') -Value $Original.TrimEnd()

    $script:ImageId = 'sha256:' + ('c' * 64)
    try {
        Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
        throw 'Wrong image accepted'
    }
    catch { if ($_.Exception.Message -notmatch 'identity mismatch') { throw } }
    $script:ImageId = 'sha256:' + ('a' * 64)

    $Data = Get-Content -LiteralPath $Receipt -Raw | ConvertFrom-Json
    $OriginalDate = $Data.created_utc
    $Data.created_utc = [DateTimeOffset]::UtcNow.AddDays(-2).ToString('o')
    [IO.File]::WriteAllText($Receipt, ($Data | ConvertTo-Json -Depth 8))
    try {
        Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
        throw 'Old scan accepted'
    }
    catch { if ($_.Exception.Message -notmatch 'older than 24 hours') { throw } }
    $Data.created_utc = $OriginalDate
    [IO.File]::WriteAllText($Receipt, ($Data | ConvertTo-Json -Depth 8))

    $script:ScanFails = $true
    try {
        Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
        throw 'CVE scanner error accepted'
    }
    catch { if ($_.Exception.Message -notmatch 'Simulated CVE') { throw } }
    $script:ScanFails = $false
    if ($script:DeployCount -ne 0) { throw 'Blocked deployment touched a container' }

    Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
    if ($script:DeployCount -ne 1) { throw 'Expected exactly one scoped redeploy' }
    if (Test-Path -LiteralPath $Receipt) { throw 'Successful deploy did not consume scan receipt' }

    # Exercise original image retention when Docker still has it.
    $script:ActiveId = $script:PreviousId
    $script:DeployCount = 0
    $script:RollbackAvailable = $true
    $script:RollbackPinned = $false
    Invoke-McpStageSecurityCheck -Action build -ComposeArguments $Args -ProjectRoot $TempRoot
    if ($script:BuildCount -ne 2 -or -not $script:RollbackPinned) {
        throw 'Build did not retain existing original image'
    }
    $SecondReceipt = Get-Content -LiteralPath $Receipt -Raw | ConvertFrom-Json
    if ($SecondReceipt.rollback_available -ne $true) {
        throw 'Existing rollback image is not recorded in receipt'
    }
    Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
    if ($script:DeployCount -ne 1) { throw 'Expected one scoped rollout with backup present' }
    # Re-apply reviewed configuration even when the image ID has not changed.
    $script:ActiveId = $script:ImageId
    $script:DeployCount = 0
    Invoke-McpStageSecurityCheck -Action build -ComposeArguments $Args -ProjectRoot $TempRoot
    Invoke-McpStageSecurityCheck -Action deploy -ComposeArguments $Args -ProjectRoot $TempRoot
    if ($script:DeployCount -ne 1) { throw 'Config-only stage was incorrectly skipped' }
    Write-Output 'Staged MCP security gate (with and without old image): PASS'
}
finally {
    $env:LOCALAPPDATA = $OldLocal
    if (Test-Path -LiteralPath $TempRoot) {
        Remove-Item -LiteralPath $TempRoot -Recurse -Force
    }
}