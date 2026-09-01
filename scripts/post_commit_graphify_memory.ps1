$ErrorActionPreference = "Continue"

Write-Host "[post-commit] graphify + obsidian maintenance"

function Invoke-Graphify {
    param(
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$Args
    )

    $bin = $env:EXECLAW_GRAPHIFY_BIN
    if (-not [string]::IsNullOrWhiteSpace($bin)) {
        & $bin @Args
        return
    }

    $venvGraphify = Join-Path ".venv" "Scripts\graphify.exe"
    if (Test-Path $venvGraphify) {
        & $venvGraphify @Args
        return
    }

    $g = Get-Command graphify -ErrorAction SilentlyContinue
    if ($g) {
        & graphify @Args
        return
    }

    $py = Get-Command py -ErrorAction SilentlyContinue
    if ($py) {
        & py -m graphify @Args
        return
    }

    $python = Get-Command python -ErrorAction SilentlyContinue
    if ($python) {
        & python -m graphify @Args
        return
    }

    throw "graphify executable not found (graphify / py -m graphify / python -m graphify)"
}

try {
    Invoke-Graphify update .
} catch {
    Write-Warning "graphify update failed: $($_.Exception.Message)"
}

try {
    node scripts/graphify_sync_preview.mjs
} catch {
    Write-Warning "graphify preview sync failed: $($_.Exception.Message)"
}

try {
    python scripts/weekly_lessons_maintenance_report.py --vault-dir .obsidian --stale-days 30
} catch {
    Write-Warning "weekly lessons report failed: $($_.Exception.Message)"
}
