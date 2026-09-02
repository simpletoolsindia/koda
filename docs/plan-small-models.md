# Small local models: what actually breaks, and what to do about it

Research target: koda driving 3B–14B models served by Ollama / llama.cpp /
LM Studio / vLLM over an OpenAI-compatible endpoint.

Everything marked **[measured]** was verified on this machine against this
codebase. Everything marked **[reported]** comes from external sources and has
*not* been reproduced here — treat it as a lead, not a fact.

---

## Executive summary

Small models look far dumber than they are, because of two things that have
nothing to do with the model:

1. **The server silently truncates the prompt.** Ollama 0.33.2 defaults to a
   **2048-token** context on `/v1/chat/completions`, and `num_ctx` is *ignored*
   on that endpoint. A 12 597-token prompt came back as
   `prompt_tokens=2050` with a garbage answer. **[measured]**
2. **koda's own preamble is bigger than that window.** The first request carries
   **~3 306 tokens** before the user's task: 682 for the system prompt and
   **2 552 for the tool schema alone** (15 tools). **[measured]**

Put together: on a default Ollama install, koda's instructions and tool
definitions *do not fit in the context window*. The model is answering from a
truncated fragment. That single interaction explains most "small models can't use
koda" reports, and no amount of prompt engineering fixes it.

So the work splits cleanly:

- **Make the window problem impossible to miss** (detect truncation, tell the
  user the exact fix). Cheap, and the highest-value change by a wide margin.
- **Shrink what we send** so a 4k–8k effective window has room for the task.
  The tool schema, not the system prompt, is the thing to cut.
- **Add the decoding controls we currently don't send at all** (`stop`,
  `repeat_penalty`, `top_k`, `min_p`, `seed`) so loops and runaway output can be
  bounded per model.
- **Stop pretending one config fits every model** — per-model profiles.

koda's *recovery* scaffolding is already good (text-protocol fallback, JSON
repair, fenced-call recovery, empty-reply nudge, identical-failure breaker,
trim/auto-compact, codegraph-first ordering, mode-based tool subsetting). None of
it should be removed. What is missing is *prevention* upstream of it.

---

## Measured baseline

First request of a fresh turn, default config, this repo as the workspace,
read from koda's own trace API (`/api/trace/<id>` → `ModelCall.request`):

| Component | chars | ≈tokens |
|---|---|---|
| system prompt | 2 731 | 682 |
| tool schema (15 tools) | 10 209 | **2 552** |
| whole request body | 13 227 | **3 306** |

Largest tool schemas: `edit_file` 1 187 chars, `manage_agent` 1 056,
`ask_user` 1 006, `codegraph` 986, `delegate` 956, `todo` 695. **[measured]**

Fields koda sends today: `messages, model, stream, temperature, top_p,
tool_choice, tools, reasoning_effort`. **[measured]**

Fields it never sends: `top_k, min_p, repeat_penalty, presence_penalty,
frequency_penalty, seed, stop, response_format`/grammar, `num_ctx`. **[measured]**

### The Ollama context trap, precisely

| Request | prompt_tokens seen by server | answer correct? |
|---|---|---|
| `/v1/chat/completions`, no `num_ctx` | 2 050 | no |
| `/v1/chat/completions`, `options.num_ctx=16384` | 2 050 | no |
| `/v1/chat/completions`, top-level `num_ctx=16384` | 2 050 | no |
| **native `/api/chat`, `options.num_ctx=16384`** | **11 939** | **yes** |

Needle-in-a-haystack, needle at the very top of a ~12.6k-token prompt,
`granite4.1:8b`, Ollama 0.33.2. **[measured]**

Conclusion: **`num_ctx` cannot be set from an OpenAI-compatible client.** The
window must be raised server-side (`OLLAMA_CONTEXT_LENGTH`, a Modelfile
`PARAMETER num_ctx`, or Ollama's own settings) — or the client must switch to the
native API. Any plan that says "send `num_ctx`" is wrong for koda's transport.

---

## Top 5, by impact ÷ cost

| # | Change | Where | Why it matters | Class |
|---|---|---|---|---|
| 1 | **Detect prompt truncation and say so** — compare `usage.prompt_tokens` against what we sent; if the server saw far less, print the exact fix (`OLLAMA_CONTEXT_LENGTH=16384 ollama serve`, or a Modelfile) | `llm.rs` (capture `usage` from the stream), `agent.rs::stream_step` | Turns the #1 silent failure into a one-line, actionable message. Nothing else helps while the prompt is being cut in half. | ship now |
| 2 | **Terse tool schema** for small models — one choke point already exists | `tools::openai_schema_for`, `specs()` descriptions | 2 552 → ~800 tokens frees ~40% of a 4k window. Biggest single reduction available. | ship now, flag-gated |
| 3 | **Send `stop` + bounded `max_tokens`** (`max_tokens` default is currently 0 = unbounded) | `llm.rs::ChatRequest::to_json`, `config.rs` | Bounds runaway generation and terminates text-protocol turns cleanly. | ship now |
| 4 | **Anti-repetition controls** (`repeat_penalty`, `presence_penalty`) | `llm.rs::ChatRequest`, `config.rs` | Directly targets the re-issue-the-same-failed-call loop koda currently only catches *after* three tries. | ship now, omitted unless set |
| 5 | **Per-model profiles** keyed on model id — sampling, terse tools, compact prompt, edit format, prompt prefix | new resolver in `config.rs`, applied before `agent.rs::stream_step` | The umbrella that makes everything above safe: small-model tuning that cannot touch cloud behaviour. | needs design |

---

## Detail

### A. Context window — do this first

**A1. Truncation detector. [ship now]**
Read `usage` from the final SSE frame (koda currently ignores it), compare
`prompt_tokens` with our own estimate of the request. If the server saw
substantially less than we sent, emit one clear notice naming the endpoint, the
window it actually used, and the fix. Also worth surfacing in `/api/trace` so the
web console shows it per turn.

Verification: the needle test above, run through koda instead of curl; assert the
notice appears with a 2048-window server and does not with a 16k one.

**A2. Do not try to send `num_ctx`. [measured — ruled out]**
It is ignored on `/v1`. Options, in order of honesty:
- document the server-side fix (env var / Modelfile) — cheapest, do this now;
- optionally detect Ollama (`/api/version` responds) and *offer* to use the
  native `/api/chat` for that provider. That is a second transport and a real
  design decision; do not do it casually.

**A3. Right-size `context_tokens` instead of assuming 16000.**
`trim`/`auto_compact` currently budget against a static config value. If the
server's real window is 2048, koda compacts far too late. Probe where possible
(llama.cpp exposes `/props`), else derive from the truncation detector.

### B. Shrink the request

**B1. Terse tool schema. [ship now, gated]**
`tools::openai_schema_for` is a single choke point, so this is a contained
change; custom tools appended in `agent.rs` need the same treatment. Target:
a one-line description per tool and no prose in parameter descriptions when the
terse profile is active. Keep today's rich descriptions for cloud models — they
demonstrably use them.

**B2. Fewer tools advertised per turn.**
15 tools is a lot for a 7B. `effective_allow` already subsets by mode
(PLAN_TOOLS / SUBAGENT_TOOLS); extend the idea so a plain question doesn't
advertise `manage_agent`, `delegate`, `web_*`, `todo`. **[reported]** that small
models degrade past roughly 5–10 tools; I have not measured the threshold here,
so treat the exact number as unverified and measure it (see below).

**B3. Compact system prompt under a small-model profile.**
682 tokens is not the main problem, but the codegraph guidance block, the
environment listing and the text-protocol help are the trimmable parts. The
text-protocol help in particular is ~2× the size of the native path.

### C. Decoding controls koda doesn't send

**C1. `stop`** — needed to terminate the `<tool_call>` text protocol cleanly.
**C2. `max_tokens`** — default 0 (unbounded) is wrong for a local model; a bound
turns a runaway into a recoverable turn.
**C3. `repeat_penalty` / `presence_penalty`** — the loop preventer.
**C4. `top_k` / `min_p`** — needed to follow published per-model sampling recipes.
**C5. `seed`** — not a correctness lever, but without it small-model debugging and
A/B measurement are guesswork.

All of these must be **omitted from the JSON unless set**, which is how koda
already treats `max_tokens` and `reasoning_effort` — so cloud endpoints see an
unchanged request.

**On temperature: do not hard-code 0.** **[reported]** that greedy decoding
degrades some Qwen models into repetition; the vendor sampling recipes use
temperature ≈0.2–0.7 with `top_p` 0.8 / `top_k` 20. I have not reproduced this.
The reliable determinism lever is constrained decoding, not temperature.

### D. Structural work (needs design)

**D1. Per-model profiles.** Match on model id → overrides for sampling, terse
tools, compact prompt, edit format, an optional system-prompt prefix (e.g.
`/no_think` for thinking models), and `use_system_prompt: false` for models with
no system role. Re-resolve on `/model`. Default profile must reproduce today's
request byte-for-byte for anything unmatched.

**D2. Constrained decoding** (`response_format` / JSON schema / GBNF grammar).
**[reported]** as the thing that takes tool-call validity to ~100% on small
models. Two cautions from the research, both unverified here: llama.cpp GBNF
compilation fails on PCRE `pattern` fields (`\d`, `\w`, `\s`, `\b`) — so tool
schemas would need sanitising first; and support differs per server, so this has
to be capability-detected rather than assumed.

**D3. Edit format per model.** **[reported]** that models not trained on diff
blocks do far better emitting whole files and letting the harness diff them. This
is plausible and cheap to test with koda's existing `write_file`/`edit_file`
split — worth measuring before building.

**D4. `tool_choice` beyond the hardcoded `"auto"`.** Forcing `"required"` on the
recovery turn after an empty reply is a targeted use of a field koda already
sends with a constant value.

---

## How to measure any of this

koda already has the instrument: `/api/trace` records, per turn, the exact
request body, the raw SSE, every tool call with its arguments and outcome, and
the turn's status. No new plumbing is needed.

Fixed task set, three tiers: (a) single-file edit, (b) search-then-edit across
2–3 files, (c) multi-file scaffold from scratch. Run each N times per model
through `tests/qa/live_qa.py`, then read the trace.

Metrics, all derivable from existing trace fields:

- **tool-call validity** — `ToolStep.args` parses and matches the schema
- **empty-reply rate** — model steps with no text and no tool calls
- **wrong-tool rate** — calls outside the task's expected tool set
- **turns/steps to completion**
- **task success** — trace status plus an external assertion (tests pass, the
  expected file changed)

Gate: an item ships if it improves ≥1 metric on a small model **and** leaves
task success unchanged on a cloud reference model with profiles off.

Baseline first. Right now there is no baseline, so nothing above can be claimed
as an improvement.

---

## What not to do

- **Don't send `num_ctx` and assume it worked.** Measured: ignored on `/v1`.
- **Don't hard-code temperature 0** for tool calling. **[reported]** harmful on
  Qwen; unverified here, but the upside is small either way.
- **Don't try to fix malformed tool JSON with prompt instructions alone.** At 3B
  this is unreliable **[reported]**; constrained decoding is the real fix, and
  koda's JSON repair already covers the rest.
- **Don't enable parallel tool calls** for small models — more concurrent JSON,
  more ways to be wrong. koda's serial execution is already right.
- **Don't treat speculative decoding as a reliability lever.** It is latency
  only.
- **Don't remove the existing recovery scaffolding.** It is what keeps a bad turn
  from becoming a bad session.
- **Don't ship one global "small model mode."** Model families differ enough
  (system-role support, thinking modes, sampling recipes) that a single switch
  will be wrong for someone. Profiles or nothing.
- **Don't invest in quantisation folklore** ("Q6 fixes JSON") without measuring.
  The one quantisation claim worth repeating to users is **[reported]**: heavy
  KV-cache quantisation degrades tool calling.

---

## Suggested default local model

**[reported]**, not benchmarked here: `qwen2.5-coder:14b` (or 7B) for the agent
loop — native tool template, works with both whole-file and diff edits, no
thinking-mode overhead. Avoid thinking models as the loop model on laptop
budgets: they spend the turn budget before calling a tool. Confirm any choice
against a tool-use benchmark rather than a chat leaderboard.

Locally available and already used by koda's QA gate: `granite4.1:8b` — which is
the model that produced the truncation measurements above.
