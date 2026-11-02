# Reviewed opt-in for a disposable Firecrawl queue/cache service.
# One service only; PostgreSQL and all other MCP projects are never recreated.
function Invoke-FirecrawlEphemeralRefresh {
    param(
        [Parameter(Mandatory)][ValidateSet('redis','rabbitmq')][string]$Service,
        [Parameter(Mandatory)][string[]]$ComposeArguments
    )
    $Container = "firecrawl-$Service-1"
    $ExpectedOld = if ($Service -eq 'redis') {
        'sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0'
    } else {
        'sha256:1aab4d911053f3ee4ff9bb231f5192ebd6a12281f8044910256a667fdb04108d'
    }
    $TargetImage = if ($Service -eq 'redis') { 'firecrawl-redis:patched' } else { 'rabbitmq:4.3.7-management-alpine' }
    $RabbitTarget = 'sha256:1528a88eebf2bc4d0ab7e9d5d9bc542e9bdf5e1b105e65848fcf8de5c2186528'
    $OtherIds = [ordered]@{}
    foreach ($Name in @('firecrawl-api-1','firecrawl-nuq-postgres-1')) {
        $Id = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Id -notmatch '^[a-f0-9]{64}$') {
            throw "Required unrelated service unavailable: $Name"
        }
        $OtherIds[$Name] = [string]$Id
    }
    foreach ($Name in @('firecrawl-redis-1','firecrawl-rabbitmq-1')) {
        $Running = (& docker inspect --format '{{.State.Running}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$Running -cne 'true') {
            throw "Required queue/cache service is not running: $Name"
        }
    }
    $RunningImage = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$RunningImage -notmatch '^sha256:[a-f0-9]{64}$') {
        throw "Cannot verify current image: $Container"
    }
    if ($Service -eq 'rabbitmq') {
        # Refuse to discard a message that a consumer has checked out.
        # Inactive READY messages may be disposable for this private setup.
        $UnackedRows = @(& docker exec $Container rabbitmqctl -q list_queues messages_unacknowledged 2>&1)
        if ($LASTEXITCODE -ne 0) { throw 'Cannot verify RabbitMQ in-flight messages.' }
        foreach ($Line in $UnackedRows) {
            $Value = ([string]$Line).Trim()
            if ($Value -eq '') { continue }
            if ($Value -cnotmatch '^[0-9]+$') { throw 'Unexpected RabbitMQ queue telemetry; refusing reset.' }
            if ([int64]$Value -gt 0) { throw 'RabbitMQ has in-flight unacknowledged messages; refusing reset.' }
        }
    }
    # Redis is rebuilt from the already-reviewed pinned 8.10.3 Alpine image.
    # RabbitMQ 4.3.7 is already pulled and the exact image ID is expected.
    if ($Service -eq 'redis') {
        & docker @ComposeArguments build redis
        if ($LASTEXITCODE -ne 0) { throw 'Redis security build failed; old container untouched.' }
    }
    $TargetId = (& docker image inspect --format '{{.Id}}' $TargetImage)
    if ($LASTEXITCODE -ne 0 -or [string]$TargetId -notmatch '^sha256:[a-f0-9]{64}$') {
        throw "Target image unavailable: $TargetImage"
    }
    if ($Service -eq 'rabbitmq' -and [string]$TargetId -cne $RabbitTarget) {
        throw 'RabbitMQ target image changed from reviewed local image.'
    }
    if ([string]$RunningImage -cne [string]$TargetId) {
        # High/Critical *fixable* issues are a hard fail. No insecure override.
        & docker scout cves --only-severity critical,high --only-fixed --exit-code ("local://" + $TargetImage) | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Docker Scout refused target image; live container untouched.' }
        $AfterScan = (& docker image inspect --format '{{.Id}}' $TargetImage)
        if ($LASTEXITCODE -ne 0 -or [string]$AfterScan -cne [string]$TargetId) {
            throw 'Target image identity changed during security scan.'
        }
        if ([string]$RunningImage -cne $ExpectedOld) {
            throw 'Unexpected original queue/cache image; refusing implicit migration.'
        }
        # Intentionally discard only this service's old ephemeral data.
        # New dedicated named volumes stabilize future routine updates.
        & docker @ComposeArguments up -d --no-deps --no-build --force-recreate --renew-anon-volumes $Service
        if ($LASTEXITCODE -ne 0) { throw "Scoped $Service recreation failed." }
    }
    $NewImage = (& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$NewImage -cne [string]$TargetId) {
        throw "Scoped $Service is not running the reviewed target image."
    }
    $ExpectedVolume = if ($Service -eq 'redis') { 'firecrawl_firecrawl-redis-data' } else { 'firecrawl_firecrawl-rabbitmq-data' }
    $Destination = if ($Service -eq 'redis') { '/data' } else { '/var/lib/rabbitmq' }
    $Volume = (& docker inspect --format ('{{range .Mounts}}{{if eq .Destination "' + $Destination + '"}}{{.Name}}{{end}}{{end}}') $Container)
    if ($LASTEXITCODE -ne 0 -or [string]$Volume -cne $ExpectedVolume) {
        throw "Scoped $Service missing its reviewed named data volume."
    }
    if ($Service -eq 'rabbitmq') {
        $HostName = (& docker inspect --format '{{.Config.Hostname}}' $Container)
        if ($LASTEXITCODE -ne 0 -or [string]$HostName -cne 'firecrawl-rabbitmq') {
            throw 'RabbitMQ stable hostname verification failed.'
        }
    }
    $Healthy = $false
    for ($Try = 0; $Try -lt 50; $Try++) {
        if ($Service -eq 'redis') {
            $Result = (& docker exec $Container redis-cli ping 2>$null)
            $Healthy = ($LASTEXITCODE -eq 0 -and [string]$Result -ceq 'PONG')
        } else {
            $Health = (& docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{end}}' $Container)
            $Healthy = ($LASTEXITCODE -eq 0 -and [string]$Health -ceq 'healthy')
        }
        if ($Healthy) { break }
        Start-Sleep -Seconds 3
    }
    if (-not $Healthy) { throw "Scoped $Service did not become healthy." }
    foreach ($Name in $OtherIds.Keys) {
        $After = (& docker inspect --format '{{.Id}}' $Name)
        if ($LASTEXITCODE -ne 0 -or [string]$After -cne [string]$OtherIds[$Name]) {
            throw "Unexpected unrelated container replacement: $Name"
        }
    }
    "EPHEMERAL_REFRESH_OK service=$Service image=$TargetId volume=$Volume; api/postgres container identities unchanged"
}
