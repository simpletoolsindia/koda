# Agent memory: which Rust library, and what to build with it

Status: **landed** (`src/memstore.rs`). Built as recommended below, with two
changes found in live testing: the text index stems words (Porter), because
"print" has to find "printed"; and filler words are never search terms,
because stemmed, "one" is "on" and matched everything. Vectors are stored as
blobs and compared in Rust — a project's memories number in the hundreds, so
`sqlite-vec` was not needed. `/memory` browses and forgets; the store lives
beside the project's sessions and index, outside the project.

## What koda has today

| Layer | Where | What it does |
| --- | --- | --- |
| Project notes | `src/memory.rs`, `.koda/memory.md` | Facts the model chose to `remember`, command outcomes, hot files. One readable file; a `brief` of it goes into the prompt. |
| Learned rules | `src/learning.rs` | Project conventions mined from what the user accepts and rejects, promoted and retired over days. |
| Context curation | `src/context.rs` | Fits history to the window: supersede stale tool results, squeeze old ones, drop whole exchanges last — never the task. |
| Compaction | `agent::compact` | Summarises the conversation with the model when the window fills. |
| Code retrieval | `src/index.rs`, `src/repomap.rs` | BM25 + optional embeddings over the code; task-ranked code maps. |

What is missing is **recall**. Notes are appended and the newest are injected
wholesale; nothing chooses the memories relevant to *this* request, nothing
replaces a fact that has changed, and nothing records *why* a decision was
made. That is the gap every current survey of agent memory points at
([rate-distortion framing: "remember the decision, not the description"][rd];
[survey of persistent memory in agents][survey]; [typed memory against
provenance collapse][typed]).

## The Rust options, measured (crates.io, September 2026)

| Crate | Downloads | Last release | What it is | Verdict |
| --- | --- | --- | --- | --- |
| [`mem0-rust`][mem0rs] | 59 | 2025-12 | A port of mem0: LLM-extracted memories, vector store, scoped by user/agent/run | Too immature; and mem0's design runs an LLM call on every write |
| [`kremory`][kremory] | 287 | 2026-09 | Embeddable bi-temporal knowledge-graph memory in one libSQL file | The right *shape* (linked in, one file, bi-temporal), far too new to depend on |
| [`rig-core`][rig] | 2.9M | 2026-08 | A whole LLM application framework: providers, agents, tools, vector-store traits | Excellent, but adopting it for memory means replacing koda's own LLM and agent layers |
| [`swiftide`][swiftide] | 85k | 2025-11 | Streaming indexing/query pipelines and agents | Pipeline-oriented and quiet for ten months |
| [`rusqlite`][rusqlite] (bundled, FTS5) | 109M | 2026-08 | SQLite, compiled in | The storage layer everything above is built on |
| [`sqlite-vec`][sqlitevec] | 2.9M | 2026-05 | Vector search inside SQLite | The vector half, in the same file |
| [`tantivy`][tantivy] | 18.5M | 2026-09 | A full-text search engine | Heavier than FTS5 for a few thousand memories |
| [`fastembed`][fastembed] | 3.5M | 2026-09 | Local ONNX embeddings | Adds the ONNX runtime (tens of MB); koda already embeds through the configured server |

## Recommendation

**Build on `rusqlite` (bundled, with FTS5) + `sqlite-vec`; do not adopt a
memory framework.** The frameworks are either immature or would replace the
parts of koda that already work. What the research asks for is a small, typed
store with recall, and those two crates give it in one local file, with no
service to run:

```
memories(id, kind, text, why, source_turn, created, valid_from, superseded_by, uses, last_used)
memories_fts   — FTS5 over text + why            (keyword recall, no model needed)
memories_vec   — sqlite-vec over embeddings      (semantic recall, when an embedder is set)
```

1. **Typed entries** — `decision` (with its *why*), `fact`, `preference`,
   `procedure`, `outcome`. The type decides how it is recalled and how long it
   lives; a decision without its reason is the description the research says
   not to keep.
2. **Recall per request, not a dump** — the top few memories by fused
   FTS5 + vector score for the current request, inside a token budget, exactly
   as `repomap` does for code. Keyword recall works with no embedder at all.
3. **Supersede, don't append** — a new fact about the same subject marks the
   old one `superseded_by` (bi-temporal: what was true then stays queryable,
   what is true now is what is recalled).
4. **Provenance** — every memory records the turn it came from, so "why do you
   think that?" has an answer and a wrong memory can be traced and removed.
5. **Stays inspectable** — `.koda/memory.md` remains the human view (rendered
   from the store) and a `/memory` picker lists, edits and forgets entries.
   koda's rule that memory is never hidden state holds.
6. **Migration** — existing `memory.md` notes import as `fact` entries on
   first run.

Cost, measured: `rusqlite` with the bundled SQLite (3.53) took the release
binary from 16.2 MB to 18.0 MB (+1.8 MB — more than the 1–1.5 MB estimated
here before building). `sqlite-vec` turned out unnecessary (see the status
note at the top).

[rd]: https://arxiv.org/pdf/2605.10870
[survey]: https://arxiv.org/pdf/2606.30306
[typed]: https://arxiv.org/pdf/2605.25869
[mem0rs]: https://crates.io/crates/mem0-rust
[kremory]: https://github.com/kgentic/kremory
[rig]: https://rig.rs/
[swiftide]: https://github.com/bosun-ai/swiftide
[rusqlite]: https://crates.io/crates/rusqlite
[sqlitevec]: https://crates.io/crates/sqlite-vec
[tantivy]: https://crates.io/crates/tantivy
[fastembed]: https://crates.io/crates/fastembed
