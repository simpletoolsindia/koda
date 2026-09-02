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

function Ensure-Rust {
    if (Get-Command cargo -ErrorAction SilentlyContinue) { return }
    # cargo may be installed but not yet on this session's PATH.
    $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
    if (Test-Path (Join-Path $cargoBin "cargo.exe")) {
        $env:Path = "$cargoBin;$env:Path"
        if (Get-Command cargo -ErrorAction SilentlyContinue) { return }
    }
    Warn "Rust/cargo not found - koda is built from source and needs it."
    if (-not [Environment]::UserInteractive) {
        Die "Install Rust from https://rustup.rs then re-run."
    }
    $ans = Read-Host "  Install Rust now? [Y/n]"
    if ($ans -match '^[Nn]') { Die "Install Rust from https://rustup.rs then re-run." }
    # Prefer winget when available; fall back to the official rustup-init.exe.
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        Info "installing Rust via winget..."
        winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
    } else {
        Info "downloading rustup-init.exe..."
        $init = Join-Path ([System.IO.Path]::GetTempPath()) "rustup-init.exe"
        Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $init
        & $init -y | Out-Null
    }
    $env:Path = "$cargoBin;$env:Path"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Die "cargo still not found after installing Rust; open a new terminal and re-run."
    }
    Ok "Rust installed"
}

function Add-ToUserPath($dir) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -and ($userPath.Split(';') -contains $dir)) {
        Ok "$dir is on your PATH"
        return
    }
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $dir } else { "$userPath;$dir" }
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    $env:Path = "$env:Path;$dir"
    Ok "added $dir to your user PATH (open a new terminal to pick it up)"
}

function Build-And-Install {
    Ensure-Rust
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

    Add-ToUserPath $BinDir
    Ok "done - run 'koda' to start, or 'koda --help'"
}

function Version-Of($exe) {
    try { (& $exe --version 2>$null | Select-Object -First 1) } catch { "unknown" }
}

# Fetch the latest source, then rebuild. The menu used to offer "Install /
# update" as a single item that never fetched anything, so choosing it rebuilt
# whatever was already checked out and reported success.
function Update {
    $exe = Join-Path $BinDir "koda.exe"
    if (-not (Test-Path $exe)) {
        Warn "koda is not installed yet - installing instead"
        Build-And-Install
        return
    }
    $before = Version-Of $exe
    Info "installed: $before"

    $Src = Resolve-Src
    if (Test-Path (Join-Path $Src ".git")) {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Die "git not found - needed to fetch updates." }
        Info "fetching the latest source..."
        # --ff-only: a fast-forward is an update. Anything else means local
        # commits or a diverged branch, which is the user's to resolve - an
        # installer must not rewrite or discard their work to save a step.
        git -C $Src pull --ff-only 2>$null | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Warn "could not fast-forward $Src (local changes or a diverged branch)"
            Warn "rebuilding from the source as it stands"
        }
    }
    Build-And-Install
    Ok "updated: $before -> $(Version-Of $exe)"
}

function Uninstall {
    $exe = Join-Path $BinDir "koda.exe"
    if (Test-Path $exe) {
        # Deleting is not the safe default; without a console there is no way to
        # ask, so refuse rather than assume yes.
        if ([Environment]::UserInteractive) {
            $ans = Read-Host "  Remove $exe? [y/N]"
            if ($ans -notmatch '^[Yy]') { Info "left alone"; return }
        } elseif (-not $env:KODA_UNINSTALL_YES) {
            Warn "not interactive, so nothing was removed"
            Warn "re-run in a console, or set KODA_UNINSTALL_YES=1 to confirm"
            return
        }
        Remove-Item $exe -Force
        Ok "removed $exe"
    } else {
        Warn "no koda binary found at $exe"
    }

    # Settings are a separate question and default to no: they hold the
    # endpoint, model and API key, which are tedious to set up again and nothing
    # to do with the binary being present.
    $cfg = if ($env:XDG_CONFIG_HOME) { Join-Path $env:XDG_CONFIG_HOME "koda" }
           else { Join-Path $env:APPDATA "koda" }
    if (Test-Path $cfg) {
        if ([Environment]::UserInteractive) {
            $ans = Read-Host "  Also delete your settings at $cfg? [y/N]"
            if ($ans -match '^[Yy]') { Remove-Item $cfg -Recurse -Force; Ok "removed $cfg" }
            else { Info "kept your settings at $cfg" }
        } else {
            Info "your settings are kept at $cfg"
        }
    }
    Info "per-project data (sessions, memory, skills) stays in each project's .koda/"
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
Write-Host "  1  Install              ($BinDir)" -ForegroundColor Green
Write-Host "  2  Update to the latest  (git pull + rebuild)" -ForegroundColor Green
Write-Host "  3  Uninstall             (binary; asks about settings)" -ForegroundColor Green
Write-Host "  4  Quit" -ForegroundColor Green
Write-Host ""
$choice = Read-Host "  choose [1]"
if ([string]::IsNullOrWhiteSpace($choice)) { $choice = "1" }
Write-Host ""

switch ($choice) {
    "1" { Build-And-Install }
    "2" { Update }
    "3" { Uninstall }
    "4" { Info "nothing to do"; exit 0 }
    default { Die "unknown choice: $choice" }
}
