# koda

![Rust](https://img.shields.io/badge/rust-1.82%2B-orange?logo=rust)
![Platform](https://img.shields.io/badge/platform-macOS%20(Apple%20Silicon)-black?logo=apple)
![License: MIT](https://img.shields.io/badge/license-MIT-blue)
![Binary](https://img.shields.io/badge/binary-~6%20MB-green)
![Startup](https://img.shields.io/badge/startup-~3%20ms-green)

A terminal coding agent for **local** LLMs. It talks to anything speaking the
OpenAI chat-completions API — Ollama, LM Studio, llama.cpp, MTPLX, vLLM — reads
and edits your code, runs commands, and shows you a diff before it changes
anything.

Built for macOS on Apple Silicon. One binary, ~6 MB, 3 ms startup, no runtime
dependencies.

```
▌ the discount test fails — fix it                      ← warm tint, your message

  plan                                     2/3 ████████░░░░
  ✓ read the failing test
  ✓ fix apply_discount
  ◐ run the suite

  ✓ codegraph symbol apply_discount  4ms                ← cool tint, tool block
  ✓ read cart.py (21 lines)  2ms  ctrl+r
  ✓ edit cart.py (1 replacement)  3ms
   │      @@ -14,3 +14,3 @@
   │   14  def apply_discount(amount, percent):
   │   15 -    return amount - percent
   │   15 +    return amount * (1 - percent / 100)
  ✓ $ python3 -m pytest -q → exit 0  380ms

Fixed. `apply_discount` subtracted the percent instead of applying it.

 EXEC  ● ready  · 3/3 steps                     @ file · ctrl+p mode · /help
 ❯
 qwen2.5-coder:14b ❯ myproject ❯ main ❯ localhost:11434   4.1k tok  ██▎░░░░░ 27%
```

## Install

Requires Rust 1.82+ (`brew install rust` or [rustup](https://rustup.rs)).

```sh
git clone https://github.com/simpletoolsindia/koda.git && cd koda
cargo build --release
cp target/release/koda ~/.local/bin/     # or /usr/local/bin
```

## Point it at a model

Run `koda`, press `/setup`, and fill in the endpoint, model and API key — it
fetches the model list from whatever URL you type and writes your config for you.
Or do it from the shell:

```sh
# Ollama
brew install ollama && ollama serve
ollama pull qwen2.5-coder:14b
koda -u http://localhost:11434/v1 -m qwen2.5-coder:14b

# LM Studio — start the server from the Developer tab
koda -u http://localhost:1234/v1

# llama.cpp
llama-server -m model.gguf -c 32768 --jinja --port 8080
koda -u http://localhost:8080/v1
```

Omit `-m` and koda uses the first model the server reports. `koda models` lists
what is available.

### Model choice matters more than anything else

Tool calling is what makes an agent work, and small models are bad at it. On a
32–64 GB Mac, in rough order: **Qwen2.5-Coder 14B/32B**, **Devstral Small**,
**GLM-4 9B**. Below ~7B, expect the model to lose the tool format; koda's text
protocol helps but cannot fix a model that will not follow instructions.

Give it at least 16k context. Agent conversations fill up fast with file
contents.

## Modes

`ctrl+p` cycles; the current mode is always in the status bar.

| Mode | What it does |
| --- | --- |
| `PLAN` | Reads and thinks. The write and command tools are unavailable, so nothing on disk changes. Produces a plan and asks you to switch. |
| `EXEC` | Normal operation. Edits and commands, each gated by approval. |
| `VIBE` | Writes an explicit spec first (goal, done-when, files, validation), does the work, then checks the result against its own spec — and verifies any subagent's claims against the files before believing them. |

## Keys

| Key | Action |
| --- | --- |
| `enter` | send · `ctrl+j` newline |
| `ctrl+c` | interrupt · twice to quit · `ctrl+d` quit |
| `ctrl+p` | cycle mode |
| `ctrl+r` | expand the last tool output |
| `ctrl+t` | expand the last reasoning block |
| `pgup`/`pgdn` | scroll · `up`/`down` input history |
| `ctrl+a/e/k/u/w` | line editing · `tab` complete a command |
| `y` / `a` / `n` | at an approval prompt: once / always / deny |

## Commands

| Command | |
| --- | --- |
| `/setup` | endpoint, model and API key, written to your config |
| `/mode` | plan, execute or vibe |
| `/logs` | every request, tool call, timing and failure this session |
| `/theme` | switch palette |
| `/skills` | list skills · `/skills reload` after editing one |
| `/websearch` | turn web search on or off |
| `/compact` | summarize the conversation to free context |
| `/resume` `/search` `/fork` | reopen, search by text, or branch a saved conversation |
| `/orc` | orchestrate: split a task across role agents |
| `/models` `/model` `/url` | what the server has, and what you are using |
| `/auto` | cycle autonomy: ask → auto-write → full-auto |
| `/settings` | interactive settings page for everything below |
| `/think` | show or hide model reasoning |
| `/motion` `/reveal` | animation on/off · progressive text reveal on/off |
| `/keys` `/tools` `/copy` `/clear` `/cwd` `/help` `/quit` | |

## Autonomy

koda has three autonomy tiers, cycled live with `/auto` (or set in `/settings`):

| Tier | Behaviour |
| --- | --- |
| `ASK` (default) | Prompts before every write and command. |
| `AUTO-WRITE` | Auto-approves file writes; still asks before running commands. |
| `FULL-AUTO` | Approves everything — autonomous, no prompts. Shown in red in the status bar. |

Approval prompts are a loud, docked block: amber for a write, red for a command, with a clear `y / a / n` action row. The agent can also ask *you* a question mid-task with the `ask_user` tool — your next message is the answer.

## Role agents and orchestration

A skill file with a `role:` field becomes a specialised subagent:

```markdown
---
name: qa-agent
role: qa
when: Testing a change end to end
---
Run the suite, report failures with the exact command and output.
```

`koda skills --init` writes a `dev` role example. The main agent can `delegate`
a subtask to a role, and `/orc <task>` turns koda into an orchestrator: it
decomposes the task, writes a goal/change/validation brief for each part, hands
each to the right role agent, then integrates and verifies the results.

## Images

Mention an image the way you mention any file — `@screenshot.png` — and koda
attaches it to the message for a vision-capable model (`.png`, `.jpg`, `.gif`,
`.webp`). Non-image mentions stay as paths the model can open with `read_file`.
A model with no vision simply never sees the extra content, so this is safe to
leave on. Attachments are size-capped at `max_file_bytes`.

## Tools

| Tool | Approval | |
| --- | --- | --- |
| `read_file` | — | numbered lines, `offset`/`limit` for big files |
| `list_dir` `find_files` `search` | — | gitignore-aware; glob and regex |
| `codegraph` | — | where a symbol is defined and who uses it |
| `skill` | — | project conventions, loaded on demand |
| `todo` | — | the plan you see in the transcript |
| `remember` | — | durable facts, kept for next session |
| `delegate` | — | hand a read-only investigation to a subagent |
| `web_search` | — | your SearXNG instance, off by default |
| `write_file` `edit_file` | asks | shows a diff first |
| `run_command` | asks | builds, tests, git |

File tools run in-process, so they are fast and cannot be talked into doing
something `sh` would.

### Safety

- Writes and commands need your approval. `y` once, `a` for the session, `n`
  denies and tells the model to ask you instead.
- Every write shows a unified diff **before** it is applied, and again after.
- `sandbox = true` confines file tools to the workspace root. Off by default so
  you can read across projects; turn it on if you want the belt.
- `-y` / `--yolo` skips approvals. Genuinely dangerous with a model that
  hallucinates `rm -rf`; use it in a git repo with a clean tree.
- Headless has nobody to ask, so a write without `--yolo` is denied and koda
  exits 2.

## Subagents

`delegate` runs a child agent with its own context window and read-only tools.
Its tokens never reach your transcript — only its written report — so a wide
search costs you a paragraph instead of thirty files. Nested calls render with a
rail:

```
✓ delegate: find every caller of parse_config  2.1s
  │ ✓ codegraph symbol parse_config
  │ ✓ read src/config.rs (240 lines)
```

In vibe mode the parent checks every path and line the report cites against the
actual files, and sends it back for another pass if they do not hold.

## Skills

A skill is instructions loaded only when relevant, so the prompt stays short.
The prompt carries one line per skill; the body arrives when the task matches.

```sh
koda skills --init      # writes a commented example
koda skills             # list what is loaded, and from where
```

```markdown
---
name: migrations
when: Writing or reviewing a database migration
---

Migrations live in db/migrate/, named <timestamp>_<verb>_<subject>.sql.

- Always write the down migration.
- Never DROP a column in the same release that stops writing to it.
- Run `just db:check` afterwards — it catches missing FK indexes.
```

Read from `~/.config/koda/skills/` (yours) and `<project>/.koda/skills/` (the
repo's — commit them). A project skill overrides a personal one of the same name.
An unused skill costs about fifteen tokens.

## Code graph

On open, koda scans the project into a symbol graph — definitions, references,
imports — so the model can ask where something lives instead of grepping for it.
Three questions: `overview` maps the project, `symbol` locates a name and its
users, `file` lists what a file defines and who depends on it. Regex-based rather
than a full parser: accurate enough to point at the right file, which the model
then reads properly.

## Memory

With `memory = true`, koda keeps `<project>/.koda/memory.md`: facts it recorded
through `remember`, plus which commands succeeded and failed here. Next session
starts knowing your test runner. It is plain markdown you can read, edit or
delete — nothing is inferred behind your back and nothing is hidden.

## Web search

Off until you point it at a SearXNG instance you control, which needs `json` in
`search.formats` in its `settings.yml`.

```toml
web_search = true
searx_url = "http://localhost:8888"
```

Toggle per session with `/websearch`.

## When things go wrong

Failures do not put stack traces on your screen. Transient ones — connection
reset, 429, 5xx, an empty stream — are retried with backoff. What you see is one
plain sentence; the full detail goes to the event log.

```
/logs          in the TUI
~/.local/state/koda/koda.log
```

The status bar shows a count when anything was logged as a warning or error, so
you know to look.

## How it looks

Blocks are grouped by a **tinted fill**, not a border: warm behind your messages,
cool behind tool output, red behind failures. A fill costs no rows and the tint
carries the block's kind, so the transcript reads as composed rather than printed.

Structured output — `/help`, `/models`, `/theme`, `/skills`, `/tools` — renders as
filled blocks with a bold amber heading, the title on the left and a hint on the
right, sized to your terminal with long rows clipped.

The bottom bar is chevron-separated segments, each in its own colour: model,
project, branch, endpoint, then tokens and a context gauge with eighth-block
precision. Above the input sits the mode chip, what the agent is doing, step
progress, and only the keys that apply right now.

The plan (`todo` tool) gets a standing block with a progress gauge: done steps are
struck through and the step in flight is the only bold row.

`/theme` shows a swatch of each palette so you can pick by eye. The default is a
dark palette, because the fills need colours that can be predicted — `ansi` uses
your terminal's own sixteen and drops the fills for a rule instead.

## Themes

`/theme` switches live. `ansi` (the default) uses your terminal's own 16 colours,
so it matches whatever you already run. Also: `catppuccin-mocha`, `tokyo-night`,
`gruvbox-dark`, `nord`, `dracula`, `rose-pine`, `solarized-light`, `mono`.

`NO_COLOR=1` or `TERM=dumb` forces monochrome — hierarchy then comes from bold
and dim alone. `icons = "ascii"` replaces box drawing and braille for terminals
that cannot render them. The layout adapts at 92 and 64 columns.

## Configuration

`~/.config/koda/config.toml`, overridden by a `koda.toml` in your project, then
environment (`KODA_BASE_URL`, `KODA_MODEL`, `KODA_API_KEY`, or the `OPENAI_*`
equivalents), then CLI flags.

```toml
base_url = "http://localhost:11434/v1"
api_key = "local"
model = "qwen2.5-coder:14b"

temperature = 0.2          # low is better for code
max_tokens = 0             # 0 = server default
context_tokens = 16000     # history is trimmed to fit
auto_compact_at = 0.85     # summarize at this fraction; 0 disables

mode = "execute"           # plan | execute | vibe
tool_protocol = "auto"     # auto | native | text
max_steps = 24             # model<->tool round trips per turn
auto_approve = false
sandbox = false

subagents = true
subagent_max_steps = 12
subagent_review_rounds = 1 # vibe-mode re-prompts of a bad report

codegraph = true
memory = true
web_search = false
searx_url = ""

max_retries = 3
log_level = "info"         # debug | info | warn | error
log_to_file = true

theme = "auto"
icons = "auto"
shell = "/bin/sh"
command_timeout_ms = 120000

instructions = ""          # extra project rules for the prompt
```

koda also reads `AGENTS.md`, `CLAUDE.md` or `.koda.md` from the workspace root.

## Tool protocols

Local servers vary in how well they implement OpenAI tool calls:

- `auto` (default) — advertise native `tools`, and also accept
  `<tool_call>{...}</tool_call>` text blocks. If the server rejects the `tools`
  field, koda switches to text mode and carries on.
- `native` — native `tool_calls` only.
- `text` — no `tools` field; the model is told to emit blocks. For models with no
  tool support at all.

Tool blocks are stripped from what you see, even when a tag arrives split across
streaming chunks.

## Why it stays fast

- Each transcript block caches its rendered lines, so a streaming token
  re-lays-out only the block that changed.
- Redraws are coalesced: a burst of tokens is one frame, and nothing is drawn
  when nothing changed.
- Only the visible window of lines reaches the renderer.
- File tools are in-process; no subprocess per read.
- The code graph is two passes over the project, not a parse per query.
- The system prompt is short on purpose — every token of it is a token the model
  does not spend on your code.

## Tests

```sh
cargo test          # unit tests
bash tests/e2e.sh   # end-to-end against a mock server, including the TUI
```

`tests/mock_server.py` is a scripted OpenAI-compatible SSE server covering both
tool protocols, the fallback path, empty and reasoning-only replies, delegation,
the code graph, and memory. `tests/tui_test.py` drives the real TUI through a
pseudo-terminal and reconstructs the screen with a small VT100 interpreter, so it
asserts on what a user actually sees.

## License

MIT
