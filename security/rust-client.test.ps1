# Only synthetic DPAPI data in a temporary directory. No Docker or user state.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot '../local-mcp/rust-client.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw 'Rust client launcher parse failure.' }
foreach ($name in @('Assert-RustClientToken','Read-RustClientToken','Save-RustClientToken','New-RustStdioProcess')) {
    $def = $ast.Find({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name},$true)
    if ($null -eq $def) { throw 'Missing reviewed Rust client function.' }
    . ([scriptblock]::Create($def.Extent.Text))
}
$dir = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$path = Join-Path $dir 'fixture.dpapi'
$key = 'a' * 64
try {
    Save-RustClientToken -Path $path -Token $key
    if ((Read-RustClientToken $path) -cne $key) { throw 'Synthetic DPAPI round trip failed.' }
    if ((Get-Content -Raw -LiteralPath $path).Contains($key)) { throw 'Credential persisted as plaintext.' }
    Save-RustClientToken -Path $path -Token $key
    try {
        Save-RustClientToken -Path $path -Token ('b' * 64)
        throw 'Credential rotation unexpectedly permitted.'
    } catch { if ($_.Exception.Message -notlike 'Existing Rust client credential differs*') { throw } }
    foreach ($bad in @('',('z' * 64),('0123456789abcdef' * 4))) {
        try { Assert-RustClientToken $bad; throw 'Invalid token permitted.' }
        catch { if ($_.Exception.Message -cne 'Invalid production Rust credential.') { throw } }
    }
    $old = [Environment]::GetEnvironmentVariable('FIRECRAWL_API_KEY','Process')
    $start = New-RustStdioProcess -NodePath 'node.exe' -Entry 'fixture.mjs' -Token $key
    if ($start.Environment['FIRECRAWL_API_KEY'] -cne $key -or
        $start.Environment['FIRECRAWL_API_URL'] -cne 'http://127.0.0.1:3002' -or
        $start.ArgumentList.Count -ne 1 -or $start.ArgumentList[0] -cne 'fixture.mjs' -or
        $start.RedirectStandardInput -or $start.RedirectStandardOutput -or $start.UseShellExecute) { throw 'Unsafe client process shape.' }
    if ([Environment]::GetEnvironmentVariable('FIRECRAWL_API_KEY','Process') -cne $old) { throw 'Parent environment mutated.' }
    [void]$start.Environment.Remove('FIRECRAWL_API_KEY')
    'RUST_CLIENT_DPAPI_FIXTURE_PASS'
} finally {
    if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
}
