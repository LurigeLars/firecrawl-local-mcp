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
