#!/usr/bin/env bash
# koda installer — one command, with a tiny interactive menu, for macOS & Linux.
#
#   From a clone:   ./install.sh
#   One-liner:      curl -fsSL https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.sh | bash
#
# When run in a terminal it shows a menu (install / system-wide / update /
# uninstall / quit). When piped (no terminal, e.g. curl | bash) it just installs
# to ~/.local so the one-liner stays a one-liner. Override the location with
# PREFIX=/usr/local ./install.sh   (system-wide may need sudo).

set -euo pipefail

REPO="https://github.com/simpletoolsindia/koda.git"
BIN_NAME="koda"

C_CYAN=$'\033[36m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'
C_BOLD=$'\033[1m'; C_DIM=$'\033[2m'; C_OFF=$'\033[0m'
info()  { printf '%s›%s %s\n' "$C_CYAN" "$C_OFF" "$1"; }
ok()    { printf '%s✓%s %s\n' "$C_GREEN" "$C_OFF" "$1"; }
warn()  { printf '%s!%s %s\n' "$C_YELLOW" "$C_OFF" "$1"; }
die()   { printf '%s✗%s %s\n' "$C_RED" "$C_OFF" "$1" >&2; exit 1; }

# --- locate the source: this checkout, the cwd, or a fresh clone -------------
resolve_src() {
    if [ -f "$(dirname "${BASH_SOURCE[0]:-$0}")/Cargo.toml" ]; then
        SRC="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
    elif [ -f Cargo.toml ] && grep -q '^name = "koda"' Cargo.toml 2>/dev/null; then
        SRC="$(pwd)"
    else
        command -v git >/dev/null 2>&1 || die "git not found — needed to fetch koda."
        SRC="$(mktemp -d)/koda"
        info "cloning koda…"
        git clone --depth 1 "$REPO" "$SRC" >/dev/null 2>&1 || die "clone failed"
    fi
}

# --- ensure Rust/cargo is available, offering to install it when it isn't -----
ensure_rust() {
    if command -v cargo >/dev/null 2>&1; then
        return 0
    fi
    # cargo may be installed but not on PATH yet (fresh rustup in this shell).
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
        command -v cargo >/dev/null 2>&1 && return 0
    fi
    warn "Rust/cargo not found — koda is built from source and needs it."
    # Non-interactive (piped) installs shouldn't silently run a network installer.
    if [ ! -t 0 ]; then
        die "install Rust from https://rustup.rs, then re-run this installer."
    fi
    printf '  Install Rust now with rustup? [Y/n]: '
    read -r ans
    case "${ans:-y}" in
        [Nn]*) die "install Rust from https://rustup.rs, then re-run." ;;
    esac
    command -v curl >/dev/null 2>&1 || die "curl not found — needed to fetch rustup."
    info "installing Rust via rustup…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y >/dev/null \
        || die "rustup install failed"
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    command -v cargo >/dev/null 2>&1 || die "cargo still not found after installing Rust."
    ok "Rust installed"
}

# --- build + copy into $BIN_DIR ----------------------------------------------
build_and_install() {
    local prefix="$1"; local bin_dir="$prefix/bin"
    ensure_rust
    resolve_src
    cd "$SRC"
    info "building the release binary (a minute or two the first time)…"
    cargo build --release --quiet
    local built="target/release/$BIN_NAME"
    [ -x "$built" ] || die "build finished but $built is missing"
    ok "built ($(du -h "$built" | cut -f1))"

    mkdir -p "$bin_dir"
    install -m 0755 "$built" "$bin_dir/$BIN_NAME"
    # macOS kills a copied ad-hoc-signed binary; re-sign in place so it runs.
    if [ "$(uname)" = "Darwin" ] && command -v codesign >/dev/null 2>&1; then
        codesign --force --sign - "$bin_dir/$BIN_NAME" >/dev/null 2>&1 || true
    fi
    ok "installed to $bin_dir/$BIN_NAME"

    case ":$PATH:" in
        *":$bin_dir:"*) ok "$bin_dir is on your PATH" ;;
        *) warn "add $bin_dir to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
    esac
    ensure_ripgrep
    ok "done — run '$BIN_NAME' to start, or '$BIN_NAME --help'"
}

# --- optional speedup: ripgrep -----------------------------------------------
# koda's `search` uses ripgrep (rg) when present for speed, and falls back to a
# built-in in-process search otherwise — so rg is never required. Offer to
# install it as a one-time speedup; any failure is non-fatal (koda still works).
ensure_ripgrep() {
    if command -v rg >/dev/null 2>&1; then
        ok "ripgrep found — koda will use it for fast search"
        return 0
    fi
    info "ripgrep (rg) not found — koda will use its built-in search."
    local installer=""
    if [ "$(uname)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
        installer="brew install ripgrep"
    elif command -v apt-get >/dev/null 2>&1; then
        installer="sudo apt-get install -y ripgrep"
    elif command -v dnf >/dev/null 2>&1; then
        installer="sudo dnf install -y ripgrep"
    elif command -v pacman >/dev/null 2>&1; then
        installer="sudo pacman -S --noconfirm ripgrep"
    elif command -v zypper >/dev/null 2>&1; then
        installer="sudo zypper install -y ripgrep"
    elif command -v apk >/dev/null 2>&1; then
        installer="sudo apk add ripgrep"
    elif command -v cargo >/dev/null 2>&1; then
        installer="cargo install ripgrep"
    fi
    if [ -z "$installer" ]; then
        warn "no known package manager — install ripgrep for faster search: https://github.com/BurntSushi/ripgrep#installation"
        return 0
    fi
    # Non-interactive (piped) installs shouldn't run a package manager silently.
    if [ ! -t 0 ]; then
        warn "for faster search, install ripgrep:  $installer"
        return 0
    fi
    printf '  Install ripgrep now for faster search? [Y/n]: '
    read -r ans
    case "${ans:-y}" in
        [Nn]*) info "skipping ripgrep — koda's built-in search still works"; return 0 ;;
    esac
    info "installing ripgrep…"
    if eval "$installer" >/dev/null 2>&1 && command -v rg >/dev/null 2>&1; then
        ok "ripgrep installed"
    else
        warn "ripgrep install failed — koda will use its built-in search (no action needed)"
    fi
}

uninstall() {
    local prefix="$1"; local bin_dir="$prefix/bin"
    if [ -f "$bin_dir/$BIN_NAME" ]; then
        rm -f "$bin_dir/$BIN_NAME"
        ok "removed $bin_dir/$BIN_NAME"
    else
        warn "no koda binary found at $bin_dir/$BIN_NAME"
    fi
}

banner() {
    local os arch
    os="$(uname -s 2>/dev/null || echo unknown)"
    arch="$(uname -m 2>/dev/null || echo unknown)"
    printf '\n%s%s  koda installer%s  %s%s %s%s\n\n' \
        "$C_BOLD" "$C_CYAN" "$C_OFF" "$C_DIM" "$os" "$arch" "$C_OFF"
}

# --- entrypoint --------------------------------------------------------------
USER_PREFIX="${PREFIX:-$HOME/.local}"
SYS_PREFIX="/usr/local"

# Non-interactive (piped, or PREFIX set explicitly): just install and exit.
if [ ! -t 0 ] || [ -n "${PREFIX:-}" ]; then
    build_and_install "$USER_PREFIX"
    exit 0
fi

banner
printf '  %s1%s  Install for me            %s(%s)%s\n' "$C_GREEN" "$C_OFF" "$C_DIM" "$USER_PREFIX/bin" "$C_OFF"
printf '  %s2%s  Install system-wide       %s(%s, may need sudo)%s\n' "$C_GREEN" "$C_OFF" "$C_DIM" "$SYS_PREFIX/bin" "$C_OFF"
printf '  %s3%s  Update to the latest\n' "$C_GREEN" "$C_OFF"
printf '  %s4%s  Uninstall\n' "$C_GREEN" "$C_OFF"
printf '  %s5%s  Quit\n\n' "$C_GREEN" "$C_OFF"
printf '  choose [1]: '
read -r choice
choice="${choice:-1}"
echo

case "$choice" in
    1) build_and_install "$USER_PREFIX" ;;
    2) build_and_install "$SYS_PREFIX" ;;
    3) build_and_install "$USER_PREFIX" ;;
    4) uninstall "$USER_PREFIX" ;;
    5|q|Q) info "nothing to do"; exit 0 ;;
    *) die "unknown choice: $choice" ;;
esac
