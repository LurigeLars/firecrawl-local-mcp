# Shared baseline handling for the one-service refresh scripts.
# The running image is the baseline: it is read before the candidate is built, must
# not change while the candidate is built and scanned, and is kept under a rollback
# tag before the container is replaced. No script hard-codes an old image id, so a
# refresh never goes stale after another deployment replaced the image.
function Get-FirecrawlRunningImage {
    param([Parameter(Mandatory)][string]$Container)
    $Id = [string](& docker inspect --format '{{.Image}}' $Container)
    if ($LASTEXITCODE -ne 0 -or $Id -cnotmatch '^sha256:[a-f0-9]{64}$') {
        throw "Cannot verify the running image of $Container."
    }
    return $Id
}

function Save-FirecrawlRollbackImage {
    param(
        [Parameter(Mandatory)][string]$Container,
        [Parameter(Mandatory)][string]$BaselineId,
        [Parameter(Mandatory)][string]$Repository
    )
    if ((Get-FirecrawlRunningImage -Container $Container) -cne $BaselineId) {
        throw "$Container changed while its candidate was staged; deployment aborted."
    }
    $Tag = "${Repository}:rollback-$($BaselineId.Substring(7, 12))"
    & docker tag $BaselineId $Tag
    if ($LASTEXITCODE -ne 0) { throw "Cannot keep rollback image $Tag; deployment aborted." }
    return $Tag
}

# Returns the reviewed Node version (vX.Y.Z) of a digest-pinned, npm-free Alpine Dockerfile.
function Get-FirecrawlPinnedNodeVersion {
    param([Parameter(Mandatory)][string]$Dockerfile)
    $Source = Get-Content -LiteralPath $Dockerfile -Raw
    $Pin = [regex]::Match($Source, '(?m)^FROM node:([0-9]+[.][0-9]+[.][0-9]+)-alpine@sha256:[a-f0-9]{64}\s*$')
    if (-not $Pin.Success -or $Source -notmatch 'rm -rf /usr/local/lib/node_modules/npm') {
        throw "$Dockerfile is not a digest-pinned, npm-free Node Alpine image."
    }
    return "v$($Pin.Groups[1].Value)"
}
