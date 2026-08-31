#!/usr/bin/env bash
# koda installer — one command to build and install koda on macOS or Linux.
#
# From a clone:              ./install.sh
# One-liner (clones for you): curl -fsSL https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.sh | bash
# Install system-wide:        PREFIX=/usr/local ./install.sh   (may need sudo)

set -euo pipefail

REPO="https://github.com/simpletoolsindia/koda.git"
BIN_NAME="koda"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

info()  { printf '\033[36m›\033[0m %s\n' "$1"; }
ok()    { printf '\033[32m✓\033[0m %s\n' "$1"; }
warn()  { printf '\033[33m!\033[0m %s\n' "$1"; }
die()   { printf '\033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }

# 1. Require cargo (the only prerequisite).
command -v cargo >/dev/null 2>&1 || \
    die "Rust/cargo not found. Install from https://rustup.rs (or 'brew install rust'), then re-run."

# 2. Find the source: use this checkout if we're in one, otherwise clone into a
#    temp dir — so the one-line curl form works with nothing set up.
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
cd "$SRC"

# 3. Build.
info "building the release binary (a minute or two the first time)…"
cargo build --release --quiet
BUILT="target/release/$BIN_NAME"
[ -x "$BUILT" ] || die "build finished but $BUILT is missing"
ok "built ($(du -h "$BUILT" | cut -f1))"

# 4. Install.
mkdir -p "$BIN_DIR"
install -m 0755 "$BUILT" "$BIN_DIR/$BIN_NAME"
ok "installed to $BIN_DIR/$BIN_NAME"

# 5. PATH check.
case ":$PATH:" in
    *":$BIN_DIR:"*) ok "$BIN_DIR is on your PATH" ;;
    *) warn "add $BIN_DIR to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

ok "done — run '$BIN_NAME' to start, or '$BIN_NAME --help'"
