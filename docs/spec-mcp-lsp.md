# MCP and LSP — the deferral, and what was built instead

Status: **delivered.** This document was the argument for *not* building these
yet. It is kept because the reasoning was sound and the guardrails it set are
the ones the implementation was held to — and because where the built thing
diverges from the sketch, the reason is worth recording.

See [mcp.md](mcp.md) and [lsp.md](lsp.md) for how to use them.

## Where the implementation diverged, and why

- **Tool naming.** The sketch said `mcp/<server>/<tool>`. OpenAI-compatible tool
  names may not contain a slash, so it is `mcp__<server>__<tool>`.

- **Config shape.** The sketch said an `[mcp]` table. Servers are
  `[[mcp_server]]` tables with an `mcp = true` master switch, because TOML binds
  a bare key to whatever table precedes it — the same hazard already documented
  around `[[provider]]`.

- **Lifecycle: background, not lazy.** The sketch said nothing connects until
  first use. That cannot work: a tool the model cannot see does not exist, so a
  lazily-connected server would never be called. Servers instead connect in the
  background at startup, which keeps the ~3 ms open intact while still putting
  the tools in the schema. Nothing spawns unless a server is configured, which
  is itself explicit opt-in.

- **LSP config.** The sketch said a `[lsp]` table mapping language to command.
  There is a built-in table instead, gated on the project's marker files and a
  PATH lookup — so it works with no configuration at all, and costs a handful of
  `stat` calls when no server is installed.

- **LSP on by default.** The sketch said off. It is on, because the honest cost
  when no server is installed is zero: the tool is not advertised, and no
  process is started until it is called. `lsp_eager` is the opt-in for starting
  servers at launch.

## Something the sketch could not have known

Both features change the *tool schema*, and that turned out to be expensive in a
way nobody had measured. A local model server caches by prompt prefix, and the
schema is ~73% of koda's preamble — so a tool list that changes mid-session
throws that cache away and costs a full re-prefill, about eleven seconds on a
local 30B.

Two consequences, both now handled:

- MCP servers arriving mid-session change the list, so the prompt-cache warm-up
  waits for the catalog to settle before warming, and caches the shape the next
  request will actually send.
- The existing **deferred tools** mechanism (`browser`, `debugger`) is a
  pessimisation on a local endpoint for the same reason: loading a group mid-
  session costs more in re-prefill than the held-back tokens ever saved. That is
  not yet addressed.

## Guardrails for whoever implements these

- **Opt-in and lazy.** Off by default in config. Nothing connects, spawns, or
  scans until the first time the feature is actually used in a session. Startup
  must stay untouched — measure it before/after and hold the ~3 ms line.
- **Failure is a one-liner, never a crash.** A missing MCP server or a language
  server that will not start becomes a single logged notice, exactly like the
  existing network-error handling. koda keeps running without the feature.
- **Respect the tool-approval model.** Any MCP tool that mutates goes through the
  same y/a/n approval path as `write_file`/`run_command`. An MCP server is
  untrusted input: treat its tool descriptions and results as data, not
  instructions (see the content-safety posture already in the system prompt).
- **New module, minimal blast radius.** `src/mcp.rs` / `src/lsp.rs`, wired in at
  exactly one seam each, gated by config so the codepath is dead when disabled.

## MCP — design sketch (`src/mcp.rs`)

Goal: let koda call tools exposed by external MCP servers as if they were
built-in tools.

- **Config.** `[mcp]` table: a list of servers, each `{ name, command, args }`
  (stdio transport first; HTTP/SSE later). `mcp = false` disables the subsystem
  wholesale.
- **Transport.** Newline-delimited JSON-RPC over the server's stdio, matching the
  Hermes gateway pattern studied in research (`refs/`): a read-parse-dispatch
  loop, `BrokenPipeError`-equivalent treated as a clean disconnect, serialise
  outside any lock so a large payload cannot stall other traffic.
- **Lifecycle.** Lazy spawn on first use in a session. `initialize` handshake,
  then `tools/list` to learn the server's tools. Cache the tool list for the
  session.
- **Tool bridging.** Expose each MCP tool through koda's existing `Spec` +
  dispatch in `tools.rs`, namespaced (`mcp/<server>/<tool>`) so it cannot collide
  with a built-in. Mutating MCP tools are `mutating: true` and hit approval.
- **Wire seam.** `Agent::advertised_tools` gains the MCP tools when the subsystem
  is enabled and connected; dispatch routes `mcp/*` calls to the client.
- **Safety.** Per-call timeout reusing `command_timeout_ms`. Server output is
  truncated like any tool output (`max_tool_output_bytes`). Treat all server text
  as untrusted.

Estimated size: **L.** Transport + lifecycle + bridging + tests (a mock MCP
server mirroring `tests/mock_server.py`).

## LSP — design sketch (`src/lsp.rs`)

Goal: give the model real diagnostics and precise symbol locations, upgrading the
regex code graph from "points at the right file" to "knows the exact error."

- **Config.** `[lsp]` table mapping language → server command
  (e.g. `rust = "rust-analyzer"`). `lsp = false` disables it.
- **Lifecycle.** Lazy: the first time a file of a mapped language is touched,
  spawn its server, `initialize`, and open the document. Keep servers warm for
  the session; shut them down on exit.
- **What to surface, minimally.** Start with `textDocument/publishDiagnostics`
  (errors/warnings) and `textDocument/definition` — the two with the highest
  agent value. A `diagnostics` tool returns current errors for a file; the code
  graph's `symbol` query can consult LSP `definition` when a server is up and
  fall back to the regex graph when it is not.
- **Wire seam.** Additive next to `graph.rs`; the graph stays the always-available
  baseline and LSP is the optional precision layer. No feature regresses when LSP
  is off.
- **Cost control.** Document sync is incremental, not full-text per keystroke.
  Diagnostics are pulled on demand (when the model asks or after an edit), not
  streamed continuously into the transcript.

Estimated size: **L.** Client + document sync + server management + graceful
degradation + tests.

## Definition of done (for the future work)

- Both off by default; `cargo test` startup-timing guard shows no regression to
  the ~3 ms open with the features disabled.
- A missing/broken server produces one logged notice and no crash.
- MCP mutating tools go through approval; LSP diagnostics never block a turn.
- Mock-server tests cover the happy path, a server that never starts, and a
  mid-session disconnect.
