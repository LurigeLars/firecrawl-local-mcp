# Synthetic read-only queue receipts, no Docker or user state.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens=$null; $errors=$null
$ast=[System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot '../research/rust-transition-preflight.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw 'Preflight parse failed.' }
foreach ($name in @('Get-PreflightCount','Get-LegacyQueueReceipt','Get-LegacyInventoryReceipt')) {
    $def=$ast.Find({param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq $name},$true)
    . ([scriptblock]::Create($def.Extent.Text))
}
$script:rabbit='[{"messages_ready":0,"messages_unacknowledged":0}]'
$script:nuq='0|0|0|0'
$script:vhosts='[{"name":"/"}]'
function Invoke-PreflightDocker([string[]]$Arguments) {
    $cmd=$Arguments -join ' '
    switch -Wildcard ($cmd) {
        'exec firecrawl-rabbitmq-1 rabbitmqctl list_vhosts*' { $script:vhosts }
        'exec firecrawl-rabbitmq-1 rabbitmqctl list_queues*' { $script:rabbit }
        'exec firecrawl-nuq-postgres-1 psql*BEGIN READ ONLY*' { $script:nuq }
        default { throw 'Unexpected Docker invocation.' }
    }
}
$r=Get-LegacyQueueReceipt
if (@($r.Values | Where-Object {$_ -ne 0}).Count -ne 0) { throw 'Idle receipt failed.' }
$script:rabbit='[]'
$r=Get-LegacyQueueReceipt
if ($r.rabbit_ready -ne 0) { throw 'Empty queues failed.' }
$script:rabbit='[{"messages_ready":2,"messages_unacknowledged":3}]'
$script:nuq='4|5|6|7'
$r=Get-LegacyQueueReceipt
if (($r.Values -join ',') -cne '2,3,4,5,6,7') { throw 'Pending job counts not preserved.' }
foreach ($bad in @('-1|0|0|0','0|0|0','not-a-receipt')) {
    $script:nuq=$bad
    try { Get-LegacyQueueReceipt; throw 'Invalid receipt accepted.' }
    catch { if ($_.Exception.Message -cne 'Invalid NUQ count receipt.') { throw } }
}
$script:nuq='0|0|0|0'
$script:rabbit='{"messages_ready":0,"messages_unacknowledged":0}'
try { Get-LegacyQueueReceipt; throw 'Invalid queue object accepted.' }
catch { if ($_.Exception.Message -cne 'Invalid RabbitMQ queue receipt.') { throw } }
$script:rabbit='[]'
$script:vhosts='[{"name":"/"},{"name":"synthetic"}]'
try { Get-LegacyQueueReceipt; throw 'Unexpected vhost accepted.' }
catch { if ($_.Exception.Message -cne 'Unreviewed RabbitMQ virtual host.') { throw } }
'RUST_TRANSITION_PREFLIGHT_FIXTURE_PASS'
