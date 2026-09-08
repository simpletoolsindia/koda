---
title: Web control center
description: An optional local page that traces every turn end to end and drives the running session.
---

An optional page served on `127.0.0.1` only. It traces every turn end to end and controls
the koda that is running — not a copy of its config on disk, the live process.

## Turning it on

Open `/settings` and toggle **web ui** on, or set it in the config:

```toml
web_ui = true
web_ui_port = 7717     # optional; the default
ui_detail = "medium"   # simple | medium | high
```

The settings toggle is remembered, and koda starts the server on the next launch. On
launch it prints the address:

```
koda: web UI at http://127.0.0.1:7717
```

Turn tracing is on whenever the web UI is. `KODA_TRACE=1` forces it on otherwise. The last
50 turns are kept in memory with truncated payloads, so a long session stays bounded.

## What is on the page

**Turn rail** — every turn, newest first, with status, duration, step and token counts. The
running turn is followed live.

**Trace waterfall** — that turn's steps in order, with duration bars: model calls, tool
calls and compactions, with retries, failures and denied approvals marked inline.

**Inspector** — the payloads behind a step: the exact request body sent, the raw SSE stream
received, the model's reasoning, and a tool's arguments, result and diff.

*Prompt Δ* is the part worth knowing about. It diffs the prompt against the previous model
call, so what compaction dropped — or what a learned rule added — is visible rather than
silent. When a turn goes wrong three steps in, this is usually where the reason is.

**Control rail** — model, endpoint, mode, autonomy tier, reasoning effort, max steps,
feature toggles, project memory, learned-rule candidates, and saved sessions to resume or
fork. Edits apply to the running koda, not just to disk.

**Logs drawer** (<kbd>L</kbd>) and a **Manage** panel for the code graph, skills and role
agents, the system prompt, and raw request/response captures.

**<kbd>⌘K</kbd>** command palette — jump to a turn, switch model or mode, toggle a feature,
export a trace, or `@symbol` + <kbd>shift+enter</kbd> to query the code graph.

## Raw captures are separate

The trace does not need `/debug`. Raw request and response *files* are a different thing:
enable `/debug` (or `debug = true`, or `KODA_DEBUG=1`) to fill the Manage panel's Raw
Captures and write them to `~/.local/state/koda/debug/`.

## What it costs

Nothing you have not already installed. The server is built on the async runtime koda
already uses, adds no dependencies, and binds to localhost only — nothing is exposed off
your machine.

The **Agents & Skills** page has a provider dropdown populated from your saved providers,
and a model box that suggests what that provider actually serves while still accepting
free text. The web UI never receives your API keys: the provider list reports only whether
a key is set.
