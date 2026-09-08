---
title: Configuration
description: Where koda reads its settings from, the layering order, and the config file worth starting from.
sidebar:
  order: 5
---

Every setting has four possible sources, and later ones win:

```
built-in defaults
  → ~/.config/koda/config.toml          your settings, every project
    → <project>/koda.toml               this project's settings
      → environment variables
        → command-line flags
```

The personal file lives at `$XDG_CONFIG_HOME/koda/config.toml`, falling back to
`~/.config/koda/config.toml`. A project file may be named `koda.toml` or `.koda.toml`.

Two commands do the work for you:

```sh
koda config           # print the effective configuration, after all four layers
koda config --init    # write a fully commented starter file
```

Inside koda, `/settings` is an interactive page covering everything below, and `/setup`
handles the four connection fields on their own.

## Environment variables

| Variable | Also accepted as |
| --- | --- |
| `KODA_BASE_URL` | `OPENAI_BASE_URL` |
| `KODA_API_KEY` | `OPENAI_API_KEY` |
| `KODA_MODEL` | `OPENAI_MODEL` |
| `KODA_DEBUG=1` | — turns on raw request capture |
| `KODA_TRACE=1` | — turns on turn tracing without the web UI |
| `NO_COLOR=1` | — forces the monochrome palette |

## A config file to start from

This is a working file with the interesting knobs turned to their defaults. The full
key-by-key table is in the [configuration reference](/koda/reference/config-keys/).

```toml
# --- connection ---------------------------------------------------------
base_url = "http://localhost:11434/v1"
api_key  = "local"
model    = "qwen2.5-coder:14b"

temperature = 0.2          # low is better for code
top_p = 0.95
max_tokens = 0             # 0 = server default
context_tokens = 110000    # requests are curated to fit this
auto_compact_at = 0.85     # summarize at this fraction of budget; 0 disables

# --- behaviour ----------------------------------------------------------
mode = "execute"           # plan | execute | vibe
tool_protocol = "auto"     # auto | native | text
max_steps = 24             # model<->tool round trips per turn
auto_tier = "ask"          # ask | write | full — cycle live with /auto
sandbox = true             # confine file tools to the workspace root

# --- capabilities -------------------------------------------------------
codegraph = true
sessions = true            # save conversations for /resume, /search, /fork
memory = true
subagents = true
web_search = false         # DuckDuckGo unless searx_url is set
watch = false              # act on AI! / AI? comment triggers
web_ui = false             # local trace + control page on 127.0.0.1
debug = false              # dump raw requests/responses

# --- appearance ---------------------------------------------------------
theme = "auto"             # auto resolves to the neon palette
icons = "auto"             # auto | unicode | ascii
motion = true
reveal = true
mouse_capture = true

# --- limits -------------------------------------------------------------
shell = "/bin/sh"
command_timeout_ms = 120000
max_file_bytes = 262144
max_tool_output_bytes = 24576
max_retries = 3
log_level = "info"

# --- prompt -------------------------------------------------------------
system_prompt = ""         # override the built-in prompt; edit in /settings
instructions = ""          # extra project rules, appended verbatim
```

## Project instructions

koda also reads `AGENTS.md`, `CLAUDE.md` or `.koda.md` from the workspace root and appends
it to the system prompt. If you already keep house rules for another agent, koda picks
them up with no extra file.

`instructions` in the config does the same thing from the config file, and is the right
place for rules that belong to you rather than to the repository.

## What is stored where

| Path | What lives there |
| --- | --- |
| `~/.config/koda/config.toml` | Your settings, all projects. |
| `~/.config/koda/skills/` | Your personal [skills](/koda/skills/). |
| `~/.local/state/koda/koda.log` | The event log behind `/logs`. |
| `~/.local/state/koda/debug/` | Raw request/response captures from `/debug`. |
| `<project>/koda.toml` | This project's settings. |
| `<project>/.koda/skills/` | This project's skills — commit these. |
| `<project>/.koda/sessions/` | Saved conversations, as append-only JSONL. |
| `<project>/.koda/memory.md` | Durable project facts, as plain markdown. |
| `<project>/.koda/learning/` | Learned-rule candidates awaiting `/learn`. |

Everything under `.koda/` is plain text you can read, edit, commit or delete. Sessions,
memory and learning candidates are local state rather than source; the skills are the part
worth committing.
