---
name: small-model-tuning
role: localtune
when: Working on how koda behaves with small local models (3B-14B on Ollama, llama.cpp, LM Studio, vLLM) — tool-call failures, loops, empty replies, truncated context, prompt/tool budget, sampling settings, or per-model profiles. Also when someone reports "koda doesn't work with my local model"
---

# Small local models — diagnose before you tune

A 7B model that looks stupid is usually a harness problem, not a capability
problem. Work in this order. Do not skip to prompt engineering.

## Rule 1: measure the request before blaming the model

koda records every request it sends. Read it, don't guess:

```bash
# with web_ui = true in koda.toml, after one turn
curl -s localhost:7717/api/trace | python3 -c "import json,sys;print(json.load(sys.stdin)['turns'][0]['id'])"
curl -s localhost:7717/api/trace/<id> | python3 -c "
import json,sys
d=json.load(sys.stdin)
m=[s for s in d['steps'] if s['kind']=='model'][0]['model']
req=json.loads(m['request'])
sys_msg=next(x for x in req['messages'] if x['role']=='system')['content']
tj=json.dumps(req.get('tools') or [])
print('system', len(sys_msg), 'chars ~', len(sys_msg)//4, 'tok')
print('tools ', len(req.get('tools') or []), '->', len(tj), 'chars ~', len(tj)//4, 'tok')
print('body  ', len(json.dumps(req)), 'chars ~', len(json.dumps(req))//4, 'tok')
print('fields', sorted(req.keys()))
"
```

Known baseline (measured on this repo, 2026-09): system prompt ~682 tok, tool
schema for 15 tools ~2552 tok, whole first request ~3306 tok. If your numbers are
much larger, something added to the prompt — find it before tuning anything.

## Rule 2: check the server's real context window first

This is the most common cause of "small models can't use koda", and it is
invisible without checking.

**Ollama defaults to a 2048-token context** (verified on 0.33.2). koda's own
preamble is ~3300 tokens, so on a default install the instructions and tool
definitions are *truncated before the user's message is even considered*. The
model then answers from a fragment and looks incompetent.

Verify with a needle test — needle at the *top* of a long prompt:

```python
# ~12k tokens of filler with a marker at the very top, then ask for the marker.
# If the reply misses it and usage.prompt_tokens is ~2050, the window is 2048.
```

`num_ctx` **cannot** be set from an OpenAI-compatible client — verified: both
`options.num_ctx` and top-level `num_ctx` are ignored on
`/v1/chat/completions`; the same value on the native `/api/chat` works
(prompt_eval_count 2050 → 11939). So the fix is server-side:

```bash
OLLAMA_CONTEXT_LENGTH=16384 ollama serve     # or a Modelfile: PARAMETER num_ctx 16384
```

Never write code that sends `num_ctx` over `/v1` and assume it took effect.

## Rule 3: cut what you send before you tune how you sample

Order of savings, largest first:

1. **Tool schema** (~2552 tok for 15 tools) — the single biggest cost. One choke
   point: `tools::openai_schema_for`. Custom tools appended in
   `agent.rs::advertised_tools` need the same treatment.
2. **Tools advertised per turn** — `effective_allow` already subsets by mode;
   a plain question does not need `manage_agent`, `delegate`, `web_*`, `todo`.
3. **System prompt** (~682 tok) — the codegraph guidance, environment listing and
   text-protocol help are the trimmable parts. The text-protocol help is roughly
   twice the size of the native path.

Anything you cut must stay full-fat for cloud models, which do use the longer
descriptions. Gate it; never make it global.

## Rule 4: know which fields koda does and does not send

Sends today: `messages, model, stream, temperature, top_p, tool_choice, tools,
reasoning_effort`.

Absent, and useful for local models: `stop`, `max_tokens` (default 0 =
unbounded), `repeat_penalty`, `presence_penalty`, `top_k`, `min_p`, `seed`,
`response_format`/grammar.

When adding any of them: **omit the field unless it is set**, the way
`max_tokens` and `reasoning_effort` already behave, so a cloud request stays
byte-identical. Add to `llm.rs::ChatRequest` + `to_json`, default in
`config.rs`, and assert the field's presence/absence from the trace, not from
reading the code.

## Rule 5: do not hard-code temperature 0

Greedy decoding is reported to degrade some Qwen models into repetition. The
published recipes use temperature ≈0.2–0.7 with `top_p` 0.8 / `top_k` 20. If you
want determinism for tool calls, use constrained decoding, not near-zero
temperature. Mark this as unverified unless you have measured it here.

## Rule 6: keep the recovery scaffolding

koda already handles a bad turn: text-protocol fallback when a server rejects
native tools, `repair_json`, fenced-call recovery, the empty-reply nudge, the
identical-failure breaker after 3 repeats, `trim` + `auto_compact`,
codegraph-first tool ordering. These are downstream safety nets. New work is
upstream *prevention*. Never remove a net to make room for a preventer.

## Rule 7: no claim without a before/after

There is no baseline yet, so nothing can be called an improvement until you make
one. Fixed task set (single-file edit / search-then-edit / multi-file scaffold),
N runs per model through `tests/qa/live_qa.py`, metrics read from `/api/trace`:

- tool-call validity — `ToolStep.args` parses and matches the schema
- empty-reply rate — model steps with no text and no tool calls
- wrong-tool rate — calls outside the task's expected set
- steps to completion
- task success — trace status plus an external assertion

Ship gate: improves ≥1 metric on a small model **and** leaves task success
unchanged on a cloud reference model with the small-model settings off.

## Reporting style

Separate what you measured from what you read. Say "measured here" or "reported,
unverified" for every claim. A confident wrong number about a context window
costs more than an honest gap.

Background and the full prioritised plan: `docs/plan-small-models.md`.
