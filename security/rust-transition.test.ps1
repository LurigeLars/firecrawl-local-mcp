# Synthetic boundaries. No real Docker, credentials, or production state.
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot '../research/rust-transition.ps1')
$script:mockId='sha256:'+('a'*64)
$script:mockDigest='sha256:'+('b'*64)
function Invoke-McpDocker {
    param([string[]]$Arguments,[int]$TimeoutSeconds=1800)
    if ($Arguments[0] -ceq 'image' -and $Arguments[1] -ceq 'inspect') {
        if ($Arguments[3] -ceq '{{.Id}}|{{json .RepoDigests}}') { return "$script:mockId|[""firecrawl-rust-canary-rust-api@$script:mockDigest""]" }
        return $script:mockId
    }
    if ($Arguments[0] -ceq 'run') {
        if ($Arguments -cnotcontains 'type=volume,source=firecrawl_rust-jobs,target=/state,readonly' -or $Arguments -cnotcontains 'none' -or $Arguments -cnotcontains '--read-only') { throw 'Unsafe queue inspection.' }
        return '0'
    }
    throw 'Unexpected synthetic Docker operation.'
}
if ((Get-AcceptedCandidateImage 'firecrawl-rust-canary-rust-api:latest' $script:mockDigest) -cne $script:mockId) { throw 'Accepted digest not resolved.' }
try { Get-AcceptedCandidateImage 'firecrawl-rust-canary-rust-api:latest' ('sha256:'+('c'*64));throw 'Wrong digest accepted.' }
catch { if ($_.Exception.Message -cne 'Candidate differs from accepted image.') { throw } }
if ((Get-RustStoppedQueueCount) -ne 0) { throw 'Queue fixture failed.' }
$source=Get-Content -LiteralPath (Join-Path $PSScriptRoot '../research/rust-transition.ps1') -Raw
$promotion=$source.Substring($source.IndexOf('function Invoke-RustPromotion'))
$stop=$promotion.IndexOf('Stop-TransitionService api')
$drain=$promotion.IndexOf('$queue=Get-LegacyQueueReceipt',$stop)
$start=$promotion.IndexOf("'pdf-ocr','api'",$drain)
$client=$promotion.IndexOf('verify-rust-client.mjs',$start)
$model=$promotion.IndexOf('verify-live-model.mjs',$client)
$reopen=$promotion.IndexOf('Start-OriginalTransitionService -Service gateway',$model)
if ($stop -lt 0 -or $drain -le $stop -or $start -le $drain -or $client -le $start -or $model -le $client -or $reopen -le $model) { throw 'Unsafe promotion ordering.' }
$rollback=$source.Substring($source.IndexOf('function Restore-LegacyTransition'),$source.IndexOf('function Invoke-RustRollback')-$source.IndexOf('function Restore-LegacyTransition'))
if ($rollback.IndexOf('Get-RustStoppedQueueCount') -gt $rollback.IndexOf("Stop-TransitionService pdf-ocr") -or
    $rollback.IndexOf("Invoke-TransitionMcpProbe") -gt $rollback.IndexOf("Remove-Item -LiteralPath (Get-TransitionPath 'rust-transition.lock')")) { throw 'Unsafe rollback ordering.' }
if ($source -match '--remove-orphans|--volumes|volume\s+rm|compose.+down') { throw 'Destructive transition command.' }
'RUST_TRANSITION_BOUNDARY_FIXTURE_PASS'

# Exercise the actual rollback orchestration against synthetic container
# identities. No production Docker, local user data, credential or filesystem.
$root=Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString('N'))
$null=New-Item -ItemType Directory -Path $root
$script:events=[Collections.Generic.List[string]]::new()
$script:states=@{}
$script:tags=@{}
$script:jobs=1
$script:failInspection=$false
$legacy=[ordered]@{}
$index=1
foreach ($service in @('api','mcp','redis','rabbitmq','nuq-postgres','gateway')) {
    $hex=$index.ToString('x')
    $legacy[$service]=@{id=($hex*64);image=('sha256:'+($hex*64));running=$true}
    $script:states[$service]=$legacy[$service].Clone()
    $index++
}
$candidate=@{api=('sha256:'+('a'*64));mcp=('sha256:'+('b'*64));'pdf-ocr'=('sha256:'+('c'*64))}
$journal=@{schema=1;revision=('e'*40);legacy=$legacy;candidate=$candidate}
function Get-TransitionPath([string]$Name) { Join-Path $root $Name }
function Get-TransitionService([string]$Service) { return $script:states[$Service] }
function Stop-TransitionService([string]$Service) {
    $script:events.Add("stop:$Service")
    if ($null -ne $script:states[$Service]) { $script:states[$Service].running=$false }
}
function Start-OriginalTransitionService([string]$Service,[string]$Id,[string]$Image) {
    $state=$script:states[$Service]
    if ($state.id -cne $Id -or $state.image -cne $Image) { throw 'Synthetic identity mismatch.' }
    $script:events.Add("original:$Service")
    $state.running=$true
}
function Get-TransitionImageId([string]$Image) { return $script:tags[$Image] }
function Set-TransitionTag([string]$Id,[string]$Tag) { $script:events.Add("tag:$Tag");$script:tags[$Tag]=$Id }
function Get-RustStoppedQueueCount {
    $script:events.Add('rust-queue-count')
    if ($script:failInspection) { throw 'Synthetic unavailable count.' }
    return $script:jobs
}
function Import-ServiceRuntimeSecrets { $script:events.Add('restore-runtime') }
function Test-PostgresRuntimePassword { $script:events.Add('pg-auth');return $true }
function Wait-LegacyTransitionHealth { $script:events.Add('legacy-health') }
function Invoke-TransitionMcpProbe([string]$ProjectRoot,[string]$Backend,[string]$Mode) {
    if ($Backend -cne 'legacy') { throw 'Unexpected backend.' }
    $script:events.Add('legacy-mcp')
}
function Invoke-McpDocker {
    param([string[]]$Arguments,[int]$TimeoutSeconds=1800)
    if ($Arguments[0] -ceq 'start') {
        $found=@($script:states.Keys | Where-Object {$script:states[$_].id -ceq $Arguments[1]})
        if ($found.Count -ne 1) { throw 'Unexpected restart.' }
        $script:events.Add("resume:$($found[0])")
        $script:states[$found[0]].running=$true
        return ''
    }
    if ($Arguments[0] -ceq 'compose' -and $Arguments -ccontains '--no-build' -and $Arguments -ccontains '--no-deps' -and $Arguments -ccontains '--force-recreate') {
        $service=$Arguments[-1]
        if ($service -cnotin @('api','mcp')) { throw 'Unreviewed recreate.' }
        $script:events.Add("legacy-recreate:$service")
        $script:states[$service]=@{id=('d'*64);image=$legacy[$service].image;running=$true}
        return ''
    }
    throw 'Unexpected rollback Docker invocation.'
}
try {
    foreach ($service in @('api','mcp')) { $script:tags["firecrawl-$($service):rust-transition-legacy"]=$legacy[$service].image }
    $script:states.api=@{id=('a'*64);image=$candidate.api;running=$true}
    # Active MCP can have been safely upgraded since initial promotion.
    $script:states.mcp=@{id=('b'*64);image=('sha256:'+('f'*64));running=$true}
    $script:states.gateway=@{id=('f'*64);image=('sha256:'+('9'*64));running=$true}
    $script:states['pdf-ocr']=@{id=('c'*64);image=$candidate['pdf-ocr'];running=$true}
    foreach ($service in @('redis','rabbitmq','nuq-postgres')) { $script:states[$service].running=$false }
    Set-Content -LiteralPath (Get-TransitionPath 'rust-transition.lock') -Value 'rust-active-v1'
    $journal | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Get-TransitionPath 'rust-transition.json')
    foreach ($unavailable in @($false,$true)) {
        $script:failInspection=$unavailable
        try { Restore-LegacyTransition -LegacyCompose @('compose','-p','firecrawl') -ProjectRoot 'synthetic' -Journal $journal;throw 'Unsafe rollback permitted.' }
        catch { if ($_.Exception.Message -cne 'Rust durable jobs are not verified idle; rollback refused.') { throw } }
        if (($script:events -join ',') -cne 'stop:gateway,stop:mcp,stop:api,rust-queue-count,resume:api,resume:mcp,original:gateway') { throw 'Busy rollback did not restore the same Rust services.' }
        if (-not (Test-Path -LiteralPath (Get-TransitionPath 'rust-transition.lock'))) { throw 'Busy rollback cleared marker.' }
        $script:events.Clear()
    }
    $script:jobs=0;$script:failInspection=$false
    $receipt=Restore-LegacyTransition -LegacyCompose @('compose','-p','firecrawl') -ProjectRoot 'synthetic' -Journal $journal
    if ($receipt -cne 'RUST_ROLLBACK_LEGACY_VERIFIED' -or (Test-Path -LiteralPath (Get-TransitionPath 'rust-transition.lock'))) { throw 'Successful rollback receipt failed.' }
    $expected='stop:gateway,stop:mcp,stop:api,rust-queue-count,stop:pdf-ocr,original:redis,original:rabbitmq,original:nuq-postgres,tag:firecrawl-api:latest,tag:firecrawl-mcp:latest,legacy-recreate:api,restore-runtime,pg-auth,legacy-health,legacy-recreate:mcp,legacy-mcp,original:gateway'
    if (($script:events -join ',') -cne $expected) { throw 'Rollback orchestration changed.' }
    'RUST_ROLLBACK_ORCHESTRATION_FIXTURE_PASS'
} finally { Remove-Item -LiteralPath $root -Recurse -Force }
