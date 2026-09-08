---
title: Watch mode
description: Aider-style inline triggers — end a comment with AI! and koda acts on it the moment it is idle.
---

Turn it on with `/watch`, the **watch mode** row in `/settings`, or `watch = true`.

Then end a comment with a trigger token, save the file, and koda picks it up:

```python
# implement binary search over `items`, return the index or -1  AI!
```

| Token | What koda does |
| --- | --- |
| `AI!` | Implement the request in that file. koda reads it, makes the change, and removes the trigger comment so it does not fire again. |
| `AI?` | Answer the question. Read-only; no edits. |

## When it fires

koda rescans the workspace every `watch_interval_ms` (default 1500), gitignore-aware, and
only acts when it is genuinely idle: no turn running, nothing queued, and no prompt open.
Only files whose modification time or size changed since the last sweep are actually read.

```toml
watch = true
watch_interval_ms = 1500
```

## Why the comment is removed

An `AI!` left in the file fires again on the next sweep, and again after that. Removing it
as part of the edit is what makes the trigger a one-shot instruction rather than a
standing one — and it means the diff you approve includes the removal, so you can see it
happen.

`AI?` does not edit anything, so its comment stays where you put it.

## What it is good for

The trigger lives where the work is. Writing a function and leaving
`# handle the empty case AI!` on the line above it carries context that a message in the
composer does not: koda reads the file, so it sees the signature, the imports and the
surrounding code without you describing any of it.

It pairs badly with `FULL-AUTO`. A trigger you left in a file yesterday and forgot about
firing against auto-approved writes is exactly the surprise you do not want; keep
approvals on while watch mode is.
