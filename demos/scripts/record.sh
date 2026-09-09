#!/usr/bin/env bash
# Record one koda demo to an asciicast, and render it to a GIF.
#
#   demos/scripts/record.sh <name> <mock-mode> <prompt> [seconds]
#
# The model is `tests/mock_server.py`, which speaks real OpenAI-shaped SSE from
# a fixed script. Everything else is koda: its parser, tool dispatch, approval
# path, diff rendering and status bar all run for real, and the tools really
# touch the disk. Only what the model *says* is scripted — which is what makes
# these reproducible, and fast enough to watch. A real model on this hardware
# emits ~33 tokens/second, so an honest recording of one is mostly dead air.
set -uo pipefail
cd "$(dirname "$0")/../.."

NAME="${1:?usage: record.sh <name> <mock-mode> <prompt> [seconds]}"
MODE="${2:?}"
PROMPT="${3:?}"
SECS="${4:-24}"

PORT=8911
CASTS="demos/casts"
WORK=/tmp/koda-demo-$$
BIN="./target/release/koda"
[ -x "$BIN" ] || BIN="./target/debug/koda"

mkdir -p "$CASTS"
rm -rf "$WORK" && mkdir -p "$WORK"
cp -R demos/fixtures/. "$WORK"/ 2>/dev/null || true
( cd "$WORK" && git init -q . && git add -A >/dev/null 2>&1 && git commit -qm init >/dev/null 2>&1 ) || true

MOCK_MODE="$MODE" python3 tests/mock_server.py "$PORT" >/tmp/koda-demo-mock.log 2>&1 &
MOCK=$!
trap 'kill $MOCK 2>/dev/null; rm -rf "$WORK"' EXIT
for _ in $(seq 1 40); do curl -s -o /dev/null "http://127.0.0.1:$PORT/v1/models" && break; sleep 0.25; done

# An isolated config, so whoever records this does not bake their own provider,
# theme or autonomy tier into the published GIF.
export XDG_CONFIG_HOME="$WORK/.config"
mkdir -p "$XDG_CONFIG_HOME"

STEPS=("type:$PROMPT" "key:enter" "wait:$SECS")

echo "recording $NAME (mode=$MODE, ${SECS}s)…"
# asciicast-v2, not the v3 default: agg 1.9.0 reads v3 without erroring but
# collapses the whole session into two frames, so the GIF comes out empty.
# The terminal size comes from COLUMNS/LINES, which asciinema honours.
COLUMNS=100 LINES=30 KODA_BIN="$PWD/$BIN" asciinema rec "$CASTS/$NAME.cast" \
  --output-format asciicast-v2 --overwrite \
  --command "python3 $PWD/demos/scripts/drive.py $WORK http://127.0.0.1:$PORT/v1 ${STEPS[*]@Q}" \
  >/dev/null 2>&1

if [ ! -s "$CASTS/$NAME.cast" ]; then
  echo "FAILED: no cast written" >&2
  exit 1
fi
echo "  cast  $(du -h "$CASTS/$NAME.cast" | cut -f1)"
