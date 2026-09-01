---
name: qa-live-ui
role: qa
when: Before any release or when changing the TUI, streaming, approvals, modes, slash commands, key handling, or the web UI — verify every UI component works for a real end user against the live model, not mocks
---

# Live UI QA — the human-end-user, zero-bug release gate

koda is a terminal UI. Unit tests and mock-server e2e tests catch logic bugs,
but they cannot tell you what a **person actually sees and can do**: does typing
land in the right box, does the slash dropdown open, does the approval modal
show the diff and accept `y`/`n`, does `/compact` recover, does Ctrl+Up scroll
while plain Up recalls history. This skill is how we prove that — by driving the
**real koda binary** through a pseudo-terminal against the **live MTPLX server**,
with no mocks, exactly as an end user would.

Treat a green run of this suite as the release gate. **Any failure blocks
release** until it is either fixed in koda or proven to be a harness artifact.

## The harness

- `tests/qa/live_qa.py` — the PTY driver. It opens a real pty, launches
  `koda` in it, types like a human (one char at a time with think-time),
  reconstructs the on-screen grid with a small VT100 interpreter, and asserts
  against what the user sees.
- `tests/qa/run_live_qa.sh` — builds the release binary and opens a **separate
  macOS Terminal window** running the QA, so you can watch koda being driven
  live. On non-macOS it runs inline.

Run it:

```bash
./tests/qa/run_live_qa.sh          # watch it live in a new Terminal window
# or, headless / CI:
cargo build --release && BIN=./target/release/koda python3 tests/qa/live_qa.py
```

It reads the endpoint, model and key from `~/.config/koda/config.toml` so it
hits the same MTPLX server koda normally uses. Exit status is non-zero if any
check fails.

## Choosing a backend (local Ollama vs remote)

A release gate must be **reproducible**. A remote box (MTPLX) can be busy or
briefly down, which produces false failures. So the harness prefers a **local
Ollama small model** when one is available — it is fast, always up, and (with
`granite4.1:8b`) supports tool calls, which the approval-modal test needs.

- `QA_BACKEND=auto` (default): use local Ollama if it is up, else the configured
  server.
- `QA_BACKEND=ollama`: force local Ollama (`OLLAMA_URL`, default
  `http://localhost:11434/v1`; picks `granite4.1:8b` or another small
  tool-capable model).
- `QA_BACKEND=config`: force the configured server (e.g. MTPLX).

```bash
QA_BACKEND=ollama BIN=./target/release/koda python3 tests/qa/live_qa.py
```

The harness auto-selects a served, non-embedding model and prints which one it
used. If no server is reachable at all, it exits with status 3 (infra, not a
UI bug) rather than reporting false failures.


## What it verifies (every user-facing component)

- **Startup & status bar**: welcome banner, ready indicator, live model + live
  endpoint, mode chip (EXEC default), workspace name, input placeholder.
- **Input focus**: typed text lands in the composer; Ctrl+U clears it.
- **Streaming**: a real model turn streams a reply; the token counter updates;
  the turn returns to ready.
- **Slash commands**: `/` opens the list; prefix filtering (`/mod`, `/comp`);
  `/help`, `/tools`, `/auto`, `/think`, and an unknown command all respond.
- **@ file mentions**: `@` opens a fuzzy list; Tab inserts the path.
- **Mode switching**: Ctrl+P cycles EXEC → VIBE → PLAN; `/mode execute` resets.
- **!cmd**: a direct shell command runs without a model turn.
- **Up/Down vs Ctrl+Up**: plain Up recalls the previous typed message; Ctrl+Up
  scrolls the transcript.
- **/compact**: shows the animated "compacting…" status, reports completion, and
  — critically — accepts input again afterward (no stuck prompt).
- **Approval modal**: appears for a write, shows the diff and `allow once` /
  `decline` hints; `n` leaves the file unchanged; `y` applies the edit on disk.
- **Interrupt**: Ctrl+C cancels a running turn and returns to ready, still
  accepting input.
- **Paste routing**: pasting into `/setup` lands in the focused field, not the
  chat composer behind it.

When you add a UI component, add a matching `test_*` case here. The inventory
above is the contract — nothing user-facing ships untested.

## Working with a reasoning model (MTPLX)

The MTPLX model streams `reasoning_content` before `content` and can be slow.
So:

- Use generous waits. `wait_saw(needle, seconds)` scans the live screen and all
  past frames (transient states still count). `wait_idle()` waits for the turn
  to finish (the `esc interrupt` hint clears and `ready` returns) before
  asserting on post-turn state like the token counter.
- Never assert on a post-turn value while the model may still be streaming.

## Be hermetic and deterministic

- The harness sets its own `XDG_CONFIG_HOME`, so the user's real config (which
  may enable plan mode or full-auto or name a stale model) can't skew results.
  The default mode is therefore EXEC, as a fresh install sees it.
- It auto-resolves the served model from `/v1/models`: the config may name
  `...-quality` while the server serves `...-speed`. Always test the model the
  server actually offers, or every turn fails for the wrong reason.

## Reading a failure honestly (do not paper over bugs)

A red check is a real bug **until proven otherwise**. When one fails:

1. Look at the printed last-screen dump under the failing check.
2. Reproduce the single step in isolation (a tiny PTY probe) to see koda's raw
   behaviour. Example: Ctrl+P appeared to fail, but an isolated probe showed the
   chip going VIBE → PLAN → EXEC and the transcript printing `mode → vibe` — so
   koda was correct and the harness assertion was too strict.
3. Decide: **koda bug** → fix koda, re-run. **Harness artifact** (timing, a
   label that changed, a list that scrolls, an SGR-mangled cell) → fix the
   assertion to match reality, and write down why. Common artifacts:
   - The status chip can be visually split by truecolor SGR runs in a long
     transcript — also accept koda's own `mode → …` notice.
   - The approval hints are literally `allow once` / `always allow` / `decline`.
   - The slash list scrolls, so assert on prefix **filtering**, not on a
     specific entry being visible in a full list.

Only sign off when the run is green **twice in a row** (no flakiness). That is
the zero-bug bar for shipping the UI.
