# Exact-image, fresh-CVE-gated Firecrawl MCP staging; no other services targeted.
function Invoke-McpDocker {
    param([Parameter(Mandatory)][string[]]$Arguments, [int]$TimeoutSeconds = 1800)
    $DockerExe = (Get-Command docker.exe -ErrorAction Stop).Source
    # Native Docker progress must not pollute the host runner's strict $Error gate.
    $ArgLine = ($Arguments | ForEach-Object {
        $Arg = [string]$_
        if ($Arg -match '[\s"]') { '"' + $Arg.Replace('"', '\"') + '"' } else { $Arg }
    }) -join ' '
    $LogFolder = Join-Path $env:TEMP 'firecrawl-stage-logs'
    if (-not (Test-Path -LiteralPath $LogFolder)) {
        New-Item -ItemType Directory -Path $LogFolder -Force | Out-Null
    }
    $Token = [guid]::NewGuid().ToString('N')
    $OutFile = Join-Path $LogFolder "$Token.out"
    $ErrFile = Join-Path $LogFolder "$Token.err"
    $Process = $null
    try {
        $Process = Start-Process -FilePath $DockerExe -ArgumentList $ArgLine -NoNewWindow -PassThru -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile -ErrorAction Stop
        if (-not $Process.WaitForExit($TimeoutSeconds * 1000)) {
            $Process.Kill($true)
            throw 'Reviewed Docker operation timed out.'
        }
        $Code = [int]$Process.ExitCode
        if ($Code -ne 0) {
            # Report only the reviewed Docker operation class and a bounded,
            # sanitized diagnostic. Never include arguments/env/secret values.
            $Kind = if ($Arguments[0] -eq 'scout') { 'scout-cves' }
                elseif ($Arguments[0] -eq 'inspect') { 'inspect-container' }
                elseif ($Arguments[0] -eq 'image' -and $Arguments[1] -eq 'tag') { 'tag-rollback-image' }
                elseif ($Arguments[0] -eq 'image') { 'inspect-image' }
                elseif ($Arguments[0] -eq 'compose' -and $Arguments -contains 'build') { 'compose-build-mcp' }
                elseif ($Arguments[0] -eq 'compose' -and $Arguments -contains 'up') { 'compose-redeploy-mcp' }
                else { 'reviewed-docker-command' }
            throw "Reviewed Docker operation $Kind failed with exit code $Code. See local private Docker diagnostics."
        }
        # Successful Docker Compose commands may write nothing to stdout.
        # Get-Content -Raw returns $null for an empty file, so never call
        # .Trim() before checking for a value.
        $Text = ''
        if (Test-Path -LiteralPath $OutFile) {
            $RawText = Get-Content -LiteralPath $OutFile -Raw -ErrorAction Stop
            if ($null -ne $RawText) { $Text = ([string]$RawText).Trim() }
        }
        return $Text
    }
    finally {
        if ($Process) { $Process.Dispose() }
        foreach ($Path in @($OutFile, $ErrFile)) {
            if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Force -ErrorAction Stop }
        }
    }
}

function Get-McpSourceHashes {
    param([Parameter(Mandatory)][string]$ProjectRoot)
    $Files = @(
        'fc.ps1', 'security/staged-mcp-image.ps1', 'upstream/firecrawl.json',
        'compose.local.yaml', 'compose.public.yaml', 'public/Dockerfile.mcp',
        'public/package.json', 'public/package-lock.json'
    )
    $Hashes = [ordered]@{}
    foreach ($File in $Files) {
        $FullPath = Join-Path $ProjectRoot $File
        if (-not (Test-Path -LiteralPath $FullPath -PathType Leaf)) {
            throw "Reviewed MCP source file missing: $File"
        }
        $Hashes[$File] = (Get-FileHash -LiteralPath $FullPath -Algorithm SHA256 -ErrorAction Stop).Hash
    }
    return $Hashes
}

function Get-McpImageId {
    # The fixed Compose image tag, never caller input.
    $Id = Invoke-McpDocker -Arguments @('image', 'inspect', '--format', '{{.Id}}', 'firecrawl-mcp:latest')
    if ($Id -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Invalid staged image ID.' }
    return $Id
}

function Invoke-McpStageSecurityCheck {
    param(
        [Parameter(Mandatory)][ValidateSet('build','deploy')][string]$Action,
        [Parameter(Mandatory)][string[]]$ComposeArguments,
        [Parameter(Mandatory)][string]$ProjectRoot
    )
    $StageDir = Join-Path $env:LOCALAPPDATA 'FirecrawlLocal\staging'
    $ReceiptPath = Join-Path $StageDir 'mcp-stage.json'
    if (-not (Test-Path -LiteralPath $StageDir)) {
        New-Item -ItemType Directory -Path $StageDir -Force | Out-Null
    }

    if ($Action -eq 'build') {
        if (Test-Path -LiteralPath $ReceiptPath) {
            Remove-Item -LiteralPath $ReceiptPath -Force -ErrorAction Stop
        }
        # Retain a fast rollback when Docker still has the old image.
        # The running container may outlive its image store entry. In that case
        # Git history is the recovery source and build must still be possible.
        $PreviousId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}','firecrawl-mcp-1')
        if ($PreviousId -cnotmatch '^sha256:[0-9a-f]{64}$') { throw 'Running MCP image identity unavailable.' }
        $RollbackTag = 'firecrawl-mcp:rollback-' + $PreviousId.Substring(7,12)
        $RollbackAvailable = $false
        # An expected "image not found" exception leaves an ErrorRecord in
        # PowerShell's $Error history, triggering the host runner's strict gate.
        # Query available IDs instead; missing images cause no errors.
        $ImageIdsText = Invoke-McpDocker -Arguments @('image','ls','--all','--no-trunc','--format','{{.ID}}')
        $AvailableIds = @($ImageIdsText -split '[\r\n]+' | Where-Object { $_ })
        $OldImagePresent = $AvailableIds -ccontains $PreviousId
        if ($OldImagePresent) {
            $OldImage = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$PreviousId)
            if ($OldImage -cne $PreviousId) { throw 'Old MCP image ID does not match running container.' }
        }
        if ($OldImagePresent) {
            $null = Invoke-McpDocker -Arguments @('image','tag',$PreviousId,$RollbackTag)
            $PreservedId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)
            if ($PreservedId -cne $PreviousId) { throw 'Rollback image preservation failed before build.' }
            $RollbackAvailable = $true
        }
        $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('build', 'mcp'))
        $Id = Get-McpImageId
        # Require 0 high/critical CVEs, including ones without a patch.
        $null = Invoke-McpDocker -Arguments @('scout','cves','--only-severity','critical,high','--exit-code','local://firecrawl-mcp:latest') -TimeoutSeconds 900
        if ((Get-McpImageId) -cne $Id) { throw 'Image changed during Docker Scout scan.' }
        $Receipt = [ordered]@{
            schema_version = 1
            image_id = $Id
            rollback_image_id = $PreviousId
            rollback_available = $RollbackAvailable
            scan = 'scout_cves_critical_high_zero'
            created_utc = [DateTimeOffset]::UtcNow.ToString('o')
            source_hashes = Get-McpSourceHashes -ProjectRoot $ProjectRoot
        }
        $Tmp = "$ReceiptPath.tmp"
        try {
            [IO.File]::WriteAllText($Tmp, ($Receipt | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
            Move-Item -LiteralPath $Tmp -Destination $ReceiptPath -Force -ErrorAction Stop
        }
        finally {
            if (Test-Path -LiteralPath $Tmp) { Remove-Item -LiteralPath $Tmp -Force -ErrorAction Stop }
        }
        "MCP image staged and scanned: $Id. No service restarted."
        return
    }

    if (-not (Test-Path -LiteralPath $ReceiptPath -PathType Leaf)) {
        throw 'Missing successful MCP image scan receipt. Run build_only first.'
    }
    $Receipt = Get-Content -LiteralPath $ReceiptPath -Raw -ErrorAction Stop | ConvertFrom-Json
    if ($Receipt.schema_version -ne 1 -or
        $Receipt.scan -cne 'scout_cves_critical_high_zero' -or
        [string]$Receipt.image_id -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        [string]$Receipt.rollback_image_id -cnotmatch '^sha256:[0-9a-f]{64}$' -or
        $Receipt.rollback_available -isnot [bool]) {
        throw 'Staged MCP image receipt is invalid.'
    }
    $Age = [DateTimeOffset]::UtcNow - [DateTimeOffset]::Parse([string]$Receipt.created_utc)
    if ($Age.TotalSeconds -lt 0 -or $Age.TotalHours -gt 24) {
        throw 'Staged MCP scan is older than 24 hours; rebuild and rescan.'
    }
    $Hashes = Get-McpSourceHashes -ProjectRoot $ProjectRoot
    foreach ($Name in $Hashes.Keys) {
        if ($null -eq $Receipt.source_hashes.PSObject.Properties[$Name] -or
            [string]$Receipt.source_hashes.PSObject.Properties[$Name].Value -cne [string]$Hashes[$Name]) {
            throw "MCP source changed after scan: $Name"
        }
    }
    $Id = Get-McpImageId
    if ($Id -cne [string]$Receipt.image_id) { throw 'Staged image identity mismatch.' }
    $null = Invoke-McpDocker -Arguments @('scout','cves','--only-severity','critical,high','--exit-code','local://firecrawl-mcp:latest') -TimeoutSeconds 900
    if ((Get-McpImageId) -cne $Id) { throw 'Image changed after final CVE scan.' }

    $PreviousId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}','firecrawl-mcp-1')
    if ($PreviousId -cnotmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Current MCP container image identity missing.'
    }
    if ($PreviousId -ceq $Id) {
        Remove-Item -LiteralPath $ReceiptPath -Force -ErrorAction Stop
        "MCP already runs scanned image $Id. No deploy needed."
        return
    }
    if ($PreviousId -cne [string]$Receipt.rollback_image_id) {
        throw 'Running MCP image changed since staging.'
    }
    $RollbackTag = 'firecrawl-mcp:rollback-' + $PreviousId.Substring(7,12)
    if ($Receipt.rollback_available) {
        $StoredId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$RollbackTag)
        if ($StoredId -cne $PreviousId) { throw 'Preserved rollback image is missing or changed.' }
    }
    $Changed = $false
    try {
        $Changed = $true
        $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate','mcp'))
        Start-Sleep -Seconds 5
        $Running = Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}','firecrawl-mcp-1')
        $ActiveId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}','firecrawl-mcp-1')
        if ($Running -cne 'true' -or $ActiveId -cne $Id) {
            throw 'MCP candidate failed post-deployment verification.'
        }
        Remove-Item -LiteralPath $ReceiptPath -Force -ErrorAction Stop
        if ($Receipt.rollback_available) {
            "MCP deployed $Id; original image retained as $RollbackTag."
        }
        else {
            "MCP deployed $Id; original image unavailable, Git history is the recovery source."
        }
    }
    catch {
        if ($Changed) {
            $FailureMessage = $_.Exception.Message
            if (-not $Receipt.rollback_available) {
                throw "MCP deploy failed; original image unavailable. Rebuild from pinned Git source for recovery. Cause: $FailureMessage"
            }
            try {
                $null = Invoke-McpDocker -Arguments @('image','tag',$PreviousId,'firecrawl-mcp:latest')
                $null = Invoke-McpDocker -Arguments (@($ComposeArguments) + @('up','-d','--no-deps','--no-build','--force-recreate','mcp'))
                $Running = Invoke-McpDocker -Arguments @('inspect','--format','{{.State.Running}}','firecrawl-mcp-1')
                $ActiveId = Invoke-McpDocker -Arguments @('inspect','--format','{{.Image}}','firecrawl-mcp-1')
                if ($Running -cne 'true' -or $ActiveId -cne $PreviousId) {
                    throw 'Rollback verification failed.'
                }
            }
            catch {
                throw "MCP deploy failed and rollback also failed. Original cause: $FailureMessage"
            }
            throw "MCP deploy failed; previous MCP image restored. Cause: $FailureMessage"
        }
        throw
    }
}