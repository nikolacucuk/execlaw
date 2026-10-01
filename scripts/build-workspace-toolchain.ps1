$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$imageTag = 'execlaw/workspace-toolchain:1.0.0'

Push-Location $repoRoot
try {
    & docker build --pull --file Dockerfile.workspace-toolchain --tag $imageTag .
    if ($LASTEXITCODE -ne 0) {
        throw "Docker build failed with exit code $LASTEXITCODE"
    }

    $imageId = & docker image inspect --format '{{.Id}}' $imageTag
    if ($LASTEXITCODE -ne 0 -or $imageId -notmatch '^sha256:[0-9a-f]{64}$') {
        throw 'Docker did not return a valid immutable workspace-toolchain image ID'
    }
    Write-Output "Built $imageTag"
    Write-Output "Local development reference: $imageId"
    Write-Output 'Use that local digest only after enabling the Controller local-artifact override; production must use a published, attested OCI digest.'
}
finally {
    Pop-Location
}
