# Self-Improving koda: a local, no-remote-call learning architecture

**Author:** SimpleTools India · research report
**Status:** design proposal + Phase 1 implemented (see §6)
**Constraint:** everything runs on the user's machine. No remote calls for the
learning loop — ever. All learned state is a file the user can read, edit, or delete.

---

## 1. The goal, stated precisely

koda should get better at *your* work the more you use it — over days and months
it should absorb **your vibe** (naming, formatting, libraries, verbosity, tone)
and **your project's idioms** (custom syntax, internal DSLs, in-house frameworks,
the commands that actually work here). It must do this **without any remote
call**: no cloud training, no telemetry upload, no external memory service.

This is not "add more memory." It is a closed feedback loop: **observe usage →
distill a lesson → store it locally → feed it back into the next turn →
measure whether it helped → keep or decay it.**

---

## 2. What koda already has (the foundation)

| Primitive | File | What it does today | Role in self-improvement |
| --- | --- | --- | --- |
| **Memory** | `src/memory.rs` | Explicit `remember` facts, command ok/fail counts, hot-file edit counts, in `.koda/memory.md` | The **inspectable store**. Extend, don't replace. |
| **Learning** | `src/learning.rs` | **(Phase 1)** observation log + rule induction + `/learn` promotion, in `.koda/learning/` | The **rule library** built from usage. |
| **Skills** | `src/skills.rs` | Markdown instructions loaded on demand by `when:` match; role agents | The **skill/rule library** (Voyager-style). |
| **Sessions** | `src/session.rs` | Append-only conversation records; resume/search/fork | The **trajectory log** to mine. |
| **Code graph** | `src/graph.rs` | Symbol/reference/import scan across 25+ languages | The **project-idiom miner's** index. |
| **Prompt assembly** | `src/prompt.rs` | Layers base + mode + env + skills + memory + learned rules + AGENTS.md | The **feedback injection point**. |
| **Undo / diffs** | `src/agent.rs` | Per-turn snapshot; every write shows a unified diff | The **accept/reject/edit signal source**. |

The design principle in `memory.rs` is preserved throughout: *"a summary the user
cannot inspect is a liability, not a feature."*

---

## 3. What the research says (grounded synthesis)

Four research tracks (academic literature, competing harnesses, local ML, and
vibe/DSL capture) converged on a consistent picture.

### 3.1 Learning happens in context, not in weights
Every proven self-improving agent improves at **inference time** via prompts,
external memory, and reusable artifacts — never by touching weights:

- **Reflexion** (Shinn et al., 2023, arXiv:2303.11366) — write a natural-language
  self-critique on failure; prepend it on the retry.
- **Voyager** (Wang et al., 2023, arXiv:2305.16291) — a growing library of
  reusable *code* skills, retrieved by embedding similarity, compounds capability.
  *Explicitly bypasses fine-tuning.*
- **ExpeL** (Zhao et al., AAAI-24, arXiv:2308.10144) — **induce natural-language
  rules** by comparing successful vs failed trajectories. Rules **generalize
  better than raw replay**.
- **Generative Agents** (Park et al., 2023, arXiv:2304.03442) — retrieve by a
  weighted score of **recency + importance + relevance**.
- **MemGPT** (Packer et al., 2023, arXiv:2310.08560) — tiered memory: small
  in-context working set + large external store paged in on demand.
- **A-MEM** (2025, arXiv:2502.12110) — memories as a **linked, evolving** note
  network beat flat logs on long-horizon retrieval.

### 3.2 The market gap is exactly koda's constraint
Surveying Claude Code, Cursor, Aider, Copilot, OpenHands, Cline, Windsurf: the
state of practice is **hierarchical markdown memory files** concatenated into
context. But **only Claude Code auto-memory and Windsurf Cascade actually *learn*
from usage — and both require a frontier cloud model.** Nobody learns a project's
custom syntax/DSL; nobody does local accept/reject/edit reinforcement; flat files
bloat and go stale. A **fully local, no-remote-call self-learner that also learns
project idioms** is unoccupied territory. That is koda's wedge.

### 3.3 Local mechanisms, ranked by value-per-cost
- **In-context learning (curated files + retrieval)** delivers ~90% of the
  adaptation at ~1% of the cost, and is fully inspectable.
- **Deterministic rule induction** (mining diffs, command outcomes, lint fixes)
  needs no ML at all and is instantly auditable.
- **Local embeddings + vector search** for semantic recall: **fastembed-rs**
  (ONNX, all-MiniLM-L6, 384-dim, Rust, no network) + **sqlite-vec** (single
  inspectable `.db`). Reach for **usearch** only at large scale.
- **Local LoRA/QLoRA (MLX-LM) is DEFERRED.** Peer-reviewed evidence shows
  **catastrophic forgetting on small data** (arXiv:2405.09673; arXiv:2512.22337)
  and it needs thousands of clean pairs. Not diffable, not incremental.

### 3.4 The richest signal is the edit *after* acceptance
Research on 53.6K real developer edits (arXiv:2607.25130) found AI code is removed
entirely in **31%** of trajectories. **The diff between what koda proposed and
what survived is free, high-quality supervision.** Style should be learned as
*mores, not laws* (NATURALIZE, Allamanis et al., arXiv:1402.4182). `(proposed,
user-edited)` pairs are DPO-style data (arXiv:2305.18290). Capture must be
**silent** and surfaced **asynchronously** as a reviewable digest.

---

## 4. The architecture: five layers of local learning

```
   INJECT ◄── L0 Prompt assembly: base + mode + env + SKILLS + MEMORY + RULES + EXAMPLES
   ┌──────────────────────────────────────────────────────────────────┐
   │  L4  Preference re-ranking / (deferred) LoRA — opt-in, batch       │
   │  L3  Example library: (task→solution), similarity retrieval        │
   │      [fastembed-rs + sqlite-vec]                                   │
   │  L2  Project-idiom miner: custom syntax / DSL / internal APIs       │
   │  L1  Rule induction (deterministic): diffs, commands, lint  ← DONE  │
   │  L0  Observation: proposal-vs-surviving diff, approvals, commands  ← DONE │
   └──────────────────────────────────────────────────────────────────┘
        every artifact is a plain file under .koda/ — read/edit/delete
```

- **L0 Observation** — capture silently from each turn: proposal-vs-surviving
  diff, approvals (`y`/`a`/`n`), command outcomes, reverts. Append-only
  `.koda/learning/observations.jsonl`.
- **L1 Rule induction** — deterministic, no ML: naming casing, command
  substitutions, formatter/linter config inference. `.koda/learning/rules.md`.
- **L2 Project-idiom miner** — use the code graph to detect internal
  decorators/macros/helpers + the user's corrections → `project-idioms.md`.
- **L3 Example library** — successful episodes embedded locally, retrieved by
  recency+importance+relevance, injected as few-shot. `fastembed-rs + sqlite-vec`.
- **L4 Preference re-ranking / LoRA** — deferred, opt-in.

---

## 5. Keeping it honest: safety, inspectability, decay

1. **Every artifact is a file under `.koda/learning/`** — readable and editable.
2. **Human-in-the-loop promotion.** L1/L2 write *candidate* rules; `/learn` shows
   them; only accepted rules enter the prompt.
3. **Decay and consolidation.** Rules carry support/last-used; unused ones decay
   out of the injected set.
4. **Scoped injection.** Path-scoped rules load only when relevant.
5. **A kill switch.** `learning = false` disables the loop; deleting
   `.koda/learning/` resets it.

---

## 6. Phased implementation plan

### Phase 1 — Observation + deterministic rules  ✅ IMPLEMENTED
Shipped in `src/learning.rs` (+ wiring in `agent.rs`, `prompt.rs`, `config.rs`,
`tui.rs`):
- `.koda/learning/observations.jsonl` capture in `agent.rs::execute` — edits
  (before from the undo snapshot, after from disk), command outcomes, and denials,
  gated on `cfg.learning` at depth 0.
- A `Learning` store mines observations deterministically into candidate rules
  (command substitutions, function-naming convention, import preferences), with
  `MIN_SUPPORT = 2` so one-offs are ignored.
- `/learn` reviews candidates and accepts (`/learn accept <n>` / `/learn all`) or
  rejects (`/learn reject <n>`). Only accepted rules enter the prompt.
- `learning = false` config flag (default off); everything lives in
  `.koda/learning/`, plain-text and deleteable.
- 9 unit tests; verified end-to-end against the mock server (an edit was captured
  with correct before/after).
- *Still open:* formatter/linter config inference.

### Phase 2 — Project-idiom miner
Use `graph.rs` to surface high-frequency internal symbols/decorators; combine with
correction observations → `project-idioms.md`. The differentiated capability.

### Phase 3 — Semantic example library
`fastembed-rs` + `sqlite-vec` behind a Cargo feature; persist successful episodes;
retrieve top-k by recency+importance+relevance; inject as few-shot; add decay.

### Phase 4 — Preference re-ranking (opt-in)
Local contrastive re-ranker over `(proposed, user-edited)` pairs.

### Phase 5 — Local LoRA (deferred, research-gated)
MLX-LM LoRA, opt-in, only with a large curated corpus and forgetting mitigation.

---

## 7. Success metrics (measured locally)

- **Correction rate** — fraction of koda's proposed lines edited/reverted in a
  window; should trend down (baseline ~31% removal, arXiv:2607.25130).
- **Approval rate** — `y`/`a` vs `n`; should trend up.
- **Command-first-try success** — using learned "commands that work here."
- **Rule utilization** — how often injected rules are relevant (drives decay).

All computed from local logs; none leave the machine.

---

## 8. Bottom line

The winning path is **not** local fine-tuning. It is a **layered, in-context,
fully local learning loop**: observe the cheap high-signal data koda already sees,
distill it into **explicit, inspectable rules and project-idiom notes**, back it
with a **local semantic example library**, and feed it back through the prompt —
with **human-in-the-loop promotion and automatic decay** so it never drifts
silently. Weight-level learning (LoRA) stays deferred and opt-in. **Phase 1 is
implemented and tested.**

---

## Appendix — key references

- Reflexion — Shinn et al., 2023 — arXiv:2303.11366
- Generative Agents — Park et al., 2023 — arXiv:2304.03442
- Voyager — Wang et al., 2023 — arXiv:2305.16291
- MemGPT — Packer et al., 2023 — arXiv:2310.08560
- ExpeL — Zhao et al., AAAI-24 — arXiv:2308.10144
- A-MEM — 2025 — arXiv:2502.12110
- Self-Evolving Agents survey — 2025 — arXiv:2507.21046
- LoRA Learns Less and Forgets Less — 2024 — arXiv:2405.09673
- LoRA catastrophic degradation on small data — arXiv:2512.22337
- Learning from developer edits of AI code — arXiv:2607.25130
- Learning Natural Coding Conventions (NATURALIZE) — Allamanis et al. — arXiv:1402.4182
- Direct Preference Optimization (DPO) — Rafailov et al., 2023 — arXiv:2305.18290
- Tooling: fastembed-rs, sqlite-vec, usearch, MLX-LM LoRA
- Practice: AGENTS.md, Claude Code memory, Cursor rules
