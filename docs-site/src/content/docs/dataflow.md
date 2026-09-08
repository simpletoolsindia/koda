---
title: Data flow
description: What happens between pressing enter and seeing an answer, step by step.
---

One user message becomes one **turn**. A turn is a loop: build a request, stream a
completion, run whatever tools the model asked for, and go round again until the model
stops asking.

## A turn, in order

```
1  you press enter
2  the message joins the conversation
3  the request is assembled
      system prompt
      + skills index (one line each)
      + memory block (scaled to the window)
      + learned rules
      + AGENTS.md / CLAUDE.md / .koda.md
      + the conversation, curated to fit context_tokens
      + the tool schemas (unless the protocol is text)
4  POST /v1/chat/completions, stream: true
5  the SSE stream arrives
      text deltas      → the transcript, revealed progressively
      reasoning deltas → the dimmed reasoning block (ctrl+t)
      tool-call deltas → accumulated until complete
6  if the model asked for tools:
      each call is checked against the mode and the autonomy tier
      a mutating call renders a diff or a command and waits for you
      approved calls run, in order
      each result is appended to the conversation
      → back to step 3
7  no tool calls: the turn ends
      the session file is appended
      memory and learning observations are recorded
      the trace is closed
```

Steps 3 through 6 repeat up to `max_steps` times (default 24). At that point `step_check`
asks the model whether work remains rather than stopping flat; a "keep going" answer buys
another `max_steps`, up to `max_steps_hard` (default 96).

## What is assembled into a request

The request is rebuilt from scratch every step, which is what makes curation possible: the
conversation on disk is never modified, only the copy that is about to be sent.

| Piece | Comes from | Notes |
| --- | --- | --- |
| System prompt | `prompt.rs`, or your `system_prompt` | Short on purpose. |
| Environment facts | Computed | Workspace path, project type, current date and UTC offset. |
| Skills index | `.koda/skills/` + `~/.config/koda/skills/` | One `when:` line each, ~15 tokens. |
| Memory | `.koda/memory.md` | 4–20 facts, one per ~1k of window. |
| Learned rules | `.koda/learning/rules.md` | Only promoted ones. |
| Project instructions | `AGENTS.md`, `CLAUDE.md`, `.koda.md`, `instructions` | Appended verbatim. |
| Conversation | In memory | [Curated](/koda/prompt/) to fit `context_tokens`. |
| Tools | `tools.rs` | Omitted entirely under the `text` protocol. |

## Streaming

koda reads server-sent events and dispatches three kinds of delta as they arrive:

- **Text** goes to the transcript and, with `reveal` on, is typed in progressively.
- **Reasoning** from thinking models goes to a dimmed block, expandable with
  <kbd>ctrl+t</kbd>. It is displayed and never sent back.
- **Tool calls** are accumulated until complete, then executed. Under the `text` protocol
  the same thing arrives as `<tool_call>{…}</tool_call>` blocks in the text stream, which
  are stripped from what you see even when a tag arrives split across chunks.

While the model is still *writing* a tool call — a `write_file` of a few hundred lines
streams for a long time before the call is made — the status row names the file and the
size climbs:

```
 ✳ writing src/context.rs · 12.4 KB (1m 04s · ↓ 14.8k tok)      esc interrupt
   Tip: /undo puts back every file the agent changed in the last turn
```

The meter is for the turn as a whole, so a wait is visibly work rather than a hang.

## Failures

Transient failures — connection reset, 429, 5xx, an empty stream — are retried with
backoff, up to `max_retries` (default 3). You see one plain sentence; the full detail goes
to the event log, and the status bar shows a count so you know to look.

Nothing puts a stack trace on your screen.

## What is written where

At the end of a turn:

- the session file gains one JSONL line per message;
- `memory.md` gains any facts `remember` recorded, and the outcome of each command;
- `learning/observations.jsonl` gains the raw signals, if learning is on;
- the trace is closed and kept in memory for the [web UI](/koda/webui/), last 50 turns.

None of that leaves your machine.
