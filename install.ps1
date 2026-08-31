# koda installer for Windows (PowerShell), with a tiny interactive menu.
#
#   From a clone:  .\install.ps1
#   One-liner:     irm https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.ps1 | iex
#
# In a console it shows a menu (install / update / uninstall / quit). When piped
# (irm | iex, no interactive host) it just installs to %LOCALAPPDATA%\koda.
# Override the location with -Prefix.

param([string]$Prefix = "$env:LOCALAPPDATA\koda")

$ErrorActionPreference = "Stop"
$Repo = "https://github.com/simpletoolsindia/koda.git"
$BinDir = Join-Path $Prefix "bin"

function Info($m) { Write-Host "> $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "OK $m" -ForegroundColor Green }
function Warn($m) { Write-Host "! $m" -ForegroundColor Yellow }
function Die($m)  { Write-Host "x $m" -ForegroundColor Red; exit 1 }

function Resolve-Src {
    if (Test-Path (Join-Path $PSScriptRoot "Cargo.toml")) {
        return $PSScriptRoot
    } elseif (Test-Path "Cargo.toml") {
        return (Get-Location).Path
    } else {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Die "git not found." }
        $s = Join-Path ([System.IO.Path]::GetTempPath()) "koda"
        Info "cloning koda..."
        git clone --depth 1 $Repo $s 2>$null
        return $s
    }
}

function Build-And-Install {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Die "Rust/cargo not found. Install from https://rustup.rs then re-run."
    }
    $Src = Resolve-Src
    Set-Location $Src
    Info "building the release binary (a minute or two the first time)..."
    cargo build --release --quiet
    $Built = Join-Path "target\release" "koda.exe"
    if (-not (Test-Path $Built)) { Die "build finished but $Built is missing" }
    Ok "built"

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    Copy-Item $Built (Join-Path $BinDir "koda.exe") -Force
    Ok "installed to $BinDir\koda.exe"

    if ($env:Path -notlike "*$BinDir*") {
        Warn "add $BinDir to your PATH (User environment variables)"
    } else {
        Ok "$BinDir is on your PATH"
    }
    Ok "done - run 'koda' to start, or 'koda --help'"
}

function Uninstall {
    $exe = Join-Path $BinDir "koda.exe"
    if (Test-Path $exe) { Remove-Item $exe -Force; Ok "removed $exe" }
    else { Warn "no koda binary found at $exe" }
}

# Non-interactive host (irm | iex): just install.
if ($Host.UI.RawUI -eq $null -or [Environment]::UserInteractive -eq $false) {
    Build-And-Install
    exit 0
}

Write-Host ""
Write-Host "  koda installer" -ForegroundColor Cyan -NoNewline
Write-Host "  Windows"
Write-Host ""
Write-Host "  1  Install / update   ($BinDir)" -ForegroundColor Green
Write-Host "  2  Uninstall" -ForegroundColor Green
Write-Host "  3  Quit" -ForegroundColor Green
Write-Host ""
$choice = Read-Host "  choose [1]"
if ([string]::IsNullOrWhiteSpace($choice)) { $choice = "1" }
Write-Host ""

switch ($choice) {
    "1" { Build-And-Install }
    "2" { Uninstall }
    "3" { Info "nothing to do"; exit 0 }
    default { Die "unknown choice: $choice" }
}
