#!/usr/bin/env bash
# koda installer — builds the release binary and drops it on your PATH.
#
# Usage:
#   ./install.sh                 # build here and install to ~/.local/bin
#   PREFIX=/usr/local ./install.sh   # install to /usr/local/bin (may need sudo)
#
# Run it from a checkout of the repo, or pipe it after cloning:
#   git clone https://github.com/simpletoolsindia/koda.git && cd koda && ./install.sh

set -euo pipefail

BIN_NAME="koda"
# Where to install. Default is ~/.local/bin (no sudo). Override with PREFIX.
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

# Pretty output.
info()  { printf '\033[36m›\033[0m %s\n' "$1"; }
ok()    { printf '\033[32m✓\033[0m %s\n' "$1"; }
warn()  { printf '\033[33m!\033[0m %s\n' "$1"; }
die()   { printf '\033[31m✗\033[0m %s\n' "$1" >&2; exit 1; }

# 1. Locate the project. Prefer the script's own directory so it works whether
#    run from the repo root or elsewhere.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"
[ -f Cargo.toml ] || die "run this from the koda repo (no Cargo.toml here)"

# 2. Require cargo.
if ! command -v cargo >/dev/null 2>&1; then
    die "Rust/cargo not found. Install it with 'brew install rust' or from https://rustup.rs"
fi

# 3. Build.
info "building the release binary (this can take a minute the first time)…"
cargo build --release --quiet
BUILT="target/release/$BIN_NAME"
[ -x "$BUILT" ] || die "build succeeded but $BUILT is missing"
ok "built $BUILT ($(du -h "$BUILT" | cut -f1))"

# 4. Install.
mkdir -p "$BIN_DIR"
install -m 0755 "$BUILT" "$BIN_DIR/$BIN_NAME"
ok "installed to $BIN_DIR/$BIN_NAME"

# 5. PATH check.
case ":$PATH:" in
    *":$BIN_DIR:"*) ok "$BIN_DIR is on your PATH" ;;
    *)
        warn "$BIN_DIR is not on your PATH. Add this to your shell profile:"
        printf '\n    export PATH="%s:$PATH"\n\n' "$BIN_DIR"
        ;;
esac

# 6. Done.
ok "run '$BIN_NAME' to start, or '$BIN_NAME --help' for options"
