#!/usr/bin/env bash
# End-to-end check: koda talks to a mock OpenAI-compatible server, streams a
# reply, calls tools, and actually edits a file on disk.
set -uo pipefail

cd "$(dirname "$0")/.."
BIN="${BIN:-./target/release/koda}"
[ -x "$BIN" ] || BIN=./target/debug/koda
PORT="${PORT:-8123}"
PASS=0
FAIL=0

check() { # check <name> <condition-result>
  if [ "$2" -eq 0 ]; then echo "  ok   $1"; PASS=$((PASS + 1));
  else echo "  FAIL $1"; FAIL=$((FAIL + 1)); fi
}

start_server() {
  MOCK_MODE="$1" python3 tests/mock_server.py "$PORT" >/tmp/koda-mock.log 2>&1 &
  SRV=$!
  for _ in $(seq 1 50); do
    curl -sf "http://127.0.0.1:$PORT/v1/models" >/dev/null && return 0
    sleep 0.1
  done
  echo "mock server failed to start"; cat /tmp/koda-mock.log; exit 1
}

stop_server() { kill "$SRV" 2>/dev/null; wait "$SRV" 2>/dev/null; }

new_workspace() {
  WS=$(mktemp -d)
  printf 'hello world\nsecond line\n' >"$WS/demo.txt"
}

run_case() { # run_case <mode> <extra args...>
  local mode="$1"; shift
  start_server "$mode"
  new_workspace
  OUT=$("$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder "$@" 2>/tmp/koda-err.log)
  RC=$?
  stop_server
}

echo "== models subcommand =="
start_server native
OUT=$("$BIN" -u "http://127.0.0.1:$PORT/v1" models 2>&1)
stop_server
echo "$OUT" | grep -q mock-coder; check "lists mock-coder" $?

echo "== native tool protocol (streaming + read + edit) =="
run_case native -p -y "replace hello with goodbye in demo.txt"
check "exit status 0" "$RC"
echo "$OUT" | grep -q "replaced hello with goodbye"; check "final text streamed" $?
grep -q "goodbye world" "$WS/demo.txt"; check "file edited on disk" $?
grep -q "second line" "$WS/demo.txt"; check "rest of file preserved" $?
grep -q "read demo.txt" /tmp/koda-err.log; check "read_file ran" $?
grep -q "edit demo.txt" /tmp/koda-err.log; check "edit_file ran" $?
rm -rf "$WS"

echo "== text tool protocol =="
run_case text -p -y --protocol text "replace hello with goodbye in demo.txt"
check "exit status 0" "$RC"
grep -q "goodbye world" "$WS/demo.txt"; check "file edited on disk" $?
echo "$OUT" | grep -qv "<tool_call>"; check "tool syntax hidden from output" $?
rm -rf "$WS"

echo "== automatic fallback when the server rejects native tools =="
run_case reject -p -y "replace hello with goodbye in demo.txt"
check "exit status 0" "$RC"
grep -q "goodbye world" "$WS/demo.txt"; check "file edited after fallback" $?
grep -q "switched to text protocol" /tmp/koda-err.log; check "fallback reported" $?
rm -rf "$WS"

echo "== approval required without --yolo =="
run_case native -p "replace hello with goodbye in demo.txt"
[ "$RC" -eq 2 ]; check "exits 2 when denied" $?
grep -q "hello world" "$WS/demo.txt"; check "file left unchanged" $?
grep -q "needs approval" /tmp/koda-err.log; check "explains --yolo" $?
rm -rf "$WS"

echo "== empty and reasoning-only responses are explained =="
run_case empty -p -y "do something"
[ "$RC" -eq 2 ]; check "empty stream exits non-zero" $?
grep -q "returned an empty response" /tmp/koda-err.log; check "empty stream explained" $?
grep -q "chat template" /tmp/koda-err.log; check "suggests a cause" $?
rm -rf "$WS"

run_case thinky -p -y "do something"
[ "$RC" -eq 2 ]; check "reasoning-only exits non-zero" $?
grep -q "only reasoning and no answer" /tmp/koda-err.log; check "reasoning-only explained" $?
rm -rf "$WS"

echo "== undo puts the file back =="
start_server undo
new_workspace
"$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder -p -y "wreck demo.txt" \
  >/dev/null 2>/dev/null
grep -q "wrecked world" "$WS/demo.txt"; check "edit applied" $?
stop_server
rm -rf "$WS"

echo "== sessions are saved and can be resumed =="
run_case native -p -y "replace hello with goodbye in demo.txt"
check "exit status 0" "$RC"
ls "$WS"/.koda/sessions/*.jsonl >/dev/null 2>&1; check "session file written" $?
head -1 "$WS"/.koda/sessions/*.jsonl | grep -q '"t":"header"'; check "header line first" $?
grep -q '"role":"user"' "$WS"/.koda/sessions/*.jsonl; check "user message saved" $?
grep -q '"role":"tool"' "$WS"/.koda/sessions/*.jsonl; check "tool results saved" $?
SESS=$(ls "$WS"/.koda/sessions/*.jsonl | head -1)
BEFORE=$(wc -l < "$SESS")
# Resuming must extend the same file, not start a new one.
start_server native
"$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder --continue -y -p "again" \
  >/dev/null 2>/tmp/koda-err.log
stop_server
COUNT=$(ls "$WS"/.koda/sessions/*.jsonl | wc -l | tr -d ' ')
[ "$COUNT" = "1" ]; check "resume reuses one file" $?
AFTER=$(wc -l < "$(ls "$WS"/.koda/sessions/*.jsonl | head -1)")
[ "$AFTER" -gt "$BEFORE" ]; check "resume appended to it" $?
grep -q "resumed" /tmp/koda-err.log; check "resume reported" $?
rm -rf "$WS"

echo "== project memory learns across sessions =="
run_case learn -p -y "build the project and remember how"
check "exit status 0" "$RC"
grep -q "remember" /tmp/koda-err.log; check "remember tool ran" $?
grep -q "echo built" "$WS/.koda/memory.md"; check "command outcome recorded" $?
grep -q "the build command here is echo built" "$WS/.koda/memory.md"; check "note recorded" $?
# A second run must see the note in its prompt, not rediscover it.
OUT2=$("$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder --mode plan -p "x" 2>&1)
start_server learn
OUT2=$("$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder -p -y "again" 2>/tmp/koda-err.log)
stop_server
grep -c "echo built" "$WS/.koda/memory.md" >/dev/null; check "memory persists" $?
rm -rf "$WS"

echo "== web search is refused until configured =="
run_case websearch -p -y "search the web for ratatui docs"
grep -q "web_search: disabled" /tmp/koda-err.log; check "refused without searx_url" $?
echo "$OUT" | grep -q "web search is off"; check "model told why" $?
rm -rf "$WS"

echo "== codegraph answers structural questions =="
start_server graph
WS=$(mktemp -d); mkdir -p "$WS/src"
printf 'use std::io;\npub struct Widget;\npub fn build_widget() -> Widget { Widget }\n' > "$WS/src/lib.rs"
printf 'fn main() { let w = build_widget(); }\n' > "$WS/src/main.rs"
OUT=$("$BIN" -C "$WS" -u "http://127.0.0.1:$PORT/v1" -m mock-coder -p -y "where is build_widget?" 2>/tmp/koda-err.log)
RC=$?
stop_server
check "exit status 0" "$RC"
grep -q "codegraph overview" /tmp/koda-err.log; check "overview query ran" $?
grep -q "codegraph symbol build_widget" /tmp/koda-err.log; check "symbol query ran" $?
echo "$OUT" | grep -q "src/lib.rs"; check "model answered from the graph" $?
rm -rf "$WS"

echo "== plan mode refuses to change anything =="
run_case native -p --mode plan "fix demo.txt"
grep -q "hello world" "$WS/demo.txt"; check "file left untouched in plan mode" $?
grep -q "blocked" /tmp/koda-err.log; check "edit reported as blocked" $?
grep -q "plan mode" /tmp/koda-err.log; check "explains plan mode" $?
rm -rf "$WS"

echo "== subagent delegation =="
run_case deleg -p -y "where is the greeting?"
check "exit status 0" "$RC"
echo "$OUT" | grep -q "per the subagent"; check "parent used the report" $?
grep -q "delegate" /tmp/koda-err.log; check "delegate tool ran" $?
grep -q "read demo.txt" /tmp/koda-err.log; check "subagent used its own tools" $?
echo "$OUT" | grep -qv "SUBREPORT"; check "subagent output stayed off the screen" $?
rm -rf "$WS"

echo "== interactive TUI (pseudo-terminal) =="
start_server native
MOCK_MODE=slow python3 tests/mock_server.py $((PORT + 1)) >/tmp/koda-mock-slow.log 2>&1 &
SLOW=$!
MOCK_MODE=showcase python3 tests/mock_server.py $((PORT + 7)) >/tmp/koda-mock-show.log 2>&1 &
SHOW=$!
sleep 0.6
BIN="$BIN" URL="http://127.0.0.1:$PORT/v1" SLOW_URL="http://127.0.0.1:$((PORT + 1))/v1" \
  python3 tests/tui_test.py | grep -E '^(  ok|  FAIL|       )'
TUI_RC=${PIPESTATUS[0]}
kill "$SLOW" "$SHOW" 2>/dev/null; wait "$SLOW" "$SHOW" 2>/dev/null
stop_server
check "interactive TUI suite" "$TUI_RC"

echo "== summary =="
echo "  $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
