---
title: FAQ
description: Cost, privacy, hardware, model choice, and how koda compares to the alternatives.
---

## Is it free?

Yes. MIT licensed, no account, no subscription, no metering. If you run a local model, the
whole thing costs you electricity. If you point it at a hosted endpoint, you pay that
provider directly — koda is not in the path.

## Is my code private?

With a local endpoint, your code is read by a process you started on hardware you own.
Nothing is uploaded and there is no telemetry.

Three things can leave your machine, and all three are off by default or explicitly
chosen:

- the request to your model endpoint, wherever you pointed it;
- a `web_search` query, if you turned search on;
- a `web_fetch` or `browse` request, if you turned those on.

Everything else — sessions, memory, learned rules, the code graph, the trace — is a local
file you can read.

## Does it work offline?

Yes, with a local model. `web_search`, `web_fetch` and `browse` are the only network
features and all three are off by default.

The one thing that needs the network once is the browser engine, which the installer
fetches. `KODA_AGENT_BROWSER_URL` points that at a mirror for an air-gapped install.

## What hardware do I need?

For a useful coding agent, roughly:

| RAM | What runs |
| --- | --- |
| 16 GB | A 7B model. Expect it to lose the tool format sometimes. |
| 32 GB | Qwen2.5-Coder 14B. The point where this starts working well. |
| 64 GB+ | Qwen2.5-Coder 32B, Devstral Small. |

koda itself is about 6 MB and starts in 3 milliseconds; it is not what your machine will
be working on.

## Which model should I use?

Qwen2.5-Coder 14B or 32B, or Devstral Small. This matters more than any setting on this
site — see
[Model choice](/koda/providers/#model-choice-matters-more-than-anything-else).

## Can I use OpenAI, Claude or another hosted model?

Any endpoint that speaks the OpenAI chat-completions API, which is most of them —
OpenRouter, Groq, Together, Mistral, DeepSeek, your company's gateway. Point `-u` at it and
give it a key.

## Does it run on Linux and Windows?

Yes. Both installers are supported and the binary has no platform-specific runtime
requirement. Development and testing happen primarily on macOS with Apple Silicon, so that
is the best-exercised path.

## How is this different from Aider, Claude Code or Cursor?

koda is a terminal agent built for models running on your own machine. The differences that
follow from that: a single 6 MB binary with nothing to install alongside it, context
curation aimed at 8k–32k windows rather than 200k ones, a text tool-call protocol for
models with no tool training, and every capability — the debugger, the code graph, the
trace UI — compiled in rather than fetched.

If you are running a frontier model through a hosted API, tools built around that
assumption will use it better. koda's design choices only pay off when the model is small
and local.

## Will it change my files without asking?

Not at the default settings. Every write shows a unified diff and waits; every command
asks. `PLAN` mode removes the write tools entirely.

You can turn that off — `/auto` raises the tier, `--yolo` removes approvals altogether —
but it is a decision you make out loud, and `FULL-AUTO` is shown in red in the status bar
for as long as it is on.

## What does it store on my machine?

| Path | What |
| --- | --- |
| `~/.config/koda/` | Your config and personal skills. |
| `~/.local/state/koda/` | The event log, and debug captures if you enabled them. |
| `<project>/.koda/` | Sessions, memory, learned rules, project skills. |

All plain text. Delete any of it at any time.

## Can I read what it thinks it knows about my project?

Yes — that is the point of keeping it in markdown. `.koda/memory.md` holds the facts and
command outcomes. `.koda/learning/rules.md` holds promoted rules, and
`.koda/learning/journal.md` is a dated record of what was learned and when. Editing or
deleting any of it is authoritative.

## How do I stop it learning from me?

`learning = false`, or delete `.koda/learning/`. It is off by default.

## Something is wrong and I do not know what

Start at [Troubleshooting](/koda/troubleshooting/), then `/logs`. If a specific turn went
wrong, turn on the [web control center](/koda/webui/) and look at the trace.
