# Exercise the actual fc.ps1 secret-restoration functions without executing
# Docker, fetching upstream, or touching user credentials.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Launcher = Join-Path (Split-Path $PSScriptRoot -Parent) 'fc.ps1'
$Tokens = $null
$ParseErrors = $null
$Ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $Launcher, [ref]$Tokens, [ref]$ParseErrors
)
if ($ParseErrors.Count -gt 0) { throw 'fc.ps1 failed parsing.' }

foreach ($FunctionName in @('Restore-OptionalEnvironmentVariable', 'Restore-ComposeServiceSecrets')) {
    $Definition = $Ast.Find({
        param($Node)
        $Node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $Node.Name -eq $FunctionName
    }, $true)
    if ($null -eq $Definition) { throw "Missing reviewed function $FunctionName." }
    . ([scriptblock]::Create($Definition.Extent.Text))
}

$Names = @(
    'FIRECRAWL_POSTGRES_PASSWORD_SECRET',
    'FIRECRAWL_SEARXNG_SECRET_SECRET',
    'FIRECRAWL_PUBLIC_PROXY_SERVER_SECRET',
    'FIRECRAWL_PUBLIC_PROXY_USERNAME_SECRET',
    'FIRECRAWL_PUBLIC_PROXY_PASSWORD_SECRET',
    'PLAYWRIGHT_MICROSERVICE_URL'
)
$Original = @{}
foreach ($Name in $Names) {
    $Path = "Env:$Name"
    $WasSet = Test-Path -LiteralPath $Path
    $OriginalValue = if ($WasSet) { (Get-Item -LiteralPath $Path).Value } else { $null }
    $Original[$Name] = @{ WasSet = $WasSet; Value = $OriginalValue }
}

try {
    foreach ($Name in $Names) {
        $Path = "Env:$Name"
        if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -ErrorAction Stop }
    }

    # This exact scenario used to leave six suppressed ItemNotFound errors in
    # $Error even though the pinned upstream Git operation succeeded.
    $Error.Clear()
    Restore-ComposeServiceSecrets -PostgresWasSet $false -PostgresValue $null -SearxngWasSet $false -SearxngValue $null
    foreach ($Name in $Names | Select-Object -Skip 2) {
        Restore-OptionalEnvironmentVariable -Name $Name -WasSet $false -Value $null
    }
    if ($Error.Count -ne 0) { throw "Absent environment cleanup added $($Error.Count) PowerShell error records." }

    # The path must also restore present values without deleting them.
    foreach ($Name in $Names) { Set-Item -LiteralPath "Env:$Name" -Value 'fixed-regression-value' -ErrorAction Stop }
    $Error.Clear()
    Restore-ComposeServiceSecrets -PostgresWasSet $true -PostgresValue 'restored' -SearxngWasSet $true -SearxngValue 'restored'
    foreach ($Name in $Names | Select-Object -Skip 2) {
        Restore-OptionalEnvironmentVariable -Name $Name -WasSet $true -Value 'restored'
    }
    foreach ($Name in $Names) {
        if ((Get-Item -LiteralPath "Env:$Name").Value -ne 'restored') {
            throw "Present environment variable was not restored: $Name"
        }
    }
    if ($Error.Count -ne 0) { throw 'Present environment restoration emitted an error.' }
    Write-Output 'Environment restoration (missing and present): PASS'
}
finally {
    foreach ($Name in $Names) {
        $Path = "Env:$Name"
        if ($Original[$Name].WasSet) {
            Set-Item -LiteralPath $Path -Value $Original[$Name].Value -ErrorAction Stop
        }
        elseif (Test-Path -LiteralPath $Path) {
            Remove-Item -LiteralPath $Path -ErrorAction Stop
        }
    }
}


# Exercise recovery with dummy values only. No real Docker or DPAPI calls.
$Definition = $Ast.Find({
    param($Node)
    $Node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $Node.Name -eq 'Import-PublicProxyRuntimeSecrets'
}, $true)
. ([scriptblock]::Create($Definition.Extent.Text))
$compose = @('compose', '-f', 'review.yaml')
$PublicProxyConfigured = $true
$script:Writes = [System.Collections.Generic.List[object]]::new()
$script:Checks = [System.Collections.Generic.List[string]]::new()
$script:FailCamofox = $false
function Get-PublicProxyRuntimeValues {
    return [ordered]@{ proxy_server = 'http://example.invalid:80'; proxy_username = 'dummy-user'; proxy_password = 'dummy-password' }
}
function docker {
    $global:LASTEXITCODE = 0
    $cmd = $args -join ' '
    if ($cmd -match 'ps -q (playwright-public-proxy-service|camofox-research-proxy)$') { return 'dummy-container' }
    if ($cmd -match 'exec -T (playwright-public-proxy-service|camofox-research-proxy) sh -c test -s ') {
        $script:Checks.Add($cmd)
        return
    }
    throw 'Unexpected Docker invocation in proxy recovery test.'
}
function Invoke-DockerWithExactStdin {
    param([string]$InputText, [string[]]$Arguments)
    $cmd = $Arguments -join ' '
    if ($InputText -notmatch '^(dummy-user|dummy-password|http://example\.invalid:80)$') { throw 'Unexpected test input.' }
    if ($cmd.Contains($InputText)) { throw 'Secret appeared in command arguments.' }
    $script:Writes.Add($cmd)
    if ($script:FailCamofox -and $cmd.Contains('camofox-research-proxy')) { return 1 }
    return 0
}
Import-PublicProxyRuntimeSecrets
if ($script:Writes.Count -ne 5 -or $script:Checks.Count -ne 2) { throw 'Expected three Playwright and two Camofox imports and two checks.' }
foreach ($Key in @('proxy_username','proxy_password')) {
    $Expected = "camofox-research-proxy sh -ec umask 077; cat > /run/firecrawl-proxy-secrets/$Key.tmp; test -s /run/firecrawl-proxy-secrets/$Key.tmp; mv -f /run/firecrawl-proxy-secrets/$Key.tmp /run/firecrawl-proxy-secrets/$Key"
    if (@($script:Writes | Where-Object { $_.EndsWith($Expected) }).Count -ne 1) { throw "Missing atomic Camofox import: $Key" }
}
$script:FailCamofox = $true
try {
    Import-PublicProxyRuntimeSecrets
    throw 'Camofox import failure was ignored.'
} catch {
    if ($_.Exception.Message -notlike 'Camofox proxy runtime secret import failed*') { throw }
}
$PublicProxyConfigured = $false
$Count = $script:Writes.Count
Import-PublicProxyRuntimeSecrets
if ($script:Writes.Count -ne $Count) { throw 'Disabled proxy attempted secret import.' }
Write-Output 'Public proxy recovery (both sidecars, failure, disabled): PASS'
