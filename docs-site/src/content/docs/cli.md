---
title: Command line
description: Flags, subcommands, headless mode, and the two prefixes that skip the agent entirely.
---

```
koda [OPTIONS] [PROMPT]...
```

A bare prompt seeds the TUI with a first message. With `-p` it runs headless: the answer
streams to stdout and koda exits.

## Flags

| Flag | What it does |
| --- | --- |
| `[PROMPT]...` | First message. Without `-p` it seeds the TUI. |
| `-p`, `--print` | Headless: stream the answer to stdout and exit. |
| `-m`, `--model <MODEL>` | Model name, e.g. `qwen2.5-coder:14b`. |
| `-u`, `--url <BASE_URL>` | OpenAI-compatible base URL, e.g. `http://localhost:1234/v1`. |
| `--api-key <KEY>` | API key, if the server needs one. |
| `-C`, `--dir <DIR>` | Workspace root. Defaults to the current directory. |
| `-y`, `--yolo` | Approve file writes and commands without asking. |
| `--protocol <PROTOCOL>` | Tool-call protocol: `auto`, `native`, or `text`. |
| `--no-sandbox` | Allow file tools outside the workspace root. |
| `-t`, `--temperature <T>` | Sampling temperature. |
| `--theme <THEME>` | Palette name — see [Themes](/koda/themes/). |
| `--icons <ICONS>` | Glyphs: `auto`, `unicode`, `ascii`. |
| `--mode <MODE>` | Start in `plan`, `execute` or `vibe`. |
| `-c`, `--continue` (`--resume`) | Reopen the most recent conversation in this project. |

## Subcommands

| Subcommand | What it does |
| --- | --- |
| `koda models` | List the models the endpoint reports. |
| `koda skills [--init]` | List skills; `--init` writes a starter skill into `<project>/.koda/skills/`. |
| `koda config [--init]` | Show the effective configuration; `--init` writes a commented starter file. |

## Headless mode

```sh
koda -p "which module owns the retry logic?"
```

The answer goes to stdout and tool activity to stderr, so you can pipe one without the
other:

```sh
koda -p "summarise what changed in src/ this week" > summary.md
```

Headless has nobody to answer an approval prompt, so:

- a write or command **without** `--yolo` is denied, and koda exits with status **2**;
- if the agent calls `ask_user`, koda reports the question and proceeds with no answer.

Use `-y` deliberately here, and prefer a task that only reads.

## Running things yourself

Two prefixes skip the agent entirely. Neither costs tokens, and neither enters the
conversation the model sees — the output is yours, not context.

| Prefix | What it runs |
| --- | --- |
| `!` | A shell command in the workspace: `!git status`, `!npm test`, `!git commit -am wip`. |
| `$` | Python: `$ print(sum(range(1, 11)))` |

```
$ import json; print(json.load(open("package.json"))["version"])
```

`$` uses `python3` when it is on your `PATH` and falls back to `python`. Both show their
output in the transcript as a tool block, exactly like a command the agent ran — the
difference is invisible to you and total to the model.

This is the answer to "I need a number, not a guess": an interpreter's arithmetic instead
of a language model's.

## Editing what you typed — `#`

Press `#` at the end of what you have written to open a palette of actions on the text
itself. None of them reach the model or cost a turn.

| Action | What it does |
| --- | --- |
| `#copy` | Copy the whole prompt to the clipboard. |
| `#copyline` | Copy the line the caret is on. |
| `#cutline` | Delete that line. |
| `#start` / `#end` | Move the caret to the beginning or the end. |
| `#clear` | Empty the input. |
| `#undo` | Restore what the last action removed. |
| `#paste` | Insert the clipboard's text. |

Type a few letters to narrow the list (`#cl` → `#clear`), <kbd>↑</kbd>/<kbd>↓</kbd> to
pick, <kbd>enter</kbd> to run, <kbd>esc</kbd> to close and keep what you wrote. A `#` in
the middle of a sentence is left alone as ordinary text, so `fix #3 in the parser` behaves
normally.
