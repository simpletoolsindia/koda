# koda installer for Windows (PowerShell), with a tiny interactive menu.
#
#   From a clone:  .\install.ps1
#   One-liner:     irm https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.ps1 | iex
#
# In a console it shows a menu (install / update / uninstall / quit). When piped
# (irm | iex, no interactive host) it just installs to %LOCALAPPDATA%\koda.
# Override the location with -Prefix.
#
# Fast path: when a published release matches the version on the branch, its
# prebuilt binary is downloaded and checked against its SHA-256 -- seconds, and
# no Rust toolchain. Otherwise koda is built from source, in a clone kept under
# %LOCALAPPDATA%\koda\cache so the next update rebuilds only what changed. Run
# from a koda checkout, the checkout itself is built.
#
#   $env:KODA_FROM_SOURCE = "1"      always build from source (the branch tip)
#   $env:KODA_VERSION = "0.1.0"      install that release's prebuilt binary
#   $env:KODA_BRANCH = "name"        the branch to fetch

param(
    [string]$Prefix = "$env:LOCALAPPDATA\koda",
    # Branch to build when this script has to clone. It must exist on the
    # remote: the default was "uncensored", which does not, so every
    # `irm | iex` install died on "clone failed". Matches install.sh.
    [string]$Branch = $(if ($env:KODA_BRANCH) { $env:KODA_BRANCH } else { "master" })
)

$ErrorActionPreference = "Stop"
# Like PREFIX for install.sh, naming the location asks for a plain install: no
# menu and no questions -- which is also what makes this scriptable.
$Interactive = [Environment]::UserInteractive -and ($null -ne $Host.UI.RawUI) -and
    -not ($PSBoundParameters -and $PSBoundParameters.ContainsKey("Prefix"))
$Repo = "https://github.com/simpletoolsindia/koda.git"
$BinDir = Join-Path $Prefix "bin"
$Releases = "https://github.com/simpletoolsindia/koda/releases/download"
$Raw = "https://raw.githubusercontent.com/simpletoolsindia/koda"
$CacheDir = Join-Path $env:LOCALAPPDATA "koda\cache"
# Windows PowerShell 5.1 redraws its progress bar for every chunk, which makes
# a 10 MB download take minutes; and it may not offer TLS 1.2 by default.
$ProgressPreference = "SilentlyContinue"
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch { }

function Info($m) { Write-Host "> $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "OK $m" -ForegroundColor Green }
function Warn($m) { Write-Host "! $m" -ForegroundColor Yellow }
function Die($m)  { Write-Host "x $m" -ForegroundColor Red; exit 1 }

# Is this directory a koda checkout, rather than just some Rust project? Piped
# through iex there is no $PSScriptRoot, so without the name check the one-liner
# run inside any other crate would build that crate and install it as koda.
function Test-KodaSrc($dir) {
    if (-not $dir) { return $false }
    $manifest = Join-Path $dir "Cargo.toml"
    if (-not (Test-Path $manifest)) { return $false }
    return [bool](Select-String -Path $manifest -Pattern '^name = "koda"' -Quiet)
}

# A koda checkout beside this script, or the cwd: the developer's own source,
# built as it stands. $null when there is none.
function Get-LocalSrc {
    if (Test-KodaSrc $PSScriptRoot) { return $PSScriptRoot }
    if (Test-KodaSrc (Get-Location).Path) { return (Get-Location).Path }
    return $null
}

# Otherwise a clone kept in the cache. Keeping it (and its target\ directory)
# is what makes an update an incremental rebuild instead of a from-scratch
# compile of every dependency. The cache is the installer's own, so bringing it
# to the branch tip may discard whatever is in it.
function Get-CachedSrc {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Die "git not found." }
    $s = Join-Path $CacheDir "src"
    if ((Test-Path (Join-Path $s ".git")) -and (Test-KodaSrc $s)) {
        Info "fetching the latest $Branch..."
        git -C $s fetch --quiet --depth 1 origin $Branch 2>$null
        if ($LASTEXITCODE -eq 0) {
            git -C $s reset --quiet --hard FETCH_HEAD 2>$null
            if ($LASTEXITCODE -eq 0) { return $s }
        }
        Warn "the cached source at $s is unusable - cloning afresh"
    }
    if (Test-Path $s) { Remove-Item $s -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $CacheDir | Out-Null
    Info "cloning koda ($Branch edition)..."
    git clone --quiet --depth 1 --single-branch --branch $Branch $Repo $s 2>$null
    if ($LASTEXITCODE -ne 0) { Die "clone failed" }
    return $s
}

function Resolve-Src {
    $local = Get-LocalSrc
    if ($local) { return $local }
    return Get-CachedSrc
}

# The path of a downloaded, verified koda.exe, or $null -- and the caller
# builds from source. Only the release whose version is the branch's own is
# taken: "latest release" would install something older than the source asked for.
function Get-Prebuilt {
    if ($env:KODA_FROM_SOURCE) { return $null }
    # Releases are built from master. Another branch -- the uncensored edition
    # -- is a different program under the same version number, so it is built.
    if ($Branch -ne "master" -and -not $env:KODA_VERSION) { return $null }
    # The one Windows build is x64, which Windows on ARM runs under emulation;
    # the run check below settles whether it works here.
    if (-not [Environment]::Is64BitOperatingSystem) { return $null }
    $target = "x86_64-pc-windows-msvc"
    $version = $env:KODA_VERSION
    if (-not $version) {
        try {
            $toml = (Invoke-WebRequest -UseBasicParsing -Uri "$Raw/$Branch/Cargo.toml").Content
            if ($toml -match '(?m)^version = "([^"]+)"') { $version = $Matches[1] }
        } catch { }
        if (-not $version) { return $null }
    }
    $version = $version.TrimStart("v")
    $asset = "koda-$version-$target.zip"
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("koda-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    # The checksum first: it is tiny, and its absence means no such release.
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$Releases/v$version/$asset.sha256" -OutFile "$work\sum"
    } catch {
        if ($env:KODA_VERSION) { Die "no prebuilt koda $version for $target" }
        Info "no prebuilt binary for $version yet - building from source"
        return $null
    }
    Info "downloading koda $version for $target..."
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$Releases/v$version/$asset" -OutFile "$work\$asset"
    } catch {
        Warn "download failed - building from source instead"
        return $null
    }
    $want = ((Get-Content "$work\sum" -Raw).Trim() -split '\s+')[0].ToLower()
    $got = (Get-FileHash "$work\$asset" -Algorithm SHA256).Hash.ToLower()
    # A mismatch is not a network hiccup to shrug off: stop, loudly.
    if ($want -ne $got) { Die "checksum mismatch for $asset (expected $want, got $got)" }
    Expand-Archive -Path "$work\$asset" -DestinationPath "$work\x" -Force
    $exe = Get-ChildItem -Path "$work\x" -Filter "koda.exe" -Recurse | Select-Object -First 1
    if (-not $exe) { Die "$asset does not contain koda.exe" }
    try { & $exe.FullName --version *> $null } catch { }
    if ($LASTEXITCODE -ne 0) {
        Warn "the prebuilt binary does not run here - building from source"
        return $null
    }
    Ok "downloaded and verified (sha256 $($got.Substring(0, 12))...)"
    return $exe.FullName
}

# Returns the built path, so everything else here goes to the host: in
# PowerShell any stray pipeline output would become part of the return value.
function Build-From-Source {
    Ensure-Rust | Out-Host
    $Src = Resolve-Src
    Push-Location $Src
    try {
        Info "building the release binary (a few minutes the first time, seconds after)..."
        # --locked: build exactly the dependency versions that were tested.
        cargo build --release --locked --quiet | Out-Host
        if ($LASTEXITCODE -ne 0) { Die "build failed - see the errors above" }
    } finally { Pop-Location }
    $built = Join-Path $Src "target\release\koda.exe"
    if (-not (Test-Path $built)) { Die "build finished but $built is missing" }
    Ok "built"
    return $built
}

# A checkout is the developer's own work and is always built; for everyone
# else the release binary is tried first.
function Get-KodaBinary {
    if (Get-LocalSrc) { return Build-From-Source }
    $pre = Get-Prebuilt
    if ($pre) { return $pre }
    return Build-From-Source
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
    if (-not $Interactive) {
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
    $Built = Get-KodaBinary
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
    if (-not $Interactive) {
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

    # A checkout is fast-forwarded here; the cache and the download fetch the
    # latest themselves.
    $Src = Get-LocalSrc
    if ($Src -and (Test-Path (Join-Path $Src ".git"))) {
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
        if ($Interactive) {
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
        if ($Interactive) {
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

# Non-interactive host (irm | iex), or -Prefix given: just install.
if (-not $Interactive) {
    Build-And-Install
    exit 0
}

Write-Host ""
Write-Host "  koda installer" -ForegroundColor Cyan -NoNewline
Write-Host "  Windows"
Write-Host ""
Write-Host "  1  Install              ($BinDir)" -ForegroundColor Green
Write-Host "  2  Update to the latest  (download or rebuild)" -ForegroundColor Green
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
