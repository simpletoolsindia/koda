# Research: oh-my-pi scroll/history model + debugger → design for koda

Research engineer notes for the koda team. Two topics: (1) how oh-my-pi
resolves the up/down scroll-vs-history conflict, and (2) what its `/debug`
actually does (log viewing vs real breakpoint debugging), with concrete,
implementable recommendations for koda and effort estimates.

Everything below is read from `/Users/sridhar/research/refs/oh-my-pi` (TypeScript
/ Bun monorepo) and cross-checked against koda's current Rust source. **No koda
source was modified.** File paths are given so each claim is verifiable.

> Architectural caveat that drives every effort estimate: **oh-my-pi is
> TypeScript/Bun; koda is a single Rust binary** (`src/tui.rs`, `src/view.rs`,
> `src/editor.rs`, `src/debug.rs`). koda cannot import oh-my-pi's code. The DAP
> subsystem in particular is ~110 KB of TS that would have to be re-implemented
> in Rust or delegated to an external process. Recommendations account for this.

---

## Part 1 — Scroll vs. input-history model

### 1.1 What oh-my-pi does (exact mechanism)

**Key finding: oh-my-pi never scrolls the transcript in-app.** The transcript is
committed to the terminal's *native scrollback* as an append-only history
stream, and the renderer explicitly "never probes the user's scroll position."
So there is no in-app scroll-vs-history conflict to resolve for the transcript —
they live in two different planes:

- **Transcript scrollback** → owned by the terminal emulator (mouse wheel, the
  terminal's own PgUp/scrollbar). Source of truth:
  `docs/tui-core-renderer.md` §1/§7 and `docs/tui-runtime-internals.md`
  ("Terminal write path"). The core invariant (renderer §7.8): *"The renderer
  never probes terminal scroll position."* Finalized rows are pushed via a
  monotonic `HistoryBatch` append stream (`packages/tui/src/tui.ts`,
  `TerminalFramePlan { history?, viewport }`).
- **Up/Down arrows** → handled entirely inside the editor component, never the
  transcript.

(a) **Separate key bindings?** Partially. The relevant defaults live in
`packages/tui/src/keybindings.ts` (`TUI_KEYBINDINGS`):

| Action ID | Default key | Effect |
| --- | --- | --- |
| `tui.editor.cursorUp` | `up` | Move caret up **or** recall older history (context-sensitive, see below) |
| `tui.editor.cursorDown` | `down` | Move caret down **or** recall newer history |
| `tui.editor.pageUp` | `pageUp` | Page **within the editor's own multiline draft** — NOT the transcript |
| `tui.editor.pageDown` | `pageDown` | Page down within the draft |
| `tui.select.up` / `.down` | `up` / `down` | Move selection in overlays/menus |
| `tui.select.pageUp` / `.pageDown` | `pageUp` / `pageDown` | Page a selector list |
| `app.history.search` | `Ctrl+R` | Open the fuzzy prompt-history overlay (`docs/keybindings.md`) |

Note `pageUp`/`pageDown` are **overloaded by focus**: in the editor they page
the draft (`#pageScroll`, `packages/tui/src/components/editor.ts:3199`); in a
selector they page the list. Neither pages the transcript, because the
transcript isn't an in-app scroll region.

(b) **Mode toggle (transcript focus vs input focus)?** No explicit user-facing
"scroll mode." Focus is always on the editor/active component; transcript
reading is delegated to the terminal. There is a `session-focus-controller.ts`
(`packages/coding-agent/src/modes/controllers/`) but it governs multi-session
focus, not a transcript/input scroll toggle.

(c) **Context-sensitive up/down disambiguation (the actual trick).** Inside the
editor, arrow keys choose between caret movement and history recall based on
caret position and whether the draft is empty. From
`packages/tui/src/components/editor.ts:1715-1740`:

```ts
// Up:
if (this.#isEditorEmpty()) {
    this.#navigateHistory(-1);              // empty draft → start browsing history
} else if (this.#historyIndex > -1 && this.#isOnFirstVisualLine()) {
    this.#navigateHistory(-1);              // already browsing + on first line → older entry
} else if (this.#isOnFirstVisualLine()) {
    this.#moveToLineStart();                // top of a real draft → jump to line start
} else {
    this.#moveCursor(-1, 0);                // otherwise → move caret up within draft
}
// Down: symmetric, using #isOnLastVisualLine() and #navigateHistory(1)
```

`#navigateHistory` (`editor.ts:800`) walks `#historyIndex` (`-1` = not
browsing, `0` = most recent) over an in-memory `#history: string[]` and replaces
the draft via `#setTextInternal`, anchoring the caret at start (going older) or
end (going newer). Persistence is `HistoryStorage`
(`packages/coding-agent/src/session/history-storage.ts`), exposing
`getRecent(limit)` and `search(query, limit)`.

**How a user navigates back through their own previous messages, three ways:**

1. **Up-arrow history recall** in the editor (above) — replays past *prompts*
   into the draft for re-send/edit.
2. **`Ctrl+R` fuzzy history search overlay** — `HistorySearchComponent`
   (`packages/coding-agent/src/modes/components/history-search.ts`). An
   `OverlayPanel` with a search `Input`, token-highlighted results,
   `↑↓`/PageUp/PageDown/Home/End to move the selection, Enter to load the chosen
   prompt into the editor. Backed by `HistoryStorage.search`.
3. **User-message branch selector** — `UserMessageSelectorComponent`
   (`packages/coding-agent/src/modes/components/user-message-selector.ts`).
   Lists prior *user messages in this session*, arrow keys wrap-navigate, Enter
   **branches/forks the session from that message**. This is session editing,
   not scrollback.

**How a user navigates back through agent responses:** via the terminal's own
scrollback for the live session, and for **subagent** transcripts via
`AgentTranscriptViewerComponent`
(`packages/coding-agent/src/modes/components/agent-transcript-viewer.ts`), which
borrows the **alternate screen** to show a file-backed transcript, then restores
the previous surface on close (`docs/tui.md`, "Built-in full-screen surfaces").

### 1.2 What koda does today (verified)

koda's architecture is the **opposite** of oh-my-pi: it renders an *in-app*
scroll region (`self.scroll`, `self.body_h`, `self.follow`) and owns the
transcript viewport itself. `src/tui.rs`:

- `scroll_by(delta)` (`tui.rs:1106`) clamps `self.scroll` to
  `total_lines - body_h` and sets `follow = next >= max` (auto-stick to bottom).
- `PageUp`/`PageDown` (`tui.rs:732-733`) already scroll the transcript by half a
  body height.
- `key_up`/`key_down` (`tui.rs:1050-1081`) already implement essentially the
  same context-sensitive disambiguation as oh-my-pi, and arguably a better one
  for koda's in-app-scroll model:
  1. not on first/last visual line → move caret within the draft;
  2. empty draft → **scroll the transcript** (the comment even says: *"the
     common case when the user wants to read back through the agent's
     responses — scroll the transcript, don't hijack it for input history"*);
  3. draft has text + caret on first/last line → `history_prev()`/`history_next()`,
     falling back to a scroll when history is exhausted.

**Conclusion: koda has already solved the core up/down conflict.** It did not
copy oh-my-pi's "delegate to terminal scrollback" approach; it built an in-app
scroll region and disambiguates arrows by caret position + emptiness. That is a
valid, self-consistent design.

### 1.3 What koda should adopt (specific)

Because koda already owns an in-app scroll region and already disambiguates
arrows, the recommendations are **gap-closing**, not a rewrite. Do **not** adopt
oh-my-pi's "no in-app scroll, use terminal scrollback" model — it would throw
away working code and koda's overlay/animation system depends on owning the
viewport.

- **R1 — `Ctrl+R` fuzzy prompt-history overlay.** koda has `history_prev/next`
  in the editor but no search overlay. Port the *behaviour* of
  `HistorySearchComponent`: an overlay panel with a query line, fuzzy-ranked
  recent prompts (koda already has `src/fuzzy.rs`), `↑↓`/PgUp/PgDn to move the
  selection, Enter to load into the editor, Esc to cancel. Persist prompts
  across sessions (koda stores sessions under `.koda/sessions/`; add a
  `history` store analogous to `history-storage.ts`).
  *Value:* the single most-used "find my old prompt" affordance; today koda only
  offers linear up-arrow stepping.

- **R2 — User-message jump/branch selector.** Port
  `UserMessageSelectorComponent`: list this session's user messages, Enter to
  either (minimal) scroll the transcript to that message, or (full) fork the
  session from that point. koda has session infra (`src/session.rs`) so the
  scroll-to-message variant is cheap; forking is a larger, separate feature.

- **R3 — A visible "reading vs composing" affordance.** oh-my-pi leans on native
  scrollback so it needs no indicator. koda owns the viewport, so when
  `follow == false` (user scrolled up) it should show a persistent status hint
  (e.g. `↓ N new lines · End to jump to latest`) and bind `End`/`Ctrl+End` (and
  `G`) to snap back (`scroll = max; follow = true`). This is the koda-specific
  ergonomic that oh-my-pi doesn't need. Grep shows koda already tracks `follow`;
  this just surfaces it.

- **R4 — Make the arrow/scroll bindings configurable.** oh-my-pi routes every
  editor/selection key through `KeybindingsManager` with a user-editable
  `keybindings.yml` (`docs/keybindings.md`). koda hardcodes `KeyCode::Up` etc.
  in `tui.rs`. Introduce a small keybinding action map (koda has `src/config.rs`
  + `src/settings.rs`) so power users can remap. Lower priority than R1–R3.

- **R5 — Do NOT overload PgUp/PgDn onto the editor draft.** oh-my-pi's editor
  PgUp/PgDn pages the *draft*, which is confusing. koda already uses PgUp/PgDn
  for the transcript — keep it. Recommendation is "keep koda's choice," recorded
  so nobody 'ports' the oh-my-pi behaviour by mistake.

### 1.4 Effort — Part 1

| Item | Scope | Estimate |
| --- | --- | --- |
| R1 `Ctrl+R` history overlay + persistent history store | new overlay component in `view.rs`/`panel.rs`, fuzzy reuse, JSON store | **2–3 days** |
| R2 user-message selector (scroll-to variant) | new overlay, reuse session entries | **1–1.5 days** |
| R2 full session fork-from-message | session branching semantics | **+3–5 days** (separate epic) |
| R3 scrolled-away indicator + End/G snap-back | status line + 2 key handlers | **0.5 day** |
| R4 configurable keybindings | action map + config plumbing | **2–3 days** |
| R5 keep PgUp/PgDn on transcript | doc-only decision | **0** |

---

## Part 2 — `/debug` and breakpoint-style debugging

### 2.1 What oh-my-pi does (exact mechanism)

**Key finding: oh-my-pi's debug story is TWO distinct subsystems.** The user's
goal ("run a program and put breakpoints") maps to the first; koda today only
has the second.

#### A. The `debug` **agent tool** — real DAP breakpoint/step debugging (model-driven)

A full [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
client. This is genuine step-through debugging, not log viewing. Sources:

- Tool entry: `packages/coding-agent/src/tools/debug.ts`
- `packages/coding-agent/src/dap/session.ts` — session lifecycle, breakpoint/state cache (63 KB)
- `packages/coding-agent/src/dap/client.ts` — adapter process/socket transport, DAP message loop (31 KB)
- `packages/coding-agent/src/dap/config.ts` — adapter resolution/auto-selection
- `packages/coding-agent/src/dap/defaults.json` — 14 built-in adapters
- `packages/coding-agent/src/dap/types.ts` — DAP request/response/capability shapes
- Model-facing prompt: `packages/coding-agent/src/prompts/tools/debug.md`
  ("Debugger access. Prefer over bash for program state, breakpoints, stepping,
  or thread inspection. Only one active session at a time.")
- Full reference: `docs/tools/debug.md`

The tool takes an `action` enum and drives one debug session. Actions
(verbatim from `debug.ts`):

`launch`, `attach`, `set_breakpoint`, `remove_breakpoint`,
`set_instruction_breakpoint`, `remove_instruction_breakpoint`,
`data_breakpoint_info`, `set_data_breakpoint`, `remove_data_breakpoint`,
`continue`, `step_over`, `step_in`, `step_out`, `pause`, `evaluate`,
`stack_trace`, `threads`, `scopes`, `variables`, `disassemble`, `read_memory`,
`write_memory`, `modules`, `loaded_sources`, `custom_request`, `output`,
`terminate`, `sessions`.

So it supports source breakpoints (`file`+`line`), function breakpoints,
conditional + hit-count breakpoints, instruction breakpoints, data/watch
breakpoints, full step controls, expression evaluation (REPL), stack/threads/
scopes/variables inspection, disassembly and memory read/write — the complete
DAP surface.

How it works (`docs/tools/debug.md` "Flow", verified against `dap/session.ts`,
`dap/client.ts`):

1. `DebugTool.createIf()` returns the tool only when `debug.enabled` (default
   `true`).
2. `DapSessionManager.launch()/attach()` enforce **one root session**
   (`#ensureLaunchSlot()`), spawn the adapter via `DapClient.spawn()`, send DAP
   `initialize`, cache capabilities, then `launch`/`attach` and complete the
   `initialized` → `configurationDone` handshake.
3. Transport modes (`dap/client.ts`): `stdio` (adapter pipes), `socket` (Unix
   socket on Linux / TCP callback elsewhere), `tcp` (spawn a local DAP server,
   substitute `${port}`). The JS/TS adapter and recursive child sessions use
   `tcp`.
4. Reverse requests handled: `runInTerminal` (spawns the debuggee detached via
   `ptree.spawn()`), `startDebugging` (recursive **child** sessions on the same
   TCP server — a stopped child becomes the active target). Events `stopped`,
   `continued`, `output`, `exited`, `terminated` update cached state.
5. `continue`/`step_*` clear cached stop state, subscribe for a stop/terminate
   event anywhere in the session tree, send the DAP request, then
   `#awaitStopOutcome()` returns the new stopped location (or reports "still
   running" past the timeout — deliberately non-fatal, `details.timedOut`).

Built-in adapters (`dap/defaults.json`): `gdb`, `lldb-dap`, `codelldb`,
`debugpy`, `dlv` (Go, directory-capable), `js-debug-adapter`, `netcoredbg`,
`kotlin-debug-adapter`, `rdbg`, `php-debug-adapter`, `bash-debug-adapter`,
`dart-debug-adapter`, `flutter-debug-adapter`, `elixir-ls-debugger`. Most set
`stopOnEntry: true` in `launchDefaults`. Users add/override via
`dap.json`/`.dap.yaml` (same search order as LSP config). Auto-selection ranks
available adapters by file extension → root-marker → native-debugger preference.

Caps (`dap/client.ts`, `dap/session.ts`): `DEFAULT_REQUEST_TIMEOUT_MS = 30_000`,
single active root session, `IDLE_TIMEOUT_MS = 10 min`, `HEARTBEAT = 5 s`,
`MAX_OUTPUT_BYTES = 128 KB`, `STOP_CAPTURE_TIMEOUT_MS = 5_000`.

Approval is action-sensitive: read-only actions (`output`, `threads`,
`stack_trace`, `scopes`, `variables`, `disassemble`, `read_memory`,
`loaded_sources`, `modules`, `sessions`) request *read* approval; everything
else requests *exec* approval (`DEBUG_READONLY_ACTIONS` in `debug.ts`).

#### B. The `/debug` **interactive selector** — diagnostics UI (user-driven, NOT breakpoints)

This is the `/debug` slash command a *user* runs. It is a menu, not a debugger.
Source: `packages/coding-agent/src/debug/index.ts` (`DEBUG_MENU_ITEMS`,
`DebugSelectorComponent`). Routes:

`open-artifacts`, `performance` (CPU profile + 30 s work profile + report
bundle), `work` (flamegraph), `dump` (report bundle), `memory` (heap snapshot),
`logs` (recent-log TUI viewer → `debug/log-viewer.ts`), `system`
(`debug/system-info.ts`), `terminal` (`debug/terminal-info.ts`), `protocols`
(`debug/protocol-probe.ts`), `raw-sse` (live provider SSE frames →
`debug/raw-sse.ts` + bounded `raw-sse-buffer.ts`), `remote-debugger`
(experimental JavaScriptCore inspector socket → `debug/remote-debugger.ts`),
`transcript` (export), `clear-cache`.

These are UI-only and **not** model-callable (`docs/tools/debug.md`: "not
model-callable through `debugSchema`; they are local TUI menu routes").

### 2.2 What koda does today (verified — `src/debug.rs`)

koda's `/debug` is exactly oh-my-pi's **raw-sse + logs** category and nothing
else: a global atomic switch that, when on (`debug = true`, `/debug`, or
`KODA_DEBUG=1`), writes each LLM request body and its raw SSE response to
`<state>/koda/debug/rr-session-N.json` + `.res.log`. `report()` prints where the
artifacts and event log live and a captured-session count.

So koda has: request/response capture (≈ oh-my-pi `raw-sse`) and log location
reporting (≈ `logs`/`dump`). koda has: **no DAP, no breakpoints, no
step-through, no program execution control.** This is precisely the gap the user
described ("not just log viewing").

### 2.3 What koda should adopt (specific, implementable)

The user wants "run a program and put breakpoints." That is subsystem **A** (the
DAP tool), which koda lacks entirely. Recommendation is to build a Rust DAP
client. Two viable designs:

- **Option 1 (recommended) — native Rust DAP client.** Implement a `src/dap/`
  module mirroring oh-my-pi's split:
  - `dap/client.rs` — spawn adapter, DAP wire framing
    (`Content-Length` headers + JSON bodies over stdio; add socket/tcp later),
    request/response correlation by `seq`, event pump.
  - `dap/session.rs` — session state: capabilities cache, breakpoint set,
    stop location, threads/frames/scopes cache, output ring, single-active-session
    guard, idle cleanup.
  - `dap/config.rs` + `dap/defaults` — adapter registry + auto-selection by file
    extension/root marker; ship a **subset** first: `debugpy`, `codelldb`/`lldb-dap`,
    `dlv`, `js-debug-adapter` (koda-relevant languages). Allow `dap.json` override.
  - Wire a **new agent tool** `debug` in `src/tools.rs` with the action enum, so
    the model can drive it (koda already has an approval system —
    `tests/probe_approval.py` — reuse it for the read-vs-exec action split).
  - Rust crates that shorten this: `dap` (DAP types) or `debug-adapter-protocol`,
    plus koda's existing async runtime and `serde_json`.
  - Scope the MVP to: `launch` (with `stopOnEntry`), `set_breakpoint`
    (file+line), `continue`, `step_over/in/out`, `pause`, `stack_trace`,
    `scopes`, `variables`, `evaluate`, `output`, `terminate`. Add conditional/
    function/data/instruction breakpoints, `disassemble`, `read/write_memory`,
    and recursive `startDebugging` children *later* — they are ~40% of
    oh-my-pi's surface but a small fraction of real usage.

- **Option 2 (cheaper, less capable) — shell out to a CLI debugger.** Drive
  `lldb`/`gdb`/`pdb`/`dlv` in batch/`-batch`/MI mode from koda and parse text.
  Fast to prototype, but brittle, per-debugger bespoke parsing, no uniform
  capability model, and no clean breakpoint-hit event stream. Fine as a stopgap
  for one language; do not build the whole feature this way.

Also worth porting from subsystem **B** (small, high value, no DAP needed):

- **R6 — `/debug` menu with a live SSE viewer.** koda already *captures* SSE to
  disk; add a TUI viewer over a bounded in-memory ring (mirror
  `raw-sse-buffer.ts` caps: 1000 events / 512 KB) with tail-follow + copy. Turns
  koda's write-only capture into an interactive panel.
- **R7 — `system` / `terminal` info panels + a `dump` report bundle** (tar.gz of
  session JSONL + recent logs). koda already knows its log/artifact dirs
  (`src/log.rs`), so this is mostly formatting + archiving.

### 2.4 Model-facing framing to copy

Adopt oh-my-pi's prompt guidance verbatim in spirit
(`prompts/tools/debug.md`): tell the model to *prefer the debugger over bash*
for program state / breakpoints / stepping / thread inspection, one active
session at a time, `program` is a path not a shell command. This is what makes
the model actually use breakpoints instead of `println` debugging.

### 2.5 Effort — Part 2

| Item | Scope | Estimate |
| --- | --- | --- |
| Native Rust DAP client — MVP (stdio, `launch`/breakpoint/step/inspect/evaluate, one language e.g. debugpy) | `src/dap/{client,session,config}.rs` + `debug` tool + approval wiring | **8–12 days** |
| DAP MVP — add 3 more adapters (lldb/codelldb, dlv, js-debug over tcp) + auto-select + `dap.json` | transport + config | **+4–6 days** |
| DAP full parity (conditional/function/data/instruction bps, disassemble, memory r/w, recursive `startDebugging`) | remaining DAP surface | **+8–12 days** |
| Option 2 shell-out stopgap (single debugger, text parsing) | one adapter, bespoke parse | **3–4 days** (throwaway) |
| R6 `/debug` menu + live SSE viewer over in-memory ring | overlay + ring buffer | **2–3 days** |
| R7 system/terminal panels + tar.gz report bundle | formatting + archive | **1.5–2 days** |
| R? adopt debugger-preference prompt text | prompt edit | **0.25 day** |

**Recommended sequencing:** R6 + R7 first (fast wins that upgrade the existing
capture into a real diagnostics UI), then the native DAP MVP for one language
(debugpy or codelldb) to prove the breakpoint UX end-to-end, then expand
adapters and DAP surface. Avoid Option 2 unless a demo is needed this week.

---

## Summary table

| Question | oh-my-pi answer | koda today | Recommendation |
| --- | --- | --- | --- |
| Up/Down: scroll vs history | No in-app transcript scroll; terminal owns scrollback. Editor arrows are context-sensitive (empty→history, caret-position gated). Ctrl+R fuzzy history overlay; user-message branch selector. | In-app scroll region; arrows already disambiguate (multiline→caret, empty→scroll, text+edge→history→scroll). **Core conflict already solved.** | Keep koda's model. Add Ctrl+R history overlay (R1), user-message selector (R2), scrolled-away indicator + End/G snap-back (R3), configurable keys (R4). |
| `/debug`: logs or breakpoints? | **Both, separately.** A model-driven DAP tool = real breakpoints/step-through (14 adapters, full DAP). A user-driven `/debug` menu = diagnostics (logs, SSE, profiles, reports) — no breakpoints. | Only request/response SSE capture + log paths (≈ oh-my-pi's raw-sse/logs). No DAP. | Build a native Rust DAP client + `debug` agent tool for real breakpoints (Option 1). Quick wins: `/debug` menu with live SSE viewer (R6) + system/report bundles (R7). |

### Primary sources (all under `refs/oh-my-pi/`)
- Scroll/renderer: `docs/tui-core-renderer.md`, `docs/tui-runtime-internals.md`, `packages/tui/src/components/editor.ts` (lines 800, 1715-1740, 3199), `packages/tui/src/keybindings.ts`
- History/selectors: `packages/coding-agent/src/modes/components/history-search.ts`, `.../user-message-selector.ts`, `.../agent-transcript-viewer.ts`, `packages/coding-agent/src/session/history-storage.ts`, `docs/keybindings.md`
- Debug (DAP): `docs/tools/debug.md`, `packages/coding-agent/src/tools/debug.ts`, `packages/coding-agent/src/dap/{session,client,config,types}.ts`, `dap/defaults.json`, `packages/coding-agent/src/prompts/tools/debug.md`
- Debug (selector): `packages/coding-agent/src/debug/index.ts` (+ `log-viewer.ts`, `raw-sse.ts`, `raw-sse-buffer.ts`, `report-bundle.ts`, `system-info.ts`, `terminal-info.ts`)
- koda cross-check (read-only): `src/tui.rs` (key_up/key_down:1050, scroll_by:1106, PgUp/PgDn:732), `src/debug.rs`, `src/editor.rs`
