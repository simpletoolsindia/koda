---
title: Modes & autonomy
description: Two independent dials — what the agent is allowed to attempt, and how often it stops to ask you.
---

koda has two separate controls that people often conflate. **Mode** decides which tools
the model is offered at all. **Autonomy** decides how often you are asked before a
permitted tool runs. They are orthogonal: you can be in the most capable mode with the
most cautious approvals, and that is the default.

## Modes

<kbd>ctrl+p</kbd> cycles them; `/mode` shows or sets one directly. The current mode is
always the first chip above the input.

| Mode | Label | What it does |
| --- | --- | --- |
| `plan` | `PLAN` | Reads and thinks. The write and command tools are not offered to the model, so nothing on disk can change. Produces a plan and asks you to switch. |
| `execute` | `EXEC` | Normal operation. Edits and commands, each gated by your autonomy tier. |
| `vibe` | `VIBE` | Spec-driven delivery. Writes an explicit spec first — goal, done-when, files, validation — plans the steps, does the work, then checks the result against its own spec. |

`PLAN` is not a weaker mode; it is a different job. The write tools being *absent* rather
than *denied* is what makes it useful: the model does not spend a turn proposing an edit
it cannot make, so you get a plan instead of a rejection.

`VIBE` is the mode for "just get it done". It orchestrates large or many-part tasks by
delegating self-contained subtasks to [role agents](/koda/skills/), and — this is the part
that matters — it verifies any subagent's claims against the actual files before believing
them. A report citing a line that does not say what it claims gets sent back for another
pass.

## Autonomy tiers

Cycle live with `/auto`, or set `auto_tier` in the config, or use `/settings`.

| Tier | Behaviour |
| --- | --- |
| `ASK` *(default)* | Prompts before every write and every command. |
| `AUTO-WRITE` | Auto-approves file writes. Still asks before running commands. |
| `FULL-AUTO` | Approves everything. Shown in red in the status bar, because it should be. |

`AUTO-WRITE` is the tier most people settle on in a git repository with a clean tree: a
bad edit is one `git checkout` away, but a bad command is not.

Even under auto-approval, `confirm_destructive = true` (the default) still stops for
irreversible shell commands.

## The approval prompt

An approval is a docked block, not a line you might scroll past — amber for a write, red
for a command, with the action row spelled out.

| Key | What it means |
| --- | --- |
| <kbd>y</kbd> | Approve this one call. |
| <kbd>a</kbd> | Approve this tool for the rest of the session. |
| <kbd>n</kbd> | Deny, and tell the model to ask you what to do instead. |
| <kbd>↑</kbd> <kbd>↓</kbd> | Scroll the pending preview — the diff, or the command. |

A denial is not silent failure. The model is told it was denied and that it should ask,
which is what turns a refused edit into a question rather than a retry loop.

## When the agent asks you

The `ask_user` tool lets the agent put a question to you mid-task. It opens a centred
dialog and takes the answer **in the dialog**: pick from the options with <kbd>↑</kbd>
<kbd>↓</kbd> or <kbd>1</kbd>–<kbd>9</kbd>, or just start typing and the dialog turns into
an answer field with the usual line-editing keys. <kbd>esc</kbd> goes back to the options.
It never sends you to the composer underneath, which is where people used to answer by
mistake.

## Skipping approvals entirely

`-y` / `--yolo` approves everything for the run, and `auto_approve = true` does the same
from the config.

:::danger
`--yolo` is genuinely dangerous with a model that can hallucinate an `rm -rf`. Use it in a
git repository with a clean tree, or in a container, and nowhere else.
:::

Headless mode (`-p`) has nobody to ask. A write or command without `--yolo` is therefore
denied and koda exits with status 2.

## Keeping the plan honest

The `todo` tool draws the plan above the input. Left to itself, a model writes that plan
once and never touches it again — real sessions show one `todo` call at the start and, at
best, one at the end — so the list sits on step one while the work runs past it, which is
worse than showing no plan at all.

Two things keep it moving, both placed where the model is actually reading. Every `todo`
result echoes the merged list back and names the step it is on, with what to send when
that step is done. And if six tool calls go by without the list moving, the next tool
result carries a reminder naming the stale step — once per turn, so it is a nudge and not
a nag.
