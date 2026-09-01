$ErrorActionPreference = "Stop"

if (-not (Test-Path $PROFILE)) {
    New-Item -ItemType File -Path $PROFILE -Force | Out-Null
}

$block = @'
function Sync-CopilotObsidian {
    param(
        [Parameter(Mandatory=$true)][string]$TranscriptPath,
        [string]$VaultDir = ".obsidian"
    )
    $repo = (Get-Location).Path
    & "$repo\scripts\sync_copilot_obsidian.ps1" -VaultDir $VaultDir -TranscriptPath $TranscriptPath
}
'@

$content = Get-Content $PROFILE -Raw
if ($content -notmatch "function Sync-CopilotObsidian") {
    Add-Content -Path $PROFILE -Value "`n$block`n"
    Write-Host "Added Sync-CopilotObsidian to $PROFILE"
} else {
    Write-Host "Sync-CopilotObsidian already exists in $PROFILE"
}
