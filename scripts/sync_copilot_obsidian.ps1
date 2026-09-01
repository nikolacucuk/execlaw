Param(
    [string]$VaultDir = ".obsidian",
    [string]$TranscriptPath,
    [string]$SourceLabel = "copilot-session"
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($TranscriptPath)) {
    Write-Error "Provide -TranscriptPath to a JSON/JSONL transcript file."
}

$py = Join-Path $PSScriptRoot "copilot_to_obsidian.py"
if (-not (Test-Path $py)) {
    Write-Error "Missing script: $py"
}

python $py --vault-dir $VaultDir --transcript $TranscriptPath --source-label $SourceLabel
python (Join-Path $PSScriptRoot "weekly_lessons_maintenance_report.py") --vault-dir $VaultDir --stale-days 30

Write-Host "Obsidian sync completed."
