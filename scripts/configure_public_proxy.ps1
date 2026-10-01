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

$secureApiKey = Read-Host "Klistra in Webshare API key (används en gång och sparas inte)" -AsSecureString
$apiPtr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureApiKey)
try {
    $apiKey = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($apiPtr)
    if ([string]::IsNullOrWhiteSpace($apiKey)) {
        throw "Webshare API key must not be empty."
    }
    if ($apiKey -notmatch '^[A-Za-z0-9]{40}$') {
        throw "Webshare API key must be a 40-character alphanumeric key."
    }

    $headers = @{ Authorization = "Token $apiKey" }
    $listUri = "https://proxy.webshare.io/api/v2/proxy/list/?mode=direct&page=1&page_size=100&valid=true&ordering=-valid,proxy_address"

    try {
        $response = Invoke-RestMethod -Method Get -Uri $listUri -Headers $headers -TimeoutSec 30
    }
    catch {
        throw "Webshare Proxy List API request failed: $($_.Exception.Message)"
    }

    $candidates = @($response.results | Where-Object {
        $_.valid -eq $true -and
        -not [string]::IsNullOrWhiteSpace([string]$_.proxy_address) -and
        [int]$_.port -gt 0 -and
        -not [string]::IsNullOrWhiteSpace([string]$_.username) -and
        -not [string]::IsNullOrWhiteSpace([string]$_.password)
    })
    if ($candidates.Count -lt 1) {
        throw "Webshare returned no valid direct proxies for the default plan."
    }

    $selected = $null
    foreach ($candidate in $candidates) {
        $server = "http://$($candidate.proxy_address):$([int]$candidate.port)"
        $passwordSecure = ConvertTo-SecureString -String ([string]$candidate.password) -AsPlainText -Force
        $credential = [Management.Automation.PSCredential]::new([string]$candidate.username, $passwordSecure)
        try {
            $probe = Invoke-WebRequest -Uri "https://ipv4.webshare.io/" -Method Get -Proxy $server -ProxyCredential $credential -TimeoutSec 20
            if ($probe.StatusCode -eq 200) {
                $selected = $candidate
                break
            }
        }
        catch {
            # Try the next valid proxy. Do not print credentials or proxy endpoint details.
        }
        finally {
            $credential = $null
            $passwordSecure = $null
        }
    }

    if ($null -eq $selected) {
        throw "Webshare returned valid proxies, but none passed the outbound connectivity probe."
    }

    $server = "http://$($selected.proxy_address):$([int]$selected.port)"
    $username = [string]$selected.username
    $password = [string]$selected.password

    Save-DpapiValue -Value $server -Path (Join-Path $SecretDir "public_proxy_server.dpapi")
    Save-DpapiValue -Value $username -Path (Join-Path $SecretDir "public_proxy_username.dpapi")
    Save-DpapiValue -Value $password -Path (Join-Path $SecretDir "public_proxy_password.dpapi")

    Write-Host ("WEB_SHARE_PROXY_SELECTED country=" + [string]$selected.country_code)
}
finally {
    if ($apiPtr -ne [IntPtr]::Zero) {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($apiPtr)
    }
    $apiKey = $null
    $secureApiKey = $null
    $headers = $null
    $response = $null
    $candidates = $null
    $selected = $null
    $username = $null
    $password = $null
    $server = $null
}

Write-Host "PUBLIC_PROXY_STORED_DPAPI"
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_server.dpapi"))
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_username.dpapi"))
Write-Host ("Path: " + (Join-Path $SecretDir "public_proxy_password.dpapi"))
Write-Host "Run .\fc.ps1 redeploy to enable the isolated public-proxy route."
