function Use-ExeclawLocalCargoTarget {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepoRoot
    )

    # Respect an explicit target directory first. Some qualification scripts
    # deliberately isolate Cargo artifacts by feature set or test purpose.
    if ($env:CARGO_TARGET_DIR) {
        Write-Host "Using explicit Cargo target: $env:CARGO_TARGET_DIR"
        return
    }

    if ($env:EXECLAW_REPO_CARGO_TARGET -eq '1' -or -not $env:LOCALAPPDATA) {
        return
    }

    $resolvedRoot = [System.IO.Path]::GetFullPath($RepoRoot).TrimEnd('\')
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $rootBytes = [System.Text.Encoding]::UTF8.GetBytes($resolvedRoot.ToLowerInvariant())
        $rootHash = [BitConverter]::ToString($sha256.ComputeHash($rootBytes)).Replace('-', '').Substring(0, 12).ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }

    $targetDir = Join-Path $env:LOCALAPPDATA "execlaw\cargo-target-$rootHash"
    $seedMarker = Join-Path $targetDir '.execlaw-target-seeded'
    $workspaceTarget = Join-Path $resolvedRoot 'target'
    $workspaceInfo = Join-Path $workspaceTarget '.rustc_info.json'

    # Seed the unsynced cache from the existing workspace cache once. Cargo
    # validates fingerprints and rebuilds stale artifacts; retaining the old
    # target avoids an unnecessary full dependency rebuild on first use.
    if (-not (Test-Path -LiteralPath $seedMarker)) {
        New-Item -ItemType Directory -Force -Path $targetDir -ErrorAction Stop | Out-Null
        if (Test-Path -LiteralPath $workspaceInfo) {
            Write-Host "Seeding local Cargo cache from $workspaceTarget"
            & robocopy.exe $workspaceTarget $targetDir /E /COPY:DAT /DCOPY:DAT /R:1 /W:1 /MT:16 /NFL /NDL /NJH /NJS /NP | Out-Null
            $copyExitCode = $LASTEXITCODE
            if ($copyExitCode -ge 8) {
                throw "Cargo cache copy failed with robocopy exit code $copyExitCode"
            }
        }
        # Mark only after a successful copy so an interrupted first seed can
        # safely resume on the next invocation.
        New-Item -ItemType File -Force -Path $seedMarker -ErrorAction Stop | Out-Null
    }

    New-Item -ItemType Directory -Force -Path $targetDir -ErrorAction Stop | Out-Null
    $env:CARGO_TARGET_DIR = $targetDir
    Write-Host "Using local Cargo target: $env:CARGO_TARGET_DIR"
}
