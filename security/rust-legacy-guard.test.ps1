# Exercise the actual guard with synthetic Docker metadata; no Docker or
# user files accessed. Check stopped containers, foreign names, and marker.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot '../fc.ps1'), [ref]$null, [ref]$null)
$definition = $ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Assert-LegacyEngineAction'
}, $true)
. ([scriptblock]::Create($definition.Extent.Text))
$root = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $root
$script:found = @('firecrawl-api-1')
$script:shape = 'firecrawl-api|firecrawl|api|'
$script:exitCode = 0
$script:calls = 0
function docker {
    $script:calls++
    $global:LASTEXITCODE = $script:exitCode
    switch -Exact ($args -join ' ') {
        'container ls --all --filter name=^/firecrawl-api-1$ --format {{.Names}}' { $script:found }
        'inspect --format {{.Config.Image}}|{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}|{{range .Mounts}}{{if eq .Destination "/var/lib/research"}}rust-jobs{{end}}{{end}} firecrawl-api-1' { $script:shape }
        default { throw 'Unexpected Docker invocation' }
    }
}
function Expect-Refusal([string]$action) {
    try { Assert-LegacyEngineAction -ActionName $action -StateRoot $root; throw 'Guard permitted unsafe action' }
    catch { if ($_.Exception.Message -notlike '*legacy launcher action refused*') { throw } }
}
try {
    Assert-LegacyEngineAction -ActionName up -StateRoot $root
    $script:shape = 'firecrawl-api:latest|firecrawl|api|'
    Assert-LegacyEngineAction -ActionName redeploy -StateRoot $root
    $script:shape = 'firecrawl-rust-api:local|firecrawl|api|rust-jobs'
    foreach ($action in @('up','redeploy','down','stage-api','deploy-api','maintain-stateless','recover','refresh-rabbitmq','redeploy-mcp','test')) { Expect-Refusal $action }
    $script:shape = 'firecrawl-api|firecrawl|api|rust-jobs'
    Expect-Refusal up
    $script:shape = 'firecrawl-api|foreign|api|'
    Expect-Refusal up
    $script:found = @()
    Assert-LegacyEngineAction -ActionName up -StateRoot $root
    $script:exitCode = 1
    Expect-Refusal up
    $script:exitCode = 0
    # Recover the mocked native exit code after the expected refusal.
    Assert-LegacyEngineAction -ActionName up -StateRoot $root
    $null = New-Item -ItemType File -Path (Join-Path $root 'rust-transition.lock')
    $before = $script:calls
    Expect-Refusal up
    foreach ($action in @('status','logs','url')) { Assert-LegacyEngineAction -ActionName $action -StateRoot $root }
    if ($script:calls -ne $before) { throw 'Marker/readonly branch must not invoke Docker' }
    'RUST_LEGACY_LAUNCHER_GUARD_PASS'
} finally { Remove-Item -LiteralPath $root -Recurse -Force }

# Test active-mode decision independently of existing legacy fallback checks.
$definition = $ast.Find({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-RustMaintenanceMode'},$true)
. ([scriptblock]::Create($definition.Extent.Text))
$root = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $root
function docker {
    $global:LASTEXITCODE=0
    if ($args[0] -cne 'inspect' -or $args[-1] -cne 'firecrawl-api-1') { throw 'Unexpected active mode invocation' }
    return $script:shape
}
function Expect-ModeRefusal([string]$ActionName,[string]$ServiceName='') {
    try { Get-RustMaintenanceMode -ActionName $ActionName -ServiceName $ServiceName -StateRoot $root; throw 'Unsafe action allowed.' }
    catch { if ($_.Exception.Message -notlike '*maintenance refused*') { throw } }
}
try {
    if (Get-RustMaintenanceMode -ActionName up -StateRoot $root) { throw 'Unexpected mode without marker.' }
    $marker=Join-Path $root 'rust-transition.lock'
    Set-Content -LiteralPath $marker -Value 'rust-pending-v1'
    if (Get-RustMaintenanceMode -ActionName up -StateRoot $root) { throw 'Pending mode permitted.' }
    Set-Content -LiteralPath $marker -Value 'rust-active-v1'
    $script:shape='firecrawl-rust-api:local|firecrawl|api|true|firecrawl_rust-jobs:true'
    foreach ($action in @('refresh-searxng','refresh-gateway','refresh-llm-proxy','refresh-camofox-research','refresh-search-proxy','refresh-playwright-router','recover','import-gemini','redeploy-mcp','rust-deploy-document','rust-deploy-api','status')) {
        if (-not (Get-RustMaintenanceMode -ActionName $action -StateRoot $root)) { throw 'Expected shared sidecar action refused.' }
    }
    if (-not (Get-RustMaintenanceMode -ActionName maintain-stateless -ServiceName mcp -StateRoot $root)) { throw 'Expected MCP action refused.' }
    foreach ($action in @('up','redeploy','down','test','stage-api','deploy-api','refresh-redis','refresh-rabbitmq','repair-postgres-auth','migrate-nuq-postgres','sync-upstream')) { Expect-ModeRefusal $action }
    Expect-ModeRefusal maintain-stateless api
    $script:shape='firecrawl-rust-api:local|firecrawl|api|false|firecrawl_rust-jobs:true'
    Expect-ModeRefusal refresh-gateway
    $script:shape='firecrawl-api|firecrawl|api|true|'
    Expect-ModeRefusal recover
    'RUST_SHARED_MAINTENANCE_MODE_FIXTURE_PASS'
} finally { Remove-Item -LiteralPath $root -Recurse -Force }

# Real filesystem lock in a synthetic temporary directory, no user state.
$definition=$ast.Find({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Enter-FirecrawlActionLock'},$true)
. ([scriptblock]::Create($definition.Extent.Text))
$root=Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$held=$null;$again=$null
try {
    $held=Enter-FirecrawlActionLock $root
    try {
        $again=Enter-FirecrawlActionLock $root
        throw 'Concurrent mutation allowed.'
    } catch { if ($_.Exception.Message -notlike '*holds the launcher lock*') { throw } }
    $held.Dispose();$held=$null
    $again=Enter-FirecrawlActionLock $root
    'FIRECRAWL_ACTION_SERIALIZATION_PASS'
} finally {
    if ($null -ne $held) { $held.Dispose() }
    if ($null -ne $again) { $again.Dispose() }
    Remove-Item -LiteralPath $root -Recurse -Force
}
