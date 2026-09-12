[CmdletBinding()]
param(
    [switch]$SkipRust,
    [switch]$SkipWeb,
    [switch]$IncludeSqlCipher,
    [switch]$IncludePackaging,
    [switch]$IncludeDocker,
    [string]$LiveBaseUrl = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot

$cargoOnPath = Get-Command cargo -ErrorAction SilentlyContinue
$cargoFallback = Join-Path $HOME ".cargo/bin/cargo.exe"
$cargoCommand = if ($cargoOnPath) {
    $cargoOnPath.Source
} elseif (Test-Path $cargoFallback) {
    $cargoFallback
} else {
    throw "cargo was not found on PATH or at $cargoFallback"
}
$isWindowsHost = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
$npmCommand = if ($isWindowsHost) { "npm.cmd" } else { "npm" }

$passed = [System.Collections.Generic.List[string]]::new()
$skipped = [System.Collections.Generic.List[string]]::new()

function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Action
    )

    Write-Host "`n==> $Name"
    & $Action
    $passed.Add($Name)
}

function Assert-LastExitCode {
    param([Parameter(Mandatory = $true)][string]$CommandName)
    if ($LASTEXITCODE -ne 0) {
        throw "$CommandName failed with exit code $LASTEXITCODE"
    }
}

try {
    Invoke-Step "Documentation links" {
        $markdownFiles = @((Resolve-Path README.md).Path) +
            (Get-ChildItem docs -Filter *.md -File | ForEach-Object FullName)
        $broken = [System.Collections.Generic.List[string]]::new()
        foreach ($file in $markdownFiles) {
            $content = Get-Content $file -Raw
            foreach ($match in [regex]::Matches($content, '\[[^\]]*\]\(([^)]+)\)')) {
                $target = $match.Groups[1].Value.Split('#')[0]
                if (-not $target -or $target -match '^(https?|mailto):') {
                    continue
                }
                $path = Join-Path (Split-Path $file) ([uri]::UnescapeDataString($target))
                if (-not (Test-Path $path)) {
                    $relative = Resolve-Path $file -Relative
                    $broken.Add("${relative}: $target")
                }
            }
        }
        if ($broken.Count -gt 0) {
            throw "Broken Markdown links:`n$($broken -join "`n")"
        }
    }

    Invoke-Step "Plugin manifest inventory" {
        $manifests = Get-ChildItem plugins -Filter plugin.toml -Recurse
        if ($manifests.Count -eq 0) {
            throw "No plugin manifests found"
        }
        foreach ($manifest in $manifests) {
            $text = Get-Content $manifest.FullName -Raw
            $id = [regex]::Match($text, '(?m)^id\s*=\s*"([^"]+)"').Groups[1].Value
            $version = [regex]::Match($text, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
            if (-not $id -or -not $version) {
                throw "Missing plugin id/version in $($manifest.FullName)"
            }
        }
        Write-Host "Validated $($manifests.Count) plugin manifests"
    }

    if ($SkipRust) {
        $skipped.Add("Rust fmt, tests, and clippy (-SkipRust)")
    } else {
        Invoke-Step "Rust formatting" {
            & $cargoCommand fmt --all -- --check
            Assert-LastExitCode "cargo fmt"
        }
        Invoke-Step "Rust workspace tests" {
            & $cargoCommand test --workspace
            Assert-LastExitCode "cargo test --workspace"
        }
        Invoke-Step "Rust clippy" {
            & $cargoCommand clippy --workspace -- -D warnings
            Assert-LastExitCode "cargo clippy"
        }
    }

    if ($SkipWeb) {
        $skipped.Add("SPA tests, typecheck, and build (-SkipWeb)")
    } else {
        Invoke-Step "SPA tests" {
            & $npmCommand --prefix web test
            Assert-LastExitCode "npm test"
        }
        Invoke-Step "SPA typecheck" {
            & $npmCommand --prefix web run lint
            Assert-LastExitCode "npm run lint"
        }
        Invoke-Step "SPA production build" {
            & $npmCommand --prefix web run build
            Assert-LastExitCode "npm run build"
        }
    }

    if ($IncludeSqlCipher) {
        Invoke-Step "SQLCipher workspace tests" {
            & $cargoCommand test --workspace --no-default-features -F execlaw-core/sqlcipher
            Assert-LastExitCode "SQLCipher tests"
        }
    } else {
        $skipped.Add("SQLCipher tests (use -IncludeSqlCipher)")
    }

    if ($IncludePackaging) {
        Invoke-Step "Plugin packaging" {
            & "$PSScriptRoot\package-plugins.ps1"
            Assert-LastExitCode "package-plugins.ps1"
            $archives = Get-ChildItem dist -Filter *.zip
            $manifests = Get-ChildItem plugins -Filter plugin.toml -Recurse
            if ($archives.Count -ne $manifests.Count) {
                throw "Expected $($manifests.Count) plugin ZIPs, found $($archives.Count)"
            }
        }
    } else {
        $skipped.Add("Plugin packaging (use -IncludePackaging)")
    }

    if ($IncludeDocker) {
        Invoke-Step "Docker runner E2E" {
            docker info *> $null
            Assert-LastExitCode "docker info"
            docker image inspect execlaw/runner:dev *> $null
            Assert-LastExitCode "docker image inspect execlaw/runner:dev"
            $previous = $env:EXECLAW_E2E_DOCKER
            try {
                $env:EXECLAW_E2E_DOCKER = "1"
                & $cargoCommand test -p execlaw-server --test runner_e2e_docker -- --ignored
                Assert-LastExitCode "runner Docker E2E"
            } finally {
                $env:EXECLAW_E2E_DOCKER = $previous
            }
        }
    } else {
        $skipped.Add("Docker runner E2E (use -IncludeDocker after building execlaw/runner:dev)")
    }

    if ($LiveBaseUrl) {
        Invoke-Step "Live deployment smoke" {
            $base = $LiveBaseUrl.TrimEnd('/')
            $health = Invoke-RestMethod "$base/api/health"
            if ($health.status -ne "ok") {
                throw "Unexpected health response: $($health | ConvertTo-Json -Compress)"
            }
            $openApi = Invoke-RestMethod "$base/api/openapi.json"
            if (-not $openApi.paths) {
                throw "OpenAPI response has no paths"
            }
            $root = Invoke-WebRequest "$base/" -UseBasicParsing
            if ($root.StatusCode -ne 200) {
                throw "SPA root returned HTTP $($root.StatusCode)"
            }
        }
    } else {
        $skipped.Add("Live deployment smoke (set -LiveBaseUrl)")
    }

    Write-Host "`nPASS: $($passed.Count) test stages"
    foreach ($name in $passed) {
        Write-Host "  [pass] $name"
    }
    foreach ($name in $skipped) {
        Write-Host "  [skip] $name"
    }
} finally {
    Pop-Location
}
