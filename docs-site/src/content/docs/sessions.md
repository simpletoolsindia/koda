---
title: Sessions & history
description: Every conversation saved as append-only JSONL, and the three commands that make that useful.
---

With `sessions = true`, koda records each conversation to `<project>/.koda/sessions/` as
append-only JSONL — one header line, then one line per message. It is a plain text format,
per project, that you can grep yourself.

## Three commands

**`/resume`** (also `koda -c`, `--continue` or `--resume` from the shell) opens a picker of
every past session in this project, not just the last one. `koda -c` from the shell
reopens the most recent one directly.

**`/search <text>`** does a case-insensitive full-text scan across every saved session and
shows the matches newest-first, each with a hit count. This is the one that pays for the
feature: *"what did we decide about the retry backoff"* is a question the transcript can
answer months later.

**`/fork`** branches the current conversation into a copy. Take a session that got
somewhere useful and try a second approach from that point without losing the first.

`/session` shows which session is in play.

## When to compact instead

A long session eventually costs more in context than it is worth. Two different answers:

| Command | What happens |
| --- | --- |
| `/compact` | koda writes a real summary of the conversation and keeps that. The project understanding survives; the token cost does not. |
| `/clear` | The conversation context is dropped. Run twice to confirm. |

`auto_compact_at = 0.85` compacts automatically once context passes that fraction of the
budget. Set it to `0` to disable.

Neither touches the session file. Compaction and the request curation described in
[Prompt & context](/koda/prompt/) both operate on what is *sent*, so the transcript on disk
stays complete.

## Where it lives

```
<project>/.koda/sessions/1788175551-3000-0000.jsonl
```

These are local state rather than source. koda's own `.gitignore` excludes them, and yours
probably should too — unlike `.koda/skills/`, which is worth committing.
