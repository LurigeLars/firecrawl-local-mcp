# Backup existing Redis and RabbitMQ without replacing or upgrading containers.
# Inspect is read-only. Backup briefly stops Firecrawl API and RabbitMQ.
# Backups contain RabbitMQ authentication material: keep private on this computer.
param([ValidateSet('Inspect','Backup')][string]$Action = 'Inspect')
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA unavailable.' }
$Docker = (Get-Command docker.exe -ErrorAction Stop).Source
function Invoke-Docker {
    param([string[]]$Arguments)
    $Lines = @(& $Docker @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "Docker $($Arguments[0]) failed (exit $LASTEXITCODE)." }
    return [string]($Lines -join [Environment]::NewLine)
}
function Inspect-Value {
    param([string]$Container,[string]$Format)
    return (Invoke-Docker -Arguments @('inspect','--format',$Format,$Container)).Trim()
}
$Originals = [ordered]@{}
foreach ($Name in @('firecrawl-api-1','firecrawl-redis-1','firecrawl-rabbitmq-1')) {
    if ((Inspect-Value $Name '{{.State.Running}}') -cne 'true') { throw "Not running: $Name" }
    $Originals[$Name] = Inspect-Value $Name '{{.Id}}'
}
$Hostname = Inspect-Value 'firecrawl-rabbitmq-1' '{{.Config.Hostname}}'
$RabbitVolume = Inspect-Value 'firecrawl-rabbitmq-1' '{{range .Mounts}}{{if eq .Destination "/var/lib/rabbitmq"}}{{.Name}}{{end}}{{end}}'
$RedisVolume = Inspect-Value 'firecrawl-redis-1' '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Name}}{{end}}{{end}}'
if ($Hostname -cnotmatch '^[a-f0-9]{12,64}$' -or $RabbitVolume -cnotmatch '^[a-f0-9]{64}$' -or $RedisVolume) {
    throw 'Unexpected data storage or RabbitMQ node identity: backup refused.'
}
$Node = "rabbit@$Hostname"
$null = Invoke-Docker -Arguments @('exec','firecrawl-rabbitmq-1','rabbitmq-diagnostics','-q','check_running')
$KeyCount = (Invoke-Docker -Arguments @('exec','firecrawl-redis-1','redis-cli','--raw','DBSIZE')).Trim()
if ($KeyCount -cnotmatch '^[0-9]+$') { throw 'Redis key count failed.' }
$Report = [ordered]@{
    action=$Action; redis_keys=[int64]$KeyCount; redis_volume_mounted=$false
    rabbitmq_node=$Node; rabbitmq_volume_kind='anonymous'
    original_api_id=$Originals['firecrawl-api-1']
    original_redis_id=$Originals['firecrawl-redis-1']
    original_rabbit_id=$Originals['firecrawl-rabbitmq-1']
}
if ($Action -eq 'Inspect') { $Report | ConvertTo-Json -Depth 3; return }

$Folder = Join-Path $env:LOCALAPPDATA ('FirecrawlLocal\backups\stateful-' + [DateTimeOffset]::UtcNow.ToString('yyyyMMdd-HHmmss'))
$RabbitBackup = Join-Path $Folder 'rabbitmq-data'
$null = New-Item -Path $RabbitBackup -ItemType Directory -Force
$ApiStopAttempted = $false
$BrokerStopAttempted = $false
try {
    # Prevent the API and its queue workers from writing during this snapshot.
    $ApiStopAttempted = $true
    $null = Invoke-Docker -Arguments @('stop','--time','90','firecrawl-api-1')
    if ((Inspect-Value 'firecrawl-api-1' '{{.State.Running}}') -cne 'false') { throw 'API failed to stop.' }

    # Capture the current Redis snapshot in a local file; never empty its keys.
    if ((Invoke-Docker -Arguments @('exec','firecrawl-redis-1','redis-cli','--raw','SAVE')).Trim() -cne 'OK') { throw 'Redis SAVE failed.' }
    $null = Invoke-Docker -Arguments @('exec','firecrawl-redis-1','redis-check-rdb','/data/dump.rdb')
    $Rdb = Join-Path $Folder 'redis-dump.rdb'
    $null = Invoke-Docker -Arguments @('cp','firecrawl-redis-1:/data/dump.rdb',$Rdb)
    if (-not (Test-Path -LiteralPath $Rdb -PathType Leaf) -or (Get-Item -LiteralPath $Rdb).Length -le 0) {
        throw 'Redis backup missing or empty.'
    }

    # Rabbit definitions are exported while RabbitMQ is still running.
    $TempDefinitions = '/tmp/firecrawl-stateful-definitions.json'
    $null = Invoke-Docker -Arguments @('exec','firecrawl-rabbitmq-1','rabbitmqctl','export_definitions',$TempDefinitions)
    $DefinitionsFile = Join-Path $Folder 'rabbitmq-definitions.json'
    $null = Invoke-Docker -Arguments @('cp',"firecrawl-rabbitmq-1:$TempDefinitions",$DefinitionsFile)
    $null = Invoke-Docker -Arguments @('exec','firecrawl-rabbitmq-1','rm','-f',$TempDefinitions)
    $Definitions = Get-Content -LiteralPath $DefinitionsFile -Raw | ConvertFrom-Json
    if ($null -eq $Definitions.queues -or @($Definitions.queues).Count -lt 1) { throw 'No queue definitions exported.' }

    # A consistent RabbitMQ quorum message backup requires the node stopped.
    $BrokerStopAttempted = $true
    $null = Invoke-Docker -Arguments @('stop','--time','90','firecrawl-rabbitmq-1')
    if ((Inspect-Value 'firecrawl-rabbitmq-1' '{{.State.Running}}') -cne 'false') { throw 'RabbitMQ failed to stop.' }
    $null = Invoke-Docker -Arguments @('cp','firecrawl-rabbitmq-1:/var/lib/rabbitmq/.',$RabbitBackup)
    if (-not (Test-Path -LiteralPath (Join-Path (Join-Path $RabbitBackup 'mnesia') $Node) -PathType Container)) {
        throw 'RabbitMQ Mnesia / quorum node backup missing.'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $RabbitBackup '.erlang.cookie') -PathType Leaf)) {
        throw 'RabbitMQ Erlang cookie missing from backup.'
    }
    $Report['redis_sha256'] = (Get-FileHash -LiteralPath $Rdb -Algorithm SHA256).Hash
    $Report['definitions_sha256'] = (Get-FileHash -LiteralPath $DefinitionsFile -Algorithm SHA256).Hash
    $Report['rabbitmq_queue_count'] = @($Definitions.queues).Count
}
finally {
    # Independently try to restore BOTH original services, even if broker
    # restart fails. Never recreate a container or swap its volume.
    if ($BrokerStopAttempted) {
        try {
            if ((Inspect-Value 'firecrawl-rabbitmq-1' '{{.State.Running}}') -cne 'true') {
                $null = Invoke-Docker -Arguments @('start','firecrawl-rabbitmq-1')
            }
        } catch { Write-Warning 'Original RabbitMQ restart failed; inspect immediately.' }
    }
    if ($ApiStopAttempted) {
        try {
            if ((Inspect-Value 'firecrawl-api-1' '{{.State.Running}}') -cne 'true') {
                $null = Invoke-Docker -Arguments @('start','firecrawl-api-1')
            }
        } catch { Write-Warning 'Original Firecrawl API restart failed; inspect immediately.' }
    }
}
foreach ($Name in $Originals.Keys) {
    if ((Inspect-Value $Name '{{.Id}}') -cne [string]$Originals[$Name]) {
        throw "Original container changed during backup: $Name"
    }
}
for ($Try = 0; $Try -lt 50; $Try++) {
    $Health = Inspect-Value 'firecrawl-rabbitmq-1' '{{if .State.Health}}{{.State.Health.Status}}{{end}}'
    if ($Health -ceq 'healthy') { break }
    Start-Sleep -Seconds 2
}
if ($Health -cne 'healthy' -or (Inspect-Value 'firecrawl-api-1' '{{.State.Running}}') -cne 'true') {
    throw 'Original services did not recover; backup not approved.'
}
$Report['backup_path'] = $Folder
$Report['image_changes'] = 0
$Report['restore_tested'] = $false
[IO.File]::WriteAllText((Join-Path $Folder 'manifest.json'),($Report | ConvertTo-Json -Depth 4),[Text.UTF8Encoding]::new($false))
$Report | ConvertTo-Json -Depth 4
