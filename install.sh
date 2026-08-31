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

# --- build + copy into $BIN_DIR ----------------------------------------------
build_and_install() {
    local prefix="$1"; local bin_dir="$prefix/bin"
    command -v cargo >/dev/null 2>&1 || \
        die "Rust/cargo not found. Install from https://rustup.rs then re-run."
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
    ok "done — run '$BIN_NAME' to start, or '$BIN_NAME --help'"
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
    printf '\n%s%s  koda installer%s  %smacOS · Linux%s\n\n' \
        "$C_BOLD" "$C_CYAN" "$C_OFF" "$C_DIM" "$C_OFF"
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
