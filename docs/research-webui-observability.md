# Observability for koda's web UI — what Langfuse is, and what koda should take from it

Status: research + design. Nothing here has landed.

Every factual sentence is tagged. **[measured]** means it was read out of this
working tree (file and line given) or run on this machine. **[reported]** means
it comes from a cited external source — link in §11. **[estimated]** means it is
arithmetic over the other two, and the arithmetic is shown. Untagged sentences
are design judgement and should be argued with, not cited.

The question is "more features in the web UI, like Langfuse". The honest answer
has three parts: most of Langfuse is a hosted multi-tenant product koda must not
grow; a surprising amount of what Langfuse shows you, koda already records and
does not display; and there are three defects in the current UI that outrank
every new feature on the list.

---

## 1. What koda already records

`src/trace.rs` is the whole observability substrate. One `Turn` per top-level
user message, holding ordered `Step`s. **[measured]** `src/trace.rs:120-151`:

| Field | Where | Shown in the UI? |
| --- | --- | --- |
| `Turn { id, started, ended, mode, model, endpoint, input, status, reply, tokens }` | `trace.rs:138` | yes — `TraceWaterfall.jsx:76-90` |
| `Step { seq, kind, label, started, ms, running, note }` | `trace.rs:120` | yes — waterfall bars |
| `ModelCall.request` — the exact JSON body, pretty-printed | `trace.rs:91`, set at `agent.rs:1625` *before* the call goes out | yes — Inspector "Request" |
| `ModelCall.response` — raw SSE bytes, verbatim | `trace.rs:93`, appended by `llm.rs` via `trace::append_sse` | yes — Inspector "Response" |
| `ModelCall.reasoning` | `trace.rs:94` | yes |
| `ModelCall.finish_reason` | `trace.rs:96` | yes — waterfall subtitle |
| `ModelCall.retries` | `trace.rs:97`, set by `llm.rs:640` on each backoff | yes — `TraceWaterfall.jsx:136` |
| `ModelCall.prompt_tokens` / `completion_tokens` | `trace.rs:98-99` | yes — `TraceInspector.jsx:183`, labelled "(est)" |
| `ModelCall.tool_calls` — names asked for, in order | `trace.rs:101` | yes |
| `ModelCall.error` | `trace.rs:102` | yes |
| `ToolStep { name, args, ok, summary, detail, approval, diff }` | `trace.rs:107-116` | yes — Inspector tool tabs |
| Compaction `note` — `"18400 → 4200 tokens"` | `trace.rs:340` | yes — amber waterfall row |
| Prompt Δ between consecutive model calls | computed client-side, `TraceInspector.jsx:7-28` | yes |

**[measured]** every scalar field on `ModelCall` and `ToolStep` is referenced
somewhere in `web-ui/src/`. There is no dark data at the *step* level.

The gaps are one level up and one level down:

- **Up.** Nothing aggregates *across* turns. There is no per-session view, no
  "which tool fails most", no latency distribution, no comparison of two turns.
  Langfuse's entire value over a log file is aggregation, and koda has none.
- **Down.** Subagents are invisible. `Agent::child()` sets `trace_turn: None`
  **[measured]** `src/agent.rs:604`, and `begin_turn` only fires at `depth == 0`
  **[measured]** `src/agent.rs:965`. A `delegate` call is one opaque tool step
  no matter how many model calls happened inside it.
- **Sideways.** The ring is memory-only. `trace.rs` has no file I/O at all
  **[measured]**; `clear()` empties a `VecDeque` and that is the only lifecycle.
  Restart koda and every trace is gone, while `session.rs` keeps the messages
  forever. So the two halves of "what happened" have different lifetimes.

`src/session.rs` persists the other half: append-only JSONL under
`.koda/sessions/`, one `Header { id, started, model, endpoint, cwd }` line then
one `Message` per turn **[measured]** `src/session.rs:19-32`. It stores no
timings, no token counts, no tool outcomes beyond the message content, and no
link back to a trace id.

---

## 2. What Langfuse actually is

Langfuse's data model is four nouns **[reported]**:

- **Trace** — "a single request or operation, for example one chatbot
  interaction from the user's question to the final response"; the logical
  grouping of all observations sharing a `trace_id`.
- **Observation** — "the individual steps of your application: LLM calls, tool
  calls, retrieval steps, and so on", nestable. Specialised into *generations*,
  *spans* and *events*.
- **Session** — "used to group traces that are part of the same user
  interaction", with replay, public-link sharing, bookmarking and annotation.
- **Score** — "a numeric, categorical, or boolean quality signal attached to a
  trace, observation, or session", ingested by SDK, REST API, code evaluators,
  annotation queues, or LLM-as-a-judge. Data types are numeric, categorical,
  boolean and text (1–500 chars).

Enrichment attributes propagated to every observation in a trace: `user_id`,
`session_id`, `tags`, `metadata`, plus environments and releases/versions
**[reported]**.

Feature surface beyond the data model **[reported]**: token and cost tracking,
custom dashboards over cost/latency/volume/quality, threshold alerts,
LLM-as-a-judge evaluation, prompt management with versioning and one-click
deploy/rollback, a playground for side-by-side model comparison, and
datasets/experiments.

### The single-user local filter

Self-hosted Langfuse requires, at minimum **[reported]**: a `langfuse/langfuse`
web container, a `langfuse/worker` container, Postgres ("main database for
transactional workloads"), ClickHouse ("stores traces, observations, and
scores"), Redis/Valkey ("queue and cache operations"), and S3/blob storage
("persist all incoming events, multi-modal inputs, and large exports").

koda is one statically-linked binary. That list is the whole argument for what
must not be copied.

| Langfuse feature | Meaningful for koda? | Why |
| --- | --- | --- |
| Traces / observations | **Already have it** | `trace.rs` is a trace tree minus nesting |
| Sessions grouping traces | **Yes** | koda already has sessions on disk; nothing joins them to traces |
| Session replay | **Yes, cheaply** | the JSONL *is* the replay; no UI reads it |
| Latency + token dashboards | **Yes, if honest** | koda has `ms` on every step |
| Scores — human annotation | **Yes, narrow form** | one person marking a turn good/bad is useful; a queue is not |
| Scores — LLM-as-a-judge | **No** | needs a second model, ground truth koda does not have, and turns a debugging tool into an inference workload |
| Datasets / experiments | **No** | no ground truth, no CI, no team |
| Prompt management + versioning | **Partly, already exists** | `/api/settings` edits the system prompt; versioning it is a git problem koda should not re-solve |
| Playground | **Maybe, small** | "re-send this exact request with a tweak" is genuinely useful locally |
| Cost tracking in dollars | **No** | see §5 |
| Users, tags, environments, releases | **No** | one user, one machine, one checkout |
| Alerts | **No** | requires a daemon and a delivery channel; koda is a foreground process |
| Public link sharing | **No** | direct violation of "nothing leaves the machine" |

---

## 3. The standard to align to: OpenTelemetry GenAI semantic conventions

If koda's trace is ever to leave the process — exported to a file another tool
reads, or piped to a collector the user already runs — it should not invent a
schema. There is one.

The GenAI conventions have moved out of the main semconv repo to
`open-telemetry/semantic-conventions-genai` **[reported]**; the attributes are
still **Development** (not stable) **[reported]**, which is exactly the right
posture for koda: align field names, do not take a dependency.

Span name is `{gen_ai.operation.name} {gen_ai.request.model}` **[reported]**.
Required: `gen_ai.operation.name`, `gen_ai.provider.name`. Conditionally
required: `gen_ai.request.model`, `error.type` on failure, `gen_ai.conversation.id`
when readily available **[reported]**.

Mapping koda's existing fields **[measured]** against the registry
**[reported]**:

| koda field | OTel GenAI attribute | Notes |
| --- | --- | --- |
| `Step.kind = Model` | `gen_ai.operation.name = "chat"` | exact enum member |
| `Step.kind = Tool` | `gen_ai.operation.name = "execute_tool"` | exact enum member |
| `Turn.model` | `gen_ai.request.model` | direct |
| `Turn.endpoint` | `server.address` / `gen_ai.provider.name` | provider enum has no "local OpenAI-compatible"; `openai` is the wire format, not the vendor |
| `Turn.id` | `gen_ai.conversation.id` | or the session id — see F3 |
| `ModelCall.finish_reason` | `gen_ai.response.finish_reasons` (string[]) | koda stores one, spec wants an array |
| `ModelCall.prompt_tokens` | `gen_ai.usage.input_tokens` | **not honest today** — §5 |
| `ModelCall.completion_tokens` | `gen_ai.usage.output_tokens` | **not honest today** — §5 |
| `ToolStep.name` | `gen_ai.tool.name` | direct |
| `ToolStep.args` | `gen_ai.tool.call.arguments` | direct |
| `ToolStep.detail` | `gen_ai.tool.call.result` | direct |
| `ModelCall.request.messages` | `gen_ai.input.messages` | koda stores the whole body; the spec wants the messages |
| `ModelCall.text` | `gen_ai.output.messages` | direct |
| — | `gen_ai.response.time_to_first_chunk` | **koda does not record TTFT** — F5 |
| — | `gen_ai.usage.reasoning.output_tokens` | koda has `reasoning_len` at `agent.rs:1742` but folds it into completion |
| `ToolStep.approval` | none | koda-specific; there is no approval concept in the spec |
| `Step.kind = Compaction` | none | koda-specific |

Two of koda's most valuable fields — approval and compaction — have no home in
the spec. That is fine, and it is the reason the recommendation below is
"borrow the names, keep the shape", not "become an OTel exporter".

---

## 4. Comparable tools, and the minimum useful subset

- **Arize Phoenix** — tracing, evaluation, prompt engineering, datasets and
  experiments; built on OpenTelemetry and OpenInference, ingests OTLP; runs
  locally at `localhost:6006` and **supports SQLite with a persistent
  directory** rather than requiring a server database **[reported]**. This is
  the closest architectural cousin to what koda should do: a local page, a file
  on disk, an open wire format.
- **OpenLLMetry** (Traceloop) — extensions on top of OpenTelemetry, exports to
  25+ backends, "no dedicated backend is required" **[reported]**. Evidence
  that "emit OTel-shaped data, own no storage" is a viable product shape.
- **Helicone** — a proxy: point your SDK at it and requests are logged,
  rate-limited, cached and cost-analysed **[reported]**. Wrong shape for koda —
  koda *is* the client; inserting a proxy adds a hop and a process.
- **Braintrust** — eval-driven: datasets, scorers, CI-style gates before
  deployment **[reported]**. Requires ground truth and a CI. koda has neither.
- **LangSmith** — strongest when the stack is LangChain/LangGraph, surfacing
  chains and agents in views matching the code **[reported]**. The transferable
  idea is that the trace view should mirror *koda's* own vocabulary — turns,
  modes, autonomy tiers, approvals, compactions — not a generic span tree. The
  current waterfall already does this and should keep doing it.

**On what developers actually use.** The only primary evidence found is Chen et
al., *Design Principles and Guidelines for LLM Observability: Insights from
Developers*, CHI '25 Extended Abstracts — three focus groups of ten developers
each across proficiency levels, plus a designer rating survey **[reported]**. It
yields four principles: design for **awareness**, **monitoring**,
**intervention**, **operability**, and reports a tension that tools optimised
for fast intervention reduce developers' mental-model clarity, while
awareness-first designs deepen understanding but slow response **[reported]**.

Everything else found on "what developers use day to day" was vendor blog and
SEO comparison content with no method behind it, and is not cited here. Treat
the market-size and adoption numbers circulating in those posts as unverified.

The CHI framing is directly useful, because koda's UI already scores well on
*awareness* (the waterfall, the prompt Δ) and *intervention* (the control rail
writes to the live process). It scores badly on *monitoring* (no aggregation,
no history) and *operability* (nothing survives a restart). That is the same
conclusion §1 reached from the code.

**Minimum useful subset, from all five tools:** a trace tree, durations, the
exact request and response, a way to find yesterday's trace, and a way to
compare two runs. Everything past that is a team feature.

---

## 5. Three defects that outrank every new feature

### D1 — The page is not local. It fetches four scripts from two CDNs.

**[measured]** `web-ui/src/_head.html`:

```
11: <script src="https://cdn.tailwindcss.com"></script>
45: <script crossorigin src="https://unpkg.com/react@18/umd/react.production.min.js"></script>
46: <script crossorigin src="https://unpkg.com/react-dom@18/umd/react-dom.production.min.js"></script>
47: <script src="https://unpkg.com/@babel/standalone/babel.min.js"></script>
```

`docs-site/src/content/docs/webui.md` says the server "binds to localhost only —
nothing is exposed off your machine" **[measured]**. That is true of the server
and false of the page. Opening the UI makes four requests to third-party CDNs,
tells `unpkg.com` and `cdn.tailwindcss.com` your IP and timing, and **the UI
does not work offline at all** — which is the one environment koda is built for.
`@babel/standalone` is requested unversioned, so the page executes whatever
JavaScript unpkg serves that day, inside a page that can read your source tree
through `/api/codegraph`.

Fix: vendor the three libraries into `web-ui/dist/index.html` at build time (or
precompile the JSX and drop Babel entirely, and replace the Tailwind Play CDN
with a generated stylesheet). The current `dist/index.html` is 146,419 bytes
**[measured]**; React UMD + a static CSS file puts it around 300–400 KB
**[estimated]**, still one file, still embedded in the binary.

This is not an observability feature. It is the precondition for the product
claim the observability features are being built under.

### D2 — `Access-Control-Allow-Origin: *` on an unauthenticated localhost API.

**[measured]** `src/webui.rs:1504` — every response carries
`Access-Control-Allow-Origin: *`. Combined with no auth, any web page open in
the same browser can `fetch('http://127.0.0.1:7717/api/trace')` and read your
prompts, your source code via `/api/codegraph`, and your project memory — and
`POST /api/config` to repoint the running agent's endpoint at a server it
chooses.

Fix: drop the header (the UI is same-origin and does not need it), and reject
requests whose `Origin` is present and not the server's own. A one-line change
with a test.

### D3 — Token counts are estimates presented as counts.

**[measured]** `src/agent.rs:1630`: `prompt_tokens = self.history_tokens()`,
which sums `Message::approx_tokens()` — **[measured]** `src/llm.rs:189-196`,
`content.len() / 4`. **[measured]** `src/agent.rs:1742`:
`completion_tokens: (text.len() + reasoning_len) / 4`.

So every token number in the UI is bytes-over-four. The Inspector honestly
labels them "(est)" **[measured]** `TraceInspector.jsx:183`, which is good, but
the turn rail and `/api/status` do not, and `Turn.tokens` feeds the context
budget display.

The fix is available and cheap. **[measured]** `src/llm.rs:285-291` builds the
request body with `model`, `messages`, `stream`, `temperature`, `top_p` and
never sets `stream_options`. Ollama's OpenAI-compatible endpoint lists
`stream_options` with `include_usage` as supported **[reported]**; llama.cpp's
server returns a standard `usage` object plus a `timings` object with `prompt_n`,
`prompt_ms`, `prompt_per_second`, `predicted_n`, `predicted_ms`,
`predicted_per_second` and `cache_n` ("number of prompt tokens reused from
cache"), and a `timings_per_token` option that includes speed information in
each response **[reported]**.

Adding `stream_options: {include_usage: true}` and parsing the final chunk turns
the estimate into a measurement on both major local servers, and `cache_n` gives
koda something no hosted tool can show: how much of your prompt the server
actually reprocessed.

---

## 6. Cost accounting: what koda can say honestly

koda talks to local models. Dollars are the wrong unit and any dollar figure
would be fabricated. Do not add a price table.

What is real and locally meaningful, and what it needs:

| Honest metric | Source | Status |
| --- | --- | --- |
| Wall-clock per step and per turn | `Step.ms`, `Turn.started/ended` **[measured]** `trace.rs:124,140` | have it, not aggregated |
| Tokens in / out | `usage` from the server, once D3 lands | needs D3 |
| Generation speed (tok/s) | `usage.completion_tokens / Step.ms`, or llama.cpp `predicted_per_second` **[reported]** | needs D3 |
| Prefill vs decode split | llama.cpp `prompt_ms` / `predicted_ms` **[reported]** | needs D3, llama.cpp only |
| Prompt cache hit rate | llama.cpp `timings.cache_n / prompt_n` **[reported]** | needs D3, llama.cpp only |
| Context pressure | `publish_status(context_tokens)` **[measured]** `webui.rs:134-143` | have it, shown as a number, not a trend |
| Retries and their cost in seconds | `ModelCall.retries` **[measured]** `trace.rs:97` | have it, not aggregated |
| Compaction loss | `Step.note`, `"before → after tokens"` **[measured]** `trace.rs:340` | have it, unparsed |

The frame to use in the UI is **"this turn cost you 47 seconds, 12 400 prompt
tokens re-processed, and one compaction"** — time, tokens and context, never
currency.

---

## 7. Ranked features

Ranking is by (value to a single local user) ÷ (new machinery required), with
"koda already records this and does not show it" scoring highest by
construction. Effort is in the repo's own units: **S** ≈ an afternoon, **M** ≈
one to three days, **L** ≈ a week or more, with rough line counts.

### F1 — Turn analytics panel *(cheapest win; all data exists)* — **S**, ~40 lines Rust, ~180 JSX

**What.** One panel over the 50 turns already in the ring: tool call counts and
failure rate by tool name, model-call count and total model time vs tool time,
retry count, denied-approval count, compaction count and total tokens dropped,
turn duration distribution, and status breakdown.

**Data.** 100% present. `trace::summaries()` already returns `steps`,
`model_calls`, `tool_calls`, `ms`, `status`, `tokens` per turn **[measured]**
`trace.rs:161-175`; per-tool detail needs the full turns, which `trace::turn(id)`
serves.

**New route.** `GET /api/trace/stats` returning a computed rollup so the browser
does not fetch 50 full turns:
```json
{ "window": {"turns": 50, "span_s": 3120.4},
  "tools": [{"name":"read_file","calls":141,"failed":3,"denied":0,"ms_total":8210,"ms_p50":31,"ms_p90":210}],
  "models": {"calls":88,"ms_total":412300,"retries":4,"errors":1,"ms_p50":3900,"ms_p90":11200},
  "compactions": {"count":3,"tokens_dropped":41300},
  "turns": {"ok":41,"error":2,"cancelled":1,"ms_p50":18400,"ms_p90":92000} }
```

**UI.** A fourth right-hand tab beside Inspect. Bar list for tools, two
histograms. No charting library — the repo has no bundler (`web-ui/build.sh`
concatenates files **[measured]**), so these are `div`s with widths.

**Why first.** It is the *monitoring* axis the CHI principles name **[reported]**
and the one koda scores worst on, and it costs no new capture.

### F2 — Honest usage: `stream_options.include_usage` + `timings` — **S/M**, ~80 lines Rust

**What.** Ask the server for real token counts and speeds; show them; keep
"(est)" only when the server did not answer.

**Data.** New. `llm.rs:285` must add `stream_options: {"include_usage": true}`;
the SSE parser must handle a final chunk with an empty `choices` array carrying
`usage` **[reported]** (this is the documented shape and the source of a known
llama.cpp compatibility wrinkle **[reported]**, so parse defensively). Add
`usage_source: "server" | "estimated"` to `ModelCall` so the UI never lies. If
the server sends llama.cpp's `timings`, keep `prompt_per_second`,
`predicted_per_second` and `cache_n`.

**Risk.** Some servers ignore `stream_options`; some emit `usage` unrequested
**[reported]**. Both are tolerable — the field is additive and the fallback is
today's estimate.

**Why second.** It fixes D3, unlocks every number in F1 and F6, and is the only
item here that makes koda's data *correct* rather than merely more visible.

### F3 — Join traces to sessions, and persist a trace digest — **M**, ~200 lines Rust, ~120 JSX

**What.** Give the trace ring the session id, and write a compact per-turn
digest to `.koda/sessions/<id>.trace.jsonl` alongside the messages. Then the
sessions list can show, per saved session, how long it took, how many tool calls
failed, and how many compactions it survived — and clicking a past session opens
its turn rail instead of nothing.

**Data.** Session id exists **[measured]** `session.rs:117 Store::id()`; the
trace has no idea about it. `Turn` needs `session_id` and — importantly — a wall
clock. **[measured]** `trace.rs:39-53`: `now()` is seconds since process start,
so nothing in the trace can be dated. Add `unix_started: u64` on `Turn`.

**Digest, not payloads.** Persist only the summary plus per-step
`{seq,kind,label,ms,ok,approval,tokens}` — no request bodies, no SSE. **[estimated]**
~250 bytes/step × 40 steps × 30 turns ≈ 300 KB per session, versus tens of MB if
payloads were included. Payloads stay memory-only and die with the process,
which is the right privacy default.

**New routes.** `GET /api/sessions/<id>/trace` → the digest. `sessions_json`
**[measured]** `webui.rs:1042` gains `turns`, `tool_calls`, `failures`, `ms_total`.

**Why.** This is Langfuse's Sessions feature **[reported]** rebuilt at koda's
scale, and it is the difference between a live monitor and a tool you can ask
"what did I do on Tuesday".

### F4 — Trace search and filter — **S**, ~60 lines Rust, ~90 JSX

**What.** A filter box over the turn rail: free text against input/reply, plus
facets for status, mode, model, "has a failed tool", "has a denied approval",
"has a compaction", "has retries".

**Data.** All present in `TurnSummary` except the boolean facets, which are two
lines of counting in `summaries()`.

**New route.** None — extend `TurnSummary` with `failed_tools: usize`,
`denied: usize`, `compactions: usize`, `retries: usize` and filter client-side;
50 turns is nothing.

**Why.** Once F3 makes history real, the rail gets long. `session.rs:328`
already has a `search` for message text **[measured]** — the same idea, one
level up.

### F5 — Time-to-first-token, and the step timeline told honestly — **S/M**, ~50 lines Rust

**What.** Record when the first content byte arrived on each model call, and
draw the model bar in two shades: waiting, then streaming.

**Data.** New but nearly free — `llm.rs` already streams events through a
counting forwarder **[measured]** `src/llm.rs:653-671`; stamping an `Instant` on
the first forwarded event and calling a new `trace::set_ttft` is a handful of
lines. Field name should be `time_to_first_chunk` to match
`gen_ai.response.time_to_first_chunk` **[reported]**.

**Why.** On a local model, TTFT is prompt-processing time and it is the single
number that tells you whether a slow turn is your context or your GPU. Nothing
in koda currently separates the two.

### F6 — Turn diff / A-B compare — **M**, ~30 lines Rust, ~250 JSX

**What.** Pin two turns; show them side by side — steps aligned, durations
compared, and a diff of the two system prompts and first request bodies.

**Data.** Present. `GET /api/trace/<id>` already returns everything, and the LCS
diff in `TraceInspector.jsx:7-28` **[measured]** already does line diffing; this
is the same machinery pointed at two turns instead of two steps.

**Why.** This is the honest local substitute for Langfuse's experiments and
playground: you cannot run a controlled eval, but "I changed the prompt and
re-asked the same question — what moved?" is the actual local workflow, and koda
records both halves already.

### F7 — Subagent tracing (nested steps) — **M**, ~150 lines Rust, ~100 JSX

**What.** Make `delegate` expand into its child's model and tool calls instead
of being one opaque row.

**Data.** New plumbing. **[measured]** `agent.rs:604` `child()` sets
`trace_turn: None`; **[measured]** `agent.rs:965` `begin_turn` is gated on
`depth == 0`. Give `Step` a `parent: Option<usize>` and hand the child a
`StepRef` to nest under, or open a child `Turn` linked by `parent_turn`. The
first is truer to the data; the second is less invasive. Also enforce a nested
step budget — `MAX_STEPS` is per turn today and a delegating turn would blow
through it.

**Why not higher.** Real value, but it is the first item requiring surgery on
the agent rather than the UI, and subagents are read-only **[measured]**
(`child()` sets `allow: Some(tools::SUBAGENT_TOOLS)`), so they are the least
dangerous thing koda does.

### F8 — Human annotation: mark a turn, note why — **S**, ~120 lines Rust, ~80 JSX

**What.** Langfuse's Scores **[reported]**, reduced to what one person on one
machine will actually use: a 👍/👎/🚩 and a free-text note per turn, written to
`.koda/sessions/<id>.notes.jsonl`, shown in the rail, filterable via F4.

**Data.** New, tiny. Needs F3's persisted turn identity to be worth anything.

**Deliberately not.** No score schema, no numeric/categorical/boolean/text
taxonomy, no annotation queue, no judge. One person flagging "this turn went
wrong" and finding it again next week is the entire local use case.

**Bonus.** A flagged turn is exactly the right thing to attach to a bug report,
which is what F9 is for.

### F9 — OTel-shaped export — **S/M**, ~150 lines Rust

**What.** `GET /api/trace/<id>?format=otlp` (and a `/export` for the whole ring)
emitting the turn as an OTLP-JSON span tree with `gen_ai.*` attributes per §3.
File download only — koda never posts it anywhere.

**Data.** All present; this is a serializer. Field names taken from the registry
**[reported]**, with koda-specific extras (`koda.approval`, `koda.mode`,
`koda.compaction.tokens_before/after`) under their own prefix, which is what the
spec's own extension guidance implies.

**Why not higher.** It serves the user who already runs Phoenix or a collector —
real, but a minority — and the conventions are still Development-stage
**[reported]**, so the schema will move. Doing it *after* F2 also means the
exported token counts are real ones.

**Why at all.** It is how koda gets Langfuse-class dashboards without shipping
Postgres and ClickHouse **[reported]**: emit the standard shape, let the user's
existing tool render it. Phoenix ingests OTLP and runs locally on SQLite
**[reported]**; OpenLLMetry demonstrates the "no backend of our own" posture
**[reported]**.

### F10 — Request replay ("playground", local flavour) — **L**, ~250 lines Rust, ~200 JSX

**What.** Take the exact recorded request body, let the user edit
temperature/model/one message, re-send it out-of-band, and diff the two
responses. Never touches the live conversation.

**Data.** The request is captured verbatim **[measured]** `trace.rs:91`, so the
input side is done. The work is a second, isolated HTTP path, a streaming
response viewer, and the guardrails to stop this from mutating agent state.

**Why last.** It is the most Langfuse-like feature and the most machinery. It is
also where the `web_ui` server stops being a viewer and starts being a client,
which deserves its own design pass. Note also **[reported]** that Ollama does
not support `tool_choice`, which koda always sends **[measured]**
`src/llm.rs:304` — a replay UI would surface that kind of mismatch, which is an
argument for it, later.

---

## 8. What not to build

Each of these was considered and rejected for a stated reason, not overlooked.

- **A database.** SQLite would be the temptation. `session.rs` already argues
  the case against it **[measured]** `src/session.rs:6-9`: "JSONL rather than a
  database on purpose … no schema migration and no dependency, a truncated file
  from a crash still reads back to the last complete line". Trace digests are
  append-only and small (F3). Keep the format.
- **LLM-as-a-judge / automated evals.** Requires a second model, doubles local
  inference load, and scores koda's output against nothing. There is no ground
  truth in a coding session — the compiler and the test suite are the ground
  truth, and koda already runs those.
- **Datasets and experiments.** Same reason, plus they presuppose a CI and a
  team. Braintrust's model is CI-style eval gates before deployment
  **[reported]**; koda has no deployment.
- **Cost in dollars.** §6.
- **Users, tags, environments, releases, projects.** Langfuse propagates
  `user_id`, `session_id`, `tags`, `metadata` to every observation **[reported]**
  because it is multi-tenant. koda is one user, one machine, one checkout.
- **Alerts and thresholds.** Needs a resident daemon and a delivery channel.
  koda is a foreground process the user is already looking at.
- **Public share links.** Directly contradicts the product claim.
- **Prompt versioning as a feature.** `/api/settings` already edits the system
  prompt **[measured]** `webui.rs:1418` (`settings_json`). Versioning text files is git's job.
  At most: write the previous prompt to `.koda/` before overwriting, so a bad
  edit is recoverable. That is a safety fix, not a feature.
- **A charting library.** No bundler exists **[measured]** `web-ui/build.sh`.
  Every visual in F1 is achievable with CSS widths, and adding a CDN chart
  library would deepen D1.
- **A long-lived SSE stream.** Tempting given `/api/events` is a one-shot
  snapshot **[measured]** `webui.rs:447-455`, but see §9 — the fix is to shrink
  the payload, not to hold a task per tab.

---

## 9. One performance defect to fix while touching this

**[measured]** `webui.rs:620-632`: `trace_json()` returns *every* turn summary
plus `trace::live()` — the running turn **with all payloads**. `/api/events`
serves that same blob and closes; the browser's `EventSource` reconnects
automatically, so the snapshot is re-sent every few seconds for the whole
duration of a turn.

The per-step budget is `CAP_REQUEST` 128 KB plus three `CAP_FIELD` fields of
32 KB each **[measured]** `trace.rs:32-33`, and `MAX_STEPS` is 300
**[measured]** `trace.rs:29`. **[estimated]** a worst-case model step is 224 KB;
a 300-step turn is up to ~67 MB; the ring's stated bound is 50 turns, so the
documented "bounded on both axes" ceiling is **~3.4 GB of resident memory and a
~67 MB JSON body re-serialised every reconnect**. Real turns are far smaller,
but the ceiling is not a ceiling anyone intended.

Two changes, both small, both prerequisites for F1/F3 being pleasant:

1. `/api/trace` and `/api/events` return the live turn's **summary plus step
   headers only** (`seq,kind,label,started,ms,running,ok`), never payloads. The
   UI already fetches `/api/trace/<id>` for detail **[measured]** — it just also
   gets the payloads for free today and has no reason to.
2. Add a per-turn payload budget alongside the per-field one, dropping the
   oldest step payloads (keeping their headers) once a turn exceeds, say, 8 MB.
   The turn shape survives; the bytes do not.

---

## 10. Phase plan

**P1 — surface and correct what already exists.** No new capture except usage.
- D1 vendor the CDN scripts (unblocks the offline/privacy claim)
- D2 drop `Access-Control-Allow-Origin: *`, check `Origin`
- §9 trim the live-turn payload out of `/api/trace` and `/api/events`
- F1 turn analytics panel
- F4 trace search and facets
- F2 real usage via `stream_options.include_usage`, with `usage_source`

Result: the page works offline, cannot be read by other tabs, stops shipping
megabytes per poll, tells the truth about tokens, and answers "which tool keeps
failing" — with roughly 200 lines of Rust and 300 of JSX.

**P2 — small additions with new fields.**
- F5 time-to-first-chunk, two-tone model bars
- F3 wall-clock on turns, session id on turns, persisted per-session trace digest
- F6 turn A-B compare
- F8 flag-and-note annotation

Result: history survives restart, a slow turn is diagnosable as prompt vs decode,
and two runs of the same question can be compared.

**P3 — larger, each deserving its own design pass.**
- F7 subagent tracing (agent surgery, nested step budget)
- F9 OTel-shaped export (schema still Development-stage **[reported]**)
- F10 request replay (the web server becomes an LLM client)

**Explicitly out of scope, permanently:** databases, hosted anything, accounts,
alerts, dollar costs, LLM-judge evals, share links.

---

## 11. Citations

**Langfuse**
- Data model — traces, observations, sessions, trace-level attributes:
  https://langfuse.com/docs/observability/data-model
- Observability overview — tracing, token/cost tracking, scores, dashboards,
  alerts, LLM-as-a-judge, prompt management, experiments:
  https://langfuse.com/docs/observability/overview
- Sessions — grouping, replay, public link, bookmark, annotate:
  https://langfuse.com/docs/observability/features/sessions
- Scores — numeric/categorical/boolean/text, attachment points, ingestion paths:
  https://langfuse.com/docs/evaluation/evaluation-methods/custom-scores
- Self-hosting components — web, worker, Postgres, ClickHouse, Redis/Valkey,
  S3/blob: https://langfuse.com/self-hosting
- Source: https://github.com/langfuse/langfuse

**OpenTelemetry GenAI semantic conventions**
- Attribute registry (`gen_ai.*`), with the notice that the conventions have
  moved: https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/
- Current home of the conventions:
  https://github.com/open-telemetry/semantic-conventions-genai
- Span conventions — span naming `{gen_ai.operation.name} {gen_ai.request.model}`,
  requirement levels, Development stability:
  https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/gen-ai-spans.md
- (Superseded, retained for provenance):
  https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/

**Comparable tools**
- Arize Phoenix — features, OTel/OpenInference, OTLP ingest:
  https://arize.com/docs/phoenix
- Phoenix self-hosting — SQLite support, port 6006, `PHOENIX_WORKING_DIR`:
  https://arize.com/docs/phoenix/self-hosting/deployment-options/docker
- OpenLLMetry — OTel extensions, 25+ backends, no dedicated backend required:
  https://github.com/traceloop/openllmetry

**Local inference servers**
- Ollama OpenAI compatibility — `stream_options` / `include_usage` supported,
  `tool_choice` not: https://docs.ollama.com/api/openai-compatibility
- llama.cpp server — `timings` object (`prompt_n`, `prompt_ms`,
  `prompt_per_second`, `predicted_n`, `predicted_ms`, `predicted_per_second`,
  `cache_n`), `timings_per_token`:
  https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
- llama.cpp streaming usage behaviour without `stream_options`:
  https://github.com/ggml-org/llama.cpp/issues/12102

**Developer evidence**
- Chen et al., *Design Principles and Guidelines for LLM Observability: Insights
  from Developers*, CHI '25 Extended Abstracts. Three focus groups (n=10 each)
  plus a designer rating survey; principles of awareness, monitoring,
  intervention, operability: https://dl.acm.org/doi/10.1145/3706599.3719914

**Not cited, deliberately.** Vendor comparison blogs and "top N tools 2026"
listicles were read while surveying and are excluded: none state a method, and
the adoption and market-size figures they repeat could not be traced to a
primary source.

**koda source read for this document** (all paths relative to the repo root):
`src/trace.rs`, `src/webui.rs`, `src/agent.rs`, `src/llm.rs`, `src/session.rs`,
`src/debug.rs`, `src/log.rs`, `src/memory.rs`, `src/learning.rs`,
`web-ui/src/_head.html`, `web-ui/build.sh`, `web-ui/src/App.jsx`,
`web-ui/src/components/*.jsx`, `docs-site/src/content/docs/webui.md`,
`docs/plan-trace-ui.md`.
