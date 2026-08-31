#!/usr/bin/env bash
# Demonstrates koda's Phase 1 self-improvement loop end to end, with hard
# evidence at each stage. Uses the mock server so it is deterministic and
# offline — no model download, no network.
#
#   OBSERVE  koda logs what it did          -> .koda/learning/observations.jsonl
#   INDUCE   repeated evidence -> a rule     -> .koda/learning/rules.md (candidate)
#   PROMOTE  accept it (what `/learn all` does)
#   INJECT   the accepted rule appears in the REAL request koda sends the model
#            (proven by reading koda's own KODA_DEBUG capture of the body)
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KODA="$ROOT/target/release/koda"
PORT=8137
STATE="$(mktemp -d)"; PROJ="$(mktemp -d)"
export XDG_STATE_HOME="$STATE"   # koda writes its debug capture under here

line(){ printf '\n\033[1;36m== %s ==\033[0m\n' "$1"; }
cleanup(){ kill "$MOCKPID" 2>/dev/null; rm -rf "$PROJ" "$STATE"; }
trap cleanup EXIT

pkill -f mock_server.py 2>/dev/null; sleep 0.3
MOCK_MODE=learn python3 "$ROOT/tests/mock_server.py" "$PORT" >/tmp/koda-demo-mock.log 2>&1 &
MOCKPID=$!; sleep 0.8

cd "$PROJ"; git init -q 2>/dev/null
printf 'learning = true\n' > koda.toml
run(){ "$KODA" -u "http://127.0.0.1:$PORT/v1" -m mock-coder -y -p "$1" >/dev/null 2>&1; }

line "RUN 1 — koda runs a build command (mock: 'echo built')"
run "build the project"
cat .koda/learning/observations.jsonl 2>/dev/null

line "RUN 2 — same command again (2 successes = enough evidence)"
run "build it again"
cat .koda/learning/observations.jsonl 2>/dev/null

line "INDUCED CANDIDATE RULE (deterministic, mined from the log above)"
cat .koda/learning/rules.md 2>/dev/null || echo "(none)"

line "PROMOTE — accept the candidate (exactly what '/learn all' writes)"
# koda's own save format: header, then '## Accepted' with '- [key] text — (n)'.
{
  echo "# koda learned rules"
  echo
  echo "## Accepted"
  grep -A50 '## Candidates' .koda/learning/rules.md | grep '^- \[' 
} > .koda/learning/rules.md.new && mv .koda/learning/rules.md.new .koda/learning/rules.md
cat .koda/learning/rules.md

line "INJECT — run once more with KODA_DEBUG=1; read the REAL request body koda sent"
KODA_DEBUG=1 run "build once more"
REQ="$(ls -t "$STATE"/koda/debug/rr-session-*.json 2>/dev/null | head -1)"
echo "captured request: $REQ"
python3 - "$REQ" <<'PY'
import sys, json
req = json.load(open(sys.argv[1]))
msgs = req.get("body", req).get("messages", [])
sys_prompt = next((m["content"] for m in msgs if m.get("role") == "system"), "")
i = sys_prompt.find("Conventions learned in this project")
print("--- system-prompt slice actually sent to the model ---")
if i != -1:
    print(sys_prompt[i:i+200].rstrip())
    print("\n\033[1;32mPROVEN: the learned rule is in the prompt koda sent.\033[0m")
else:
    print("\033[1;31mFAIL: learned rule not found in the system prompt.\033[0m")
    sys.exit(1)
PY

line "DONE"
