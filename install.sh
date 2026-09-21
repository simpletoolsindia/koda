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
#
# Fast path: when a published release matches the version on the branch, its
# prebuilt binary is downloaded and checked against its SHA-256 -- seconds, and
# no Rust toolchain. Otherwise koda is built from source, in a clone kept under
# ~/.cache/koda so the next update rebuilds only what changed. Run from a koda
# checkout, the checkout itself is built.
#
#   KODA_FROM_SOURCE=1   always build from source (the tip of the branch)
#   KODA_VERSION=0.1.0   install that release's prebuilt binary
#   KODA_BRANCH=name     the branch to fetch (default below)

set -euo pipefail

REPO="https://github.com/simpletoolsindia/koda.git"
BIN_NAME="koda"
# Branch to build when this script has to clone. It must exist on the remote:
# the default was `uncensored`, which does not, so every `curl | bash` install
# died on "clone failed" with nothing to act on. Override with KODA_BRANCH.
BRANCH="${KODA_BRANCH:-master}"
RELEASES="https://github.com/simpletoolsindia/koda/releases/download"
RAW="https://raw.githubusercontent.com/simpletoolsindia/koda"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/koda"

C_CYAN=$'\033[36m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'
C_BOLD=$'\033[1m'; C_DIM=$'\033[2m'; C_OFF=$'\033[0m'
info()  { printf '%s›%s %s\n' "$C_CYAN" "$C_OFF" "$1"; }
ok()    { printf '%s✓%s %s\n' "$C_GREEN" "$C_OFF" "$1"; }
warn()  { printf '%s!%s %s\n' "$C_YELLOW" "$C_OFF" "$1"; }
die()   { printf '%s✗%s %s\n' "$C_RED" "$C_OFF" "$1" >&2; exit 1; }

# --- locate the source: this checkout, the cwd, or a fresh clone -------------
# Is this directory a koda checkout, rather than just some Rust project?
# Piped through bash, `dirname "$0"` is ".", so without the name check the
# one-liner run inside any other crate would happily build that crate and
# install it as koda.
is_koda_src() {
    [ -f "$1/Cargo.toml" ] && grep -q '^name = "koda"' "$1/Cargo.toml" 2>/dev/null
}

# A koda checkout next to this script, or the cwd: the developer's own source,
# which is built as it stands.
local_src() {
    local here
    here="$(dirname "${BASH_SOURCE[0]:-$0}")"
    if is_koda_src "$here"; then
        SRC="$(cd "$here" && pwd)"
    elif is_koda_src "$(pwd)"; then
        SRC="$(pwd)"
    else
        return 1
    fi
}

# Otherwise a clone kept in the cache. Keeping it (and its target/ directory)
# is what makes an update an incremental rebuild instead of a from-scratch
# compile of every dependency. The cache is the installer's own, so bringing it
# to the branch tip may discard whatever is in it.
cached_src() {
    command -v git >/dev/null 2>&1 || die "git not found — needed to fetch koda."
    SRC="$CACHE_DIR/src"
    if [ -d "$SRC/.git" ] && is_koda_src "$SRC"; then
        info "fetching the latest ${BRANCH}…"
        if git -C "$SRC" fetch --quiet --depth 1 origin "$BRANCH" 2>/dev/null \
            && git -C "$SRC" reset --quiet --hard FETCH_HEAD 2>/dev/null; then
            return 0
        fi
        warn "the cached source at $SRC is unusable — cloning afresh"
    fi
    rm -rf "$SRC"
    mkdir -p "$CACHE_DIR"
    info "cloning koda ($BRANCH edition)…"
    git clone --quiet --depth 1 --single-branch --branch "$BRANCH" "$REPO" "$SRC" \
        >/dev/null 2>&1 || die "clone failed"
}

resolve_src() {
    local_src || cached_src
}

# --- the prebuilt fast path ----------------------------------------------------
# curl or wget, whichever is here: to a file, or to stdout without one.
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 2 --connect-timeout 10 -o "${2:--}" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -T 10 -O "${2:--}" "$1"
    else
        return 1
    fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        return 1
    fi
}

# The release target this machine runs, as the release workflow names it.
target_triple() {
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64|Linux/amd64)   echo x86_64-unknown-linux-gnu ;;
        Linux/aarch64|Linux/arm64)  echo aarch64-unknown-linux-gnu ;;
        Darwin/x86_64)              echo x86_64-apple-darwin ;;
        Darwin/arm64|Darwin/aarch64) echo aarch64-apple-darwin ;;
        *) return 1 ;;
    esac
}

WORK=""
cleanup() { [ -z "$WORK" ] || rm -rf "$WORK"; }
trap cleanup EXIT

# Sets BUILT to a downloaded, verified binary, or returns 1 -- quietly when
# there is simply nothing to download -- and the caller builds from source.
#
# Only the release whose version is the branch's own is taken: "latest release"
# would happily install something older than the source the user asked for.
download_prebuilt() {
    [ -z "${KODA_FROM_SOURCE:-}" ] || return 1
    # Releases are built from master. Another branch -- the uncensored edition
    # -- is a different program under the same version number, so it is built.
    [ "$BRANCH" = "master" ] || [ -n "${KODA_VERSION:-}" ] || return 1
    local target version asset want got bin
    target="$(target_triple)" || return 1
    version="${KODA_VERSION:-}"
    if [ -z "$version" ]; then
        version="$(fetch "$RAW/$BRANCH/Cargo.toml" 2>/dev/null \
            | sed -n 's/^version = "\(.*\)"$/\1/p' | head -1)" || true
        [ -n "$version" ] || return 1
    fi
    version="${version#v}"
    asset="koda-${version}-${target}.tar.gz"
    WORK="$(mktemp -d)"
    # The checksum first: it is tiny, and its absence means no such release.
    if ! fetch "$RELEASES/v$version/$asset.sha256" "$WORK/sum" 2>/dev/null; then
        [ -z "${KODA_VERSION:-}" ] || die "no prebuilt koda $version for $target"
        info "no prebuilt binary for $version yet — building from source"
        return 1
    fi
    info "downloading koda $version for ${target}…"
    fetch "$RELEASES/v$version/$asset" "$WORK/$asset" || {
        warn "download failed — building from source instead"
        return 1
    }
    want="$(cut -d' ' -f1 < "$WORK/sum")"
    got="$(sha256_of "$WORK/$asset")" || {
        warn "no sha256sum or shasum to verify the download — building from source"
        return 1
    }
    # A mismatch is not a network hiccup to shrug off: stop, loudly.
    [ "$want" = "$got" ] || die "checksum mismatch for $asset (expected $want, got $got)"
    tar -xzf "$WORK/$asset" -C "$WORK" || die "could not unpack $asset"
    bin="$WORK/koda-${version}-${target}/$BIN_NAME"
    [ -f "$bin" ] || die "$asset does not contain $BIN_NAME"
    chmod +x "$bin"
    # Linux builds link the build machine's glibc; an older or musl system
    # cannot run them. Find that out here, and compile there instead.
    if ! "$bin" --version >/dev/null 2>&1; then
        warn "the prebuilt binary does not run here — building from source"
        return 1
    fi
    ok "downloaded and verified (sha256 $(printf '%s' "$got" | cut -c1-12)…)"
    BUILT="$bin"
}

# --- the browse tool's engine ------------------------------------------------
# koda ships the engine and installs it itself (`koda browser install`), so this
# needs no npm, no Node, and no second step from the user. Best-effort: the
# browse tool only needs it when browser=true, and a network hiccup must not
# fail the whole install.
ensure_browse_engine() {
    local koda="$1"
    info "fetching the browse engine…"
    # No stdin: piped through bash, stdin is the rest of this script.
    if "$koda" browser install </dev/null >/dev/null 2>&1; then
        ok "browse engine ready"
    else
        warn "could not fetch the browse engine — koda still runs; get it later with:"
        warn "  $koda browser install"
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

# --- get a binary: download it, or build it ------------------------------------
build_from_source() {
    ensure_rust
    resolve_src
    cd "$SRC"
    info "building the release binary (a few minutes the first time, seconds after)…"
    # --locked: build exactly the dependency versions that were tested, and do
    # not stop to re-resolve the whole graph against the registry.
    cargo build --release --locked --quiet \
        || die "build failed — see the errors above"
    BUILT="$SRC/target/release/$BIN_NAME"
    [ -x "$BUILT" ] || die "build finished but $BUILT is missing"
    ok "built ($(du -h "$BUILT" | awk '{print $1}'))"
}

# Sets BUILT. A checkout is the developer's own work and is always built; for
# everyone else the release binary is tried first.
obtain_binary() {
    if local_src; then
        build_from_source
    elif ! download_prebuilt; then
        build_from_source
    fi
}

# Check before fetching anything: finding out that /usr/local/bin needs root is
# worth knowing now rather than two minutes into a compile.
check_writable() {
    local prefix="$1"
    if [ ! -w "$prefix" ] && [ ! -w "$prefix/bin" ] && [ ! -w "$(dirname "$prefix")" ]; then
        die "cannot write $prefix/bin — re-run with sudo, or install for yourself with PREFIX=\$HOME/.local"
    fi
}

# --- copy BUILT into <prefix>/bin ----------------------------------------------
install_binary() {
    local bin_dir="$1/bin"
    mkdir -p "$bin_dir"
    # Install beside, then rename over. Writing straight onto the destination
    # fails with "text file busy" when the koda being replaced is running --
    # which is exactly when people re-run this script -- while a rename works
    # even then, and is atomic: the binary is never half-written.
    local staged="$bin_dir/.$BIN_NAME.new"
    install -m 0755 "$BUILT" "$staged"
    # macOS kills a copied ad-hoc-signed binary; re-sign before it is in place.
    if [ "$(uname)" = "Darwin" ] && command -v codesign >/dev/null 2>&1; then
        codesign --force --sign - "$staged" >/dev/null 2>&1 || true
    fi
    mv -f "$staged" "$bin_dir/$BIN_NAME" || {
        rm -f "$staged"
        die "could not install to $bin_dir/$BIN_NAME"
    }
    ok "installed to $bin_dir/$BIN_NAME"
}

# Everything after the binary is in place: once, however many copies there are.
finish() {
    local bin_dir="$1/bin"
    ensure_browse_engine "$bin_dir/$BIN_NAME"
    case ":$PATH:" in
        *":$bin_dir:"*) ok "$bin_dir is on your PATH" ;;
        *) warn "add $bin_dir to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
    esac
    ensure_ripgrep
    ensure_tesseract
    ok "done — run '$BIN_NAME' to start, or '$BIN_NAME --help'"
}

build_and_install() {
    check_writable "$1"
    obtain_binary
    install_binary "$1"
    finish "$1"
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

# --- optional: image OCR ------------------------------------------------------
# Attaching a picture to a model that cannot see one goes through OCR, which has
# two backends: a vision model named in `ocr_model`, which needs nothing
# installed and reads layout, tables and handwriting far better; and the
# `tesseract` CLI as the offline fallback.
#
# Installed the same way as ripgrep above: offered, attempted, and non-fatal --
# a failure here costs the offline fallback, not koda.
ensure_tesseract() {
    if command -v tesseract >/dev/null 2>&1; then
        ok "tesseract found — offline image OCR is available (turn it on in /settings)"
        return 0
    fi
    info "tesseract (image OCR) not found."
    local installer=""
    if [ "$(uname)" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
        installer="brew install tesseract"
    elif command -v apt-get >/dev/null 2>&1; then
        installer="sudo apt-get install -y tesseract-ocr"
    elif command -v dnf >/dev/null 2>&1; then
        installer="sudo dnf install -y tesseract"
    elif command -v pacman >/dev/null 2>&1; then
        # The engine alone recognises nothing; the language data is a separate
        # package here, unlike every other manager in this list.
        installer="sudo pacman -S --noconfirm tesseract tesseract-data-eng"
    elif command -v zypper >/dev/null 2>&1; then
        installer="sudo zypper install -y tesseract-ocr"
    elif command -v apk >/dev/null 2>&1; then
        installer="sudo apk add tesseract-ocr"
    fi
    if [ -z "$installer" ]; then
        warn "no known package manager — install tesseract for image OCR: https://tesseract-ocr.github.io/tessdoc/Installation.html"
        return 0
    fi
    # Every command above but brew begins with sudo. A piped install (curl |
    # bash) has no terminal to ask at and no way to show a password prompt, so
    # it prints the command instead of escalating on the user's behalf -- the
    # same line ensure_ripgrep draws, and for a stronger reason here.
    if [ ! -t 0 ]; then
        warn "for offline image OCR, install tesseract:  $installer"
        return 0
    fi
    printf '  Install tesseract now for offline image OCR? [Y/n]: '
    read -r ans
    case "${ans:-y}" in
        [Nn]*)
            info "skipping tesseract — 'ocr vision model' in /settings does OCR without it"
            return 0
            ;;
    esac
    info "installing tesseract…"
    if eval "$installer" >/dev/null 2>&1 && command -v tesseract >/dev/null 2>&1; then
        ok "tesseract installed — turn OCR on in /settings"
    else
        warn "tesseract install failed — koda still runs; for OCR either install it"
        warn "by hand ($installer) or set 'ocr vision model' in /settings"
    fi
}

# --- where koda is actually installed ----------------------------------------
# Both prefixes plus whatever is first on PATH, deduplicated by real path: a
# system-wide install used to be invisible to uninstall, and updating the copy
# in ~/.local while a stale /usr/local one shadowed it on PATH looked, to the
# user, as though the update had silently done nothing.
find_installs() {
    local seen=" " cand real
    for cand in "$USER_PREFIX/bin/$BIN_NAME" "$SYS_PREFIX/bin/$BIN_NAME" \
                "$(command -v "$BIN_NAME" 2>/dev/null || true)"; do
        [ -n "$cand" ] && [ -f "$cand" ] || continue
        real="$(cd "$(dirname "$cand")" && pwd)/$(basename "$cand")"
        case "$seen" in *" $real "*) continue ;; esac
        seen="$seen$real "
        printf '%s\n' "$real"
    done
}

# Removing from a system prefix needs root; asking for it only when the path
# really is unwritable keeps the common ~/.local case password-free.
maybe_sudo() {
    local target="$1"; shift
    if [ -w "$(dirname "$target")" ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        warn "$target needs elevated permission"
        sudo "$@"
    else
        die "cannot write $target and sudo is not available"
    fi
}

# The `|| echo` guarded `head`, not the binary: a koda that could not run left
# this empty, and the update line read "updated:  → koda 0.1.0".
version_of() {
    local v
    v="$("$1" --version 2>/dev/null | head -1)"
    printf '%s' "${v:-unknown}"
}

# --- update: fetch the latest source, then rebuild ---------------------------
# The old option 3 was byte-for-byte identical to option 1: it rebuilt whatever
# happened to be checked out and never fetched anything, so "update to the
# latest" reinstalled the same version and reported success.
update() {
    local targets first before
    targets="$(find_installs)"
    if [ -z "$targets" ]; then
        warn "koda is not installed yet — installing instead"
        build_and_install "$USER_PREFIX"
        return
    fi
    first="$(printf '%s\n' "$targets" | head -1)"
    before="$(version_of "$first")"
    info "installed: $before"
    printf '%s\n' "$targets" | while IFS= read -r t; do printf '    %s\n' "$t"; done

    # A checkout is fast-forwarded here; the cache and the download fetch
    # the latest themselves.
    if local_src && [ -d "$SRC/.git" ]; then
        command -v git >/dev/null 2>&1 || die "git not found — needed to fetch updates."
        info "fetching the latest source…"
        # --ff-only: a fast-forward is an update. Anything else means local
        # commits or a diverged branch, which is the user's to resolve -- an
        # installer must not rewrite or discard their work to save a step.
        if ! git -C "$SRC" pull --ff-only >/dev/null 2>&1; then
            warn "could not fast-forward $SRC (local changes or a diverged branch)"
            warn "rebuilding from the source as it stands"
        fi
    fi

    # Fetch or build once, then update every copy found, so a shadowed one
    # cannot keep serving old code.
    #
    # A `while read ... done <<EOF` loop would redirect stdin to the heredoc for
    # everything inside it, and the steps below prompt on stdin -- they would
    # see a non-tty and take the non-interactive branch. Splitting on newlines
    # with IFS leaves stdin alone.
    local prefix t oldifs="$IFS"
    IFS='
'
    for t in $targets; do
        IFS="$oldifs"
        [ -n "$t" ] && check_writable "$(dirname "$(dirname "$t")")"
        IFS='
'
    done
    IFS="$oldifs"
    obtain_binary
    IFS='
'
    for t in $targets; do
        IFS="$oldifs"
        [ -n "$t" ] || continue
        prefix="$(dirname "$(dirname "$t")")"
        install_binary "$prefix"
        IFS='
'
    done
    IFS="$oldifs"
    finish "$(dirname "$(dirname "$first")")"
    ok "updated: $before → $(version_of "$first")"
}

# --- uninstall ---------------------------------------------------------------
uninstall() {
    local targets t n ans
    targets="$(find_installs)"
    if [ -z "$targets" ]; then
        warn "no koda binary found in $USER_PREFIX/bin, $SYS_PREFIX/bin, or on your PATH"
    else
        n="$(printf '%s\n' "$targets" | wc -l | tr -d ' ')"
        info "found:"
        printf '%s\n' "$targets" | while IFS= read -r t; do printf '    %s\n' "$t"; done
        # Deleting is not the safe default. Without a terminal there is no way
        # to ask, so refuse and say how to confirm rather than assuming yes --
        # an installer that cannot ask should never guess in favour of removal.
        if [ -t 0 ]; then
            if [ "$n" -gt 1 ]; then
                printf '  Remove these %s binaries? [y/N]: ' "$n"
            else
                printf '  Remove it? [y/N]: '
            fi
            read -r ans
            case "${ans:-n}" in [Yy]*) ;; *) info "left alone"; return 0 ;; esac
        elif [ -z "${KODA_UNINSTALL_YES:-}" ]; then
            warn "not a terminal, so nothing was removed"
            warn "re-run in a terminal, or set KODA_UNINSTALL_YES=1 to confirm"
            return 0
        fi
        local oldifs="$IFS"
        IFS='
'
        for t in $targets; do
            IFS="$oldifs"
            [ -n "$t" ] || continue
            maybe_sudo "$t" rm -f "$t" && ok "removed $t"
            IFS='
'
        done
        IFS="$oldifs"
    fi

    # Config is deliberately a separate question and defaults to no: it holds
    # the endpoint, model and API key, which are tedious to set up again and
    # nothing to do with the binary being present.
    local cfg="${XDG_CONFIG_HOME:-$HOME/.config}/$BIN_NAME"
    if [ -d "$cfg" ]; then
        if [ -t 0 ]; then
            printf '  Also delete your settings at %s? [y/N]: ' "$cfg"
            read -r ans
            case "${ans:-n}" in
                [Yy]*) rm -rf "$cfg" && ok "removed $cfg" ;;
                *) info "kept your settings at $cfg" ;;
            esac
        else
            info "your settings are kept at $cfg"
        fi
    fi

    # Per-project state lives in <project>/.koda and is the user's data; say
    # where it is rather than hunting the filesystem for directories to delete.
    info "per-project data (sessions, memory, skills) stays in each project's .koda/"

    if command -v "$BIN_NAME" >/dev/null 2>&1; then
        warn "'$BIN_NAME' is still on your PATH at $(command -v "$BIN_NAME") — remove it by hand"
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
printf '  %s3%s  Update to the latest      %s(download or rebuild)%s\n' "$C_GREEN" "$C_OFF" "$C_DIM" "$C_OFF"
printf '  %s4%s  Uninstall                 %s(binary; asks about settings)%s\n' "$C_GREEN" "$C_OFF" "$C_DIM" "$C_OFF"
printf '  %s5%s  Quit\n\n' "$C_GREEN" "$C_OFF"
printf '  choose [1]: '
read -r choice
choice="${choice:-1}"
echo

case "$choice" in
    1) build_and_install "$USER_PREFIX" ;;
    2) build_and_install "$SYS_PREFIX" ;;
    3) update ;;
    4) uninstall ;;
    5|q|Q) info "nothing to do"; exit 0 ;;
    *) die "unknown choice: $choice" ;;
esac
