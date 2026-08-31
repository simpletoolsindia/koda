# koda installer for Windows (PowerShell).
#
# From a clone:  .\install.ps1
# One-liner:     irm https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.ps1 | iex
#
# Installs to %LOCALAPPDATA%\koda\bin by default; override with -Prefix.

param([string]$Prefix = "$env:LOCALAPPDATA\koda")

$ErrorActionPreference = "Stop"
$Repo = "https://github.com/simpletoolsindia/koda.git"
$BinDir = Join-Path $Prefix "bin"

function Info($m) { Write-Host "> $m" -ForegroundColor Cyan }
function Ok($m)   { Write-Host "OK $m" -ForegroundColor Green }
function Die($m)  { Write-Host "x $m" -ForegroundColor Red; exit 1 }

# 1. Require cargo.
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Die "Rust/cargo not found. Install from https://rustup.rs then re-run."
}

# 2. Find source or clone.
if (Test-Path (Join-Path $PSScriptRoot "Cargo.toml")) {
    $Src = $PSScriptRoot
} elseif (Test-Path "Cargo.toml") {
    $Src = (Get-Location).Path
} else {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { Die "git not found." }
    $Src = Join-Path ([System.IO.Path]::GetTempPath()) "koda"
    Info "cloning koda..."
    git clone --depth 1 $Repo $Src 2>$null
}
Set-Location $Src

# 3. Build.
Info "building the release binary (a minute or two the first time)..."
cargo build --release --quiet
$Built = Join-Path "target\release" "koda.exe"
if (-not (Test-Path $Built)) { Die "build finished but $Built is missing" }
Ok "built"

# 4. Install.
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Copy-Item $Built (Join-Path $BinDir "koda.exe") -Force
Ok "installed to $BinDir\koda.exe"

# 5. PATH hint.
if ($env:Path -notlike "*$BinDir*") {
    Write-Host "! add $BinDir to your PATH (User environment variables)" -ForegroundColor Yellow
}
Ok "done - run 'koda' to start, or 'koda --help'"
