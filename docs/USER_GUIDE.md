# koda user guide

koda is a terminal coding agent for **local**, OpenAI-compatible LLMs. It talks
to anything speaking the OpenAI chat-completions API — Ollama, LM Studio,
llama.cpp, and similar — reads and edits your code, runs commands, and shows you
a diff before it changes anything.

This guide covers installation, configuration, every slash command, the tool
set, the operating modes, autonomy tiers, keyboard shortcuts, sessions, role
agents and orchestration, web search, image input, memory, and themes. Every
command, flag, and config key below is drawn directly from the source.

---

## Contents

1. [Install](#install)
2. [Point it at a model](#point-it-at-a-model)
3. [Command line](#command-line)
4. [Configuration](#configuration)
5. [Modes](#modes)
6. [Autonomy tiers](#autonomy-tiers)
7. [Slash commands](#slash-commands)
8. [Keyboard shortcuts](#keyboard-shortcuts)
9. [Tools](#tools)
10. [Sessions: resume, search, fork](#sessions-resume-search-fork)
11. [Role agents and orchestration](#role-agents-and-orchestration)
12. [Web search](#web-search)
13. [Image input](#image-input)
14. [Skills](#skills)
15. [Memory and self-improvement](#memory-and-self-improvement)
16. [Code graph](#code-graph)
17. [Themes and appearance](#themes-and-appearance)

---

## Install

Requires Rust 1.82+.

```sh
git clone https://github.com/simpletoolsindia/koda.git && cd koda
./install.sh
```

`install.sh` builds the release binary and installs it in `~/.local/bin` (no
sudo). Install elsewhere with `PREFIX=/usr/local ./install.sh`.

Or build by hand:

```sh
cargo build --release
cp target/release/koda ~/.local/bin/     # or /usr/local/bin
```

---

## Point it at a model

Start koda and run `/setup`, which fetches the model list from whatever endpoint
you type and writes your config. Or configure from the shell:

```sh
# Ollama
ollama serve
ollama pull qwen2.5-coder:14b
koda -u http://localhost:11434/v1 -m qwen2.5-coder:14b

# LM Studio
koda -u http://localhost:1234/v1

# llama.cpp
koda -u http://localhost:8080/v1
```

If you omit `-m`, koda uses the first model the server reports. `koda models`
lists what is available.

---

## Command line

`koda [OPTIONS] [PROMPT]...` — a bare prompt seeds the TUI; with `-p` it runs
headless.

| Flag | Description |
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
| `--theme <THEME>` | Palette (see [Themes](#themes-and-appearance)). |
| `--icons <ICONS>` | Glyphs: `auto`, `unicode`, `ascii`. |
| `--mode <MODE>` | Start in `plan`, `execute`, or `vibe` mode. |
| `-c`, `--continue` (alias `--resume`) | Reopen the most recent conversation in this project. |

### Subcommands

| Subcommand | Description |
| --- | --- |
| `koda models` | List models reported by the endpoint. |
| `koda skills [--init]` | List skills; `--init` writes a starter skill into `<project>/.koda/skills/`. |
| `koda config [--init]` | Show the effective configuration; `--init` writes a starter config file. |

### Headless mode

`koda -p "your question"` streams the answer to stdout and tool activity to
stderr, then exits. Because there is no user to answer prompts, a write or
command without `--yolo` is denied and koda exits with status 2. If the agent
tries to ask a question with `ask_user`, headless mode reports it and proceeds
with no answer.

---

## Configuration

koda layers configuration in this order (later wins): built-in defaults →
`~/.config/koda/config.toml` → a project `koda.toml` / `.koda.toml` →
environment variables → CLI flags.

Environment variables: `KODA_BASE_URL` / `OPENAI_BASE_URL`, `KODA_API_KEY` /
`OPENAI_API_KEY`, `KODA_MODEL` / `OPENAI_MODEL`.

`koda config --init` writes a fully commented starter file. Every field, with
its default:

| Key | Default | Description |
| --- | --- | --- |
| `base_url` | `http://localhost:11434/v1` | OpenAI-compatible base URL. |
| `api_key` | `local` | API key, if the server needs one. |
| `model` | `""` | Empty auto-picks the first model the server reports. |
| `temperature` | `0.2` | Sampling temperature. |
| `top_p` | `0.95` | Nucleus sampling. |
| `max_tokens` | `0` | 0 = let the server decide. |
| `context_tokens` | `16000` | Soft budget; history is trimmed to fit. |
| `tool_protocol` | `auto` | `auto`, `native`, or `text`. |
| `max_steps` | `24` | Max model↔tool round trips per user turn. |
| `auto_approve` | `false` | Skip approval prompts (equivalent to `auto_tier = full`). |
| `auto_tier` | `ask` | Tiered autonomy: `ask`, `write`, or `full`. |
| `sandbox` | `true` | Confine file tools to the workspace root. |
| `shell` | `/bin/sh` | Shell used for commands. |
| `command_timeout_ms` | `120000` | Command timeout. |
| `max_file_bytes` | `262144` | Max bytes read from a file (also caps attached images). |
| `max_tool_output_bytes` | `24576` | Max bytes of tool output. |
| `instructions` | `""` | Appended verbatim to the system prompt. |
| `sync_output` | `true` | Wrap each frame in DEC 2026 synchronized-update markers. |
| `motion` | `true` | Animate the UI (spinners, gauges, text reveal). |
| `reveal` | `true` | Reveal streaming replies progressively. Needs `motion`. |
| `mouse_capture` | `true` | Capture the mouse for wheel-scrolling. `/mouse` writes this back. |
| `theme` | `auto` | Palette name, or `auto`/`""`. |
| `icons` | `auto` | `auto`, `unicode`, or `ascii`. |
| `sessions` | `true` | Record each session to `<project>/.koda/sessions`. |
| `memory` | `true` | Carry notes and command outcomes in `<project>/.koda/memory.md`. |
| `codegraph` | `true` | Scan the project into a symbol graph on open. |
| `web_search` | `false` | Allow the `web_search` tool. |
| `searx_url` | `""` | Base URL of a SearXNG instance with JSON output enabled. |
| `search_results` | `6` | Results per search. |
| `max_retries` | `3` | Attempts per request before giving up (1 = no retry). |
| `log_level` | `info` | `debug`, `info`, `warn`, or `error`. |
| `log_to_file` | `true` | Mirror the event log to `~/.local/state/koda/koda.log`. |
| `log_detail` | `false` | Show debug-level telemetry in the `/logs` view. |
| `mode` | `execute` | Starting mode: `plan`, `execute`, or `vibe`. |
| `auto_compact_at` | `0.85` | Auto-compact once context passes this fraction of budget; 0 disables. |
| `subagents` | `true` | Let the agent delegate read-only investigations to subagents. |
| `subagent_max_steps` | `12` | Step budget for one subagent run. |
| `subagent_review_rounds` | `1` | Vibe-mode re-prompts of a subagent report; 0 disables review. |
| `max_subagent_depth` | `1` | How deep delegation may nest (1 = subagents cannot delegate). |

koda also reads `AGENTS.md`, `CLAUDE.md`, or `.koda.md` from the workspace root
and appends it to the system prompt.

The config file lives at `$XDG_CONFIG_HOME/koda/config.toml`, or
`~/.config/koda/config.toml` when `XDG_CONFIG_HOME` is unset.

---

## Modes

`ctrl+p` cycles modes (`plan → execute → vibe`); `/mode` shows or sets one. The
current mode is always in the status bar.

| Mode | Label | What it does |
| --- | --- | --- |
| `plan` | `PLAN` | Reads and thinks only. The write and command tools are unavailable, so nothing on disk changes. Produces a plan. |
| `execute` | `EXEC` | Normal operation: edits and commands, each gated by approval. |
| `vibe` | `VIBE` | Autonomous, spec-driven delivery. Writes an explicit spec, plans the steps with the todo list, does the work — **orchestrating** large or many-part tasks by delegating self-contained subtasks to role agents — then verifies its own work (and its subagents') against the spec before finishing. This is the mode for "just get it done." |

In plan mode the agent may only use read-only tools: `read_file`, `list_dir`,
`find_files`, `search`, `delegate`, `todo`, `skill`, `web_search`, `codegraph`,
`remember`.

---

## Autonomy tiers

Autonomy is independent of mode. It decides whether mutating actions need a
prompt. Reading is always free. Cycle live with `/auto` (`ask → write → full`),
or set a tier directly with `/auto ask|write|full`, or in `/settings`.

| Tier | Label | Behaviour |
| --- | --- | --- |
| `ask` (default) | `ASK` | Prompts before every write and command. |
| `write` | `AUTO-WRITE` | Auto-approves `write_file` and `edit_file`; still asks before running commands. |
| `full` | `FULL-AUTO` | Approves everything — autonomous, no prompts. |

`auto_approve = true` (or the `-y`/`--yolo` flag) is equivalent to the `full`
tier.

At an approval prompt, the action row is `y / a / n`:

- `y` (or `enter`) — approve once.
- `a` — always allow this tool for the session.
- `n` (or `esc`) — deny, and tell the model to ask you what to do instead.

The agent can also ask *you* a question mid-task with the `ask_user` tool. Your
next message is treated as the answer, not a new turn.

---

## Browsing live pages

`web_fetch` does a plain HTTP GET, which is enough for a docs page and useless
for anything that renders client-side — you get an empty shell, a spinner, or a
cookie wall. The **browser** setting adds a `browse` tool that opens the URL in a
real headless Chromium and reads the page after its JavaScript has run.

It is **off by default**: it needs Node and Playwright installed and is far
slower than a fetch. Turn it on in `/settings` → **browser**, or:

```toml
browser = true
browser_path = ""   # only if koda cannot find Playwright itself
```

Install what it needs:

```sh
npm i -D playwright && npx playwright install chromium
```

koda looks for Playwright in `browser_path` first, then the project's
`node_modules`, then the global npm root, then the npx cache — which is usually
where it is, because `npx playwright` is how most people first run it. If none
of those has it, `browse` says so and names the install command rather than
failing obscurely.

The tool is called `browse`, and its description says it is the Playwright /
browser tool — so asking koda to "use Playwright" or "open this in a browser"
finds it, rather than getting a literal answer about having no tool by that name.

When `web_fetch` comes back with almost no text, koda adds a note saying the page
probably renders with JavaScript, and points at `browse` — or at the setting, if
the browser is off. A near-empty page otherwise looks like a real one that
happens to be short, and the model reasons about the wrong thing.

The tool takes a `url` and an optional `wait_for` CSS selector for a page that
fills in late. Only `http` and `https` are accepted. What comes back is page
text, and the agent is told to treat it as untrusted data rather than
instructions.

## Watching files for `AI!` comments

`/watch` scans for comments ending in a trigger and acts on them when koda is
idle — the same idea as aider's watch mode.

| | |
| --- | --- |
| `/watch` | Watch the whole workspace. |
| `/watch calc.py` | Watch just those files (`@` prefix optional). |
| `/unwatch` | Stop, and clear the watched list. |

```python
def main():
    pass

# implement sum of digits AI!
```

Save the file and koda picks it up: `AI!` means *do this*, `AI?` means *answer
this*. The trigger has to end the line, and koda acts only when it is idle and
nothing is waiting on you, so triggers never interrupt a running turn or an
approval prompt.

**`AI!` needs execute mode.** Plan mode cannot edit files, so an `AI!` trigger
there comes back as a plan rather than code. koda says so when it happens.
Press `ctrl+p`, or set `mode = "execute"` in your config.

## Multiple providers

koda can hold several named endpoints and switch between them in a word. A
provider is the four things you have to get right before koda can say anything
— where to talk, the key, the model, and whether it takes images — kept together
under a name.

Add one with `/provider add`, which opens the setup page: fill in the **name**
field and it is saved as a provider and selected. Leave the name blank and the
page behaves as it always did, editing the single set of settings.

| Command | What it does |
| --- | --- |
| `/provider` | List saved providers; the active one is marked. |
| `/provider <name>` | Switch to it, and remember the choice. |
| `/provider add` | Open the setup page to save a new one. |

`/settings` has a **provider** row that cycles the same list. The active
provider's name replaces the host in the status bar, because "omniroute" says
more at a glance than "localhost:20128".

In the config file they are table arrays:

```toml
base_url = "http://localhost:11434/v1"   # used when no provider is active
active_provider = "omniroute"

[[provider]]
name = "omniroute"
base_url = "http://localhost:20128/v1"
api_key = "sk-..."
model = "auto"
vision = "on"

[[provider]]
name = "ollama"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:14b"
```

A field left out of a provider falls through to the top-level one, so a provider
can name just a model and inherit the rest. Note that TOML binds a bare key to
whatever table precedes it: keep `active_provider` and the other top-level
settings **above** the first `[[provider]]` block. koda writes them that way.

## Internal servers with a private CA

An endpoint behind a corporate proxy that re-signs TLS with a CA your machine
does not trust will fail to connect. The right fix is installing that CA. When
you cannot, `insecure_tls` accepts the certificate anyway:

```toml
insecure_tls = true          # global

[[provider]]
name = "internal"
base_url = "https://llm.internal.corp/v1"
insecure_tls = true          # or just this one endpoint
```

It is also the **tls** field on `/setup` and the **tls verification** row in
`/settings`, both toggled with `←`/`→`. Changing it takes effect immediately —
koda rebuilds its HTTP client rather than waiting for a restart.

This turns off the check that the server is who it claims to be, so anything
able to sit in the path can read and alter the traffic — your API key and your
source included. koda prints a warning at startup whenever it is on. Set it per
provider rather than globally where you can: a provider may relax TLS for
itself and can never relax it for another, so trusting one internal host cannot
quietly weaken your connection to a public API in the same config.

## Context budget

`context_tokens` is the history budget: koda trims older turns to stay under it.
It defaults to **110000**, and is the **context** field on `/setup`. A provider
can carry its own, since context size belongs to the model behind the endpoint —
a local 8k model and a hosted 200k one should not share a number:

```toml
[[provider]]
name = "local"
context_tokens = 8000        # 0 or omitted inherits the global value
```

## Agents with their own model

A role skill defines an agent you can delegate to. It can now name the model it
runs on, which is a property of the role rather than of whatever the session
happens to be using — a reviewer wants a careful model, a scaffolder a fast one.

```markdown
---
name: reviewer
role: reviewer
model: omniroute/auto
when: reviewing a diff before it ships
---

Read the diff. Flag correctness bugs first, style last.
```

`provider/model` runs the agent on that saved provider — its endpoint and its
key, not the session's. A bare model name uses the current endpoint. Leave
`model` out and the agent runs on whatever the session is using.

In the web UI (`web_ui = true`, then `/logs` or the printed address), the
**Agents & Skills** page has a provider dropdown and a model box that suggests
what that provider actually serves. The dropdown is populated from your saved
providers; the model box still accepts free text, since a provider may serve
models it does not list.

The web UI never receives your API keys — the provider list reports only whether
a key is set.

## Editing what you typed — `#`

Press `#` at the end of what you have written to open a small palette of actions
on the text itself. None of them reach the model or cost a turn.

| Action | What it does |
| --- | --- |
| `#copy` | Copy the whole prompt to the clipboard. |
| `#copyline` | Copy the line the caret is on. |
| `#cutline` | Delete that line (`#undo` puts it back). |
| `#start` / `#end` | Move the caret to the beginning or the end. |
| `#clear` | Empty the input (`#undo` puts it back). |
| `#undo` | Restore what the last action removed. |
| `#paste` | Insert the clipboard's text. |

Type a few letters to narrow the list (`#cl` → `#clear`), `↑`/`↓` to pick,
`enter` to run, `esc` to close and keep what you wrote. A `#` in the middle of a
sentence is left alone as ordinary text, so `fix #3 in the parser` behaves
normally.

## Running things yourself

Two prefixes skip the agent entirely. Neither costs tokens, and neither enters
the conversation the model sees — the output is yours, not context.

| Prefix | What it runs |
| --- | --- |
| `!` | A shell command: `!git status`, `!npm test`, `!git commit -am wip`. |
| `$` | Python: `$ print(sum(range(1, 11)))`, `$ import json; print(json.load(open("p.json"))["version"])`. |

`$` uses `python3` when it is on your PATH and falls back to `python`. Both show
their output in the transcript as a tool block, exactly like a command the agent
ran.

## Slash commands

These are the exact command names koda recognizes. Type `/` to see them all;
`↑`/`↓` to pick, `tab` to complete.

| Command | Description |
| --- | --- |
| `/help` (`/?`) | Keys and commands. |
| `/keys` | Keyboard shortcuts (same panel as `/help`). |
| `/model [name]` | Show the current model, or switch to `name`. |
| `/models` | List models on the server. |
| `/mode [plan\|execute\|vibe]` | Show or set the mode. |
| `/logs` | What the agent has been doing this session. |
| `/debug` | Toggle raw request/response capture (also `KODA_DEBUG=1`). |
| `/reason [off\|low\|medium\|high]` | Cycle or set the model's reasoning effort. |
| `/watch` | Toggle watch mode: act on `AI!` / `AI?` comment triggers when idle. |
| `/websearch` (`/web`) | Turn web search on or off. |
| `/skills [reload]` | List skills, or reload them from disk (`/skills reload`). |
| `/orc <task>` | Orchestrate: decompose a task and delegate to role agents. |
| `/setup` (`/provider`, `/config`) | Set the endpoint, model, and API key. |
| `/settings` (`/preferences`, `/prefs`) | Interactive settings page. |
| `/resume` (`/sessions`) | Open a picker of every saved conversation in this project. |
| `/search <text>` (`/find`) | Search saved conversations by text. |
| `/fork` (`/branch`) | Branch the current conversation into a copy. |
| `/undo` | Put back the files the agent changed in the last turn. |
| `/learn [accept <n>\|all\|reject <n>]` | Review rules koda learned from your usage; accept or reject them (needs `learning = true`). |
| `/session` | Show which session is in play. |
| `/theme [name]` | Show a palette swatch list, or switch to `name`. |
| `/url [url]` (`/endpoint`) | Show or change the API base URL. |
| `/clear` (`/new`, `/reset`) | Drop the conversation context (confirm by running twice). |
| `/compact` | Summarize context to free tokens. |
| `/auto [ask\|write\|full]` | Cycle or set the autonomy tier. |
| `/tools` | List available tools. |
| `/think` | Show or hide model reasoning. |
| `/motion` | Turn animation on or off. |
| `/mouse` (`/select`) | Toggle mouse capture. On, the wheel scrolls and dragging selects. Saved to config. |
| `/reveal` | Toggle progressive text reveal. |
| `/copy` | Copy the last reply to the clipboard. |
| `/cwd` (`/pwd`) | Show the workspace root. |
| `/quit` (`/exit`, `/q`) | Exit koda. |

Notes:

- `/help` and `/keys` open the same combined examples/keys panel.
- `/clear` requires running twice to confirm before wiping the conversation.
- `/mouse` off hands click-drag text selection back to the terminal; scroll then
  uses `pgup`/`pgdn`. The choice is written to the config, so it holds for every
  later session.
- `/motion` and `/reveal` are distinct: `/motion` governs all animation
  (spinners, gauges, text reveal); `/reveal` controls only the progressive
  typing-in of streaming text and takes effect only when motion is on.

---

## Reasoning effort

Thinking models can be told how hard to think. Cycle with `/reason`
(`off → low → medium → high`), or set a level directly (`/reason high`), or set
`reasoning_effort` in config, or use the **reasoning** row in `/settings`. koda
sends it as `reasoning_effort` on the request; servers that do not support it
ignore the field, and `off` omits it.

## Watch mode

Watch mode (aider-style) acts on inline comment triggers automatically. Enable it
with `/watch`, the **watch mode** row in `/settings`, or `watch = true`.

End a comment with a trigger token:

- `AI!` — implement the request in that file. koda reads the file, makes the
  change, and removes the trigger comment so it does not run again.
- `AI?` — answer the question (read-only; no edits).

```python
# implement a retry wrapper around fetch(), 3 attempts with backoff  AI!
```

koda rescans the workspace (gitignore-aware) every `watch_interval_ms` and only
acts when it is idle — no turn running, nothing queued, no prompt open.

## Debug capture

`/debug` toggles raw request/response capture (also `debug = true` or launching
with `KODA_DEBUG=1`). While on, koda writes each turn's exact request body and
raw streamed response to `~/.local/state/koda/debug/rr-session-N.json` and
`rr-session-N.res.log`. `/debug` prints the directory. This is what the web
control center's **Raw Captures** panel reads. (The turn trace below does not
need it — tracing is always on when the web UI is.)

## Web control center (trace + control)

koda can serve a local React page on `127.0.0.1` that traces every turn end to
end and lets you drive the running session from the browser. One page, three
regions:

- **Turn rail (left)** — every turn, newest first, with status, duration, step
  and token counts. The turn that is running is followed automatically.
- **Trace waterfall (centre)** — that turn's steps in order, as a timeline with
  duration bars: each model call, each tool call, and each compaction. Retries,
  failures and denied approvals are called out inline. A live turn streams new
  steps in as they happen.
- **Inspector + control rail (right)** — two tabs. *Inspector* shows the payloads
  behind the selected step: the exact request body koda sent, the raw SSE stream
  that came back, the model's reasoning, and, for a tool, its arguments, result
  and applied diff. It also shows a **Prompt Δ** — a diff against the previous
  model call, which is how you see exactly what compaction dropped or what a
  newly learned rule added. *Control* is the live session: model, endpoint, mode,
  autonomy tier, reasoning effort, max steps, feature toggles, project memory,
  learned-rule candidates (accept/reject), and saved sessions (resume/fork).

Two more surfaces sit alongside it: a **Logs** drawer (the live event log, press
`L`) and a **Manage** panel with the code graph, skills/role-agent editor, system
prompt editor and raw captures.

`⌘K` / `Ctrl+K` opens a command palette: jump to a turn, switch model or mode,
toggle a feature, export a trace, or type `@name` and press `Shift+Enter` to look
a symbol up in the code graph.

Changes made in the browser are applied to the *running* koda, not just written
to disk: the control rail queues a request, the TUI picks it up within a fraction
of a second, and the terminal shows the same notice it would for `/mode`,
`/remember` or `/learn`.

To run it:

1. Enable it — the **web ui** toggle in `/settings`, or in config:

   ```toml
   web_ui = true
   web_ui_port = 7717     # optional, the default
   ui_detail = "medium"   # simple | medium | high — how much the log drawer shows
   ```

2. Start koda. It prints the address, e.g. `koda: web UI at http://127.0.0.1:7717`.

3. Open that URL. The server binds to localhost only. Tracing keeps the last 50
   turns in memory with truncated payloads, so a long session stays bounded; the
   palette's *Clear the trace ring* drops them.

`KODA_TRACE=1` forces tracing on even without the web UI (useful when you want
the ring populated for a later look).

---

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `enter` | Send the message. |
| `ctrl+j` (or `alt/ctrl+enter`) | Insert a newline. |
| `ctrl+c` | Interrupt the current turn; press twice to quit. |
| `ctrl+d` | Quit (deletes a character if the input is non-empty). |
| `ctrl+l` | Clear the screen (confirm by pressing twice). |
| `ctrl+p` | Cycle mode (`plan → execute → vibe`). |
| `ctrl+r` | Expand the last tool output. |
| `ctrl+t` | Expand the last reasoning block. |
| `pgup` / `pgdn` | Scroll the transcript (mouse wheel works too). |
| `up` / `down` | Input history / picker navigation. |
| `tab` | Complete a command, or pick a mentioned file. |
| `@` | Mention a file (or attach an image). |
| `ctrl+a` / `ctrl+e` | Start / end of line. |
| `ctrl+k` / `ctrl+u` / `ctrl+w` | Kill to end / start / previous word. |
| `alt+b` / `alt+f` (or `alt+←/→`) | Move by word. |
| `esc` | Interrupt if busy, otherwise clear the input; closes an overlay. |
| `y` / `a` / `n` | At an approval prompt: once / always this tool / deny. |
| `↑` / `↓` at an approval prompt | Scroll the pending tool preview. |

---

## Tools

The model chooses these itself; you approve the mutating ones according to your
autonomy tier. `/tools` lists them live.

| Tool | Mutating | Description |
| --- | --- | --- |
| `read_file` | no | Read a UTF-8 text file; returns numbered lines. Use `offset`/`limit` for large files. |
| `list_dir` | no | List directory entries; respects `.gitignore`; `depth>1` recurses. |
| `find_files` | no | Find files by glob, e.g. `**/*.rs`; respects `.gitignore`. |
| `search` | no | Regex search across file contents; returns `path:line:text`. |
| `write_file` | **yes** | Create or overwrite a file; parent dirs are created. |
| `edit_file` | **yes** | Replace an exact substring in a file (`replace_all` for every occurrence). |
| `ask_user` | no | Ask the user a question and wait for the answer. |
| `remember` | no | Record a durable project fact (or `forget` one that turned out wrong). |
| `codegraph` | no | Query the symbol graph: `overview`, `symbol`, or `file`. |
| `skill` | no | Read a project skill by name before doing that kind of work. |
| `web_search` | no | Search the web for docs, errors, versions (returns titles/URLs/snippets). |
| `todo` | no | Track a multi-step plan the user can watch progress on. |
| `delegate` | no | Hand a read-only investigation to a subagent with fresh context. |
| `manage_agent` | **yes** | Create, update or delete a specialised role agent on the fly (saved as a skill). |
| `run_command` | **yes** | Run a shell command in the workspace root; returns exit code, stdout, stderr. |

File tools run in-process, so they are fast and confined by the `sandbox`
setting. Writes and `run_command` require approval per your autonomy tier; every
write shows a unified diff before it is applied.

---

## Sessions: resume, search, fork

With `sessions = true`, koda records each conversation to
`<project>/.koda/sessions/` as append-only JSONL — one header line, then one
line per message. That store powers three commands:

- **`/resume`** (also `koda -c` / `--continue` / `--resume` from the shell)
  opens a picker of every past session in this project, not just the last one.
  `koda -c` from the shell reopens the most recent one directly.
- **`/search <text>`** does a case-insensitive full-text scan across every saved
  session and shows the matches newest-first, each with a hit count. Press
  `enter` to open one.
- **`/fork`** branches the current conversation. It byte-copies the JSONL with a
  fresh id, then continues on the branch, so the original is left untouched.
  You need to have said something first (there must be a session to fork).

`/session` reports which session file is currently in play.

---

## Role agents and orchestration

A skill file with a `role:` line becomes a specialised subagent. The role's body
becomes that agent's operating instructions.

```markdown
---
name: qa-agent
role: qa
when: Testing a change end to end
---
Run the suite, report failures with the exact command and output.
```

`koda skills --init` writes both an `example` skill and a `dev`-role starter
(`dev-agent.md`). Common roles are `dev`, `qa`, `tester`, and `manager`, but any
role name works as long as a skill file defines it.

Two ways to use roles:

- **`delegate`** — the main agent can hand a subtask to a role by passing
  `role` (e.g. `dev`, `qa`). The subagent runs read-only in its own context and
  returns a written report.
- **`/orc <task>`** — a shortcut that switches to **vibe** mode and runs the
  task there. Vibe is the unified spec-driven mode: it lays out subtasks with
  `todo`, delegates each to the right role agent, and verifies the integrated
  result. (Orchestration used to be a separate concept; it is now just what vibe
  mode does, so `/orc` and `ctrl+p → vibe` lead to the same place.)

Subagents cannot modify files or run commands — they can only read, list, find,
and search. In vibe mode, the parent checks each subagent report against the
actual files and can send it back for another pass (`subagent_review_rounds`).

---

## Web search

Web search is off until you turn it on with `/websearch` (or `web_search = true`
in config). It has two backends, chosen automatically:

- **SearXNG** — used when `searx_url` is set. Private and self-hosted; the
  instance needs `json` in `search.formats` in its `settings.yml`.
- **DuckDuckGo** — the fallback when no `searx_url` is configured. It uses
  DuckDuckGo's keyless HTML endpoint, so search works out of the box with
  nothing to set up.

```toml
web_search = true
searx_url = "http://localhost:8888"   # optional; omit to use DuckDuckGo
search_results = 6
```

When you enable it with `/websearch`, koda tells you which backend is active
("your SearXNG instance" or "DuckDuckGo"). Results are titles, URLs, and
snippets — not full pages.

---

## Image input

Mention an image the way you mention any file — `@screenshot.png` — and koda
attaches it to your message as vision content for a vision-capable model.
Recognized image types are detected by extension; a `@`-token that resolves to
an image under the workspace is read, size-checked against `max_file_bytes`, and
encoded as a data URL. koda shows an "attached image" notice when it succeeds.

Non-image `@` mentions are left as plain paths that the model can open with
`read_file`. A text-only model simply never sees the extra vision content, so it
is safe to leave on.

---

## Skills

A skill is instructions loaded only when relevant, so the system prompt stays
short. The prompt carries one line per skill (its `name` and `when`); the body
arrives only when you call the `skill` tool.

```sh
koda skills --init      # writes a commented example + a dev-role starter
koda skills             # list what is loaded, and from where
```

Skills are plain markdown with frontmatter:

```markdown
---
name: migrations
when: Writing or reviewing a database migration
---

Migrations live in db/migrate/, named <timestamp>_<verb>_<subject>.sql.
```

`when:` may also be written as `description:` or `desc:`. A `role:` line turns
the skill into a [role agent](#role-agents-and-orchestration).

Skills are read from `~/.config/koda/skills/` (yours, every project) and
`<project>/.koda/skills/` (the repo's — commit them for your team). A project
skill overrides a personal one of the same name. Reload after editing with
`/skills reload`.

### Skills koda writes for itself

You don't have to author them all. When koda works out a procedure that was not
obvious and will come up again — how to run this repo's integration suite, how to
add a subsystem end to end, a release checklist — it writes it down itself with
the `manage_skill` tool, into `<project>/.koda/skills/<name>.md`. The next session
starts with that procedure instead of rediscovering it.

Three artifacts, kept separate on purpose:

| You get | For | Reviewed with |
| --- | --- | --- |
| `remember` | a durable **fact** ("tests run with `cargo test -- --test-threads=1`") | `.koda/memory.md` |
| a learned rule | a **style/convention** koda inferred from your corrections | `/learn` |
| a **skill** | a **procedure**: steps, commands, what to check | `/skills`, the file itself |

Adding a `role` makes the same file a delegatable agent — which is how koda spins
up a `qa` or `reviewer` agent for itself mid-task. See
[role agents](#role-agents-and-orchestration).

It is deliberately hard to spam: writing a skill is a file write, so it is
approval-gated like any other and appears in the transcript; a body too thin to be
a procedure is refused as a fact; a vague `when` is refused because nothing would
match it later; an existing name must be updated explicitly; and a second skill
claiming a trigger an existing one already covers is refused. Everything is a
plain markdown file, so you can read, edit, commit or delete whatever it wrote —
`koda skills` shows what has accumulated and where each one came from.

---

## Memory and self-improvement

With `memory = true`, koda keeps `<project>/.koda/memory.md` — plain markdown you
can read, edit, or delete. Nothing is inferred behind your back and nothing is
hidden. It records three things:

- **Notes** — durable facts the agent chose to remember through the `remember`
  tool (say `forget` with a phrase to drop one). Capped at 60.
- **Commands** — which shell commands succeeded and failed here, so the next
  session runs your real test command instead of guessing. Capped at 40.
- **Files** — how many times each file has been edited, so koda learns which
  parts of the project you actually work in ("hot files") and can orient there
  first next session. This is observed fact, not inference about intent.

### Learning from usage (`learning = true`, `/learn`)

A step beyond memory: with `learning = true` (off by default), koda watches how
you actually work and distils **explicit, inspectable rules** it can follow next
time. It is fully local — no model, no network — and every artifact is a plain
file under `<project>/.koda/learning/` you can read, edit, or delete.

- **Observation log** — `.koda/learning/observations.jsonl` records the raw
  signals koda sees each turn: the edits it made (the before/after of a write),
  which commands succeeded or failed, and approvals you denied. Append-only and
  greppable.
- **Rule induction** — at the end of each turn koda mines those observations,
  deterministically, into **candidate rules**: the command that actually works
  here (and ones that only ever failed), the project's function-naming convention,
  and library preferences. A rule needs repeated evidence before it is proposed,
  so one-offs are ignored.
- **Learning from your corrections** — koda remembers what it last wrote to each
  file (under `.koda/learning/last_writes/`, so it works across sessions). When
  you change that file yourself — swapping koda's `logging` for your internal
  `log.audit`, say — koda notices the difference next time it reads or edits the
  file and, once you've made the same kind of change a couple of times, proposes a
  rule like *"prefer `log.audit` over `logging`."* This is the strongest signal
  for adapting to your style; koda only acts on changes it can attribute clearly
  (a single swapped token), never guesses.
- **Learning your project's idioms** — koda reads the code graph and notices the
  internal helpers, APIs and modules that are load-bearing here — a function
  defined once and called across many files, a module imported everywhere. It
  proposes rules like *"`log_audit` is a load-bearing function here — prefer it
  over reinventing an equivalent,"* so koda reaches for your project's own tools
  instead of generic ones. This runs once per session from the code graph, with
  no model involved.
- **Human-in-the-loop promotion** — candidates never touch the prompt on their
  own. Run `/learn` to review them, then `/learn accept <n>` (or `/learn all`) to
  accept, or `/learn reject <n>` to drop one. Accepted rules are written to
  `.koda/learning/rules.md` and injected into the system prompt so koda follows
  them automatically from then on.
- **Kill switch** — set `learning = false`, or delete `.koda/learning/`, and the
  whole loop is gone with no residue. If you edit or delete a rule, that is
  authoritative.

This is Phases 1–3 of a larger self-improvement design (see
`docs/research-self-improvement.md`): a later phase adds a local semantic example
library, on the same all-local, fully-inspectable footing.

---

## Code graph

On open, with `codegraph = true`, koda scans the project into a symbol graph —
definitions, references, imports — exposed to the model through the `codegraph`
tool so it can ask where something lives instead of grepping. Three questions:

- `overview` — maps the project.
- `symbol` — where a name is defined and which files use it.
- `file` — what a file defines and imports, and who depends on it.

It is regex-based rather than a full parser: accurate enough to point at the
right file, which the model then reads properly.

---

## Themes and appearance

`/theme` switches the palette live and, with no argument, shows a swatch of each
so you can pick by eye. The `--theme` flag and `theme` config key set it at
start.

Available palettes: `dark`, `neon`, `ansi`, `catppuccin-mocha`, `tokyo-night`,
`gruvbox-dark`, `nord`, `dracula`, `rose-pine`, `solarized-light`, `mono`.

The default (`theme = "auto"` or empty) resolves to the vibrant **neon**
palette, because the block fills that give the transcript its shape need
predictable colours. `NO_COLOR=1` or `TERM=dumb` forces the monochrome `mono`
palette regardless of config. The `ansi` palette uses your terminal's own 16
colours and drops the block fills for a rule.

Other appearance controls:

- `icons` (`auto`/`unicode`/`ascii`) picks the glyph set; `ascii` replaces box
  drawing and braille for terminals that cannot render them.
- `/motion` toggles all animation; `/reveal` toggles just the progressive text
  reveal. `NO_MOTION`/`REDUCED_MOTION` env vars and a non-tty stdout disable
  animation regardless of config.
- `/mouse` toggles mouse capture: on, the wheel scrolls the transcript; off, you
  can select and copy text with the mouse and scroll with `pgup`/`pgdn`. The
  toggle is saved to the config, and the same switch sits on the `/settings`
  page as **mouse capture**.
- **Selecting text while capture is on.** An application reading the mouse takes
  click-drag away from the terminal, so koda does the selecting itself: drag
  across the transcript to highlight, release to copy. The wheel keeps
  scrolling, so you no longer have to choose between the two. koda asks only for
  the tracking modes it actually reads — button, drag, and SGR coordinates — and
  deliberately not any-event tracking (`?1003`), which floods the event loop and
  stops many terminals honouring their own selection override.
- Prefer your terminal's native selection? `/mouse` turns capture off and hands
  the mouse back; scrolling is then `pgup`/`pgdn`.
- `sync_output` (DEC 2026 synchronized updates) presents each frame atomically
  to stop tearing.
