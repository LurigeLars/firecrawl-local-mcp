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
