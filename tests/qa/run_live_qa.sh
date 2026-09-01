#!/usr/bin/env bash
# run_live_qa.sh — open a SEPARATE macOS Terminal window and run the live UI QA
# there, so you can watch koda being driven like a real end user in real time.
#
#   ./tests/qa/run_live_qa.sh
#
# It builds the release binary first, then launches the PTY QA against the LIVE
# MTPLX server (endpoint/model/key from ~/.config/koda/config.toml). The window
# stays open at the end so you can read the pass/fail summary.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$REPO/target/release/koda"

echo "› building release binary…"
( cd "$REPO" && cargo build --release --quiet )
[ -x "$BIN" ] || { echo "build failed: $BIN missing"; exit 1; }
echo "✓ built"

# The command the new window runs. Keep it self-contained. Default to the local
# Ollama backend for a stable, reproducible gate (override with QA_BACKEND).
QA_BACKEND="${QA_BACKEND:-auto}"
RUN_CMD="cd $(printf %q "$REPO") && \
clear && \
echo '════════════════════════════════════════════════' && \
echo '  koda LIVE UI QA — watch it drive koda for you' && \
echo '════════════════════════════════════════════════' && \
QA_BACKEND=$(printf %q "$QA_BACKEND") BIN=$(printf %q "$BIN") python3 tests/qa/live_qa.py; \
echo; echo 'QA finished — press any key to close.'; read -n 1 -s"

if [ "$(uname)" = "Darwin" ]; then
  # Open a new Terminal.app window and run the QA there.
  osascript >/dev/null <<OSA
tell application "Terminal"
    activate
    do script "$(printf '%s' "$RUN_CMD" | sed 's/\\\\/\\\\\\\\/g; s/"/\\\\"/g')"
end tell
OSA
  echo "✓ launched the live QA in a new Terminal window — watch it there."
else
  # Non-macOS fallback: run inline.
  echo "! not macOS — running the QA inline instead of a new window:"
  cd "$REPO"
  BIN="$BIN" python3 tests/qa/live_qa.py
fi
