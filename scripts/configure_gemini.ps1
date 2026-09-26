param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $IsWindows) {
    throw "Gemini DPAPI bootstrap is supported on Windows only."
}
if (-not $env:LOCALAPPDATA) {
    throw "LOCALAPPDATA is required."
}

$Repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$SecretDir = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets"
$SecretPath = Join-Path $SecretDir "gemini_api_key.dpapi"
$LegacyPath = Join-Path $Repo "secrets.env"
New-Item -ItemType Directory -Force -Path $SecretDir | Out-Null

$secureKey = $null
$migratedLegacy = $false
$legacyEntryPresent = $false
$legacyLines = @()

if (Test-Path -LiteralPath $LegacyPath -PathType Leaf) {
    $legacyLines = @(Get-Content -LiteralPath $LegacyPath)
    $legacyEntries = @($legacyLines | Where-Object { $_ -match '^\s*GEMINI_API_KEY=' })
    if ($legacyEntries.Count -gt 1) {
        throw "secrets.env contains more than one GEMINI_API_KEY entry."
    }
    if ($legacyEntries.Count -eq 1) {
        $legacyEntryPresent = $true
        $legacyValue = ($legacyEntries[0] -split '=', 2)[1].Trim()
        if (-not [string]::IsNullOrWhiteSpace($legacyValue)) {
            $secureKey = ConvertTo-SecureString -String $legacyValue -AsPlainText -Force
            $migratedLegacy = $true
        }
        $legacyValue = $null
    }
}

if ($null -eq $secureKey) {
    $secureKey = Read-Host "Klistra in Gemini API key" -AsSecureString
}

$ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureKey)
try {
    $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
    if ([string]::IsNullOrWhiteSpace($plain)) {
        throw "Gemini API key must not be empty."
    }
}
finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
    $plain = $null
}

$encrypted = ConvertFrom-SecureString -SecureString $secureKey
$tmp = "$SecretPath.tmp"
[IO.File]::WriteAllText($tmp, $encrypted, [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $tmp -Destination $SecretPath -Force

# Verify that the current Windows user can decrypt the persisted blob before removing any legacy plaintext.
$verifySecure = ConvertTo-SecureString -String (Get-Content -LiteralPath $SecretPath -Raw)
$verifyPtr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($verifySecure)
try {
    $verifyPlain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($verifyPtr)
    if ([string]::IsNullOrWhiteSpace($verifyPlain)) {
        throw "DPAPI verification failed."
    }
}
finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($verifyPtr)
    $verifyPlain = $null
    $verifySecure = $null
}

if ($legacyEntryPresent) {
    $remaining = @($legacyLines | Where-Object { $_ -notmatch '^\s*GEMINI_API_KEY=' })
    $meaningful = @($remaining | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_) -and -not $_.TrimStart().StartsWith("#")
    })

    if ($meaningful.Count -eq 0) {
        Remove-Item -LiteralPath $LegacyPath -Force
        Write-Host "LEGACY_SECRETS_ENV_REMOVED"
    }
    else {
        $legacyTmp = "$LegacyPath.tmp"
        [IO.File]::WriteAllText(
            $legacyTmp,
            (($remaining -join [Environment]::NewLine) + [Environment]::NewLine),
            [Text.UTF8Encoding]::new($false)
        )
        Move-Item -LiteralPath $legacyTmp -Destination $LegacyPath -Force
    }

    if ($migratedLegacy) {
        Write-Host "GEMINI_KEY_MIGRATED_TO_DPAPI"
    } else {
        Write-Host "GEMINI_KEY_STORED_DPAPI"
    }
} else {
    Write-Host "GEMINI_KEY_STORED_DPAPI"
}

Write-Host "Path: $SecretPath"
Write-Host "Run .\fc.ps1 import-gemini to refresh an already-running llm-proxy."
