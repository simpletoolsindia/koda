---
title: What koda is
description: koda is a terminal coding agent that drives a model running on your own machine — reading and editing your code, running your tests, and showing you a diff before it changes anything.
sidebar:
  order: 1
---

koda is a coding assistant that lives in your terminal. You describe what you want in
plain language — *"the discount test fails, fix it"* — and it works out which files to
read, proposes the change, and, once you approve, makes it. It can run your tests, search
your project, drive a debugger, and explain what it did.

The difference from a cloud chatbot is where the model runs. koda talks to an
OpenAI-compatible chat endpoint, and the endpoint it expects by default is one on your own
machine: Ollama, LM Studio, llama.cpp, vLLM, MLX. Your code is read by a process you
started, on hardware you own. Cloud endpoints work identically — OpenAI, OpenRouter, Groq,
Together, Mistral, anything speaking the same API — but that is a decision you make, not
the default you inherit.

## What you get

**One binary.** About 6 MB, starting in roughly 3 milliseconds, with nothing to install
alongside it. No sidecar process per tool, no runtime, no package manager at run time. The
TUI, the agent loop, the tools, the symbol graph and the HTTP client are all compiled into
the same Rust executable.

**Approval before action.** Every file write shows you a unified diff before it is applied
and again afterwards. Every shell command asks. In `PLAN` mode the write and command tools
are not offered to the model at all, so nothing on disk can change while you are still
deciding what to do.

**Tools that do real work.** Reading and editing files, searching, running commands — but
also a symbol graph of your project, a real debug adapter for stepping through a running
program, subagents with their own context windows, durable memory, web search and fetch, a
headless browser for pages that need JavaScript, and PDF/Word/Excel reading.

**Deliberate handling of small context windows.** A local model with an 8k window spends
most of it on tool results it can no longer use. koda curates each request instead of
truncating the conversation — see [Prompt & context](/koda/prompt/).

## What it is not

koda does not have a hosted service, an account, or a subscription. There is nothing to
sign up for and no telemetry. It also does not bundle a model: you bring the endpoint, and
[which model you pick](/koda/providers/#model-choice-matters-more-than-anything-else)
matters more to the result than anything else on this site.

It is built and tested primarily on macOS with Apple Silicon. Linux and Windows are
supported by the installers and the code, and the binary has no platform-specific runtime
requirements.

## Two editions

koda ships on two branches. Both are maintained; neither is merged into the other.

| Edition | Branch | What is different |
| --- | --- | --- |
| Official | `master` | The standard build. `browse` drives [agent-browser](https://www.npmjs.com/package/agent-browser), which koda ships and installs itself. |
| Uncensored | `uncensored` | Everything in Official, plus a stealth browsing stack so the `browse` tool is not flagged by bot-detection. Drives your real Google Chrome, non-headless. |

To switch an existing checkout, `git checkout master` or `git checkout uncensored`, then
re-run `./install.sh`. The install one-liners honour `KODA_BRANCH` if you want a specific
branch.

:::caution
The uncensored edition's stealth stack exists so an agent can read pages that block
automation. Respect each site's terms of service and robots policy; how you use it is your
call.
:::

## Where to go next

If you want it running, start with the [Quickstart](/koda/quickstart/) — install, a model,
and a first fix, in about five minutes. If you want to understand the machine before you
run it, start with [Architecture](/koda/architecture/).
