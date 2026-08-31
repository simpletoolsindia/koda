# Spec: MCP and LSP — deferred (P2), with a design sketch

Status: **deferred, not abandoned.** Both are on the roadmap as P2. This document
records *why* they are not in the current batch and *how* they should be built
when picked up, so the decision is reviewable and the work is pre-scoped.

## Why deferred

Both MCP (Model Context Protocol) and LSP (Language Server Protocol) are large,
self-contained subsystems, and both put pressure on koda's two hardest-won
properties:

1. **~3 ms startup.** koda opens instantly because it does almost nothing on the
   startup path. An MCP client that dials external servers, or an LSP client that
   spawns a language server and waits for `initialize`, adds hundreds of
   milliseconds to seconds — on the critical path, for every launch, even when
   the feature is unused.
2. **Zero runtime dependencies / one ~6 MB binary.** A serious LSP integration
   pulls in a JSON-RPC stack, per-language server discovery, and document
   synchronisation state. MCP pulls in a transport layer and a server lifecycle
   manager. Neither is free in binary size or maintenance surface.

Neither is a quick win, and shipping a half-built version would rot. So the
decision is: **design now, build behind an opt-in flag later, never on the
startup path.**

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
