$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot '../fc.ps1'), [ref]$tokens, [ref]$parseErrors
)
if ($parseErrors.Count) { throw 'fc.ps1 failed parsing.' }
$guard = $ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Assert-StatefulComposeDataSafety'
}, $true)
. ([scriptblock]::Create($guard.Extent.Text))

$compose = @('compose', '-f', 'review.yaml')
$runningRef = 'rabbitmq:4.4.0-management-alpine'
$runningId = 'sha256:' + ('a' * 64)
function docker {
    $global:LASTEXITCODE = 0
    switch -Exact ($args -join ' ') {
        'container ls --all --filter name=^/firecrawl-rabbitmq-1$ --format {{.Names}}' { 'firecrawl-rabbitmq-1' }
        'container ls --all --filter name=^/firecrawl-redis-1$ --format {{.Names}}' { }
        'container ls --all --filter name=^/firecrawl-nuq-postgres-1$ --format {{.Names}}' { }
        'inspect --format {{json .Mounts}} firecrawl-rabbitmq-1' {
            '[{"Type":"volume","Destination":"/var/lib/rabbitmq","Name":"firecrawl_firecrawl-rabbitmq-data"}]'
        }
        'compose -f review.yaml config --images rabbitmq' { $desiredRef }
        'inspect --format {{.Config.Image}}|{{.Image}} firecrawl-rabbitmq-1' { "$runningRef|$runningId" }
        "image inspect --format {{.Id}} $desiredRef" { $desiredId }
        default { throw 'Unexpected Docker call in stateful Compose guard.' }
    }
}

$desiredRef = $runningRef
$desiredId = $runningId
Assert-StatefulComposeDataSafety

foreach ($case in @(
    @{ Ref = 'rabbitmq:4.4.1-management-alpine'; Id = $runningId },
    @{ Ref = $runningRef; Id = ('sha256:' + ('b' * 64)) }
)) {
    $desiredRef = $case.Ref
    $desiredId = $case.Id
    try {
        Assert-StatefulComposeDataSafety
        throw 'Image change was allowed by the stateful Compose guard.'
    } catch {
        if ($_.Exception.Message -notlike 'RabbitMQ image change pending.*') { throw }
    }
}
Write-Output 'RabbitMQ whole-stack guard (same image, new tag, changed image ID): PASS'
