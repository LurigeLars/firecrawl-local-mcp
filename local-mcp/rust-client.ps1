# Future Rust production client. Not selected by the current launcher.
# Import uses only the reviewed production API's read-only token mount.
# Stdio passes a DPAPI-decrypted key only in the child process environment.
[CmdletBinding()]
param([ValidateSet('Import','Stdio')][string]$Mode = 'Stdio')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-RustClientToken([string]$Token) {
    if ($Token -cnotmatch '^[0-9a-f]{64}$' -or $Token -ceq ('0123456789abcdef' * 4)) {
        throw 'Invalid production Rust credential.'
    }
}

function Read-RustClientToken([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw 'Rust client credential is not provisioned.'
    }
    $secure = $null
    $ptr = [IntPtr]::Zero
    try {
        $secure = ConvertTo-SecureString -String (Get-Content -LiteralPath $Path -Raw)
        $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
        $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
        Assert-RustClientToken $token
        return $token
    } catch { throw 'Rust client credential cannot be decrypted or validated.' }
    finally {
        if ($ptr -ne [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
        if ($null -ne $secure) { $secure.Dispose() }
        $token = $null
    }
}

function Save-RustClientToken([string]$Path,[string]$Token) {
    Assert-RustClientToken $Token
    if (Test-Path -LiteralPath $Path) {
        if ((Read-RustClientToken $Path) -cne $Token) {
            throw 'Existing Rust client credential differs; rotation requires explicit review.'
        }
        return
    }
    $directory = Split-Path -Parent $Path
    $null = New-Item -ItemType Directory -Force -Path $directory
    $tmp = Join-Path $directory ([guid]::NewGuid().ToString('N') + '.dpapi.tmp')
    $secure = ConvertTo-SecureString -String $Token -AsPlainText -Force
    try {
        $encrypted = ConvertFrom-SecureString -SecureString $secure
        [IO.File]::WriteAllText($tmp,$encrypted,[Text.UTF8Encoding]::new($false))
        if ((Read-RustClientToken $tmp) -cne $Token) { throw 'DPAPI verification failed.' }
        # No overwrite, including concurrent attempts.
        [IO.File]::Move($tmp,$Path)
    } finally {
        $secure.Dispose()
        if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp -Force }
    }
}

function New-RustStdioProcess([string]$NodePath,[string]$Entry,[string]$Token) {
    Assert-RustClientToken $Token
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $NodePath
    $start.ArgumentList.Add($Entry)
    $start.UseShellExecute = $false
    # Do not redirect or reinterpret JSON-RPC stdio in PowerShell.
    $start.Environment['FIRECRAWL_API_URL'] = 'http://127.0.0.1:3002'
    $start.Environment['FIRECRAWL_API_KEY'] = $Token
    [void]$start.Environment.Remove('FIRECRAWL_API_KEY_FILE')
    return $start
}

if (-not $IsWindows -or -not $env:LOCALAPPDATA) { throw 'Rust client requires Windows DPAPI.' }
$state = Join-Path $env:LOCALAPPDATA 'FirecrawlLocal'
if (-not (Test-Path -LiteralPath (Join-Path $state 'rust-transition.lock'))) {
    throw 'No reviewed Rust transition is active.'
}
$path = Join-Path $state 'secrets/rust_api_token.dpapi'
if ($Mode -eq 'Import') {
    $api = 'firecrawl-api-1'
    $format = '{{.Config.Image}}|{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}|{{.State.Running}}|{{range .Mounts}}{{if eq .Destination "/run/research-token"}}{{.Name}}:{{.RW}}{{end}}{{end}}'
    $shape = @(& docker inspect --format $format $api 2>$null)
    if ($LASTEXITCODE -ne 0 -or $shape.Count -ne 1 -or
        $shape[0] -cne 'firecrawl-rust-api:local|firecrawl|api|true|firecrawl_rust-token:false') {
        throw 'Reviewed production Rust credential source unavailable.'
    }
    $token = $null
    $lines = $null
    try {
        # Captured stdout is never emitted by this script.
        $lines = @(& docker exec $api cat /run/research-token/token 2>$null)
        if ($LASTEXITCODE -ne 0 -or $lines.Count -ne 1) { throw 'Rust credential import failed.' }
        $token = [string]$lines[0]
        Save-RustClientToken -Path $path -Token $token
        'RUST_CLIENT_DPAPI_READY'
    } finally { $token = $null; $lines = $null }
    return
}
if ((Test-Path Env:FIRECRAWL_API_KEY) -or (Test-Path Env:FIRECRAWL_API_KEY_FILE) -or
    ((Test-Path Env:FIRECRAWL_API_URL) -and $env:FIRECRAWL_API_URL -cne 'http://127.0.0.1:3002')) {
    throw 'Conflicting Rust client credential or endpoint configuration.'
}
$token = $null
$start = $null
$process = $null
try {
    $node = Get-Command node -CommandType Application | Select-Object -First 1
    $token = Read-RustClientToken $path
    $start = New-RustStdioProcess -NodePath $node.Source -Entry (Join-Path $PSScriptRoot 'stdio-proxy.mjs') -Token $token
    $process = [Diagnostics.Process]::Start($start)
    [void]$start.Environment.Remove('FIRECRAWL_API_KEY')
    $process.WaitForExit()
    $code = $process.ExitCode
} finally {
    if ($null -ne $start) { [void]$start.Environment.Remove('FIRECRAWL_API_KEY') }
    if ($null -ne $process) { $process.Dispose() }
    $token = $null
}
exit $code
