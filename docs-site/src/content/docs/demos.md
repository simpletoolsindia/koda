---
title: See it work
description: Short recordings of koda doing real work, so you can judge it before installing it.
---

Reading about an agent tells you what it claims. These are recordings of koda
running — its own rendering, its own diff, its own status bar, its own tools
touching the disk.

## Fixing a bug, start to finish

![koda finding and fixing a bug: plan, read, edit, verify](/koda/demos/hero.gif)

One prompt, and koda works the loop: it writes a plan, reads the file, proposes
an edit as a diff you can see before it lands, marks its plan done, and reports
back with a table of what changed. The `+1/−1  31 tokens` under the diff is koda
telling you what the edit cost your context window.

Notice what it *doesn't* do: nothing happens off-screen. Every file it touched
is in the transcript.

## The interface

![koda's theme picker](/koda/demos/themes.gif)

Eleven palettes, switched live with `/theme`. The block fills, the context
gauge and the mode chip all re-tint together, because they are all drawn from
the same palette rather than hard-coded.

## How these were made

They are recorded from the real binary, driven through a pty, against
`tests/mock_server.py` — a scripted model that speaks the same
OpenAI-shaped SSE a real one does.

That matters for honesty, so here is the exact line: **everything you see is
koda.** Its markdown renderer, its diff, its approval path, its tool dispatch,
its status bar — all real, and the tools really wrote to a real directory. The
only thing scripted is what the *model* replies, because a real local model on
a laptop emits around 33 tokens a second, which makes a recording both very
long and different every time. Scripting the model is what lets these be short,
and lets CI regenerate them when the UI changes.

Reproduce any of them:

```bash
demos/scripts/record.sh hero showcase "the add function is subtracting - find it and fix it" 18
```
