# Read-only production transition preflight. No Docker mutations or job data.
# The eventual cutover must repeat these checks after ingress is quiesced.
[CmdletBinding()]
param([switch]$Library)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-PreflightDocker([string[]]$Arguments) {
    $output = @(& docker @Arguments 2>$null)
    if ($LASTEXITCODE -ne 0) { throw 'Transition inventory command failed.' }
    return $output
}

function Get-PreflightCount($Value) {
    if ($null -eq $Value -or [string]$Value -cnotmatch '^[0-9]{1,18}$') { throw 'Invalid queue counter.' }
    return [int64]$Value
}

function Get-LegacyQueueReceipt {
    $vhosts = @(Invoke-PreflightDocker @('exec','firecrawl-rabbitmq-1','rabbitmqctl','list_vhosts','--formatter=json','name','--quiet'))
    try { $hostsFound = @(($vhosts -join [Environment]::NewLine) | ConvertFrom-Json) }
    catch { throw 'Invalid RabbitMQ vhost receipt.' }
    if ($hostsFound.Count -ne 1 -or $hostsFound[0].name -cne '/') { throw 'Unreviewed RabbitMQ virtual host.' }
    $queues = @(Invoke-PreflightDocker @('exec','firecrawl-rabbitmq-1','rabbitmqctl','list_queues','-p','/','--formatter=json','messages_ready','messages_unacknowledged','--quiet'))
    try {
        $parsed = ConvertFrom-Json -InputObject ($queues -join [Environment]::NewLine) -NoEnumerate
        if ($parsed -isnot [array]) { throw 'Invalid queue list.' }
    } catch { throw 'Invalid RabbitMQ queue receipt.' }
    $ready = [int64]0; $inFlight = [int64]0
    foreach ($q in $parsed) {
        $ready += Get-PreflightCount $q.messages_ready
        $inFlight += Get-PreflightCount $q.messages_unacknowledged
    }
    # Pinned upstream schema: queued/active are nonterminal; backlog has no
    # status; active group jobs are not safe to strand on the other engine.
    $sql = "BEGIN READ ONLY; SET LOCAL statement_timeout='10s'; SELECT (SELECT count(*) FROM nuq.queue_scrape WHERE status IN ('queued','active')),(SELECT count(*) FROM nuq.queue_scrape_backlog),(SELECT count(*) FROM nuq.queue_crawl_finished WHERE status IN ('queued','active')),(SELECT count(*) FROM nuq.group_crawl WHERE status='active'); COMMIT;"
    $rows = @(Invoke-PreflightDocker @('exec','firecrawl-nuq-postgres-1','psql','-X','-q','-A','-t','-v','ON_ERROR_STOP=1','-U','postgres','-d','postgres','-c',$sql))
    if ($rows.Count -ne 1 -or $rows[0] -cnotmatch '^[0-9]{1,18}(\|[0-9]{1,18}){3}$') { throw 'Invalid NUQ count receipt.' }
    $counts = @($rows[0].Split('|') | ForEach-Object { Get-PreflightCount $_ })
    return [ordered]@{
        rabbit_ready=$ready; rabbit_in_flight=$inFlight
        nuq_scrape_pending=$counts[0]; nuq_backlog=$counts[1]
        nuq_crawl_finish_pending=$counts[2]; nuq_active_groups=$counts[3]
    }
}

function Get-LegacyInventoryReceipt {
    $expected = [ordered]@{
        'api' = 'firecrawl-api(:latest)?'
        'mcp' = 'firecrawl-mcp(:latest)?'
        'redis' = 'firecrawl-redis:patched'
        'rabbitmq' = 'rabbitmq:4\.4\.0-management-alpine'
        'nuq-postgres' = 'firecrawl-nuq-postgres(:latest)?'
    }
    $images = [ordered]@{}
    foreach ($service in $expected.Keys) {
        $name = "firecrawl-$service-1"
        $format = '{{.Config.Image}}|{{.Image}}|{{index .Config.Labels "com.docker.compose.project"}}|{{index .Config.Labels "com.docker.compose.service"}}|{{.State.Running}}'
        $shape = @(Invoke-PreflightDocker @('inspect','--format',$format,$name))
        if ($shape.Count -ne 1) { throw 'Invalid production inventory.' }
        $parts = $shape[0].Split('|')
        if ($parts.Count -ne 5 -or $parts[0] -cnotmatch ('^'+$expected[$service]+'$') -or
            $parts[1] -cnotmatch '^sha256:[0-9a-f]{64}$' -or $parts[2] -cne 'firecrawl' -or
            $parts[3] -cne $service -or $parts[4] -cne 'true') { throw "Unexpected running legacy service: $service" }
        $images[$service] = $parts[1]
    }
    $volumes = [ordered]@{
        'redis'=@('/data','firecrawl_firecrawl-redis-data')
        'rabbitmq'=@('/var/lib/rabbitmq','firecrawl_firecrawl-rabbitmq-data')
        'nuq-postgres'=@('/var/lib/postgresql/data','firecrawl_nuq-postgres-data')
    }
    foreach ($service in $volumes.Keys) {
        $destination,$volume=$volumes[$service]
        $format='{{range .Mounts}}{{if eq .Destination "'+$destination+'"}}{{.Type}}|{{.Name}}|{{.RW}}{{end}}{{end}}'
        $mount=@(Invoke-PreflightDocker @('inspect','--format',$format,"firecrawl-$service-1"))
        if ($mount.Count -ne 1 -or $mount[0] -cne "volume|$volume|true") { throw "Unreviewed data mount: $service" }
    }
    return $images
}

if ($Library) { return }
$images = Get-LegacyInventoryReceipt
$queues = Get-LegacyQueueReceipt
$idle = @($queues.Values | Where-Object { $_ -ne 0 }).Count -eq 0
[pscustomobject]@{
    verdict=$(if($idle){'IDLE_SNAPSHOT'}else{'BUSY'})
    scope='read-only preflight; ingress not quiesced; not cutover authorization'
    images=$images; queues=$queues; stable_named_volumes=$true
} | ConvertTo-Json -Depth 5
if (-not $idle) { exit 1 }
