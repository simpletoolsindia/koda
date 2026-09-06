# koda installer for Windows (PowerShell), with a tiny interactive menu.
#
#   From a clone:  .\install.ps1
#   One-liner:     irm https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.ps1 | iex
#
# In a console it shows a menu (install / update / uninstall / quit). When piped
# (irm | iex, no interactive host) it just installs to %LOCALAPPDATA%\koda.
# Override the location with -Prefix.

param(
    [string]$Prefix = "$env:LOCALAPPDATA\koda",
    # Edition to build when this script has to clone. Matches install.sh, which
    # defaults to the same branch -- installing a different edition depending on
    # which OS you are on is not a difference anyone asked for.
    [string]$Branch = $(if ($env:KODA_BRANCH) { $env:KODA_BRANCH } else { "uncensored" })
)

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
        # A unique directory: cloning into a leftover %TEMP%\koda from an
        # earlier run fails with "destination path already exists", which read
        # as "clone failed" with nothing to act on.
        $s = Join-Path ([System.IO.Path]::GetTempPath()) ("koda-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
        Info "cloning koda ($Branch edition)..."
        git clone --depth 1 --branch $Branch $Repo $s 2>$null
        if ($LASTEXITCODE -ne 0) { Die "clone failed" }
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

# The browse tool's engine. koda ships it and installs it itself
# (`koda browser install`), so this needs no npm, no Node, and no second step
# from the user. Best-effort: a network hiccup must not fail the install.
function Ensure-BrowseEngine($koda) {
    Info "fetching the browse engine..."
    try {
        & $koda browser install 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Ok "browse engine ready"
            return
        }
    } catch { }
    Warn "could not fetch the browse engine - koda still runs; get it later with:"
    Warn "  koda browser install"
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
    $dest = Join-Path $BinDir "koda.exe"
    # Windows refuses to overwrite a running executable, which is exactly the
    # case when someone re-runs this to update. Renaming one is allowed, so move
    # the old copy aside first and clear it out next time.
    $old = "$dest.old"
    if (Test-Path $old) { Remove-Item $old -Force -ErrorAction SilentlyContinue }
    if (Test-Path $dest) {
        try { Rename-Item $dest $old -Force -ErrorAction Stop } catch {
            Die "koda.exe is in use and could not be replaced - close any running koda and re-run"
        }
    }
    Copy-Item $Built $dest -Force
    Ok "installed to $dest"

    Ensure-BrowseEngine (Join-Path $BinDir "koda.exe")

    Add-ToUserPath $BinDir
    Ensure-Tesseract
    Ok "done - run 'koda' to start, or 'koda --help'"
}

# --- optional: image OCR -----------------------------------------------------
# Attaching a picture to a model that cannot see one goes through OCR, which has
# two backends: a vision model named in `ocr_model`, which needs nothing
# installed and reads layout, tables and handwriting far better; and the
# `tesseract` CLI as the offline fallback.
#
# Offered, attempted, and non-fatal: a failure here costs the offline fallback,
# not koda.
function Ensure-Tesseract {
    if (Get-Command tesseract -ErrorAction SilentlyContinue) {
        Ok "tesseract found - offline image OCR is available (turn it on in /settings)"
        return
    }
    Info "tesseract (image OCR) not found."
    $installer =
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            "winget install --id UB-Mannheim.TesseractOCR -e --accept-source-agreements --accept-package-agreements"
        } elseif (Get-Command choco -ErrorAction SilentlyContinue) {
            "choco install tesseract -y"
        } elseif (Get-Command scoop -ErrorAction SilentlyContinue) {
            "scoop install tesseract"
        } else {
            $null
        }
    if (-not $installer) {
        Warn "no winget/choco/scoop - install tesseract for image OCR: https://github.com/UB-Mannheim/tesseract/wiki"
        return
    }
    if (-not [Environment]::UserInteractive) {
        Warn "for offline image OCR, install tesseract:  $installer"
        return
    }
    $ans = Read-Host "  Install tesseract now for offline image OCR? [Y/n]"
    if ($ans -match '^[Nn]') {
        Info "skipping tesseract - 'ocr vision model' in /settings does OCR without it"
        return
    }
    Info "installing tesseract..."
    try { Invoke-Expression $installer | Out-Null } catch { }

    # koda shells out to `tesseract` by name, so a copy that isn't on PATH is a
    # copy koda cannot use -- and the UB-Mannheim build that winget installs
    # lands in Program Files without adding itself, which is exactly why OCR
    # would otherwise still report "not available" after this succeeded. Put it
    # on PATH here rather than leaving the user a manual step, since an install
    # koda cannot see is not an install. choco and scoop do this themselves, so
    # the probe simply finds nothing to fix.
    if (-not (Get-Command tesseract -ErrorAction SilentlyContinue)) {
        foreach ($d in @("$env:ProgramFiles\Tesseract-OCR", "${env:ProgramFiles(x86)}\Tesseract-OCR")) {
            if (Test-Path (Join-Path $d "tesseract.exe")) { Add-ToUserPath $d; break }
        }
    }
    if (Get-Command tesseract -ErrorAction SilentlyContinue) {
        Ok "tesseract installed - turn OCR on in /settings"
    } else {
        Warn "tesseract install failed - koda still runs; for OCR either install it"
        Warn "by hand ($installer) or set 'ocr vision model' in /settings"
    }
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
            # $(...) ends the variable name: "$exe?" parses as a variable
            # called "exe?", which is null, and the prompt loses the path.
            $ans = Read-Host "  Remove $($exe)? [y/N]"
            # Require a positive yes rather than testing for "not no". Read-Host
            # returns $null when there is no console to read from, and
            # `$null -notmatch ...` does not evaluate to $true -- the guard did
            # not fire and the binary was deleted with no answer given.
            if (-not ($ans -match '^[Yy]')) { Info "left alone"; return }
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
           elseif ($env:APPDATA) { Join-Path $env:APPDATA "koda" }
           else { $null }
    if ($cfg -and (Test-Path $cfg)) {
        if ([Environment]::UserInteractive) {
            $ans = Read-Host "  Also delete your settings at $($cfg)? [y/N]"
            if ($ans -match '^[Yy]') { Remove-Item $cfg -Recurse -Force; Ok "removed $cfg" }
            else { Info "kept your settings at $cfg" }
            # (this one already required a positive match, so it was safe)
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
