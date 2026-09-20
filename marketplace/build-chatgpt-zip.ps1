# Builds the ChatGPT-web variant of the firecrawl-mcp plugin as a zip for @plugin-creator.
# The local plugin (Codex/Claude Code) is skills-only and uses a local Firecrawl instance; the ChatGPT variant instead
# references the ChatGPT developer app (which reaches the PC through Cloudflare).
# Usage: .\build-chatgpt-zip.ps1 [-AppId asdk_app_...] [-PluginName local-firecrawl] [-DisplayName 'Local-Firecrawl']
#                                [-OutDir <output-directory>]
#        .\build-chatgpt-zip.ps1 -SkillsOnly   (no app reference; let ChatGPT's @plugin-creator wire the app itself)
param(
    [switch]$SkillsOnly,
    [string]$AppId,
    [string]$PluginName = 'local-firecrawl',
    [string]$DisplayName = 'Local-Firecrawl',
    [string]$OutDir = (Join-Path $env:USERPROFILE 'Downloads')
)
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot 'plugins\firecrawl-mcp'
$overlay = Join-Path $PSScriptRoot 'chatgpt-overlay\.app.json'
if (-not $SkillsOnly) {
    if (-not $AppId) {
        if (-not (Test-Path $overlay)) { throw 'Provide -AppId or copy chatgpt-overlay/.app.json.example to .app.json and fill in your app id' }
        $AppId = @((Get-Content $overlay -Raw | ConvertFrom-Json).apps.PSObject.Properties.Value)[0].id
    }
    if ($AppId -notmatch '^asdk_app_[0-9a-f]+$') { throw "Unexpected app id: $AppId" }
}
if ($PluginName -notmatch '^[a-z0-9]+(-[a-z0-9]+)*$') { throw "Plugin name must be kebab-case: $PluginName" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) "firecrawl-chatgpt-$([guid]::NewGuid().ToString('N'))"
$stage = Join-Path $tmp $PluginName
try {
    Copy-Item $src $stage -Recurse
    foreach ($local in '.mcp.json', 'codex.mcp.json', '.claude-plugin') {
        $path = Join-Path $stage $local
        if (Test-Path $path) { Remove-Item $path -Recurse -Force }
    }
    if (-not $SkillsOnly) {
        @{ apps = @{ $PluginName = @{ id = $AppId } } } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $stage '.app.json')
    }

    $codex = Join-Path $stage '.codex-plugin\plugin.json'
    $c = Get-Content $codex -Raw | ConvertFrom-Json
    $c.PSObject.Properties.Remove('mcpServers')
    $c.name = $PluginName
    $c.interface.displayName = $DisplayName
    if (-not $SkillsOnly) { $c | Add-Member -NotePropertyName apps -NotePropertyValue './.app.json' -Force }
    $c.interface.longDescription = $c.interface.longDescription -replace ' directly on this PC', ' (reached through a Cloudflare-protected endpoint)'
    $c | ConvertTo-Json -Depth 10 | Set-Content $codex

    $portable = Join-Path $stage 'plugin.json'
    $p = Get-Content $portable -Raw | ConvertFrom-Json
    $p.name = $PluginName
    $p.extensions.'com.openai'.interface.displayName = $DisplayName
    if (-not $SkillsOnly) { $p.extensions.'com.openai' | Add-Member -NotePropertyName apps -NotePropertyValue './.app.json' -Force }
    $p | ConvertTo-Json -Depth 10 | Set-Content $portable

    $suffix = if ($SkillsOnly) { 'skills-only' } else { 'chatgpt' }
    $zip = Join-Path $OutDir "$PluginName-plugin-$($c.version)-$suffix.zip"
    # Windows PowerShell's Compress-Archive writes backslash entry names, which non-Windows unzip tools
    # treat as flat file names. The bundled bsdtar writes standard forward-slash zip entries.
    if (Test-Path $zip) { Remove-Item $zip -Force }
    & "$env:SystemRoot\System32\tar.exe" -a -c -f $zip -C $tmp $PluginName
    if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
    $appNote = if ($SkillsOnly) { 'no app reference' } else { "app $AppId" }
    "built $zip (plugin $PluginName, $appNote)"
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
