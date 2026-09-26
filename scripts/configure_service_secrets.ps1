param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $IsWindows) {
    throw "Firecrawl service-secret DPAPI bootstrap is supported on Windows only."
}
if (-not $env:LOCALAPPDATA) {
    throw "LOCALAPPDATA is required."
}

$Repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$SecretDir = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets"
$LegacyPath = Join-Path $Repo "secrets.env"
New-Item -ItemType Directory -Force -Path $SecretDir | Out-Null

$specs = @(
    [ordered]@{
        Name = "POSTGRES_PASSWORD"
        File = "postgres_password.dpapi"
        Prompt = "Klistra in nuvarande PostgreSQL password"
    },
    [ordered]@{
        Name = "SEARXNG_SECRET"
        File = "searxng_secret.dpapi"
        Prompt = "Klistra in nuvarande SearXNG secret"
    }
)

$legacyLines = @()
if (Test-Path -LiteralPath $LegacyPath -PathType Leaf) {
    $legacyLines = @(Get-Content -LiteralPath $LegacyPath)
}

function Get-LegacyValue([string]$Name) {
    $pattern = "^\s*" + [regex]::Escape($Name) + "="
    $entries = @($legacyLines | Where-Object { $_ -match $pattern })
    if ($entries.Count -gt 1) {
        throw "secrets.env contains more than one $Name entry."
    }
    if ($entries.Count -eq 0) {
        return $null
    }
    return (($entries[0] -split "=", 2)[1]).Trim()
}

function Save-DpapiSecret([Security.SecureString]$Secure, [string]$Path) {
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Secure)
    try {
        $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
        if ([string]::IsNullOrWhiteSpace($plain)) {
            throw "Secret must not be empty."
        }
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
        $plain = $null
    }

    $encrypted = ConvertFrom-SecureString -SecureString $Secure
    $tmp = "$Path.tmp"
    [IO.File]::WriteAllText($tmp, $encrypted, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $tmp -Destination $Path -Force

    $verifySecure = ConvertTo-SecureString -String (Get-Content -LiteralPath $Path -Raw)
    $verifyPtr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($verifySecure)
    try {
        $verifyPlain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($verifyPtr)
        if ([string]::IsNullOrWhiteSpace($verifyPlain)) {
            throw "DPAPI verification failed for $Path."
        }
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($verifyPtr)
        $verifyPlain = $null
        $verifySecure = $null
    }
}

$migrated = @()
$stored = @()

foreach ($spec in $specs) {
    $path = Join-Path $SecretDir $spec.File
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        $existing = ConvertTo-SecureString -String (Get-Content -LiteralPath $path -Raw)
        $existingPtr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($existing)
        try {
            $existingPlain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($existingPtr)
            if ([string]::IsNullOrWhiteSpace($existingPlain)) {
                throw "Existing DPAPI secret is empty: $path"
            }

            $legacyForComparison = Get-LegacyValue $spec.Name
            if (
                -not [string]::IsNullOrWhiteSpace($legacyForComparison) -and
                $existingPlain -cne $legacyForComparison
            ) {
                throw "Existing DPAPI secret for $($spec.Name) does not match secrets.env. Refusing to remove the legacy value."
            }
            $legacyForComparison = $null
        }
        finally {
            [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($existingPtr)
            $existingPlain = $null
            $existing = $null
        }
        $stored += $spec.Name
        continue
    }

    $legacy = Get-LegacyValue $spec.Name
    if (-not [string]::IsNullOrWhiteSpace($legacy)) {
        $secure = ConvertTo-SecureString -String $legacy -AsPlainText -Force
        $legacy = $null
        Save-DpapiSecret -Secure $secure -Path $path
        $secure = $null
        $migrated += $spec.Name
        $stored += $spec.Name
        continue
    }

    if ($spec.Name -eq "POSTGRES_PASSWORD") {
        Write-Host "POSTGRES_PASSWORD saknas i secrets.env. På en befintlig installation måste du ange exakt samma lösenord som databasen redan använder."
    }
    $secure = Read-Host $spec.Prompt -AsSecureString
    Save-DpapiSecret -Secure $secure -Path $path
    $secure = $null
    $stored += $spec.Name
}

if ($stored.Count -ne $specs.Count) {
    throw "Not all service secrets were stored successfully."
}

if (Test-Path -LiteralPath $LegacyPath -PathType Leaf) {
    $patterns = @(
        "^\s*POSTGRES_PASSWORD=",
        "^\s*SEARXNG_SECRET="
    )
    $remaining = @(
        $legacyLines | Where-Object {
            $line = $_
            -not ($patterns | Where-Object { $line -match $_ })
        }
    )

    $meaningful = @($remaining | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_) -and -not $_.TrimStart().StartsWith("#")
    })

    if ($meaningful.Count -eq 0) {
        Remove-Item -LiteralPath $LegacyPath -Force
        Write-Host "LEGACY_SECRETS_ENV_REMOVED"
    }
    else {
        $tmp = "$LegacyPath.tmp"
        [IO.File]::WriteAllText(
            $tmp,
            (($remaining -join [Environment]::NewLine) + [Environment]::NewLine),
            [Text.UTF8Encoding]::new($false)
        )
        Move-Item -LiteralPath $tmp -Destination $LegacyPath -Force
        Write-Warning "secrets.env still contains unrelated entries and was preserved. fc.ps1 no longer loads it."
    }
}

if ($migrated.Count) {
    Write-Host ("SERVICE_SECRETS_MIGRATED_TO_DPAPI: " + ($migrated -join ", "))
}
else {
    Write-Host "SERVICE_SECRETS_STORED_DPAPI"
}

foreach ($spec in $specs) {
    Write-Host ("Path: " + (Join-Path $SecretDir $spec.File))
}
Write-Host "Run .\fc.ps1 up to recreate services with Docker Compose secrets."
