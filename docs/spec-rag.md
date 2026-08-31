# koda RAG design — retrieval-augmented generation for a local-LLM coding agent

Status: design (P2 subsystem). No code in this document lands in the tree until
the phased plan below is implemented and gated by `cargo test` + e2e.

## Goal

Give koda semantic + structural retrieval so the model can pull the *right*
slices of a codebase (and its docs and past sessions) into context on demand,
instead of relying only on `grep`, the symbol `codegraph`, and whatever the user
`@`-mentions. It must:

- work **offline** (no embedding server required) via a keyword fallback;
- fit a **single Rust binary** — no external vector database, no daemon;
- **reuse** koda's existing `graph.rs` symbol graph, `memory.rs` signals, and
  `llm.rs` HTTP client rather than duplicating them;
- respect the existing `context_tokens` budget and history-trimming behaviour.

## What we learned from the reference projects

**context7** (upstash/context7) indexes up-to-date library/framework docs and
serves them to agents over MCP. Takeaways applicable here: (a) docs are chunked
by *heading/section*, not fixed windows; (b) each chunk keeps provenance
(library, version, source URL) so retrieval is explainable and versionable;
(c) retrieval is a *tool the agent calls with a query*, not an always-on stuffing
of context — the model decides when it needs docs. koda mirrors this with a
`retrieve` tool that the model calls like `web_search`/`codegraph`.

**Code-graph + RAG hybrids** (e.g. CodeGRAG, arXiv:2405.02355, and production
hybrid-search write-ups): structural graph context (what a symbol defines, who
calls it) meaningfully improves code retrieval over pure vector similarity.
Takeaway: fuse vector/keyword hits with graph neighbours and attach a small
structural header ("defines `X`; used by N files") to each retrieved chunk.

## Architecture overview

```
             ┌───────────────┐
 user turn ─▶│  agent.rs     │── model calls `retrieve(query)` ─┐
             └───────────────┘                                  │
                                                                ▼
                                                   ┌──────────────────────┐
                                                   │  rag::Index           │
                                                   │  ├─ Bm25 (always on)  │
                                                   │  ├─ Vectors (optional)│
                                                   │  └─ graph edge (join) │
                                                   └──────────┬───────────┘
                     Reciprocal Rank Fusion  ◀───────────────┘
                              │
                              ▼
                    ranked Chunks → budgeted → injected into prompt
```

## 1. What to retrieve, and chunking

Sources (each a `SourceKind`): `Code`, `Doc` (markdown/txt), `Session` (past
conversation exchanges), optionally `Memory`.

Chunking is **symbol-aware and reuses `graph::parse_file`** — no new parser:

- For code, use the definition spans the graph already extracts. A chunk spans
  from a definition's start line to the next definition's start, capped at
  ~120 lines; over-long definitions are split with a few lines of overlap.
- A leading "preamble" chunk captures imports/top-of-file before the first def.
- Markdown is chunked by heading; plain text by paragraph blocks.
- Sessions are chunked per user↔assistant exchange.

```rust
struct Chunk {
    id: u64,
    source: SourceKind,
    path: PathBuf,
    lang: Option<Lang>,     // reuse graph's Lang
    lines: (u32, u32),
    symbol: Option<String>, // the def this chunk centres on
    kind: Option<DefKind>,  // fn/struct/class/...
    text: String,
    tokens: usize,          // precomputed for budgeting
    hash: u64,              // content hash for staleness
}
```

At retrieval time each returned chunk is decorated with a **live graph header**
(CodeGRAG-style): `// defines `parse_file`; referenced in 6 files` — computed
from the current `Graph`, so it never goes stale in the stored index.

## 2. Embeddings (with an offline fallback)

Add one method to the existing client, reusing `req()` + retry + bearer auth:

```rust
impl Client {
    pub async fn embeddings(&self, model: &str, input: &[String])
        -> Result<Vec<Vec<f32>>>; // POST {base_url}/embeddings
}
```

An `Embedder` trait abstracts the source so the rest of the system is
provider-agnostic:

```rust
trait Embedder {
    fn dim(&self) -> usize;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
struct OpenAiEmbedder { client: Client, model: String, dim: usize }
struct NullEmbedder; // dim 0 — semantic disabled
```

Recommended local models (auto-detect dim from the first response): `nomic-embed-text`
(Ollama), `bge-small/base`, `all-MiniLM-L6-v2`. If no embed model is configured
or the endpoint has no `/embeddings`, koda runs **lexical-only** — no error.

**Fallback: Okapi BM25**, always built, over identifier-split tokens (reuse
`graph::identifiers` so `getUserName` → `get user name`). Fully offline; this is
the default when embeddings are unavailable.

## 3. Vector store that fits one binary

Rejected: sqlite-vec, qdrant, lancedb — all violate "single binary / no DB / no
daemon". Chosen:

- **In-memory flat store** with brute-force cosine similarity. At koda's current
  scale (`graph` caps at `MAX_FILES = 4000`, so low tens of thousands of chunks)
  a flat scan is sub-millisecond-to-few-ms and needs zero dependencies.
- **On-disk snapshot** under `.koda/rag/`:
  - `meta.json` — index version, embed `(model, dim)`, per-file content hashes;
  - `chunks.jsonl` — chunk metadata; `chunks.txt` — chunk bodies;
  - `vectors.bin` — f16-quantized vectors (half the RAM/disk, negligible recall
    loss for retrieval);
  - `lexical.bin` — BM25 postings/df stats.
- **Optional ANN later**: `hnsw_rs` behind a cargo feature, only switched on
  above ~50k chunks. Not needed for the initial target.

## 4. Hybrid retrieval and ranking

Three retrievers run per query: **vector** (if enabled), **BM25**, and **graph**
(exact/fuzzy symbol match → include the def chunk and its immediate ref/def
neighbours). Merge with **Reciprocal Rank Fusion** (rank-based, so no score
calibration across heterogeneous retrievers):

```
score(chunk) = Σ_retriever  weight_r / (K + rank_r(chunk))     // K = 60
```

with `weight_graph ≈ 1.2` (structure is a strong signal for code). Post-fusion
boosts: `log(1 + edits)` from `memory.rs` hot-files, exact-symbol match, and a
small source-kind weighting (prefer `Code` for code questions). A per-file
diversity cap prevents one file from flooding results. Optional LLM rerank is
**off by default** (a "fast" vs "quality" mode, like context7's modes).

## 5. Injection and token budget

Primary path is a **read-only `retrieve` tool** the model calls with its own
query — same shape as `web_search`/`codegraph`, so it's in `PLAN_TOOLS` and
`PARALLEL_SAFE`. Results return as a compact, cited block (path + lines +
graph header + body).

Optional **auto-context**: prepend top-k retrieved chunks for the turn, like
`memory.brief()` does today. Budget:

```
budget = min(rag_max_tokens (=2000), context_tokens * rag_budget_frac (=0.15))
```

Add chunks by fused rank until the budget is spent, using each chunk's
precomputed `tokens`. Respect koda's existing `bytes/4` token estimate and the
`trim()` reserve (`system.len()/4 + 1024`). Auto-context is injected **before**
history so `trim()` sheds it first under pressure — retrieval is a hint, never a
reason to drop the actual conversation.

## 6. Incremental updates (tied to the existing graph hooks)

The index is built on the same off-main-thread pass as `graph::scan`:

- lexical (BM25) is built **synchronously** — cheap, offline, immediately useful;
- embeddings fill in **in the background** and never block startup.

`Index::update_file` is paired with `Graph::update_file` (drop old chunks for the
path, re-chunk, append, queue for embedding), mirroring `Graph::remove_file`.
Staleness is content-hash based in `meta.json`. Changing the embed `(model, dim)`
invalidates only the **vector** half; BM25 and chunks survive.

## 7. New code, config, and tool

New module `src/rag.rs`:

```rust
pub enum SourceKind { Code, Doc, Session, Memory }
pub struct Chunk { /* as above */ }
pub struct Meta { version: u32, embed_model: Option<String>, dim: usize,
                  hashes: HashMap<PathBuf, u64> }
pub trait Embedder { /* ... */ }
pub struct Bm25 { /* postings, df, avgdl */ }
pub struct Index { chunks, vectors, bm25, meta, embedder }
impl Index {
    pub fn open(root, cfg) -> Result<Self>;
    pub async fn build(root, graph, embedder) -> Result<Self>;
    pub fn update_file(&mut self, path, graph);
    pub fn remove_file(&mut self, path);
    pub async fn embed_pending(&mut self);
    pub fn retrieve(&self, q: &Query, graph: &Graph) -> Vec<Retrieved>;
    pub fn save(&self, root) -> Result<()>;
}
pub struct Query { text: String, top_k: usize, kinds: Vec<SourceKind> }
pub struct Retrieved { chunk: Chunk, score: f32, graph_header: String }
```

- `llm.rs`: add `Client::embeddings`.
- `agent.rs`: hold `index: Arc<RwLock<Option<Index>>>` (mirrors the graph
  handle); add a `retrieve` dispatch arm; wire `index.update_file` next to the
  existing `graph.update_file` after successful writes/edits.
- `tools.rs`: add `"retrieve"` to `PLAN_TOOLS` and `PARALLEL_SAFE`.

New config keys (all `#[serde(default)]`, documented in
`DEFAULT_CONFIG_TEMPLATE`):

| key | default | meaning |
|---|---|---|
| `rag` | `false` | master switch |
| `rag_mode` | `"auto"` | `auto` \| `lexical` \| `semantic` |
| `embed_model` | `""` | model for `/embeddings`; empty ⇒ lexical only |
| `embed_batch` | `64` | embed request batch size |
| `rag_index_docs` | `true` | index markdown/txt |
| `rag_index_sessions` | `false` | index past sessions |
| `rag_auto_context` | `false` | prepend top-k automatically |
| `rag_rerank` | `false` | LLM rerank of fused results |
| `rag_max_tokens` | `2000` | hard cap on injected retrieval tokens |
| `rag_budget_frac` | `0.15` | fraction of `context_tokens` for RAG |
| `rag_top_k` | `8` | chunks returned by `retrieve` |

No new heavy dependencies: `reqwest`, `serde`, `ignore`, `regex`, `tokio` are
already present. Only an optional `hnsw_rs` behind a feature flag, later.

## 8. Phased implementation plan

- **P0** — symbol-aware chunking on top of `graph::parse_file`; `Chunk`/`Meta`
  types; snapshot format. (No retrieval yet; test chunk boundaries.)
- **P1** — BM25 index + `retrieve` tool, fully offline. Ship this first; it is
  useful with zero server setup.
- **P2** — `Client::embeddings` + `OpenAiEmbedder`; background embedding;
  vector store with f16 quantization.
- **P3** — hybrid RRF fusion + graph-edge retriever + boosts.
- **P4** — incremental `update_file`/`remove_file` wiring, auto-context,
  docs/sessions indexing.
- **P5** — optional ANN (`hnsw_rs`) and quantization tuning at scale.

## 9. Tradeoffs and risks

- **Flat index scaling** — fine to tens of thousands of chunks; ANN is the
  escape hatch, gated behind a feature so the default stays dependency-light.
- **Embed latency** — mitigated by background fill + batching; lexical covers
  the gap and the offline case.
- **Index/graph drift** — both updated from the same hooks; content hashes catch
  missed updates on next open.
- **Stale embeddings on model change** — `(model, dim)` in `meta.json`
  invalidates only the vector half.
- **Approximate chunk boundaries** — symbol spans are heuristic; overlap and the
  preamble chunk reduce boundary loss.
- **Prompt injection** — retrieved chunks are *data*, never executed or treated
  as instructions; the same untrusted-content rules as file reads apply.
- **Budget vs quality** — `rag_max_tokens`/`rag_budget_frac` bound the cost;
  auto-context is injected first so it is trimmed first.
- **Disk footprint** — f16 vectors + text under `.koda/rag/`; documented and
  removable (it is a cache, rebuildable from source).
- **Retrieval-tool confusion** — the model may over-call `retrieve`; the tool
  description scopes it to "find code/docs relevant to a question" and it is
  read-only and parallel-safe, so mis-calls are cheap.
