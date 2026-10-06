[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    $cargoOnPath = Get-Command cargo -ErrorAction SilentlyContinue
    $cargoFallback = Join-Path $HOME ".cargo\bin\cargo.exe"
    $cargoCommand = if ($cargoOnPath) {
        $cargoOnPath.Source
    } elseif (Test-Path -LiteralPath $cargoFallback) {
        $cargoFallback
    } else {
        throw "cargo was not found on PATH or at $cargoFallback"
    }

    if (-not $env:CARGO_TARGET_DIR) {
        # Keep this focused check away from synced or ACL-protected workspace
        # artifacts while still reusing the existing Cargo cache when possible.
        . (Join-Path $PSScriptRoot 'cargo-target-local.ps1')
        Use-ExeclawLocalCargoTarget -RepoRoot $repoRoot
    }

    # --lib prevents Cargo from printing unrelated zero-test reports for
    # helper binaries and integration targets in the same package.
    & $cargoCommand test -p execlaw-core --lib migrations
    if ($LASTEXITCODE -ne 0) {
        throw "Focused migration tests failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}
