$ErrorActionPreference = "Stop"

$hookDir = Join-Path ".git" "hooks"
if (-not (Test-Path $hookDir)) {
    throw "Not a git repository root (missing .git/hooks)."
}

$hookPath = Join-Path $hookDir "post-commit"
$runner = @'
#!/usr/bin/env pwsh
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/post_commit_graphify_memory.ps1
'@

Set-Content -Path $hookPath -Value $runner -Encoding UTF8
Write-Host "Installed post-commit hook at $hookPath"
