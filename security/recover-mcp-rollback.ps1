# Deterministic one-off recovery path for the 2026-10-06 known-working public MCP.
# Never reads, exports, commits, restarts or attaches to the running container.
# All Git objects and the historical Node base must match the reviewed pins.
function Invoke-McpRollbackGit {
    param(
        [Parameter(Mandatory)][string]$ProjectRoot,
        [Parameter(Mandatory)][string[]]$Arguments,
        [string]$Destination
    )
    $GitExe = (Get-Command git.exe -ErrorAction Stop).Source
    $Start = [System.Diagnostics.ProcessStartInfo]::new()
    $Start.FileName = $GitExe
    $Start.WorkingDirectory = $ProjectRoot
    $Start.UseShellExecute = $false
    $Start.RedirectStandardOutput = $true
    $Start.RedirectStandardError = $true
    foreach ($Argument in $Arguments) { [void]$Start.ArgumentList.Add($Argument) }
    $Process = [System.Diagnostics.Process]::new()
    $Process.StartInfo = $Start
    try {
        if (-not $Process.Start()) { throw 'Reviewed Git operation could not start.' }
        if ($Destination) {
            $OutputFile = [System.IO.File]::Open($Destination, [System.IO.FileMode]::CreateNew)
            try { $Process.StandardOutput.BaseStream.CopyTo($OutputFile) }
            finally { $OutputFile.Dispose() }
            $Output = ''
        }
        else {
            $Output = $Process.StandardOutput.ReadToEnd().Trim()
        }
        $ErrorText = $Process.StandardError.ReadToEnd()
        if (-not $Process.WaitForExit(30000)) {
            $Process.Kill($true)
            throw 'Reviewed Git operation timed out.'
        }
        if ($Process.ExitCode -ne 0 -or $ErrorText) {
            throw 'Reviewed Git source verification failed.'
        }
        return $Output
    }
    finally { $Process.Dispose() }
}

function Restore-McpRollbackFromReviewedHistory {
    param([Parameter(Mandatory)][string]$ProjectRoot)
    $Commit = '75078a0b7a15188d5121fead370dc16409f33a92'
    $BaseImageId = 'sha256:0b36e8c136b94cd4fcf02188228e76c31ad5872eef66b78deb'
    # The base digest is from the audited running host's node:26.10.0-alpine.
    # Exact Git blob SHA-1s are verified twice: in the commit tree and after export.
    $Reviewed = [ordered]@{
        'public/Dockerfile.mcp' = '3238b865bc3f68e99c385c14b4848a5e2f289454'
        'public/package.json' = '409b7cf2f1985cef943a4b90978ad35368c3c534'
        'public/package-lock.json' = 'd3a43ef4d3567bb7192099564711500f950139a2'
    }
    $Origin = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('remote','get-url','origin')
    if ($Origin -cnotin @(
        'https://github.com/LurigeLars/firecrawl-local-mcp',
        'https://github.com/LurigeLars/firecrawl-local-mcp.git'
    )) { throw 'Historical rollback Git origin does not match trusted repository.' }
    $Branch = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('branch','--show-current')
    $Dirty = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('status','--porcelain','--untracked-files=no')
    if ($Branch -cne 'main' -or $Dirty) { throw 'Historical rollback requires clean main.' }
    $VerifiedCommit = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('rev-parse','--verify',($Commit + '^{commit}'))
    if ($VerifiedCommit -cne $Commit) { throw 'Historical rollback commit is absent.' }
    $Base = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}','node:26.10.0-alpine')
    if ($Base -cne $BaseImageId) { throw 'Reviewed historical Node base image changed.' }

    $Folder = Join-Path $env:LOCALAPPDATA ('FirecrawlLocal\staging\historical-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Folder -Force -ErrorAction Stop | Out-Null
    try {
        foreach ($Path in $Reviewed.Keys) {
            $Blob = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('rev-parse',($Commit + ':' + $Path))
            if ($Blob -cne [string]$Reviewed[$Path]) {
                throw 'Historical rollback source blob differs from reviewed Git pin.'
            }
            $Name = [IO.Path]::GetFileName($Path)
            $Dest = Join-Path $Folder $Name
            $null = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('cat-file','blob',$Blob) -Destination $Dest
            $OnDiskBlob = Invoke-McpRollbackGit -ProjectRoot $ProjectRoot -Arguments @('hash-object',$Dest)
            if ($OnDiskBlob -cne $Blob) {
                throw 'Historical rollback file failed independent hash-object verification.'
            }
        }
        $ImageTag = 'firecrawl-mcp:rollback-reviewed-20261006'
        $null = Invoke-McpDocker -Arguments @(
            'build','--pull=false','--no-cache=false','-t',$ImageTag,
            '-f',(Join-Path $Folder 'Dockerfile.mcp'),$Folder
        ) -TimeoutSeconds 1800
        $ResultId = Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)
        if ($ResultId -cnotmatch '^sha256:[0-9a-f]{64}$') {
            throw 'Historical fallback image is not available after build.'
        }
        # A fixed, unprivileged, network-isolated smoke test checks the exact
        # historical MCP package version; no credentials or service ports.
        $NodeProbe = "const p=JSON.parse(require('fs').readFileSync('/app/node_modules/firecrawl-mcp/package.json','utf8'));if(p.version!=='3.27.3')process.exit(42);"
        $null = Invoke-McpDocker -Arguments @(
            'run','--rm','--network','none','--read-only',
            '--cap-drop','ALL','--security-opt','no-new-privileges',
            '--pids-limit','64','--memory','256m','--entrypoint','node',
            $ImageTag,'-e',$NodeProbe
        ) -TimeoutSeconds 120
        if ((Invoke-McpDocker -Arguments @('image','inspect','--format','{{.Id}}',$ImageTag)) -cne $ResultId) {
            throw 'Historical rollback image changed after runtime smoke test.'
        }
        return $ResultId
    }
    finally {
        if (Test-Path -LiteralPath $Folder) {
            Remove-Item -LiteralPath $Folder -Recurse -Force -ErrorAction Stop
        }
    }
}
