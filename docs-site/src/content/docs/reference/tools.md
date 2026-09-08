---
title: Tool reference
description: Every tool, its arguments, whether it needs approval, and where it is offered.
---

`/tools` lists what is available in the current session — the list changes with the mode
and with which capabilities you have turned on.

## Reading

### `read_file`
Read a file. Returns numbered lines for text; an aligned table for CSV/TSV; extracted text
for PDF, Word and Excel; an attachment for images.

| Argument | Required | |
| --- | --- | --- |
| `path` | yes | Relative to the workspace root. |
| `offset` | no | First line to return. |
| `limit` | no | How many lines. |

Capped at `max_file_bytes`. No approval.

### `list_dir`
List directory entries, gitignore-aware. `depth > 1` recurses. No approval.

### `find_files`
Find files by glob, e.g. `**/*.rs`. Gitignore-aware. No approval.

### `search`
Regex over file contents. Returns `path:line:text`. When the pattern is really a symbol
lookup, the result comes back with the `codegraph` call to make instead. No approval.

### `codegraph`
Query the symbol graph. No approval.

| `query` | What it returns |
| --- | --- |
| `overview` | The project's modules and what each defines. |
| `symbol` | Where a name is defined, and every use of it. |
| `file` | What a file defines, and who depends on it. |

## Writing

### `write_file`
Create or overwrite a file. Parent directories are created. Shows a unified diff before
and after. **Approval required.**

### `edit_file`
Replace an exact substring. `replace_all` for every occurrence. Shows a unified diff before
and after. **Approval required.**

### `run_command`
Run a shell command in the workspace root. Returns exit code, stdout and stderr. Bounded by
`command_timeout_ms`. **Approval required**, and re-confirmed for destructive commands even
under auto-approve.

## Debugging

### `debug`
Drive a debug adapter. One session at a time.

| Operation | Approval |
| --- | --- |
| `list_adapters`, `status`, `breakpoints`, `output` | none |
| `stack_trace`, `scopes`, `variables`, `evaluate` | none — reading a stopped program is a read |
| `set_breakpoint`, `set_function_breakpoint` | none |
| `launch`, `attach`, `continue`, `step_over`, `step_in`, `step_out`, `pause`, `terminate` | **required** |

`set_breakpoint` takes a `condition`, a hit condition (`>5`, `%10`) counted inside the
adapter, or a `log_message` that prints instead of stopping. See
[Debugger](/koda/debugger/).

## Planning and delegation

### `todo`
The plan drawn above the input. Every result echoes the merged list back and names the
current step. No approval.

### `delegate`
Hand a read-only investigation to a subagent with its own context window. Only the written
report reaches your transcript. Bounded by `subagent_max_steps` and `max_subagent_depth`.
No approval.

### `ask_user`
Ask you a question mid-task. Opens a centred dialog and takes the answer there. In headless
mode the question is reported and the turn proceeds with no answer. No approval.

## Knowledge

### `remember`
Record a durable project fact, or forget one that turned out wrong. Writes to
`.koda/memory.md`. No approval.

### `skill`
Read a project skill by name. No approval.

### `manage_skill`
Write a procedure koda worked out as a skill. With `role`, a delegatable agent.
**Approval required.** Refuses a one-liner (that is a fact), refuses to overwrite an
existing name implicitly, and refuses a second skill claiming the same trigger.

### `manage_agent`
Create, update or delete a role agent. Saved as a skill file. **Approval required.**

## The web

All three are off by default.

### `web_search`
SearXNG or DuckDuckGo. Returns titles, URLs and snippets. No approval.

### `web_fetch`
GET a URL and read it as text. HTML stripped, capped at `max_tool_output_bytes`. `http`
and `https` only. No approval.

### `browse`
Open a URL in a real Chromium and read it after JavaScript has run. Takes a `url` and an
optional `wait_for` CSS selector. No approval.

Content from all three is untrusted data, not instructions.

## Yours

A `[[tools]]` entry in the config becomes a tool with the name and description you give
it. Offered only to the top-level agent, never in plan mode, and run through the same
approval and shell path as `run_command`. See [Custom tools](/koda/customtools/).
