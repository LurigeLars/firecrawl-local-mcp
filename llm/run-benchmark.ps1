# Runs benchmark.mjs for each model: switches MODEL_NAME in .env, restarts the API, preloads the model.
param([string[]]$Models = @('qwen2.5-16k', 'gemma3-12b-16k'))

$root = Split-Path $PSScriptRoot -Parent
$envFile = "$root\.env"
$original = (Get-Content $envFile | Where-Object { $_ -match '^MODEL_NAME=' }) -replace '^MODEL_NAME=', ''

foreach ($m in $Models) {
    (Get-Content $envFile) -replace '^MODEL_NAME=.*', "MODEL_NAME=$m" | Set-Content $envFile
    & "$root\fc.ps1" up *> $null
    foreach ($loaded in (ollama ps | Select-Object -Skip 1 | ForEach-Object { ($_ -split '\s+')[0] } | Where-Object { $_ })) { ollama stop $loaded *> $null }
    Invoke-RestMethod -Method Post http://127.0.0.1:11434/api/generate -ContentType 'application/json' -Body (@{ model = $m; prompt = ''; keep_alive = '10m' } | ConvertTo-Json) -TimeoutSec 300 | Out-Null
    for ($i = 0; $i -lt 40; $i++) { try { Invoke-WebRequest http://127.0.0.1:3002/ -TimeoutSec 3 | Out-Null; break } catch { if ($_.Exception.Response) { break }; Start-Sleep 3 } }
    Start-Sleep 5
    node "$PSScriptRoot\benchmark.mjs" $m
    (ollama ps | Select-Object -Skip 1) -join ' '
}

(Get-Content $envFile) -replace '^MODEL_NAME=.*', "MODEL_NAME=$original" | Set-Content $envFile
& "$root\fc.ps1" up *> $null
"restored MODEL_NAME=$original"
