# Hybrid retrieval for koda — BM25 + dense vectors over the symbol graph

Status: **landed**, with §11 recording what shipped, what it measured, and what
was tried and rejected. §§1–10 are the research and design as written before the
work, kept as they were — a design document edited after the fact to agree with
the outcome stops being evidence of anything.

Every number is tagged **[measured]** (run here, in this repo, commands in §9),
**[reported]** (from a cited paper — link in §10), or **[estimated]** (arithmetic
from the two, marked as such). Untagged sentences are argument, not evidence.

This supersedes the retrieval half of `docs/spec-rag.md`, which is otherwise
still the right shape; §8 lists exactly where the two disagree and why.

---

## 1. The problem, stated as a query

`codegraph` answers three questions well and one badly.

| Question | Today |
| --- | --- |
| Where is `parse_file` defined? | `codegraph symbol` — exact, one call |
| Who uses `Retry`? | `codegraph symbol` — exact, one call |
| What does `src/llm.rs` import? | `codegraph file` — exact, one call |
| **Where is retry handled?** | nothing. `search` is a regex; the graph keys on exact names |

The fourth is the shape a user actually types, and the shape a model reaches for
when it does not yet know the vocabulary of the repo. Today it degenerates into
three or four `search` calls with guessed patterns.

The interesting empirical fact is how close a plain BM25 index gets on exactly
this query. Indexing koda's own `src/*.rs` in 40-line windows, splitting
identifiers, dropping stopwords and applying a three-suffix stemmer, the top
eight hits for `where is retry handled` are **[measured]**:

```
15.30  llm.rs:321     ApiError / Retry::Transient
14.78  trace.rs:281   retry count recorded on the step
14.68  trace.rs:361   set_retries
14.66  llm.rs:561     stream_with_retry
14.17  llm.rs:641     retry loop body
13.49  llm.rs:601     stream_traced
12.72  llm.rs:361     ApiError::permanent / classify_transport
11.17  llm.rs:681     request construction in the retry path
```

Eight of eight are retry code. That result is the single most load-bearing
measurement in this document, and it changes the recommendation: the lexical
half is not a stopgap for the offline case, it is the product. The dense half
is an accuracy add-on for the queries the lexical half misses, and it must be
optional because it costs a network round-trip and a model the user may not
have.

Two more measured queries show where the ceiling is:

- `rate limiting throttle` → `llm.rs:321`, `llm.rs:361` at ranks 1–2 (correct),
  then `anim.rs:201`, `view.rs:121` — frame *rate*. Lexical ambiguity, exactly
  what an embedding fixes. **[measured]**
- `how is the token budget trimmed` → four of the top five are `tui.rs` chunks
  that display a token budget. `agent.rs::trim`, the actual answer, is not in
  the top 12. **[measured]** Two separate failures: no per-file diversity cap,
  and a term-frequency signal that a display widget wins on.

Those three results are the design brief.

---

## 2. What the literature actually says

### 2.1 BM25 and its parameters

The reference statement of the model is Robertson & Zaragoza's *The Probabilistic
Relevance Framework: BM25 and Beyond* (FnTIR 3(4), 2009) [R&Z]. The scoring
function this document uses is the Lucene form:

```
score(D, Q) = Σ_{t ∈ Q} idf(t) · ( f(t,D) · (k1 + 1) )
                        / ( f(t,D) + k1 · (1 − b + b · |D| / avgdl) )

idf(t) = ln( 1 + (N − df(t) + 0.5) / (df(t) + 0.5) )
```

`k1` controls term-frequency saturation, `b` the strength of length
normalisation. The conventional operating range is `1.2 ≤ k1 ≤ 2.0` and
`0.5 ≤ b ≤ 0.8`; Lucene ships `k1 = 1.2, b = 0.75` **[reported]**.

The variant question is settled and can be ignored. Kamphuis et al., *Which BM25
Do You Mean? A Large-Scale Reproducibility Study of Scoring Variants* (ECIR 2020)
compared eight published BM25 variants on three newswire collections and found
**no significant effectiveness differences between them**, Lucene's approximated
document-length encoding included **[reported]**. Practical consequence: pick the
Lucene form, do not spend a day choosing.

For an implementation reference — the eager-scoring layout, and the five variants
implemented compatibly — Lù, *BM25S* (arXiv:2407.03618, 2024) is the clearest
short write-up **[reported]**.

`R&Z` also defines **BM25F**, which scores a structured document by combining
per-field term frequencies *before* saturation rather than scoring fields
separately. That matters here: a code chunk has a name, a doc comment and a
body, and they are not equally diagnostic (§2.5).

### 2.2 Tokenisation is where code retrieval is won or lost

`getUserName` must match the query word `name`. Splitting identifiers into
constituent words is the standard first step of every IR-over-source-code
pipeline, and the splitting problem has its own literature: Enslen et al.,
*Mining Source Code to Automatically Split Identifiers for Software Analysis*
(MSR 2009) introduced Samurai for the hard case of same-case concatenations
(`nametable`); Dit et al., *Can Better Identifier Splitting Techniques Help
Feature Location?* (ICPC 2011, pp. 11–20) studied whether better splitting
improves downstream feature location **[reported]**. koda does not need Samurai
— camelCase, snake_case and SCREAMING_CASE cover Rust, Go, TS, Python and Java
almost entirely — but it does need to split.

Measured here, on koda's own source, the preprocessing is worth more than any
ranking refinement:

| Tokeniser | top-1 for `where is retry handled` |
| --- | --- |
| identifier-split only | `tui.rs:5561` — a test asserting on the word "handles" |
| + stoplist (48 terms) | `llm.rs:321` — correct |
| + stoplist + light stemmer | `llm.rs:321` — correct, and 8/8 of top 8 correct |

**[measured]** The stoplist is doing most of the work: `where`, `is`, `handled`
are the terms a code corpus is saturated with, and BM25's idf does not save you
when the corpus *is* prose-in-comments.

### 2.3 Dense retrieval for code

The lineage is well established and each step is a real paper:

- **CodeBERT** (Feng et al., 2020) — bimodal NL/PL pretraining; evaluated on code
  search and doc generation **[reported]**.
- **GraphCodeBERT** (Guo et al., ICLR 2021) — adds data flow; reports SOTA on
  four downstream tasks including code search, without numbers in the abstract
  **[reported]**.
- **UniXcoder** (Guo et al., ACL 2022) — unified cross-modal pretraining;
  introduces the zero-shot code-to-code search task **[reported]**.
- **CodeT5+** (Wang et al., 2023) — encoder-decoder family, claims SOTA on
  text-to-code retrieval **[reported]**.
- **CodeXEmbed / SFR-Embedding-Code** (Liu et al., 2024) — 400M–7B retrieval-
  specialised code embedders; the 7B model beats the previous best (Voyage-Code)
  **by over 20% on CoIR** **[reported]**.

The benchmarks that matter: **CodeSearchNet** (Husain et al., 2019; 6M functions,
six languages, 99 expert-annotated queries), **CoSQA** (Huang et al., ACL 2021;
20,604 human-labelled web-query/code pairs), and **CoIR** (Li et al., ACL 2025;
10 datasets, 4 task families, 14 languages).

CoIR's baseline table is the sharpest available evidence on lexical-vs-dense for
code, nDCG@10, average over the ten datasets **[reported]**:

| Model | CoIR avg nDCG@10 | Text-to-code (CosQA) |
| --- | --- | --- |
| BM25 | 29.79 | 13.96 |
| Contriever | 36.40 | 14.21 |
| UniXcoder | 37.33 | 25.14 |
| BGE-Base | 42.77 | 32.76 |
| OpenAI ada-002 | 45.59 | 28.88 |
| E5-Base | 50.90 | 32.59 |
| E5-Mistral | 55.18 | 31.27 |
| Voyage-Code-002 | 56.26 | 29.79 |

Read carefully, because it is easy to over-read. BM25 loses badly *on average*
and catastrophically on APPS (0.95) and CosQA (13.96) — problem-description
queries with near-zero lexical overlap with the code. It is competitive where
overlap exists: StackOverflow QA 56.80, CodeFeedback-ST 54.32. And CoIR's BM25
is a plain-text BM25; it does not split identifiers, and koda's queries are
written by a model that has already seen the repo's vocabulary in its context.
Take the direction — dense wins on intent-shaped, low-overlap queries — and
discount the magnitude.

The counterweight is BEIR (Thakur et al., 2021), which found **BM25 a robust
zero-shot baseline** that many dense retrievers fail to beat out of domain
**[reported]**, and BRIGHT (Su et al., 2024), where reasoning-intensive queries
sink everything — the then-top MTEB model scores **18.3 nDCG@10 on BRIGHT versus
59.0 on its usual benchmarks** — while **augmenting the query with explicit
LLM-written reasoning improves retrieval by up to 12.2 points** **[reported]**.
That last number is directly actionable for koda and costs nothing: the query is
written by an LLM that is already running, so the tool description should push it
to write a descriptive query rather than a keyword.

### 2.4 Hybrid, and how to fuse

**Reciprocal Rank Fusion** — Cormack, Clarke & Büttcher, SIGIR 2009:

```
RRFscore(d) = Σ_{r ∈ R}  1 / (k + rank_r(d))
```

The paper's own words on the constant: *"k = 60 was fixed during a pilot
investigation and not altered during subsequent validation"*, with the intuition
that highly-ranked documents matter more while *"the importance of lower-ranked
documents does not vanish"*, and that `k` *"mitigates the impact of high rankings
by outlier systems"* **[reported]**. Its pilot sweep shows the choice is not
delicate — MAP .2123 at k=10, .2145 at k=60, .2142 at k=100, versus .2072 at k=0
**[reported]**. On the four TREC collections RRF beat Condorcet and CombMNZ and
the best individual run on three of four; on TREC 9 the best individual system
(.3519) beat RRF (.2830) **[reported]**. That failure case is the honest caveat:
fusing a strong system with weak ones can hurt.

The strongest challenge to RRF is Bruch, Gai & Ingber, *An Analysis of Fusion
Functions for Hybrid Retrieval* (TOIS 42(1), 2023). They find RRF **sensitive to
its parameters**, find convex combination of normalised scores **agnostic to the
choice of normalisation**, find **CC outperforms RRF in-domain and out-of-domain**,
and find CC **sample-efficient — a small labelled set suffices to tune its single
α** **[reported]**.

Wang et al., *Balancing the Blend* (arXiv:2508.01405, 2025) adds the failure mode
that decides koda's design: a **"weakest link"** effect, where one weak retrieval
path substantially degrades fused accuracy, so paths should be quality-checked
before fusion **[reported]**.

Code-specific hybrid evidence is thin. The most direct data point available is
Pantha et al., *Scientific Code Search at Scale* (arXiv:2607.05443, 2026), whose
repository-search baselines report BM25 at .359 MRR@5, the best single embedder
at .515, Hybrid-RRF at .473 and cross-encoder Hybrid-Rerank at .522 **[reported,
single benchmark, not independently replicated]**. Two lessons: RRF landed
*between* BM25 and the better dense model rather than above both — the weakest-
link effect in the wild — and its snippet benchmark found docstring queries far
easier than identifier queries (.76 vs .18 MRR@10 **[reported]**), which is an
argument about *chunk contents*, not ranking (§2.5).

**Decision.** Use weighted RRF, `K = 60`, with the weights defaulting to 1 and
the vector path enabled only when it is actually configured. Not because RRF is
better than convex combination — Bruch et al. show it is not — but because CC's
advantage is conditional on tuning α on labelled data from the target domain,
and koda's target domain is *whatever repo the user opened*. There is no
labelled set, no place to put one, and no user who will produce one. RRF is the
method that needs nothing. Revisit if and when §7's evaluation harness exists and
shows a stable α across repos.

### 2.5 Chunking

Zhang et al., *cAST: Enhancing Code Retrieval-Augmented Generation with
Structural Chunking via Abstract Syntax Tree* (Findings of EMNLP 2025) is the
current reference. It recursively splits large AST nodes and merges siblings
under a size budget, and reports **+4.3 Recall@5 on RepoEval retrieval and
+2.67 Pass@1 on SWE-bench** over line-based chunking **[reported]**.

The gain is real but modest, and it is measured against *fixed line windows* —
the weakest baseline. koda already has function-level boundaries from
`graph::parse_file` without any parser, which captures most of what cAST is
buying. Combined with the docstring-vs-identifier result above, the actionable
conclusions are:

1. Chunk on definition boundaries (koda has them).
2. Start each chunk at the *preceding doc comment and attributes*, not at the
   `fn` line — the doc comment is the highest-value text in the chunk.
3. Do not add a parser to get 4.3 points of Recall@5 that the def spans mostly
   already give you.

### 2.6 Late interaction and reranking

ColBERT (Khattab & Zaharia, SIGIR 2020) stores a vector *per token* and does late
interaction, buying two orders of magnitude latency and four orders of magnitude
FLOPs over a BERT cross-encoder reranker **[reported]**. The comparison it wins
is against cross-encoders at web scale. koda's corpus is ~1.3k chunks; the flat
scan over single vectors is already 0.84 ms **[measured]**, so ColBERT's win
condition does not exist here, while its index cost — one vector per token
instead of per chunk — does. Skip it.

Cross-encoder reranking is the one technique that beat everything in the code
benchmark above (Hybrid-Rerank .522 vs Hybrid-RRF .473 **[reported]**), and it is
still the wrong call: it needs a second model, and koda's alternative is free —
the agent reads the top files with `read_file` and decides for itself, which is a
strictly better reranker than a 400M cross-encoder and is already implemented.

---

## 3. Design

### 3.1 Shape: a query mode on `codegraph`, not a new tool

`codegraph` gains `query = "search"` with a `text` parameter. Not a new
`retrieve` tool, which is what `spec-rag.md` proposed.

The reason is measured and in-tree: `docs/plan-small-models.md` records the tool
schema at **2,552 tokens for 15 tools** after terse-ification, against a 4k
default window on a stock Ollama install **[measured, that doc]**, and notes
models degrade past roughly 5–10 tools. A new tool costs a new description
competing for that budget and a new decision the model can get wrong. A new enum
value on a tool whose description already says "START HERE for code analysis"
costs one line, inherits `PLAN_TOOLS` and `PARALLEL_SAFE` membership, and puts
fuzzy search one keystroke from exact symbol lookup — so a model that guesses a
symbol name wrong falls back *within the same tool*.

```
codegraph(query="search", text="where is retry handled", k=8)
```

Result shape mirrors the existing modes: a plain-text block, one entry per hit,
`path:start-end`, the graph header the existing `symbol` mode already knows how
to write (`defines X; used in N files`), then the chunk body. The model then
`read_file`s what it wants. That is the reranker.

### 3.2 What gets indexed

Reuse `graph::parse_file`'s `defs: Vec<(name, kind, line)>` — no new parser, per
the module's own stated policy.

- One chunk per definition, spanning from its start line to the next
  definition's start line.
- **Extend the start backwards** over contiguous `///`, `//!`, `#[...]`, `"""`,
  `/** */` and `@decorator` lines. The doc comment is the part a natural-language
  query matches (§2.5).
- A **preamble chunk** for everything before the first definition (imports,
  module docs) — that is where `graph::import_of` already looks.
- Split definitions longer than **120 lines** into windows with **8 lines of
  overlap**; merge adjacent definitions shorter than **8 lines** (getters, small
  `impl` stubs) up to the same 120-line cap. Both numbers are untuned defaults;
  §7 is how they stop being guesses.
- Same corpus limits as the graph: `MAX_FILES = 4000`, `MAX_FILE_BYTES = 1 MiB`,
  `is_vendor_dir`, and the `ignore` walker's gitignore handling. No second
  traversal policy to keep in sync.
- P1 indexes code only. Not docs, not sessions, not memory.

### 3.3 Tokenisation

One function, `index::terms(lang, text) -> Vec<String>`, built from parts that
already exist:

1. `graph::strip_literals_and_comments` gives the code-only view — **but for the
   index, keep comments**; invert the call so comment text is tokenised as prose
   and string literals are dropped. Comments are the highest-signal text for
   intent queries.
2. Scan identifiers as `graph::identifiers` does, then split each on
   `_`/`-`/case boundaries, emitting **both the whole token and its parts**
   (`stream_with_retry` → `stream_with_retry`, `stream`, `with`, `retry`).
   Keeping the whole token preserves exact-symbol precision.
3. Drop language keywords (`graph::keywords`) and English stopwords. **Reuse
   `context::STOPWORDS`** — `src/context.rs` already carries a 60-word list for
   exactly this purpose (focus-term extraction). Extend it with the code-ambient
   verbs the measurement above implicated: `handle/handled/handles`,
   `use/used/uses`, `get/set`, `new`, `return`.
4. Light stemmer: strip `ings|ing|ies|es|ed|s` from tokens longer than 4 chars
   with a 3-char stem floor. Not Porter. No new dependency.
5. Lowercase everything; drop 1-character tokens.

Query text goes through the same function with `lang = None`.

### 3.4 BM25

Single-field Lucene BM25 as in §2.1, `k1 = 1.2`, `b = 0.75`, not user-configurable.

Layout, all in one `Vec`-backed struct:

```rust
struct Lexical {
    vocab:  HashMap<String, u32>,   // term -> term id
    post:   Vec<Vec<(u32, u16)>>,   // term id -> [(chunk id, tf)]
    len:    Vec<u32>,               // chunk id -> token count
    avgdl:  f32,
    n:      u32,
}
```

Query: allocate an `n`-length f32 accumulator, walk the postings of each query
term, take top-k with a bounded heap. Measured at **4.2 µs per query** over
koda's 1,054 chunks **[measured]** — free.

BM25F (§2.1) with fields `name` / `doc` / `body` is the first refinement to try
in P4, gated on §7 showing it helps. Not in P1: it triples the index structure to
chase an unmeasured gain.

### 3.5 Embeddings

`llm.rs` gains one method, built on the existing `req()` so it inherits the
bearer auth, the TLS policy and the endpoint switching:

```rust
pub async fn embeddings(&self, model: &str, input: &[String])
    -> Result<Vec<Vec<f32>>>;   // POST {endpoint}/embeddings
```

This is available on the endpoints koda targets. Verified: Ollama's
OpenAI-compatibility documentation lists `/v1/embeddings` **[measured — fetched
`docs.ollama.com/api/openai-compatibility`]**; llama.cpp's server README
documents `POST /v1/embeddings` as its OpenAI-compatible embeddings API and
explicitly directs OAI clients there rather than to `/embedding` **[measured —
fetched `tools/server/README.md`]**; LM Studio documents the same path
**[reported]**.

Rules:

- `embed_model = ""` (the default) ⇒ the vector path does not exist. Not an
  error, not a warning on every start. Lexical-only is the default product.
- Dimension is read from the first response, not configured.
- `(model, dim)` is recorded in `meta.json`; changing either invalidates the
  vectors and *only* the vectors.
- Vectors are L2-normalised at write time, so cosine is a dot product.
- Stored as f16 (`u16` bit-cast, no crate — `f32::to_bits` and a 5-line
  round-trip), converted to f32 on load.
- Embedding happens on a background task after the lexical index is live. A
  query that arrives before it finishes runs lexical-only and says so in one
  line. The agent must never block on an embedding server.
- **Weakest-link guard** (§2.4): if the embedding endpoint errors, times out, or
  returns a dimension that disagrees with `meta.json`, the vector path is
  disabled for the session rather than fused in degraded.

Recommended models for a local endpoint, all small enough to sit beside a chat
model: `nomic-embed-text` (768 d), `bge-small-en-v1.5` (384 d),
`all-MiniLM-L6-v2` (384 d). If the user has the VRAM, an SFR-Embedding-Code
checkpoint is the code-specialised option (§2.3) — but koda should not
recommend a 7B embedder to someone already running a 7B chat model.

### 3.6 Vector store

Flat `Vec<u16>` of `n × dim`, brute-force scan. Measured on this machine, scalar
Rust, no SIMD **[measured]**:

| chunks × dim | scan | f16 resident |
| --- | --- | --- |
| 1,300 × 768 | 0.84 ms | 1.9 MB |
| 5,000 × 768 | 2.99 ms | 7.3 MB |
| 20,000 × 768 | 12.1 ms | 29 MB |
| 1,300 × 384 | 0.31 ms | 1.0 MB |

`MAX_FILES = 4000` bounds the corpus well inside the first two rows. There is no
ANN case to argue about (§6).

### 3.7 Fusion

Retrieve top **50** from each available path, then:

```
score(c) =  w_lex   / (60 + rank_lex(c))
         +  w_vec   / (60 + rank_vec(c))        // only if embeddings are live
         +  w_graph / (60 + rank_graph(c))

w_lex = 1.0,  w_vec = 1.0,  w_graph = 1.2,  K = 60
```

`rank_r(c)` is 1-based within path `r`'s top 50; a chunk absent from a path
contributes nothing from it. `K = 60` is Cormack et al.'s constant, kept because
their own sweep shows MAP moves by 0.2% between k=10 and k=100 **[reported]** —
there is no tuning to be had without labels. `w_graph = 1.2` is inherited from
`spec-rag.md` and is **a guess**; it is the first thing §7 should settle.

The graph path: exact and substring symbol matches against `graph.defs` for each
query term, contributing the defining chunk and the chunks of its direct
referrers, ranked by `refs` count.

Two post-fusion rules, both motivated by the §1 measurements rather than by a
paper:

- **Per-file cap of 3.** Four of the top five results for the token-budget query
  came from one file **[measured]**. Beyond three, later chunks from the same
  file drop to a spill list appended after the diverse head.
- **Test demotion.** A chunk inside a `#[cfg(test)]` block, a `tests/` path, or a
  `test_*`/`*_test.*` file gets `rank += 10` unless the query itself contains a
  test-ish term. The measured top-1 failure before stopwording was a test
  asserting on the English word "handles" **[measured]**.

Not included: the `memory.rs` hot-file boost from `spec-rag.md`. It is plausible
and untested, and a recency prior that quietly reorders results is exactly the
kind of thing that is impossible to debug from the outside. Add it after §7 can
show it helps.

### 3.8 Storage and incremental update

Under `<project>/.koda/index/`, alongside the existing `.koda/` residents
(`memory.md`, `sessions/`, `learning/`, `skills/`):

```
meta.json      version, embed (model, dim), tokeniser version, params,
               and per-file (mtime, len, content hash) — the same Stamp
               shape graph.rs already uses for refresh
chunks.bin     per chunk: file id, start line, end line, symbol, kind,
               token count, content hash.  NO BODIES.
lexical.bin    term dictionary (sorted, length-prefixed) + postings
vectors.f16    n × dim, row-major, L2-normalised
```

**Chunk bodies are not stored.** The working tree is right there; a hit re-reads
its line span at query time and verifies the chunk hash. That saves ~1.5 MB per
koda-sized repo, and — more usefully — guarantees a result shows the file as it
is *now*, not as it was at index time. On a hash mismatch the file is re-chunked
on the spot, which is the same work `Graph::update_file` already does.

Incremental update rides the hooks that exist:

- `Graph::update_file` / `Graph::remove_file` are already called after every
  successful `write_file` / `edit_file` (`agent.rs`, the `outcome.ok` branch).
  `Index::update_file` / `remove_file` go next to them.
- `Graph::refresh`'s `(mtime, len)` sweep already identifies outside edits every
  `codegraph_refresh_ms`. The index re-chunks the same file set — no second
  walker, no watcher.
- Re-embedding is queued, batched at 64, and runs in the background. A stale
  vector for one edited chunk is a rank error, not a correctness error.
- `meta.json` version bumps invalidate everything; a changed `(model, dim)`
  invalidates only `vectors.f16`; a changed tokeniser version invalidates only
  `lexical.bin`.
- The whole directory is a cache. Deleting it must be safe and must cost only a
  rebuild. Document that in `USER_GUIDE.md` next to the graph.

### 3.9 Config

Three keys, not eleven.

| key | default | meaning |
| --- | --- | --- |
| `codegraph_search` | `true` | the lexical index and the `search` query mode |
| `embed_model` | `""` | model for `/embeddings`; empty ⇒ lexical only |
| `embed_batch` | `64` | embedding request batch size |

`k1`, `b`, `K`, the fusion weights, the chunk caps and the top-50 depth are
constants in the module. They are not user problems, and every one of them
exposed is a support question about a number nobody can measure the effect of.

---

## 4. Scale, for a 50k-line repository

Anchored on koda itself: 31 files, **41,458 lines, 1,575,687 bytes**, which
40-line windowing turns into **1,054 chunks, 8,135 distinct terms, 103,436
postings** **[measured]**. Symbol-aware chunking on `graph::parse_file`'s
definitions produces a similar count — koda's source has ~1,685 definition-shaped
lines **[measured]**, and the merge-small/split-large rules pull that back toward
the same order. A 50k-line repo is ~1.2× koda.

| | 50k-line repo | basis |
| --- | --- | --- |
| chunks | ~1,300 | **[estimated]** from 1,054 @ 41.5k lines |
| distinct terms | ~10,000 | **[estimated]** |
| postings | ~125,000 | **[estimated]** |
| `lexical.bin` | ~0.8 MB | **[estimated]** 6 B/posting measured at 606 KB for 103k |
| `chunks.bin` | ~0.1 MB | **[estimated]** ~80 B/chunk, no bodies |
| `vectors.f16` @ 768 d | ~2.0 MB | **[measured]** row above |
| `vectors.f16` @ 384 d | ~1.0 MB | **[measured]** |
| **`.koda/index/` total** | **~3 MB** (0.9 MB lexical-only) | |
| resident memory | ~3–4 MB | **[estimated]**, vocab strings included |
| lexical build | ~60 ms single-threaded | **[measured]** 48 ms for koda; `parse_in_parallel` already exists if it ever matters |
| embedding build | 5 s – 60 s | **[estimated]** 21 batches of 64; entirely the server's throughput, from ~30 chunks/s CPU to ~300+/s GPU. Background, never blocking. |
| BM25 query | ~5 µs | **[measured]** 4.2 µs at 1,054 chunks |
| vector scan | ~0.9 ms | **[measured]** |
| query embedding round-trip | 10–80 ms | **[estimated]**, local server, dominates everything else |
| **end-to-end query, lexical-only** | **< 2 ms** | |
| **end-to-end query, hybrid** | **~20–100 ms** | dominated by the one embedding call |

The build cost that matters is not the index — it is the file reads, and
`graph::scan` is already doing those. The marginal cost of adding retrieval to
the existing scan is the ~60 ms tokenisation pass.

---

## 5. Phases

Each phase ships something usable and is gated by §7.

- **P1 — lexical.** Chunking on `graph::parse_file` spans, the tokeniser, BM25,
  `codegraph query="search"`, `.koda/index/` persistence, incremental update on
  the existing hooks. Offline, no config, no server. This is the phase that
  delivers the §1 result.
- **P2 — fusion scaffolding.** The graph path and RRF, fusing lexical + graph
  with `w_vec` absent. Proves the fusion code without an embedding server.
- **P3 — vectors.** `Client::embeddings`, background fill, f16 store, flat scan,
  the weakest-link guard, three-way RRF.
- **P4 — refinements, each gated.** BM25F fields, the fusion weights, the hot-file
  prior, chunk-size constants. Anything §7 cannot show a gain for does not land.

## 6. What not to build, and why

- **An ANN index (hnsw_rs or anything else).** 20,000 chunks × 768 dims scans in
  12.1 ms **[measured]**, and `MAX_FILES = 4000` keeps a real repo at a tenth of
  that. `spec-rag.md` held this behind a cargo feature "for later"; the
  measurement says delete the note.
- **A vector database or any daemon** (sqlite-vec, qdrant, lancedb). Already
  rejected in `spec-rag.md` for the right reason: single binary. Nothing here
  changes it.
- **Local embedding inference** — a tokenizer crate, ONNX, candle, a bundled
  model. koda's release binary is **8.9 MB** **[measured]**; the smallest
  credible local embedder stack is tens of megabytes of code plus hundreds of
  megabytes of weights. The endpoint the user already configured is the right
  place for this.
- **ColBERT / late interaction.** §2.6. Its win condition is cross-encoder
  latency at scale; koda's scan is 0.84 ms.
- **A cross-encoder or LLM reranker.** It is the strongest measured technique in
  the code benchmark (§2.4) and still wrong here: the agent reading the top hits
  *is* the reranker, and it is already built.
- **tree-sitter for AST chunking.** cAST's +4.3 Recall@5 is over *fixed line
  windows*, not over function boundaries **[reported]**; koda already has
  function boundaries for free. ~25 grammar crates is not the price of that
  delta.
- **Convex-combination fusion with a tuned α.** Better than RRF when you can tune
  it (§2.4); koda has no labelled data per repo and no way to get it.
- **Automatic context injection.** `spec-rag.md` floats prepending top-k chunks
  to every turn behind `rag_auto_context`. `src/context.rs` now curates the
  window in three escalating passes; adding an uninvited 2,000-token block on
  top of that is a budget regression the user cannot see or attribute. Retrieval
  stays a tool the model calls.
- **Indexing sessions, memory and docs in P1.** Different chunking, different
  provenance rules, different failure modes. Code first, measure, then decide.
- **User-facing tuning knobs** for `k1`, `b`, `K`, or the weights. §3.9.

## 7. How this stops being guesswork

Four constants in this document are unmeasured: `w_graph = 1.2`, the 120/8-line
chunk bounds, the per-file cap of 3, and whether BM25F earns its complexity.
None can be settled by reading more papers, because the target corpus is a
specific repository, not TREC.

The cheap harness: 40 queries against koda itself, each a question a user would
type, each labelled with the `path:line` a correct answer must contain — drawn
from this session's own examples (`where is retry handled` → `llm.rs`
retry machinery; `how is the token budget trimmed` → `agent.rs::trim`; `undo a
file write` → `agent.rs` snapshot code). Store as `tests/retrieval_gold.txt`.
Report Recall@10 and MRR@10. That is a `cargo test` that fails when a fusion
weight change makes retrieval worse, which is the only mechanism that turns any
of §3.7 into knowledge. It is roughly a day of work and it should precede P4, not
follow it.

## 8. Where this differs from `docs/spec-rag.md`

`spec-rag.md` got the architecture right — symbol-aware chunking on
`graph::parse_file`, flat in-memory vectors with a brute-force scan, f16
quantisation, RRF at K=60, graceful lexical-only degradation, no DB, no daemon.
All of that stands, and this document supplies the citations and measurements it
asserted without.

Five changes:

1. **A `codegraph query="search"` mode, not a separate `retrieve` tool** — §3.1,
   on the measured tool-schema budget in `plan-small-models.md`.
2. **No stored chunk bodies** — §3.8. Spans plus a hash; re-read at query time.
   Smaller index, and results that cannot be stale.
3. **Lexical on by default** (`codegraph_search = true`), where `spec-rag.md` had
   a master `rag = false`. A 60 ms build and a 0.9 MB index that answers §1's
   query does not deserve an opt-in.
4. **Three config keys, not eleven** — §3.9.
5. **Drop the deferred `hnsw_rs` feature and the auto-context mode** — §6, on the
   measured scan cost and on `context.rs` now owning the window.

One deliberate non-change: `spec-rag.md` chose RRF, and the newer literature says
convex combination is better *when tuned* (§2.4). RRF stays, for the stated
reason that koda has nowhere to tune from.

## 9. Reproducing the measurements

Every **[measured]** number above came from small standalone programs compiled
with `rustc -O`, run against `/Users/sridhar/research/koda/src/*.rs` on the
development machine (darwin 24.6.0), single-threaded, no SIMD:

- corpus and index shape (`files/bytes/chunks/vocab/postings`, 40-line windows,
  identifier-splitting tokeniser) — 1,575,687 B → 1,054 chunks, 8,135 terms,
  103,436 postings, 606 KB packed postings, 45 ms;
- BM25 build and query (`k1=1.2, b=0.75`, Lucene idf) — 17.5 ms index build on
  pre-tokenised docs, 4.2 µs mean over 1,000 query executions;
- ranked top-k with and without stoplist and stemmer — the three tables in §1
  and §2.2;
- brute-force cosine over `n × d` f32 — the table in §3.6, mean of 20 runs;
- binary size from `target/release/koda` (9,303,088 B);
- endpoint support by fetching Ollama's OpenAI-compatibility page and llama.cpp's
  `tools/server/README.md`.

The programs are throwaway; the point of listing them is that anyone can rebuild
them in twenty minutes and get the same numbers, and that nothing in §4 is a
vibe.

## 10. Citations

**Lexical retrieval**

- Robertson, S. & Zaragoza, H. (2009). *The Probabilistic Relevance Framework:
  BM25 and Beyond.* Foundations and Trends in Information Retrieval 3(4),
  333–389. https://dl.acm.org/doi/abs/10.1561/1500000019 ·
  PDF: https://www.staff.city.ac.uk/~sbrp622/papers/foundations_bm25_review.pdf
- Kamphuis, C., de Vries, A. P., Boytsov, L. & Lin, J. (2020). *Which BM25 Do You
  Mean? A Large-Scale Reproducibility Study of Scoring Variants.* ECIR 2020,
  LNCS 12036. https://link.springer.com/chapter/10.1007/978-3-030-45442-5_4 ·
  PDF: https://cs.uwaterloo.ca/~jimmylin/publications/Kamphuis_etal_ECIR2020_preprint.pdf
- Lù, X. H. (2024). *BM25S: Orders of magnitude faster lexical search via eager
  sparse scoring.* arXiv:2407.03618. https://arxiv.org/abs/2407.03618

**Fusion**

- Cormack, G. V., Clarke, C. L. A. & Büttcher, S. (2009). *Reciprocal Rank Fusion
  outperforms Condorcet and Individual Rank Learning Methods.* SIGIR 2009,
  758–759. https://dl.acm.org/doi/10.1145/1571941.1572114 ·
  PDF: https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf
- Bruch, S., Gai, S. & Ingber, A. (2023). *An Analysis of Fusion Functions for
  Hybrid Retrieval.* ACM TOIS 42(1), Article 20.
  https://dl.acm.org/doi/10.1145/3596512 · arXiv:2210.11934
  https://arxiv.org/abs/2210.11934
- Wang, M., Tan, B., Gao, Y., Jin, H., Zhang, Y., Ke, X., Xu, X. & Zhu, Y. (2025).
  *Balancing the Blend: An Experimental Analysis of Trade-offs in Hybrid Search.*
  arXiv:2508.01405. https://arxiv.org/abs/2508.01405

**Dense retrieval and code models**

- Feng, Z., Guo, D., Tang, D., Duan, N., et al. (2020). *CodeBERT: A Pre-Trained
  Model for Programming and Natural Languages.* arXiv:2002.08155.
  https://arxiv.org/abs/2002.08155
- Guo, D., Ren, S., Lu, S., Feng, Z., et al. (2021). *GraphCodeBERT: Pre-training
  Code Representations with Data Flow.* ICLR 2021. arXiv:2009.08366.
  https://arxiv.org/abs/2009.08366
- Guo, D., Lu, S., Duan, N., Wang, Y., Zhou, M. & Yin, J. (2022). *UniXcoder:
  Unified Cross-Modal Pre-training for Code Representation.* ACL 2022.
  arXiv:2203.03850. https://arxiv.org/abs/2203.03850
- Wang, Y., Le, H., Gotmare, A. D., Bui, N. D. Q., Li, J. & Hoi, S. C. H. (2023).
  *CodeT5+: Open Code Large Language Models for Code Understanding and
  Generation.* arXiv:2305.07922. https://arxiv.org/abs/2305.07922
- Liu, Y., Meng, R., Joty, S., Savarese, S., Xiong, C., Zhou, Y. & Yavuz, S.
  (2024). *CodeXEmbed: A Generalist Embedding Model Family for Multilingual and
  Multi-task Code Retrieval.* arXiv:2411.12644. https://arxiv.org/abs/2411.12644
- Khattab, O. & Zaharia, M. (2020). *ColBERT: Efficient and Effective Passage
  Search via Contextualized Late Interaction over BERT.* SIGIR 2020.
  arXiv:2004.12832. https://arxiv.org/abs/2004.12832

**Benchmarks**

- Husain, H., Wu, H.-H., Gazit, T., Allamanis, M. & Brockschmidt, M. (2019).
  *CodeSearchNet Challenge: Evaluating the State of Semantic Code Search.*
  arXiv:1909.09436. https://arxiv.org/abs/1909.09436
- Huang, J., Tang, D., Shou, L., Gong, M., et al. (2021). *CoSQA: 20,000+ Web
  Queries for Code Search and Question Answering.* ACL 2021. arXiv:2105.13239.
  https://arxiv.org/abs/2105.13239
- Li, X., Dong, K., Lee, Y. Q., Xia, W., et al. (2025). *CoIR: A Comprehensive
  Benchmark for Code Information Retrieval Models.* ACL 2025. arXiv:2407.02883.
  https://arxiv.org/abs/2407.02883
- Thakur, N., Reimers, N., Rücklé, A., Srivastava, A. & Gurevych, I. (2021).
  *BEIR: A Heterogenous Benchmark for Zero-shot Evaluation of Information
  Retrieval Models.* NeurIPS 2021 Datasets & Benchmarks. arXiv:2104.08663.
  https://arxiv.org/abs/2104.08663
- Su, H., Yen, H., Xia, M., Shi, W., et al. (2024). *BRIGHT: A Realistic and
  Challenging Benchmark for Reasoning-Intensive Retrieval.* arXiv:2407.12883.
  https://arxiv.org/abs/2407.12883
- Wang, Z. Z., Asai, A., Yu, X. V., Xu, F. F., Xie, Y., Neubig, G. & Fried, D.
  (2024). *CodeRAG-Bench: Can Retrieval Augment Code Generation?*
  arXiv:2406.14497. https://arxiv.org/abs/2406.14497
- Pantha, N., Kumbam, P. R., Awale, S., et al. (2026). *Scientific Code Search at
  Scale: A Multi-Domain Dataset and Benchmark.* arXiv:2607.05443.
  https://arxiv.org/abs/2607.05443 — recent, single-domain, not independently
  replicated; its hybrid numbers are used only directionally.

**Chunking and identifiers**

- Zhang, Y., Zhao, X., Wang, Z. Z., Yang, C., Wei, J. & Wu, T. (2025). *cAST:
  Enhancing Code Retrieval-Augmented Generation with Structural Chunking via
  Abstract Syntax Tree.* Findings of EMNLP 2025. arXiv:2506.15655.
  https://arxiv.org/abs/2506.15655 · https://aclanthology.org/2025.findings-emnlp.430/
- Enslen, E., Hill, E., Pollock, L. & Vijay-Shanker, K. (2009). *Mining Source
  Code to Automatically Split Identifiers for Software Analysis.* MSR 2009.
  https://ieeexplore.ieee.org/document/5069482/
- Dit, B., Guerrouj, L., Poshyvanyk, D. & Antoniol, G. (2011). *Can Better
  Identifier Splitting Techniques Help Feature Location?* ICPC 2011, 11–20.
  https://www.semanticscholar.org/paper/55de1fafac67f91cf0b92f485e4f000d723eda5c

**Endpoints**

- Ollama, OpenAI compatibility. https://docs.ollama.com/api/openai-compatibility
- llama.cpp server README, `POST /v1/embeddings`.
  https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md

### A note on what is not verified

- GraphCodeBERT, UniXcoder and CodeT5+ claim SOTA on code search in their
  abstracts without giving numbers there; this document does not attribute
  numbers to them.
- The `Scientific Code Search at Scale` figures come from a single recent
  benchmark and were read out of the paper's HTML rendering; treat them as
  directional.
- Build-time and embedding-throughput figures marked **[estimated]** are
  arithmetic on measured per-unit costs, not end-to-end timings of a system that
  does not exist yet. They are the numbers most likely to be wrong.

---

## 11. What landed, and what it measured

Written after implementation. Everything here is **[measured]** on koda's own
source (68 files, 1,738 chunks, 7,509 terms, 94,803 postings) on an Apple M-series
laptop, 10 cores, release build. The harness is
`index::tests::retrieval_quality_holds` over the 35 labelled queries in
`tests/retrieval_gold.txt`; the sweeps are the two `#[ignore]` diagnostics beside
it. §7 asked for exactly this and it now exists, so the constants below are
measurements rather than assertions.

### 11.1 Accuracy

| | P@1 | R@3 | R@10 | MRR@10 |
| --- | --- | --- | --- | --- |
| Before (BM25 over raw chunk text) | 0.429 | 0.800 | 0.971 | 0.617 |
| + string literals dropped | 0.486 | 0.771 | 0.971 | 0.643 |
| + BM25F field weights | 0.543 | 0.829 | 0.943 | 0.695 |
| + structural channel, abstaining | **0.600** | 0.743 | 0.971 | **0.709** |

**+40% P@1 and +15% MRR@10.** R@10 — whether the right file reaches the model at
all, which is what the tool actually returns — is unchanged at 0.971. R@3 is
down 0.057; the mid-ranks shuffle in exchange for the top rank being right more
often, which is the trade worth making for a reader that starts at the top.

Three changes, in order of how much they bought:

1. **String literals are now dropped from the indexed text** — which §3.3 always
   said happened, and which the code never did. `chunk_text` joined the raw
   lines. The cost was specific and funny: this module's own diagnostic test
   holds the sentences `"where is retry handled"` and `"rate limiting throttle"`
   as `&str`, and was therefore rank 1 for three of five sample queries, above
   the code implementing them. A string in a program is what it *says*, not what
   the code *does*.
2. **BM25F field weights**, 3/2/2/1 over symbol names, path, doc prose and body,
   combined into one frequency *before* saturation as Robertson et al. specify —
   scoring fields separately and adding lets a term that appears once in three
   fields saturate three times.
3. **A structural channel** over definition names and paths only, fused by RRF.
   This is the offline stand-in for the dense half, and the cheap form of the
   lexically-anchored graph retrieval in RepoGraph and LARGER.

### 11.2 The structural channel, and why it abstains

The first version of it made retrieval **worse at every weight and depth tried** —
a 30-point grid, all of it at or below the lexical-only baseline on MRR. The
per-channel diagnostic (`show_channels`) explained it: the channel is excellent
when the query names something real (`src/session.rs` at rank 0 for "where do we
write the session file"; `src/context.rs` at ranks 0–2 for "how is the context
window trimmed" — both of which the lexical channel missed entirely) and returns
arbitrary ties when it does not. RRF weighs by rank alone, so its fiftieth guess
votes nearly as loudly as its first, and it had no way to say "I have nothing".

Three fixes turned it positive, each measured:

- **Test files are excluded from it.** A test's name is a sentence —
  `a_plain_file_mention_is_left_for_the_model_to_fetch` — not a symbol. Admitting
  them filled the channel's top ranks with assertions on unrelated queries.
- **Terms in more than 10% of chunks are ignored**, and the file extension is not
  indexed at all. Without this, `src` and `rs` matched every chunk in the project.
- **It abstains below a coverage threshold**: a chunk must be named for about a
  third of the query's *nameable* words, not merely one of them. This is the
  change that flipped the sign.

### 11.3 What was tried and rejected

- **BM25+ (Lv & Zhai's δ lower bound), δ = 1.0.** P@1 0.571 → 0.514, MRR 0.697 →
  0.666. Rewarding a document merely for containing a term is wrong here: a
  chunk that mentions `retry` once is not evidence about retry.
- **Fusing the structural channel without abstention**, at weights 0.2–2.0 and
  depths 3–50. Best MRR 0.686 against a 0.695 baseline. Recorded because it is
  the intuitive design and it does not work.

### 11.4 Performance

Medians of five runs on an idle machine (Apple M1 Max, 10 cores, 32 GB,
rustc 1.98.0, release + thin LTO). The right-hand column is `MAX_FILES = 4000`
worth of crates.io sources — koda's own hard ceiling — via `bench_large`.

| | koda · 68 files | ceiling · 4,000 files |
| --- | --- | --- |
| chunks / terms / postings | 1,738 / 7,519 / 95k | 68,363 / 200k / 2.58M |
| cold build, 10 cores | 67 ms | 1,609 ms |
| tokenise only, forced to 1 core | 172 ms | — |
| cache save | 1.6 ms | 33 ms |
| cache load | 1.8 ms | 46 ms |
| **warm start (load + sweep tree)** | **2.6 ms** | **46 ms** |
| cache on disk, lexical only | 0.88 MB | 27.9 MB |
| resident, estimated | 1.5 MB | ~50 MB |
| query, mean over the 35 gold queries | 55 µs | 334 µs |

The build is now read-then-tokenise, with the tokenising fanned out across cores
(`prepare_in_parallel`, the same shape and the same small-job refusal as
`graph::parse_in_parallel`); reading stays serial because eight threads seeking
at once is worse on the disks that would benefit. 177 ms → 67 ms.

But the number that matters is 2.6 ms, and it is not about the build. §3.8's
`.koda/index/` cache now exists — a `meta.json` manifest of per-file
`(mtime, size)` stamps, and a hand-rolled little-endian `index.bin`. Every read
in the loader is bounds-checked and returns `None` rather than panicking, because
that file can be truncated by a full disk, a killed process, or a synced folder,
and none of those may take a session down; `None` means rebuild, which is always
correct. Writes go through a temp file and a retried rename, because on Windows a
replace fails outright while another process holds the destination open.

The cache is not there to save 66 ms. It is there to save the **embeddings**,
which cost minutes on a machine without a GPU and were previously re-fetched on
every single start. Two changes make that stick:

- `Vectors` now carries an explicit row → chunk map, so the store can cover
  *part* of the corpus. It used to be positional, which made it all-or-nothing:
  one file edited during a session invalidated every embedding in the project.
  Now an edit costs re-embedding the chunks of one file.
- The background fill tops up `unembedded()` rather than starting over, verifies
  each row still belongs to the chunk it was requested for (the tree moves while
  minutes of embedding run), and saves immediately afterwards.

Incremental update landed with it, on the hooks §3.8 named: `Index::update_file`
and `remove_file` sit beside the graph's in the `write_file`/`edit_file` success
branch, and `Index::refresh` rides the existing `codegraph_refresh_ms` sweep.
Removal is by tombstone, compacted at 20% dead, because renumbering a chunk id
means renumbering the postings, the structural index and the vector rows.

### 11.5 A cross-platform bug the work uncovered

`is_test` looks for `tests/` and `/tests/`. The walk yields `tests\probe.rs` on
Windows, so **every test file in every project silently stopped being recognised
as one** and kept its full rank. Paths are now normalised to `/` once, at the
edge, in `relative()`. The same normalisation is what makes the cache portable
across machines sharing a working tree.

### 11.6 Still not done

- **§3.9's `embed_batch`.** `EMBED_BATCH` is still a constant at 32.
- **Graph reference expansion.** The structural channel ranks by definition name
  and path; it does not yet walk `Graph::refs` to a chunk's callers, which is the
  other half of what RepoGraph does.
- **The gold set is 35 queries against one repository**, written by the person
  who wrote the ranker. It is enough to catch a regression and not enough to
  claim a general result.
