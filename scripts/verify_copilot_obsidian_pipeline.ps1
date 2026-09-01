Param(
    [string]$VaultDir = ".obsidian"
)

$ErrorActionPreference = "Stop"

$requiredDirs = @(
    "permanent", "logs", "references", "inbox", "fleeting", "templates", "graphify", "chats",
    "Patterns", "Mistakes", "Decisions", "Context"
)

$ok = $true
foreach ($d in $requiredDirs) {
    $p = Join-Path $VaultDir $d
    if (-not (Test-Path $p)) {
        Write-Host "MISSING DIR: $p"
        $ok = $false
    }
}

$requiredFiles = @(
    "$VaultDir\Lessons-Index.md",
    "scripts\copilot_to_obsidian.py",
    "scripts\sync_copilot_obsidian.ps1",
    "scripts\setup_copilot_obsidian_profile.ps1",
    "scripts\verify_copilot_obsidian_pipeline.ps1",
    "scripts\weekly_lessons_maintenance_report.py"
)

foreach ($f in $requiredFiles) {
    if (-not (Test-Path $f)) {
        Write-Host "MISSING FILE: $f"
        $ok = $false
    }
}

if ($ok) {
    Write-Host "Obsidian + lessons pipeline verification passed."
    exit 0
}

Write-Error "Verification failed. See missing paths above."
