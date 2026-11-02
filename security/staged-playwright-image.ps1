# Fixed-service staging: never touches API, Redis, RabbitMQ, Postgres or other data services.
function Get-PlaywrightStageHashes {
    param([string]$ProjectRoot)
    $Files = @('fc.ps1','security/staged-mcp-image.ps1','security/staged-playwright-image.ps1',
        'security/Dockerfile.playwright-security','compose.local.yaml','compose.public.yaml','upstream/firecrawl.json')
    $Hashes = [ordered]@{}
    foreach ($File in $Files) {
        $Path = Join-Path $ProjectRoot $File
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing reviewed Playwright source: $File" }
        $Hashes[$File] = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    }
    return $Hashes
}
function Invoke-PlaywrightStage {
    param(
        [Parameter(Mandatory)][ValidateSet('stage','deploy')][string]$Action,
        [Parameter(Mandatory)][ValidateSet('playwright-service','browser-session-service','playwright-public-proxy-service')][string]$Service,
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $Image = 'firecrawl-' + $Service
    $ImageTag = $Image + ':latest'
    $Container = 'firecrawl-' + $Service + '-1'
    $Folder = Join-Path $env:LOCALAPPDATA 'FirecrawlLocal\staging'
    if (-not (Test-Path -LiteralPath $Folder -PathType Container)) {
        New-Item -ItemType Directory -Path $Folder -Force | Out-Null
    }
    $ReceiptPath = Join-Path $Folder ('playwright-' + $Service + '-stage.json')
    $Hashes = Get-PlaywrightStageHashes -ProjectRoot $ProjectRoot
    $RunningId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)
    $Running = Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}',$Container)
    if ($RunningId -cnotmatch '^sha256:[0-9a-f]{64}$' -or $Running -cne 'true') {
        throw "Playwright service $Service baseline unavailable."
    }
    $RollbackTag = $Image + ':rollback-' + $RunningId.Substring(7,12)

    if ($Action -eq 'stage') {
        if (Test-Path -LiteralPath $ReceiptPath -PathType Leaf) {
            Remove-Item -LiteralPath $ReceiptPath -Force
        }
        $Ids = Invoke-McpDocker -Arguments @('image','ls','--all','--no-trunc','--format','{{.ID}}')
        if (@($Ids -split '[\r\n]+' | Where-Object { $_ }) -cnotcontains $RunningId) {
            throw "Previous Playwright image $Service unavailable for rollback."
        }
        $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$RollbackTag)
        if ((Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)) -cne $RunningId) {
            throw 'Rollback tag preservation failed.'
        }
        try {
            # Build only one Playwright image. Never run compose up here.
            $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('build',$Service)) -TimeoutSeconds 1500
            $Candidate = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)
            if ($Candidate -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Invalid Playwright candidate image ID.' }
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
                Remove-Item -LiteralPath $Tmp -Force -ErrorAction SilentlyContinue
            }
            "PLAYWRIGHT_STAGED $Service $Candidate; running container unchanged."
        }
        catch {
            $Reason = $_.Exception.Message
            try { $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$ImageTag) }
            catch { throw "Stage failed, rollback tag restore also failed: $Reason" }
            throw "Playwright stage rejected; live container untouched: $Reason"
        }
        return
    }

    if (-not (Test-Path -LiteralPath $ReceiptPath -PathType Leaf)) {
        throw "Missing reviewed Playwright stage receipt for $Service."
    }
    $Receipt = Get-Content -LiteralPath $ReceiptPath -Raw | ConvertFrom-Json
    if ([int]$Receipt.schema_version -ne 1 -or [string]$Receipt.service -cne $Service -or
        [string]$Receipt.scan -cne 'scout_fixable_critical_high_zero' -or
        [string]$Receipt.image_id -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        [string]$Receipt.rollback_id -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Invalid Playwright stage receipt.'
    }
    $Age = [DateTimeOffset]::UtcNow - [DateTimeOffset]::Parse([string]$Receipt.created_utc)
    if ($Age.TotalSeconds -lt 0 -or $Age.TotalHours -gt 24) { throw 'Playwright stage receipt expired.' }
    foreach ($Name in $Hashes.Keys) {
        $Property = $Receipt.source_hashes.PSObject.Properties[$Name]
        if ($null -eq $Property -or [string]$Property.Value -cne [string]$Hashes[$Name]) {
            throw "Playwright staged source hash mismatch: $Name"
        }
    }
    $CandidateId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)
    $RollbackId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)
    if ($CandidateId -cne [string]$Receipt.image_id -or $RollbackId -cne $RunningId -or
        $RunningId -cne [string]$Receipt.rollback_id) {
        throw 'Image identity changed since Playwright stage.'
    }
    $null = Invoke-McpDocker -Arguments @('scout','cves','--only-severity','critical,high','--only-fixed','--exit-code',('local://' + $ImageTag)) -TimeoutSeconds 900
    $Changed = $false
    try {
        $Changed = $true
        $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate',$Service))
        Start-Sleep -Seconds 5
        if ((Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)) -cne $CandidateId -or
            (Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}',$Container)) -cne 'true') {
            throw 'Playwright candidate did not start with expected image.'
        }
        if ($Service -eq 'playwright-public-proxy-service') {
            Import-PublicProxyRuntimeSecrets
            Invoke-PublicProxySmokeTest
        }
        Remove-Item -LiteralPath $ReceiptPath -Force
        "PLAYWRIGHT_DEPLOYED $Service $CandidateId rollback=$RollbackTag"
    } catch {
        $Reason = $_.Exception.Message
        if ($Changed) {
            try {
                $null = Invoke-McpDocker -Arguments @('image','tag',$RunningId,$ImageTag)
                $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate',$Service))
                if ((Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}',$Container)) -cne $RunningId) {
                    throw 'Rollback image identity verification failed.'
                }
                if ($Service -eq 'playwright-public-proxy-service') { Import-PublicProxyRuntimeSecrets }
            } catch { throw "Playwright deployment and rollback failed for $Service; original: $Reason" }
            throw "Playwright candidate rejected; old image restored for $Service; original: $Reason"
        }
        throw
    }
}
