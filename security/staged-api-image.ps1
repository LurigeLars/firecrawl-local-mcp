# Isolated API staging. Never rebuild or restart Redis, RabbitMQ, Postgres, or API.
function Get-ApiStageHashes {
    param([string]$ProjectRoot)
    $Files = @('fc.ps1','security/staged-mcp-image.ps1','security/staged-api-image.ps1',
        'security/firecrawl-api-runtime-hotfix.Dockerfile','compose.local.yaml','compose.public.yaml','upstream/firecrawl.json')
    $Hashes = [ordered]@{}
    foreach ($File in $Files) {
        $Path = Join-Path $ProjectRoot $File
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing reviewed API source: $File" }
        $Hashes[$File] = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    }
    return $Hashes
}
function Invoke-ApiStage {
    param(
        [Parameter(Mandatory)][ValidateSet('stage','deploy')][string]$Action,
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Service = 'api'
    $Image = 'firecrawl-' + $Service
    $ImageTag = $Image + ':latest'
    $Container = 'firecrawl-' + $Service + '-1'
    $Folder = Join-Path $env:LOCALAPPDATA 'FirecrawlLocal\staging'
    if (-not (Test-Path -LiteralPath $Folder -PathType Container)) {
        New-Item -ItemType Directory -Path $Folder -Force | Out-Null
    }
    $ReceiptPath = Join-Path $Folder 'api-stage.json'
    $Hashes = Get-ApiStageHashes -ProjectRoot $ProjectRoot
    $RunningId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)
    $Running = Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}',$Container)
    if ($RunningId -cnotmatch '^sha256:[0-9a-f]{64}$' -or $Running -cne 'true') {
        throw "API service $Service baseline unavailable."
    }
    $RollbackTag = $Image + ':rollback-' + $RunningId.Substring(7,12)

    if ($Action -eq 'stage') {
        if (Test-Path -LiteralPath $ReceiptPath -PathType Leaf) {
            Remove-Item -LiteralPath $ReceiptPath -Force
        }
        $Ids = Invoke-McpDocker -Arguments @('image','ls','--all','--no-trunc','--format','{{.ID}}')
        if (@($Ids -split '[\r\n]+' | Where-Object { $_ }) -cnotcontains $RunningId) {
            throw "Previous API image $Service unavailable for rollback."
        }
        $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$RollbackTag)
        if ((Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)) -cne $RunningId) {
            throw 'Rollback tag preservation failed.'
        }
        try {
            # Build only one API image. Never run compose up here.
            $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('build',$Service)) -TimeoutSeconds 1500
            $Candidate = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)
            if ($Candidate -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Invalid API candidate image ID.' }
            $null = Invoke-McpDocker -Arguments @('scout','cves','--only-severity','critical,high','--only-fixed','--exit-code',('local://' + $ImageTag)) -TimeoutSeconds 900
            if ((Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)) -cne $Candidate) {
                throw 'Candidate changed during CVE scan.'
            }
            $Receipt = [ordered]@{
                schema_version=1; service=$Service; image_id=$Candidate; rollback_id=$RunningId
                scan='scout_fixable_critical_high_zero'; source_hashes=$Hashes
                created_utc=[DateTimeOffset]::UtcNow.ToString('o')
            }
            $Tmp = $ReceiptPath + '.tmp'
            try {
                [IO.File]::WriteAllText($Tmp,($Receipt | ConvertTo-Json -Depth 9),[Text.UTF8Encoding]::new($false))
                Move-Item -LiteralPath $Tmp -Destination $ReceiptPath -Force
            } finally {
                # Host maintenance dispatch rejects any recorded $Error, even
                # nonterminating expected errors from SilentlyContinue. Check
                # existence explicitly and fail if actual deletion fails.
                if (Test-Path -LiteralPath $Tmp -PathType Leaf) {
                    Remove-Item -LiteralPath $Tmp -Force -ErrorAction Stop
                }
            }
            "API_STAGED $Service $Candidate; running container unchanged."
        }
        catch {
            $Reason = $_.Exception.Message
            try { $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$ImageTag) }
            catch { throw "Stage failed, rollback tag restore also failed: $Reason" }
            throw "API stage rejected; live container untouched: $Reason"
        }
        return
    }

    if (-not (Test-Path -LiteralPath $ReceiptPath -PathType Leaf)) {
        throw "Missing reviewed API stage receipt for $Service."
    }
    $Receipt = Get-Content -LiteralPath $ReceiptPath -Raw | ConvertFrom-Json
    if ([int]$Receipt.schema_version -ne 1 -or [string]$Receipt.service -cne $Service -or
        [string]$Receipt.scan -cne 'scout_fixable_critical_high_zero' -or
        [string]$Receipt.image_id -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        [string]$Receipt.rollback_id -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Invalid API stage receipt.'
    }
    $Age = [DateTimeOffset]::UtcNow - [DateTimeOffset]::Parse([string]$Receipt.created_utc)
    if ($Age.TotalSeconds -lt 0 -or $Age.TotalHours -gt 24) { throw 'API stage receipt expired.' }
    foreach ($Name in $Hashes.Keys) {
        $Property = $Receipt.source_hashes.PSObject.Properties[$Name]
        if ($null -eq $Property -or [string]$Property.Value -cne [string]$Hashes[$Name]) {
            throw "API staged source hash mismatch: $Name"
        }
    }
    $CandidateId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)
    $RollbackId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)
    if ($CandidateId -cne [string]$Receipt.image_id -or $RollbackId -cne $RunningId -or
        $RunningId -cne [string]$Receipt.rollback_id) {
        throw 'Image identity changed since API stage.'
    }
    $null = Invoke-McpDocker -Arguments @('scout','cves','--only-severity','critical,high','--only-fixed','--exit-code',('local://' + $ImageTag)) -TimeoutSeconds 900
    $Changed = $false
    try {
        $Changed = $true
        $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate',$Service))
        Start-Sleep -Seconds 5
        if ((Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)) -cne $CandidateId -or
            (Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}',$Container)) -cne 'true') {
            throw 'API candidate did not start with expected image.'
        }
        # The pinned upstream API exposes an unauthenticated liveness probe.
        # Check the API itself, not only Docker's container-running bit.
        Start-Sleep -Seconds 15
        $Http = [Net.Http.HttpClient]::new()
        try {
            $Http.Timeout = [TimeSpan]::FromSeconds(10)
            $Response = $Http.GetAsync('http://127.0.0.1:3002/v0/health/liveness').GetAwaiter().GetResult()
            try {
                if ([int]$Response.StatusCode -ne 200) { throw 'API liveness HTTP probe failed.' }
            } finally { $Response.Dispose() }
        } finally { $Http.Dispose() }
        Remove-Item -LiteralPath $ReceiptPath -Force
        "API_DEPLOYED $Service $CandidateId rollback=$RollbackTag"
    } catch {
        $Reason = $_.Exception.Message
        if ($Changed) {
            try {
                $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$ImageTag)
                $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate',$Service))
                if ((Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)) -cne $RunningId) {
                    throw 'Rollback image identity verification failed.'
                }
            } catch { throw "API deployment and rollback failed for $Service; original: $Reason" }
            throw "API candidate rejected; old image restored for $Service; original: $Reason"
        }
        throw
    }
}
