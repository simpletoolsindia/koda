# Plan: koda trace-first web control center

**Status: implemented.** Trace capture lives in `src/trace.rs` (hooked from
`agent.rs` and `llm.rs`), the API in `src/webui.rs`, the page in `web-ui/src/`.
Verified by `cargo test`, `tests/visual/webui.spec.js` (fixture API),
`tests/probe_trace.py` (real koda, real turn, live control) and
`tests/probe_web_live.py` (real browser against the real API). See the Web
control center sections of `README.md` and `docs/USER_GUIDE.md`.

Goal: replace the five disconnected viewer tabs with one page that traces every
agent turn end-to-end (LLM requests, responses, reasoning, tool calls) and lets
you manage all of koda from the same surface.

## Stage 1 — Trace model + capture (Rust)

New `src/trace.rs`: an in-memory ring of turns, shared like `log.rs`.

```
Turn   { id, started, ended, mode, model, endpoint, user_input, status, steps[], reply, tokens }
Step   { seq, kind: ModelCall | ToolCall, started, ms, ... }
ModelCall { request_json, response_sse, reasoning, text, finish_reason, retries, prompt_tokens, completion_tokens, error }
ToolCall  { name, args, ok, summary, detail, approval: Auto|Approved|Denied, diff }
```

Capture points in `src/agent.rs`:
- `turn()` start/end → open/close a `Turn` (reuse `turn_seq`).
- `stream_step()` → one `ModelCall` (request built there; response from `StepAcc`).
- `execute()` → one `ToolCall` (already has args, outcome, timing, approval).
- `compact()` → a `Compaction` step so context loss is visible in the trace.

Keep it cheap: cap ~50 turns and truncate payloads (like `debug.rs` CAP).

## Stage 2 — API (src/webui.rs)

- `GET /api/trace` — turn summaries (id, input, status, ms, step count, tokens).
- `GET /api/trace/{id}` — one full turn with all step payloads.
- `GET /api/events` — extend with a `trace` event so the in-flight turn streams live.
- `GET/POST /api/config` — live-editable: model, endpoint, mode, autonomy tier,
  toggles (learning, memory, codegraph, websearch, debug). POST maps to the
  existing `Command::UpdateConfig` path.
- `GET/POST /api/memory`, `GET/POST /api/learning` (accept/reject candidates).
- `GET /api/sessions`, `POST /api/sessions/{id}/resume|fork`.
- `GET /api/codegraph/symbol?name=` — on-demand symbol query.

## Stage 3 — UI (web-ui/, no bundler)

Single page, three regions:

1. **Turn rail (left)** — reverse-chronological turns; live one pinned on top
   with a progress indicator; status colour = semantic only.
2. **Trace waterfall (centre)** — steps as a timeline with duration bars.
   Model calls and tool calls visually distinct; failures/retries inline;
   click a step to inspect. Streaming appends steps as they happen.
3. **Inspector + control rail (right)** — tabs for Request / Response /
   Reasoning / Tool payload, with prompt diff against the previous model call
   (shows what compaction and learned rules changed). Below it: model, mode,
   autonomy, toggles, memory, learned rules, sessions.

Plus a `Cmd+K` command palette: jump to turn, switch model/mode, toggle a
feature, query a symbol, export a trace.

Keep the current architecture: `_head.html` + components + `App.jsx`
concatenated by `build.sh`, React UMD + Babel + Tailwind CDN, tokenized CSS
variables already in place.

## Stage 4 — Verification

- Rust: trace assembly (a turn with 2 model calls + 3 tools reconstructs in
  order), payload truncation, each new endpoint round-trips, config POST
  rejects bad input.
- Playwright: live trace appears while a turn runs, step inspection shows the
  real request/response, config edit persists, mobile layout has no
  horizontal overflow.
- Live PTY QA gate stays green (`tests/qa/live_qa.py`).

## Order

1. `trace.rs` + agent capture + unit tests.
2. Trace endpoints + SSE + tests.
3. Waterfall UI + inspector.
4. Management endpoints + control rail.
5. Command palette, export, polish.
6. Full test run, commit, push, install.
