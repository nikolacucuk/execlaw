<#
.SYNOPSIS
    Run the production SQLCipher workspace suite with native Windows tools.
.PARAMETER PerlExe
    Path to a complete Windows perl.exe, such as Strawberry Perl. Git for
    Windows' bundled Perl does not include modules required by OpenSSL.
.PARAMETER TargetDir
    Repo-local Cargo target directory for hosts that block or lock target/debug.
#>
[CmdletBinding()]
param(
    [string]$PerlExe = '',
    [string]$TargetDir = 'target-h020-check'
)

$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This SQLCipher test launcher requires Windows.'
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
# Unix locale values inherited from Git or WSL shells are invalid for native
# Perl and turn its harmless warning into a PowerShell 5.1 native-command error.
foreach ($name in @('LC_ALL', 'LC_CTYPE', 'LANG')) {
    $value = Get-Item -Path "Env:$name" -ErrorAction SilentlyContinue
    if ($value -and $value.Value -eq 'C.UTF-8') {
        Remove-Item -Path "Env:$name"
    }
}
$candidates = @()
if ($PerlExe) { $candidates += $PerlExe }
$candidates += @(
    (Join-Path $env:LOCALAPPDATA 'execlaw-build-tools\strawberry-perl-5.42.3.1\perl\bin\perl.exe'),
    'C:\Strawberry\perl\bin\perl.exe'
)
$perl = $null
foreach ($candidate in $candidates) {
    if (-not (Test-Path -LiteralPath $candidate)) { continue }
    try {
        & $candidate -MLocale::Maketext::Simple -e 1 2>$null
        if ($LASTEXITCODE -eq 0) { $perl = (Resolve-Path -LiteralPath $candidate).Path; break }
    } catch { continue }
}
if (-not $perl) {
    $onPath = Get-Command perl.exe -ErrorAction SilentlyContinue
    if ($onPath) {
        try {
            & $onPath.Source -MLocale::Maketext::Simple -e 1 2>$null
            if ($LASTEXITCODE -eq 0) { $perl = $onPath.Source }
        } catch { $perl = $null }
    }
}
if (-not $perl) {
    throw 'A complete Windows Perl is required. Install Strawberry Perl or pass -PerlExe to its portable perl.exe.'
}

if (-not $env:VCINSTALLDIR) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw 'Visual Studio Build Tools with the C++ workload are required.'
    }
    $vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $vsRoot) {
        throw 'Visual Studio Build Tools with the C++ workload are required.'
    }
    $vcvars = Join-Path $vsRoot 'VC\Auxiliary\Build\vcvars64.bat'
    $vcLines = & cmd.exe /d /c "`"$vcvars`" >nul && set"
    if ($LASTEXITCODE -ne 0) { throw 'vcvars64.bat failed.' }
    foreach ($line in $vcLines) {
        if ($line -match '^([^=]+)=(.*)$') {
            Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2]
        }
    }
}

$env:Path = "$(Split-Path -Parent $perl);$env:Path"
$env:CC_x86_64_pc_windows_msvc = 'cl.exe'
$env:CXX_x86_64_pc_windows_msvc = 'cl.exe'
$env:AR_x86_64_pc_windows_msvc = 'lib.exe'
$env:CARGO_TARGET_DIR = Join-Path $repoRoot $TargetDir

Push-Location $repoRoot
try {
    & cargo test --workspace --no-default-features -F execlaw/sqlcipher --no-fail-fast
    if ($LASTEXITCODE -ne 0) { throw "SQLCipher workspace tests failed ($LASTEXITCODE)." }
    & cargo run -p execlaw --no-default-features --features sqlcipher -- doctor
    if ($LASTEXITCODE -ne 0) { throw "SQLCipher CLI doctor failed ($LASTEXITCODE)." }
} finally {
    Pop-Location
}
