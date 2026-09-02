# Writing koda extensions

koda has three extension points, from easiest to most powerful. None of them
require rebuilding koda — they are all configuration and markdown you drop in
place, and they take effect on the next start (or `/settings` reload).

1. **Custom tools** — teach the agent a new shell-backed action.
2. **Skills** — give the agent reusable, on-demand instructions.
3. **Role agents** — specialised sub-agents the main agent can delegate to
   (and can even create for itself at runtime).

This guide covers all three, with copy-paste examples.

---

## 1. Custom tools

A custom tool is a named shell command the model can call like any built-in.
Declare it in your config (`~/.config/koda/config.toml` for all projects, or a
project-local `koda.toml`) as a `[[tools]]` table.

### Schema

| field | required | meaning |
|---|---|---|
| `name` | yes | Tool name the model calls. Letters, digits, underscores. |
| `description` | yes | One line telling the model what it does and when to use it. This is the *only* thing the model sees, so make it precise. |
| `command` | yes | The shell command to run. `{arg}` placeholders are filled from the call and shell-quoted. |
| `args` | no | Parameter names the command expects; each becomes a string argument the model must supply. |
| `mutating` | no (default `true`) | Whether calling it needs approval. Set `false` only for read-only commands. |

### Examples

A zero-argument tool:

```toml
[[tools]]
name = "typecheck"
description = "Type-check the project and report errors."
command = "npm run -s typecheck"
mutating = false
```

A tool with arguments — `{term}` is substituted and shell-quoted, so the model
cannot inject extra shell syntax:

```toml
[[tools]]
name = "grep_todos"
description = "Find TODO comments matching a term."
command = "rg -n 'TODO.*{term}' ."
args = ["term"]
mutating = false
```

A mutating tool (asks for approval unless autonomy is full-auto):

```toml
[[tools]]
name = "format"
description = "Format the whole repo in place."
command = "cargo fmt"
# mutating defaults to true
```

### How it runs

- Custom tools are advertised **only to the top-level agent** (subagents get a
  read-only tool set) and **never in plan mode** (plan mode cannot touch disk).
- They run through the same shell and approval path as the built-in
  `run_command`, honouring `shell`, `command_timeout_ms`, and the autonomy tier.
- `{placeholder}` values are shell-quoted. A placeholder with no matching `arg`
  is left as-is; supply every placeholder you reference in `args`.

### Tips for good tool descriptions

The description is the model's entire understanding of the tool. Write it the
way you'd write a function docstring for a teammate:

- say **what** it does and **when** to reach for it;
- name the units/format of any argument;
- if it is destructive, say so — the model plans around that.

---

## 2. Skills

A skill is a markdown file with a little frontmatter. Its one-line `when:` is
injected into the system prompt (about 15 tokens); the body is loaded only when
the agent calls the `skill` tool because the task matches. That progressive
disclosure keeps the base prompt short.

### Location

```
~/.config/koda/skills/     your own, every project
<project>/.koda/skills/    this repo's — commit them for your team
```

A project skill overrides a personal one with the same name.

### Format

```markdown
---
name: migrations
when: Writing or reviewing a database migration
---

Migrations live in db/migrate/. Always:
- add a reversible `down`;
- never DROP a column in the same release that stops writing to it;
- run `just db:check` after generating one.
```

`description:` is accepted as a synonym for `when:`. Keep a skill under ~50
lines; if it's longer, it's probably two skills.

---

## 3. Role agents

A **role agent** is a skill with an extra `role:` line. It becomes a specialised
sub-agent the orchestrator (`/orc`) or the `delegate` tool can spin up with its
own fresh context; the body is that agent's operating brief.

```markdown
---
name: qa-agent
role: qa
when: Writing or hardening tests for a change
---

You are the QA agent. Given a change to test:
- read the code and its existing tests first;
- add tests for the new behaviour and the obvious edge cases;
- run the suite and report pass/fail with the exact command you used.
```

Delegate to it explicitly, or let `/orc <task>` decompose work across roles.

### Skills koda writes for itself (`manage_skill`)

koda can **author skills at runtime**. When it works out a procedure that was not
obvious and will recur, it calls `manage_skill` and writes it down; when your
request implies repeated, distinct kinds of work ("build it, test it, review it")
it can give the skill a `role` and delegate to it. `manage_skill` supports:

- `action: create` (default) / `update` / `delete`
- `name` — the skill slug (e.g. `run-integration-tests`)
- `when` — one line: the situation it applies to, so it is found later
- `body` — the procedure: steps, exact commands, what to check
- `role` — optional; set it to make the skill a delegatable agent (e.g. `qa`)

`manage_agent` is still accepted as the old name for this tool.

What it refuses, so the directory stays useful: a body too thin to be a procedure
(that is a fact — use `remember`), a `when` too vague to match later, creating
over an existing name without `action: update`, and a second skill claiming a
trigger an existing one already covers. Writing a skill is a file write, so it
goes through the normal approval gate and shows up in the transcript.

It writes `<project>/.koda/skills/<name>.md` (a normal, editable skill file),
validates that it parses before persisting, and reloads skills immediately so the
skill is usable in the same turn. You can edit, commit or delete that file by
hand afterward — self-authored skills are just skills.

`manage_skill` is available to the top-level agent only; a subagent works from a
narrow slice of context and must not write half-learned procedures. It is a
mutating tool, so plan mode does not offer it and every write is approval-gated.
Adding a `role` needs `subagents = true` to be *delegatable*, but the skill is
written and usable either way.

---

## Choosing an extension point

| You want to… | Use |
|---|---|
| Run a project command the agent should know about | **Custom tool** |
| Encode a convention the agent must follow for a kind of work | **Skill** |
| Delegate a recurring specialised subtask to a focused helper | **Role agent** |

All three are plain config/markdown, version-controllable, and reload without a
rebuild. Start with the simplest that solves your problem.
