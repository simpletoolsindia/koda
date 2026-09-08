---
title: Skills & role agents
description: Instructions loaded only when they are relevant, subagents with their own context, and the procedures koda writes down for itself.
---

A skill is a piece of instruction that is loaded only when it matters. The system prompt
carries one line per skill — roughly fifteen tokens — and the body arrives only when the
task matches. That is what lets you accumulate a hundred project conventions without
spending the context window on them.

```sh
koda skills --init      # write a commented example
koda skills             # list what is loaded, and from where
```

## Writing one

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

The `when:` line is what the model sees in the prompt, so write it as a trigger condition
rather than a title. "Writing or reviewing a database migration" tells the model when to
load the skill; "Migrations" does not.

## Where they live

| Location | Scope |
| --- | --- |
| `~/.config/koda/skills/` | Yours, every project. |
| `<project>/.koda/skills/` | This repository's. Commit these. |

A project skill overrides a personal one of the same name. `/skills reload` picks up edits
without restarting.

## Role agents

Add a `role:` field and the same file becomes a specialised subagent you can delegate to:

```markdown
---
name: qa-agent
role: qa
when: Testing a change end to end
---

Run the suite, report failures with the exact command and output.
```

`koda skills --init` writes a `dev` role example to start from. A role agent can also
[name the model it runs on](/koda/providers/#an-agent-with-its-own-model), because a
reviewer wants a careful model and a scaffolder a fast one.

The main agent calls `delegate` to hand a subtask to a role. `/orc <task>` turns koda into
an orchestrator for the whole job: it decomposes the task, writes a goal / change /
validation brief for each part, hands each to the right role agent, then integrates and
verifies the results.

## Subagents

`delegate` runs a child agent with its own context window and read-only tools. Its tokens
never reach your transcript — only its written report — so a wide search costs you a
paragraph instead of thirty files. Nested calls render with a rail:

```
✓ delegate: find every caller of parse_config  2.1s
  │ ✓ codegraph symbol parse_config
  │ ✓ read src/config.rs (240 lines)
```

In [vibe mode](/koda/modes/) the parent checks every path and line the report cites
against the actual files, and sends it back for another pass if they do not hold. A
subagent that confidently cites a line that says something else does not get believed.

| Setting | Default | What it bounds |
| --- | --- | --- |
| `subagents` | `true` | Whether delegation is offered at all. |
| `subagent_max_steps` | `12` | Step budget for one subagent run. |
| `subagent_review_rounds` | `1` | Vibe-mode re-prompts of a report that does not hold up. |
| `max_subagent_depth` | `1` | How deep delegation nests. `1` means subagents cannot delegate. |

## Skills koda writes for itself

Skills are not only hand-written. When koda works out a procedure that was not obvious and
will come up again — how to run this repository's integration suite, how to add a
subsystem end to end, a release checklist — it calls `manage_skill` to write it down, so
the next session starts with it instead of rediscovering it.

The division is deliberate:

| What it is | Where it goes |
| --- | --- |
| A **fact** | `remember` → [memory](/koda/memory/) |
| A **style rule** | Learned and reviewed with `/learn` |
| A **procedure** | A skill |

Setting `role` makes the same file a delegatable agent, which is how koda spins up a `qa`
or `reviewer` agent for itself.

There are guards, because a directory full of near-duplicate skills is worse than none:
writing one is approval-gated like any file write, a one-liner is refused as a fact rather
than a procedure, an existing name must be updated explicitly, and a second skill claiming
the same trigger is refused.

Everything lands as an ordinary markdown file in `.koda/skills/`, so you can read, edit,
commit or delete it, and `/skills` lists what koda has accumulated.
