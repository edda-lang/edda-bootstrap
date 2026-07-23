#!/usr/bin/env pwsh
# Install the Edda bootstrap compiler: download the latest release for
# Windows from GitHub Releases, unpack it, and add it to PATH.
#
# Usage:
#   irm https://raw.githubusercontent.com/edda-lang/edda-bootstrap/main/install.ps1 | iex
#
# Env overrides:
#   $env:EDDA_INSTALL_DIR   install root (default: $HOME\.edda-bootstrap)
#   $env:EDDA_INSTALL_TAG   release tag to install instead of "latest"

$ErrorActionPreference = "Stop"

$Repo = "edda-lang/edda-bootstrap"
$Platform = "x86-64-windows-msvc"
$InstallDir = if ($env:EDDA_INSTALL_DIR) { $env:EDDA_INSTALL_DIR } else { Join-Path $HOME ".edda-bootstrap" }
$Tag = if ($env:EDDA_INSTALL_TAG) { $env:EDDA_INSTALL_TAG } else { "latest" }

function Get-ReleaseJsonUrl {
    if ($Tag -eq "latest") {
        return "https://api.github.com/repos/$Repo/releases/latest"
    }
    return "https://api.github.com/repos/$Repo/releases/tags/$Tag"
}

function Get-AssetDownloadUrl {
    $release = Invoke-RestMethod -Uri (Get-ReleaseJsonUrl) -UseBasicParsing
    $assetName = "edda-bootstrap-$Platform.zip"
    $asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
    if (-not $asset) {
        throw "could not find a release asset named '$assetName' (tag: $Tag) - see https://github.com/$Repo/releases"
    }
    return $asset.browser_download_url
}

function Add-InstallDirToPath([string]$BinDir) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -split ";" -contains $BinDir) {
        Write-Host "install.ps1: $BinDir is already on your User PATH"
    } else {
        $newPath = if ($userPath) { "$userPath;$BinDir" } else { $BinDir }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "install.ps1: added $BinDir to your User PATH (open a new shell to pick it up)"
    }
    if (-not ($env:Path -split ";" -contains $BinDir)) {
        $env:Path = "$env:Path;$BinDir"
    }
}

function Install-EddaBootstrap {
    $url = Get-AssetDownloadUrl
    Write-Host "install.ps1: downloading $url"

    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $tmpDir | Out-Null
    try {
        $archivePath = Join-Path $tmpDir "archive.zip"
        Invoke-WebRequest -Uri $url -OutFile $archivePath -UseBasicParsing

        Write-Host "install.ps1: unpacking"
        $extractDir = Join-Path $tmpDir "extracted"
        Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

        $topLevel = Get-ChildItem -Path $extractDir -Directory | Select-Object -First 1
        if (-not $topLevel) {
            throw "unexpected archive layout - no top-level directory found"
        }

        if (Test-Path $InstallDir) {
            Remove-Item -Path $InstallDir -Recurse -Force
        }
        $parent = Split-Path -Parent $InstallDir
        if ($parent -and -not (Test-Path $parent)) {
            New-Item -ItemType Directory -Path $parent | Out-Null
        }
        Move-Item -Path $topLevel.FullName -Destination $InstallDir
    } finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Host ""
    Write-Host "install.ps1: installed to $InstallDir"
    Write-Host ""

    Add-InstallDirToPath (Join-Path $InstallDir "bin")

    $runesPath = (Join-Path $InstallDir "runes\lib\<name>") -replace '\\', '/'
    Write-Host "install.ps1: runes vendored at $(Join-Path $InstallDir 'runes') - use e.g."
    Write-Host "  source = `"path+$runesPath`""
    Write-Host "install.ps1: open a new shell and run 'edda version' to verify."
}

Install-EddaBootstrap
