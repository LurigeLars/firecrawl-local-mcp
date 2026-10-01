param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not $IsWindows) {
    throw "Firecrawl public-proxy DPAPI bootstrap is supported on Windows only."
}
if (-not $env:LOCALAPPDATA) {
    throw "LOCALAPPDATA is required."
}

$SecretDir = Join-Path $env:LOCALAPPDATA "FirecrawlLocal\secrets"
New-Item -ItemType Directory -Force -Path $SecretDir | Out-Null

function Save-DpapiValue([string]$Value, [string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw "Secret value must not be empty."
    }
    $secure = ConvertTo-SecureString -String $Value -AsPlainText -Force
    try {
        $encrypted = ConvertFrom-SecureString -SecureString $secure
        $tmp = "$Path.tmp"
        [IO.File]::WriteAllText($tmp, $encrypted, [Text.UTF8Encoding]::new($false))
        Move-Item -LiteralPath $tmp -Destination $Path -Force

        $verifySecure = ConvertTo-SecureString -String (Get-Content -LiteralPath $Path -Raw)
        $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($verifySecure)
        try {
            $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
            if ($plain -cne $Value) {
                throw "DPAPI verification failed for $Path."
            }
        }
        finally {
            [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
            $plain = $null
            $verifySecure = $null
        }
    }
    finally {
        $secure = $null
    }
}

$secureUrl = Read-Host "Klistra in full Webshare proxy URL (http://user:password@host:port/)" -AsSecureString
$ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureUrl)
try {
    $plainUrl = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)
    if ([string]::IsNullOrWhiteSpace($plainUrl)) {
        throw "Proxy URL must not be empty."
    }

    try {
        $uri = [Uri]$plainUrl
    }
    catch {
        throw "Proxy URL is invalid."
    }

    if ($uri.Scheme -notin @("http", "https")) {
        throw "Proxy URL must use http or https."
    }
    if ([string]::IsNullOrWhiteSpace($uri.Host) -or $uri.Port -le 0) {
        throw "Proxy URL must include host and port."
    }
    if ([string]::IsNullOrWhiteSpace($uri.UserInfo) -or $uri.UserInfo -notmatch ":") {
        throw "Proxy URL must include username and password."
    }

    $parts = $uri.UserInfo.Split(":", 2)
    $username = [Uri]::UnescapeDataString($parts[0])
    $password = [Uri]::UnescapeDataString($parts[1])
    $server = "$($uri.Scheme)://$($uri.Host):$($uri.Port)"

    Save-DpapiValue -Value $server -Path (Join-Path $SecretDir "public_proxy_server.dpapi")
    Save-DpapiValue -Value $username -Path (Join-Path $SecretDir "public_proxy_username.dpapi")
    Save-DpapiValue -Value $password -Path (Join-Path $SecretDir "public_proxy_password.dpapi")
}
finally {
    [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr)
    $plainUrl = $null
    $secureUrl = $null
    $username = $null
    $password = $null
    $server = $null
}

Write-Host "PUBLIC_PROXY_STORED_DPAPI"
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_server.dpapi"))
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_username.dpapi"))
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_password.dpapi"))
Write-Host "Run .\fc.ps1 redeploy to enable the isolated public-proxy route."
