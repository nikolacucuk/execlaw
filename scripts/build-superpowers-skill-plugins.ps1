<#
.SYNOPSIS
    Build execlaw plugin ZIPs from a Superpowers skills checkout.

.DESCRIPTION
    Generates one or two skill-only plugin bundles:
      1) Shared plugin (community/library skills)
      2) User plugin (DjEnKa-specific overlays)

    Each bundle is a normal execlaw plugin ZIP with:
      - plugin.toml containing [[skills]] entries
      - skills/<name>/... copied from the source tree

    Result ZIPs can be installed from Settings -> Plugins -> Install.
#>

[CmdletBinding()]
param(
    [string]$SharedSkillsRoot = "$HOME/.config/superpowers/skills",
    [string]$UserSkillsRoot = "$HOME/.execlaw/skills/user",
    [string]$UserSkillNamespace = "djenka",
    [string]$DistDir = "dist",
    [string]$SharedPluginId = "superpowers-shared",
    [string]$UserPluginId = "superpowers-user",
    [switch]$SkipUser
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

function Get-PluginVersion {
    param([string]$Root)
    $datePart = Get-Date -Format 'yyyy.MM.dd'
    try {
        if (Test-Path (Join-Path $Root '.git')) {
            $sha = (git -C $Root rev-parse --short HEAD 2>$null).Trim()
            if ($sha) {
                return "$datePart+$sha"
            }
        }
    } catch {
        # Fall back to date-only version.
    }
    return "$datePart"
}

function Get-Frontmatter {
    param([string]$SkillFile)

    $name = ''
    $description = ''

    $raw = Get-Content -LiteralPath $SkillFile -Raw
    $raw = $raw -replace "`r", ''
    $lines = $raw -split "`n"

    if ($lines.Count -lt 3 -or $lines[0].Trim() -ne '---') {
        return [pscustomobject]@{ Name = $name; Description = $description }
    }

    for ($i = 1; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line.Trim() -eq '---') {
            break
        }
        if (-not $name -and $line -match '^\s*name\s*:\s*(.+?)\s*$') {
            $name = $Matches[1].Trim().Trim('"').Trim("'")
        }
        if (-not $description -and $line -match '^\s*description\s*:\s*(.+?)\s*$') {
            $description = $Matches[1].Trim().Trim('"').Trim("'")
        }
    }

    return [pscustomobject]@{ Name = $name; Description = $description }
}

function ConvertTo-TomlEscaped {
    param([string]$Value)
    if ($null -eq $Value) { return '' }
    return $Value.Replace('\', '\\').Replace('"', '\"')
}

function Get-SkillEntries {
    param([string]$Root)

    if (-not (Test-Path -LiteralPath $Root)) {
        throw "Skills root does not exist: $Root"
    }

    $skillFiles = Get-ChildItem -LiteralPath $Root -Recurse -File -Filter 'SKILL.md'
    $result = @()

    foreach ($file in $skillFiles) {
        $skillDir = Split-Path -Parent $file.FullName
        $relDir = [System.IO.Path]::GetRelativePath($Root, $skillDir)
        $relDir = $relDir -replace '\\', '/'
        if ($relDir -eq '.') { continue }

        $fm = Get-Frontmatter -SkillFile $file.FullName
        $localName = if ($fm.Name) { $fm.Name } else { ($relDir -replace '/', '-') }
        $description = if ($fm.Description) {
            $fm.Description
        } else {
            "Superpowers skill imported from $relDir"
        }

        $result += [pscustomobject]@{
            RelativeDir = $relDir
            LocalName = $localName
            Description = $description
            SkillFile = $file.FullName
        }
    }

    return $result | Sort-Object RelativeDir
}

function Write-PluginManifest {
    param(
        [string]$Path,
        [string]$PluginId,
        [string]$PluginName,
        [string]$Version,
        [string]$Description,
        [object[]]$Skills
    )

    $lines = New-Object System.Collections.Generic.List[string]
    $pluginIdEsc = ConvertTo-TomlEscaped $PluginId
    $pluginNameEsc = ConvertTo-TomlEscaped $PluginName
    $versionEsc = ConvertTo-TomlEscaped $Version
    $descriptionEsc = ConvertTo-TomlEscaped $Description
    $lines.Add('[plugin]')
    $lines.Add("id = `"$pluginIdEsc`"")
    $lines.Add("name = `"$pluginNameEsc`"")
    $lines.Add("version = `"$versionEsc`"")
    $lines.Add("description = `"$descriptionEsc`"")
    $lines.Add('author = "execlaw integration"')
    $lines.Add('license = "MIT"')
    $lines.Add('')

    foreach ($s in $Skills) {
        $entry = "skills/$($s.RelativeDir)/SKILL.md"
        $nameEsc = ConvertTo-TomlEscaped $s.LocalName
        $skillDescEsc = ConvertTo-TomlEscaped $s.Description
        $entryEsc = ConvertTo-TomlEscaped $entry
        $lines.Add('[[skills]]')
        $lines.Add("name = `"$nameEsc`"")
        $lines.Add("description = `"$skillDescEsc`"")
        $lines.Add("entry = `"$entryEsc`"")
        $lines.Add('tags = ["superpowers"]')
        $lines.Add('')
    }

    Set-Content -LiteralPath $Path -Value ($lines -join "`n") -Encoding ascii
}

function New-PluginZip {
    param(
        [string]$SourceRoot,
        [string]$PluginId,
        [string]$PluginName,
        [string]$Version,
        [string]$Description,
        [object[]]$Skills,
        [string]$OutputDir
    )

    if ($Skills.Count -eq 0) {
        Write-Warning "No skills found for $PluginId. Skipping."
        return $null
    }

    $stage = Join-Path ([System.IO.Path]::GetTempPath()) ("execlaw-superpowers-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $stage | Out-Null

    try {
        $skillsStage = Join-Path $stage 'skills'
        New-Item -ItemType Directory -Path $skillsStage | Out-Null

        foreach ($skill in $Skills) {
            $sourceDir = Join-Path $SourceRoot $skill.RelativeDir
            $destDir = Join-Path $skillsStage $skill.RelativeDir
            New-Item -ItemType Directory -Path $destDir -Force | Out-Null
            Copy-Item -Path (Join-Path $sourceDir '*') -Destination $destDir -Recurse -Force
        }

        Write-PluginManifest -Path (Join-Path $stage 'plugin.toml') -PluginId $PluginId -PluginName $PluginName -Version $Version -Description $Description -Skills $Skills

        New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
        $zipName = "$PluginId-$Version.zip"
        $zipPath = Join-Path $OutputDir $zipName
        if (Test-Path -LiteralPath $zipPath) {
            Remove-Item -LiteralPath $zipPath -Force
        }

        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem

        $zipStream = [System.IO.File]::Create($zipPath)
        try {
            $archive = New-Object System.IO.Compression.ZipArchive(
                $zipStream,
                [System.IO.Compression.ZipArchiveMode]::Create
            )
            try {
                $files = Get-ChildItem -LiteralPath $stage -Recurse -File
                foreach ($f in $files) {
                    $entryName = [System.IO.Path]::GetRelativePath($stage, $f.FullName)
                    $entryName = $entryName -replace '\\', '/'
                    $entry = $archive.CreateEntry($entryName, [System.IO.Compression.CompressionLevel]::Optimal)
                    $entryStream = $entry.Open()
                    try {
                        $src = [System.IO.File]::OpenRead($f.FullName)
                        try {
                            $src.CopyTo($entryStream)
                        } finally {
                            $src.Dispose()
                        }
                    } finally {
                        $entryStream.Dispose()
                    }
                }
            } finally {
                $archive.Dispose()
            }
        } finally {
            $zipStream.Dispose()
        }

        $hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
        Set-Content -LiteralPath "$zipPath.sha256" -Value "$hash  $zipName" -Encoding ascii -NoNewline

        return $zipPath
    }
    finally {
        if (Test-Path -LiteralPath $stage) {
            Remove-Item -LiteralPath $stage -Recurse -Force
        }
    }
}

$distAbs = if ([System.IO.Path]::IsPathRooted($DistDir)) { $DistDir } else { Join-Path $repoRoot $DistDir }

$sharedSkills = Get-SkillEntries -Root $SharedSkillsRoot
$sharedVersion = Get-PluginVersion -Root $SharedSkillsRoot
$sharedZip = New-PluginZip -SourceRoot $SharedSkillsRoot -PluginId $SharedPluginId -PluginName 'Superpowers Shared Skills' -Version $sharedVersion -Description 'Shared Superpowers skills imported into execlaw.' -Skills $sharedSkills -OutputDir $distAbs

$userZip = $null
if (-not $SkipUser) {
    if (Test-Path -LiteralPath $UserSkillsRoot) {
        $userSkills = Get-SkillEntries -Root $UserSkillsRoot
        foreach ($s in $userSkills) {
            # Prefix names to keep user scope explicit in state_skills.
            $s.LocalName = "$UserSkillNamespace-$($s.LocalName)"
        }
        $userVersion = Get-PluginVersion -Root $UserSkillsRoot
        $userZip = New-PluginZip -SourceRoot $UserSkillsRoot -PluginId $UserPluginId -PluginName 'Superpowers User Skills' -Version $userVersion -Description 'User-scoped Superpowers overlays for one operator.' -Skills $userSkills -OutputDir $distAbs
    }
    else {
        Write-Warning "User skills root not found: $UserSkillsRoot"
    }
}

Write-Host ''
Write-Host 'Superpowers skill plugin build complete.'
if ($sharedZip) {
    Write-Host "  Shared: $sharedZip"
}
if ($userZip) {
    Write-Host "  User:   $userZip"
}
Write-Host ''
Write-Host 'Install the ZIP(s) from the execlaw admin UI: Settings -> Plugins -> Install.'
