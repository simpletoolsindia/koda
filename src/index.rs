//! Lexical search over the codebase: BM25 across definition-sized chunks.
//!
//! The symbol graph answers "where is `average` defined". It cannot answer
//! "where is retry handled", because that question names no symbol. This is the
//! other half: an inverted index over the code, so a question shaped like
//! intent finds the code that serves it.
//!
//! Design and evidence: `docs/research-hybrid-retrieval.md`. The short version
//! of why it looks like this:
//!
//! - **Chunks are definitions**, taken from the spans `graph::parse_file`
//!   already finds, extended backwards over the doc comment. No second parser,
//!   and the doc comment is the part a natural-language query actually matches.
//! - **Preprocessing beats ranking.** Measured on koda's own source, "where is
//!   retry handled" returns the right answers only once identifiers are split
//!   (`stream_with_retry` -> `stream`, `with`, `retry`) and code-ambient English
//!   is stopped; without the stoplist the top hit is a test asserting on the
//!   word "handles". Both are in `terms`.
//! - **BM25 with Lucene's constants**, `k1 = 1.2`, `b = 0.75`, not configurable.
//!   Tuning them per repository is not something a user can do informedly.
//!
//! It is deliberately offline and dependency-free: this half works with no
//! embedding server, no config, and no network.

use crate::graph;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Lucene's BM25 constants. Not exposed: a user cannot tune these informedly,
/// and the literature's spread of "better" values is inside the noise for
/// corpora this size.
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// A definition longer than this is split into windows, so one enormous
/// function cannot dominate its own file's postings.
const MAX_CHUNK_LINES: usize = 120;
/// Overlap between windows of a split definition, so a match that straddles the
/// boundary is still whole in one of them.
const WINDOW_OVERLAP: usize = 8;
/// Spans from one file allowed in one page of results.
const MAX_HITS_PER_FILE: usize = 2;

/// Definitions shorter than this are merged with their neighbours: a run of
/// three-line getters is one idea, not three documents.
const MIN_CHUNK_LINES: usize = 8;

/// One indexed span of one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub path: String,
    /// 1-based, inclusive.
    pub start: usize,
    pub end: usize,
    /// The definitions this span covers, for the result header.
    pub names: Vec<String>,
}

/// The inverted index.
#[derive(Default, Serialize, Deserialize)]
pub struct Index {
    pub chunks: Vec<Chunk>,
    /// Present only once an embedding model has been configured and the
    /// background fill has finished. Absent is the normal state, not an error.
    pub vectors: Option<Vectors>,
    /// term -> id
    vocab: HashMap<String, u32>,
    /// term id -> [(chunk id, term frequency)]
    post: Vec<Vec<(u32, u16)>>,
    /// term -> chunks whose *defined symbol names or path* contain it. A second,
    /// far sparser posting list than `post`; see `rank_symbolic`.
    symbols: HashMap<String, Vec<u32>>,
    /// chunk id -> token count
    len: Vec<u32>,
    avgdl: f32,
    /// Files indexed, so a stale index can be told apart from an empty one.
    pub files: usize,
    /// path -> (mtime seconds, size) as of indexing, the same identity
    /// `graph::Stamp` keeps. What lets a cache validate itself and `refresh`
    /// find outside edits without reading anything.
    stamps: BTreeMap<String, (u64, u64)>,
    /// Retired chunks, by id. Not serialised: a save compacts first, so a cache
    /// never carries tombstones.
    #[serde(skip)]
    dead: Vec<bool>,
}

/// One search result.
#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk: usize,
    pub score: f32,
}

/// Embeddings for every chunk, L2-normalised so cosine is a dot product.
///
/// Stored as f16. Components of a normalised vector live in [-1, 1], where f16
/// carries about three decimal digits — far more than a ranking needs — and it
/// halves a store that is otherwise the largest thing koda keeps in memory for
/// a repository.
#[derive(Default, Serialize, Deserialize)]
pub struct Vectors {
    pub dim: usize,
    /// `n * dim`, row-major.
    data: Vec<u16>,
    /// Row -> chunk id.
    ///
    /// Rows used to be positionally the chunks, which made the store all or
    /// nothing: one file edited during a session invalidated every embedding in
    /// it. With the mapping explicit the dense half can cover *part* of the
    /// corpus, so an edit costs re-embedding the chunks of one file instead of
    /// the minutes a whole corpus costs on a machine without a GPU. Chunks with
    /// no row are simply absent from the dense ranking; the other two channels
    /// still rank them.
    rows_of: Vec<u32>,
    /// The model that produced these. Changing it invalidates them and nothing
    /// else, since the lexical half never depended on it.
    pub model: String,
}

impl Vectors {
    pub fn rows(&self) -> usize {
        self.data.len().checked_div(self.dim).unwrap_or(0)
    }

    fn push(&mut self, cid: u32, v: &[f32]) {
        // Normalise on the way in so every query is a dot product.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        self.data.extend(v.iter().map(|x| f16_from(x / norm)));
        self.rows_of.push(cid);
    }

    /// Add or replace one chunk's embedding, so a background fill can top up an
    /// existing store after an edit instead of rebuilding it.
    pub fn upsert(&mut self, cid: u32, v: &[f32]) {
        if v.len() != self.dim {
            return;
        }
        match self.rows_of.iter().position(|c| *c == cid) {
            Some(row) => {
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                for (i, x) in v.iter().enumerate() {
                    self.data[row * self.dim + i] = f16_from(x / norm);
                }
            }
            None => self.push(cid, v),
        }
    }

    /// Keep only the rows whose chunk survives a compaction, renumbering them.
    fn retain(&mut self, remap: impl Fn(u32) -> u32) {
        let mut data = Vec::with_capacity(self.data.len());
        let mut rows_of = Vec::with_capacity(self.rows_of.len());
        for (row, cid) in self.rows_of.iter().enumerate() {
            let to = remap(*cid);
            if to == u32::MAX {
                continue;
            }
            data.extend_from_slice(&self.data[row * self.dim..(row + 1) * self.dim]);
            rows_of.push(to);
        }
        self.data = data;
        self.rows_of = rows_of;
    }

    /// Cosine against every row. A flat scan: at koda's corpus sizes an ANN
    /// index would be more code, more memory and more failure modes than the
    /// millisecond it saves.
    fn search(&self, query: &[f32], k: usize) -> Vec<Hit> {
        if self.dim == 0 || query.len() != self.dim || k == 0 {
            return Vec::new();
        }
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        let q: Vec<f32> = query.iter().map(|x| x / norm).collect();
        // A bounded selection rather than sorting the corpus. Sorting 20,000
        // rows to look at eight of them is the kind of waste that is invisible
        // on a laptop and is not on a low-power machine, where this scan is
        // already the most expensive thing a query does.
        let mut best: Vec<Hit> = Vec::with_capacity(k + 1);
        for (i, &cid) in self.rows_of.iter().enumerate() {
            let row = &self.data[i * self.dim..(i + 1) * self.dim];
            let score = row.iter().zip(&q).map(|(a, b)| f16_to(*a) * b).sum::<f32>();
            if best.len() == k && score <= best[k - 1].score {
                continue;
            }
            let at = best.partition_point(|h| h.score >= score);
            best.insert(
                at,
                Hit {
                    chunk: cid as usize,
                    score,
                },
            );
            best.truncate(k);
        }
        best
    }
}

/// f32 -> f16 bits, round-to-nearest, flushing subnormals to zero.
///
/// Values here are components of a unit vector, so the interesting range is
/// well inside f16's normal range; anything below it contributes nothing to a
/// dot product worth keeping a denormal path for.
fn f16_from(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mant = bits & 0x007f_ffff;
    if exp >= 31 {
        return sign | 0x7bff; // saturate rather than produce an infinity
    }
    if exp <= 0 {
        return sign;
    }
    // Round to nearest, ties away from zero, on the 13 bits being dropped.
    let mut half = sign | ((exp as u16) << 10) | (mant >> 13) as u16;
    if mant & 0x1000 != 0 {
        half = half.wrapping_add(1);
    }
    half
}

fn f16_to(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x03ff) as u32;
    if exp == 0 {
        return f32::from_bits(sign);
    }
    f32::from_bits(sign | (((exp - 15 + 127) as u32) << 23) | (mant << 13))
}

impl Index {
    /// Score every chunk that shares a term with the query, best first.
    ///
    /// One f32 accumulator over the corpus and a walk of each query term's
    /// postings — measured at microseconds over a thousand chunks, so there is
    /// nothing to be clever about.
    fn rank_lexical(&self, query: &str, k: usize) -> Vec<Hit> {
        let terms = terms(None, query);
        if terms.is_empty() || self.chunks.is_empty() {
            return Vec::new();
        }
        let n = self.chunks.len() as f32;
        let mut acc = vec![0f32; self.chunks.len()];
        for t in &terms {
            let Some(&tid) = self.vocab.get(t) else {
                continue;
            };
            let postings = &self.post[tid as usize];
            if postings.is_empty() {
                continue;
            }
            // Lucene's IDF: always positive, so a term in most documents adds
            // little rather than subtracting.
            let df = postings.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for &(cid, tf) in postings {
                if !self.live(cid as usize) {
                    continue;
                }
                let dl = self.len[cid as usize] as f32;
                let tf = tf as f32;
                let norm = tf + K1 * (1.0 - B + B * dl / self.avgdl.max(1.0));
                acc[cid as usize] += idf * (tf * (K1 + 1.0)) / norm.max(1e-6);
            }
        }
        let mut hits: Vec<Hit> = acc
            .into_iter()
            .enumerate()
            .filter(|(_, s)| *s > 0.0)
            .map(|(chunk, score)| Hit { chunk, score })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));

        hits.truncate(k);
        hits
    }
}

impl Index {
    /// Rank by what a chunk *is* rather than what it contains: its defined
    /// symbols and its path.
    ///
    /// The offline half of what the dense channel buys when an embedding server
    /// is available. It exists because BM25 over bodies has a specific, and
    /// measurable, blind spot: a word that names a subsystem appears in every
    /// file that touches it, so its IDF collapses and the file the subsystem
    /// *lives in* stops being distinguishable from the twenty that call into
    /// it. Measured here: "where do we write the session file" ranked
    /// `src/session.rs` outside the top ten, behind three modules whose header
    /// comments mention sessions in passing.
    ///
    /// The fix is not a better weight — it is a different evidence source. This
    /// index holds only definition names and path components, so "session"
    /// matches the ~30 chunks that define something called session or live in
    /// `session.rs`, not the ~400 that mention one. Fusing it by rank rather
    /// than by score (RRF, below) is what keeps the collapsed IDF from
    /// following it into the fusion.
    ///
    /// This is the cheap form of the lexically-anchored graph retrieval in
    /// RepoGraph and LARGER: anchor on the query's words, then rank by position
    /// in the repository's own structure rather than by prose overlap.
    fn rank_symbolic(&self, query: &str, k: usize, coverage: f32) -> Vec<Hit> {
        let terms = terms(None, query);
        if terms.is_empty() || self.chunks.is_empty() {
            return Vec::new();
        }
        let n = self.chunks.len() as f32;
        let ceiling = (n * STRUCT_MAX_DF) as usize;
        let mut acc: HashMap<u32, (f32, u32)> = HashMap::new();
        // How many of the query's terms this channel could possibly speak to.
        // Coverage is measured against that, not against the raw query: a
        // question with one nameable word in it is not half-answered by
        // matching that word, it is fully answered.
        let answerable = terms
            .iter()
            .filter(|t| {
                self.symbols
                    .get(*t)
                    .is_some_and(|p| !p.is_empty() && p.len() <= ceiling.max(1))
            })
            .count()
            .max(1) as f32;
        for t in &terms {
            let Some(posting) = self.symbols.get(t) else {
                continue;
            };
            // A structural term shared by a tenth of the corpus is describing
            // the project, not the chunk. Skipped outright rather than
            // down-weighted: this channel's whole value is that it is thin, and
            // one common term admitted at any weight makes it thick again.
            if posting.is_empty() || posting.len() > ceiling.max(1) {
                continue;
            }
            let idf = (1.0 + (n - posting.len() as f32 + 0.5) / (posting.len() as f32 + 0.5)).ln();
            for &cid in posting {
                if !self.live(cid as usize) {
                    continue;
                }
                let e = acc.entry(cid).or_insert((0.0, 0));
                e.0 += idf;
                e.1 += 1;
            }
        }
        // Coverage *scales* the weight rather than being added to it. A chunk
        // named for two of the query's three nameable words is a better answer
        // than one named twice over for its rarest — but "better" has to stay
        // proportional, because an additive bonus made every one-term match tie
        // at the same score and the order among them was then the chunk id.
        //
        // And it only speaks when it recognises the query: a chunk has to be
        // named for *every* nameable word, not merely for one of them. This
        // abstention is the whole design. Measured over the gold set, fusing
        // this channel's full ranking made retrieval worse at every weight and
        // every depth tried — RRF weighs by rank alone, so a channel's fiftieth
        // guess votes nearly as loudly as its first, and it has no way to tell
        // "I know this one" from "I have nothing". Made to abstain instead, the
        // same channel is a clear gain: it answers the queries that name a
        // subsystem and stays silent on the ones that describe a behaviour.
        let mut hits: Vec<Hit> = acc
            .into_iter()
            .filter(|(_, (_, matched))| *matched as f32 >= (answerable * coverage).ceil())
            .map(|(cid, (score, _))| Hit {
                chunk: cid as usize,
                score,
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
        hits.truncate(k);
        hits
    }
}

/// Cormack et al.'s constant. Their own sweep moves MAP by 0.2% between k=10
/// and k=100, so there is no tuning to be had here without labelled queries.
const RRF_K: f32 = 60.0;
/// How deep each path is read before fusing.
const FUSE_DEPTH: usize = 50;
/// The structural channel's weight in the fusion, relative to 1.0 for lexical
/// and dense. Below 1 because it is silent on any query that names neither a
/// symbol nor a file, and a channel that abstains should not outvote one that
/// answered.
const W_SYMBOLIC: f32 = 0.7;
/// Share of the corpus above which a structural term is ignored. See
/// `rank_symbolic`.
const STRUCT_MAX_DF: f32 = 0.10;
/// How deep the structural channel is read. Far shallower than `FUSE_DEPTH`
/// on purpose: RRF weighs by rank alone, so at equal depth a channel's fiftieth
/// guess votes almost as loudly as its first, and this channel's fiftieth guess
/// is noise. Reading it shallow is how a precise-but-thin channel is allowed to
/// be thin.
const SYMBOLIC_DEPTH: usize = 8;
/// Share of a query's nameable words a chunk must be named for before the
/// structural channel will vote for it.
///
/// A third, nudged just above it: at three nameable words this asks for two
/// matches rather than one, which is where the gold set says the line belongs.
/// Swept together with the weight and the depth in `sweep_structural_weight` —
/// the grid is flat enough around this point that the exact value is not
/// load-bearing, and steep enough at 0.7 and above (where the channel abstains
/// on real queries) that the shape is.
const SYMBOLIC_COVERAGE: f32 = 0.34;
/// Ranks a test chunk is pushed down, unless the question is about tests. The
/// measured top-1 failure for "where is retry handled" was a test asserting on
/// the English word "handles".
const TEST_PENALTY: usize = 10;

impl Index {
    /// Fuse the lexical and vector rankings.
    ///
    /// Reciprocal rank fusion rather than a weighted sum of scores: BM25 scores
    /// and cosine similarities are not on a common scale, and the literature's
    /// better-performing alternative (convex combination) needs a weight tuned
    /// on in-domain labelled queries, which no koda user is going to produce
    /// for their own repository. RRF needs only the orderings.
    pub fn hybrid_search(&self, query: &str, query_vec: Option<&[f32]>, k: usize) -> Vec<Hit> {
        self.fuse(
            query,
            query_vec,
            k,
            W_SYMBOLIC,
            SYMBOLIC_DEPTH,
            SYMBOLIC_COVERAGE,
        )
    }

    /// `hybrid_search` with the structural weight exposed, so the gold-set
    /// sweep in the tests can measure it instead of the module asserting it.
    fn fuse(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        k: usize,
        w_sym: f32,
        d_sym: usize,
        cov: f32,
    ) -> Vec<Hit> {
        let lexical = self.rank_lexical(query, FUSE_DEPTH);
        let symbolic = self.rank_symbolic(query, d_sym, cov);
        let mut dense = match (query_vec, self.vectors.as_ref()) {
            (Some(q), Some(v)) => v.search(q, FUSE_DEPTH),
            _ => Vec::new(),
        };
        dense.retain(|h| self.live(h.chunk));
        if symbolic.is_empty() && dense.is_empty() {
            return self.finish_ranking(lexical, query, k);
        }
        let mut fused: HashMap<usize, f32> = HashMap::new();
        // Weights, not just ranks: the structural channel is precise but thin —
        // it says nothing at all about a query that names no symbol and no
        // file — so it contributes at less than full strength. The other two
        // are peers. These are the only fusion weights in the module and they
        // are gated by `tests/retrieval_gold.txt`.
        for (list, w) in [(&lexical, 1.0f32), (&symbolic, w_sym), (&dense, 1.0)] {
            for (rank, h) in list.iter().enumerate() {
                *fused.entry(h.chunk).or_insert(0.0) += w / (RRF_K + rank as f32 + 1.0);
            }
        }
        let mut hits: Vec<Hit> = fused
            .into_iter()
            .map(|(chunk, score)| Hit { chunk, score })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.chunk.cmp(&b.chunk)));
        self.finish_ranking(hits, query, k)
    }

    /// The rules that apply however the ranking was produced: demote tests,
    /// then keep the page diverse.
    fn finish_ranking(&self, mut hits: Vec<Hit>, query: &str, k: usize) -> Vec<Hit> {
        let asking_about_tests = query.to_ascii_lowercase().contains("test");
        if !asking_about_tests {
            // A stable demotion by position rather than by score, so it works
            // the same whether the score is a BM25 sum or an RRF total.
            let mut ordered: Vec<(usize, &Hit)> = hits.iter().enumerate().collect();
            ordered.sort_by_key(|(i, h)| {
                i + if is_test(&self.chunks[h.chunk].path) {
                    TEST_PENALTY
                } else {
                    0
                }
            });
            hits = ordered.into_iter().map(|(_, h)| h.clone()).collect();
        }
        let mut per_file: HashMap<&str, usize> = HashMap::new();
        let mut kept = Vec::with_capacity(k);
        for h in hits {
            let path = self.chunks[h.chunk].path.as_str();
            let seen = per_file.entry(path).or_insert(0);
            if *seen >= MAX_HITS_PER_FILE {
                continue;
            }
            *seen += 1;
            kept.push(h);
            if kept.len() >= k {
                break;
            }
        }
        kept
    }
}

/// Whether a path is test code. Cheap and syntactic on purpose: a chunk inside
/// a `#[cfg(test)]` block would need the parser to say so, and the file-level
/// signal catches most of it.
fn is_test(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.starts_with("tests/")
        || p.contains("/tests/")
        || p.contains("test_")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains("spec.")
}

/// Split text into the terms the index stores.
///
/// The order matters and each step was measured (see the module docs):
/// identifiers are emitted whole *and* split, so exact-symbol precision
/// survives alongside the parts that let prose match code; language keywords
/// and code-ambient English are dropped; a three-suffix stemmer folds plurals
/// and tenses without a dependency or a full Porter implementation.
pub fn terms(lang: Option<&str>, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        // The whole identifier, so `stream_with_retry` still beats its parts on
        // an exact search.
        if lower.len() > 1 && !is_noise(lang, &lower) {
            out.push(stem(&lower));
        }
        for part in split_identifier(raw) {
            if part.len() > 1 && !is_noise(lang, &part) && part != lower {
                out.push(stem(&part));
            }
        }
    }
    out
}

/// `stream_with_retry` / `streamWithRetry` / `StreamWithRetry` -> the parts.
fn split_identifier(id: &str) -> Vec<String> {
    let chars: Vec<char> = id.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && !cur.is_empty() {
            let prev_lower = chars[i - 1].is_lowercase() || chars[i - 1].is_numeric();
            // `streamWith` -> split here. And `HTTPServer` -> split before the
            // `S`, which is the last capital of a run before a lower-case
            // letter; without that case an acronym swallows the word after it.
            let acronym_end = !prev_lower && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev_lower || acronym_end {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Words that carry no signal about what code *does*: the language's own
/// keywords, and the English that appears in every codebase.
fn is_noise(lang: Option<&str>, w: &str) -> bool {
    if CODE_AMBIENT.contains(&w) || crate::context::STOPWORDS.contains(&w) {
        return true;
    }
    match lang {
        Some(l) => graph::keywords(l).contains(&w),
        // A query has no language, so check every keyword set we might have
        // indexed under.
        None => ["rust", "python", "js", "go"]
            .iter()
            .any(|l| graph::keywords(l).contains(&w)),
    }
}

/// English that means nothing in a codebase. `handle` earned its place by
/// measurement: without it, "where is retry handled" ranks a test asserting on
/// the word "handles" above the retry code.
const CODE_AMBIENT: &[&str] = &[
    "handle", "handled", "handles", "handler", "use", "used", "uses", "using", "get", "set", "new",
    "return", "returns", "value", "values", "data", "item", "items", "self", "none", "some",
    "true", "false", "null", "type", "types", "name", "names", "call", "called", "calls", "run",
    "runs", "make", "makes", "test", "tests", "todo", "line", "lines",
];

/// Fold plurals and tenses. Three suffixes, a 3-character floor, no dependency —
/// a full stemmer would change more words than it helps at this corpus size.
fn stem(w: &str) -> String {
    for suf in ["ings", "ing", "ies", "es", "ed", "s"] {
        if w.len() > suf.len() + 3 && w.ends_with(suf) {
            return w[..w.len() - suf.len()].to_string();
        }
    }
    w.to_string()
}

// ------------------------------------------------------------------ building

/// Cut one file into indexable spans.
///
/// The spans come from the definitions `graph::parse_file` already found, so
/// there is no second parser to keep in step with the first. Three adjustments
/// make them worth searching:
///
/// - the start walks **backwards** over the doc comment and attributes, because
///   that prose is what an intent-shaped query matches;
/// - a definition longer than [`MAX_CHUNK_LINES`] becomes overlapping windows,
///   so one long function cannot swamp its own file;
/// - definitions shorter than [`MIN_CHUNK_LINES`] merge with their neighbours,
///   because a run of small getters is one idea rather than five documents.
///
/// Everything before the first definition — imports, the module doc — is its
/// own chunk. That is where the module says what it is for.
pub fn chunk_file(path: &str, lang: &'static str, text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let parsed = graph::parse_file(path.to_string(), lang, text);
    let mut starts: Vec<(usize, String)> = parsed
        .defs
        .iter()
        .map(|(name, _, line)| (extend_back(&lines, *line), name.clone()))
        .collect();
    starts.sort_by_key(|(l, _)| *l);

    let mut out = Vec::new();
    // The preamble: imports and the module doc, if any definition follows them.
    let first = starts.first().map(|(l, _)| *l).unwrap_or(lines.len() + 1);
    if first > 1 {
        out.push(Chunk {
            path: path.to_string(),
            start: 1,
            end: first - 1,
            names: vec!["(module header)".into()],
        });
    }

    let mut i = 0;
    while i < starts.len() {
        let start = starts[i].0;
        let mut names = vec![starts[i].1.clone()];
        // Merge forward while the span is too small to stand alone.
        let mut j = i + 1;
        let mut end = starts.get(j).map(|(l, _)| l - 1).unwrap_or(lines.len());
        while end.saturating_sub(start) < MIN_CHUNK_LINES && j < starts.len() {
            let next_end = starts.get(j + 1).map(|(l, _)| l - 1).unwrap_or(lines.len());
            if next_end.saturating_sub(start) > MAX_CHUNK_LINES {
                break;
            }
            names.push(starts[j].1.clone());
            end = next_end;
            j += 1;
        }
        // Split forward while the span is too big to be one document.
        let mut window = start;
        while window <= end {
            let stop = (window + MAX_CHUNK_LINES - 1).min(end);
            out.push(Chunk {
                path: path.to_string(),
                start: window,
                end: stop,
                names: names.clone(),
            });
            if stop >= end {
                break;
            }
            window = stop.saturating_sub(WINDOW_OVERLAP) + 1;
        }
        i = j.max(i + 1);
    }
    out
}

/// Walk a definition's start backwards over its doc comment and attributes.
fn extend_back(lines: &[&str], line: usize) -> usize {
    let mut start = line;
    while start > 1 {
        let prev = lines.get(start - 2).map(|l| l.trim()).unwrap_or("");
        let is_doc = prev.starts_with("///")
            || prev.starts_with("//!")
            || prev.starts_with("//")
            || prev.starts_with("#[")
            || prev.starts_with("#!")
            || prev.starts_with("@")
            || prev.starts_with("*")
            || prev.starts_with("/**")
            || prev.starts_with("\"\"\"");
        if !is_doc {
            break;
        }
        start -= 1;
    }
    start
}

/// The text of a chunk, as indexed: comments kept, string literals dropped.
///
/// The inverse of what the graph does. The graph strips comments because it is
/// looking for code structure; the index keeps them because a comment is the
/// densest statement of intent in the file, and drops string literals because
/// they are data, not description.
///
/// Dropping them is not a nicety. Measured on koda's own source, indexing
/// literals put this module's own diagnostic test — which holds the sentences
/// "where is retry handled" and "rate limiting throttle" as `&str` — at rank 1
/// for three of five sample queries, above the code that implements them. A
/// string in a program is what it *says*, not what the surrounding code *does*,
/// and BM25 has no way to tell the difference.
fn chunk_text(lang: &str, lines: &[&str], c: &Chunk) -> String {
    lines
        .iter()
        .skip(c.start.saturating_sub(1))
        .take(c.end + 1 - c.start)
        .map(|l| strip_literals_keep_comments(lang, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Blank out string and char literals on one line while leaving comment prose
/// intact — the mirror of `graph::strip_literals_and_comments`, which keeps the
/// code and drops the prose.
///
/// Line-local, like its counterpart, and with the same trade: a string that
/// spans lines leaks its interior. Two cases are kept whole on purpose — a line
/// that *is* a comment, so an apostrophe in "the model's name" cannot swallow
/// the rest of it, and a Python triple-quote, because a docstring is
/// syntactically a literal and semantically the best sentence in the file.
fn strip_literals_keep_comments(lang: &str, line: &str) -> String {
    let trimmed = line.trim_start();
    let line_comment: &[&str] = match lang {
        "python" | "ruby" | "shell" | "perl" | "r" | "elixir" | "julia" => &["#"],
        "lua" | "haskell" => &["--"],
        _ => &["//"],
    };
    if line_comment.iter().any(|p| trimmed.starts_with(p))
        || trimmed.starts_with('*')
        || trimmed.starts_with("/*")
        || trimmed.contains(r#"""""#)
        || trimmed.contains("\u{27}\u{27}\u{27}")
    {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // A comment starts here: the rest of the line is prose, keep it whole.
        if line_comment.iter().any(|p| line[i..].starts_with(p)) || line[i..].starts_with("/*") {
            out.push_str(&line[i..]);
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            out.push(' ');
            while let Some((_, d)) = chars.next() {
                if d == '\\' {
                    chars.next(); // skip the escaped char
                    continue;
                }
                if d == c {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// BM25F field weights.
///
/// A chunk is not one flat bag of words: its path, the symbols it defines and
/// its doc comment each say what it is for far more directly than its body
/// does. BM25F's rule is to combine the per-field frequencies *before* the
/// saturation curve rather than to score each field and add — otherwise a term
/// appearing once in three fields saturates three times over and beats a term
/// that genuinely dominates one. So each field contributes `w` occurrences to
/// one shared count, and the document length is the same weighted sum.
///
/// The numbers are deliberately coarse. They were chosen by the gold set in
/// `tests/retrieval_gold.txt`, not by a paper, because no paper is about this
/// corpus; a finer split than 3/2/2/1 is unmeasurable at 35 queries.
const W_NAME: u16 = 3;
const W_PATH: u16 = 2;
const W_DOC: u16 = 2;
const W_BODY: u16 = 1;

/// One file, tokenised but not yet merged into the postings.
///
/// The split exists so the expensive half — parsing, chunking, tokenising, all
/// of it per-file and independent — can run on every core, while the cheap half
/// (assigning chunk ids and appending to shared postings) stays on one thread
/// where it needs no synchronisation at all.
pub struct Prepared {
    chunks: Vec<Chunk>,
    /// Per chunk: weighted term counts, the weighted length, and the structural
    /// terms (empty for a test file).
    scored: Vec<(HashMap<String, u16>, u32, Vec<String>)>,
}

/// Tokenise one file. Pure, so it is safe on a worker thread.
pub fn prepare_file(path: &str, lang: &'static str, text: &str) -> Prepared {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Prepared {
        chunks: Vec::new(),
        scored: Vec::new(),
    };
    for chunk in chunk_file(path, lang, text) {
        let body = chunk_text(lang, &lines, &chunk);
        let mut tf: HashMap<String, u16> = HashMap::new();
        let mut total = 0u32;
        let field = |text: &str, w: u16, tf: &mut HashMap<String, u16>, total: &mut u32| {
            for t in terms(Some(lang), text) {
                let e = tf.entry(t).or_insert(0);
                *e = e.saturating_add(w);
                *total += w as u32;
            }
        };
        field(&body, W_BODY, &mut tf, &mut total);
        // The doc comment and any prose inside the span, counted again. It is
        // the one part of a chunk written to be read as a sentence, and an
        // intent-shaped query is a sentence.
        field(&doc_lines(lang, &lines, &chunk), W_DOC, &mut tf, &mut total);
        // What it defines. `(module header)` is a label, not a symbol.
        let names = chunk
            .names
            .iter()
            .filter(|n| !n.starts_with('('))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        field(&names, W_NAME, &mut tf, &mut total);
        // Where it lives. A path names the subsystem — a question about one is
        // half-answered by the filename — and it costs four terms.
        field(&path_words(path), W_PATH, &mut tf, &mut total);
        if tf.is_empty() {
            continue;
        }
        // The structural posting list: definition names and path components
        // only, deduplicated per chunk. `rank_symbolic` says why this is worth a
        // second index rather than another field weight.
        //
        // Test files are left out of it. A test's name is a sentence —
        // `a_plain_file_mention_is_left_for_the_model_to_fetch` — not a symbol,
        // and measured here, admitting them filled this channel's top ranks with
        // assertions on unrelated queries.
        let mut structural = Vec::new();
        if !is_test(path) {
            structural = terms(None, &names);
            structural.extend(terms(None, &path_words(path)));
            structural.sort();
            structural.dedup();
        }
        out.chunks.push(chunk);
        out.scored.push((tf, total, structural));
    }
    out
}

/// Tokenise many files across the machine's cores.
///
/// The same shape as `graph::parse_in_parallel`, including its refusal to spawn
/// threads for a small job: on a single-core machine, or a repository of thirty
/// files, the threads cost more than they save, and a low-end device is exactly
/// where a bad guess about that hurts.
fn prepare_in_parallel(inputs: Vec<(String, &'static str, String)>) -> Vec<(String, Prepared)> {
    let workers = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(2)
        .min(8);
    prepare_with(inputs, workers)
}

/// `prepare_in_parallel` with the worker count fixed, so the benchmark can
/// measure what a single-core machine actually pays instead of extrapolating.
fn prepare_with(
    inputs: Vec<(String, &'static str, String)>,
    workers: usize,
) -> Vec<(String, Prepared)> {
    let n = inputs.len();
    if n < 32 || workers <= 1 {
        return inputs
            .into_iter()
            .map(|(rel, lang, text)| {
                let p = prepare_file(&rel, lang, &text);
                (rel, p)
            })
            .collect();
    }
    let per = n.div_ceil(workers);
    let mut parts: Vec<Vec<(String, &'static str, String)>> = Vec::new();
    let mut it = inputs.into_iter();
    for _ in 0..workers {
        let c: Vec<_> = it.by_ref().take(per).collect();
        if c.is_empty() {
            break;
        }
        parts.push(c);
    }
    let mut out = Vec::with_capacity(n);
    std::thread::scope(|s| {
        let handles: Vec<_> = parts
            .into_iter()
            .map(|part| {
                s.spawn(move || {
                    part.into_iter()
                        .map(|(rel, lang, text)| {
                            let p = prepare_file(&rel, lang, &text);
                            (rel, p)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            if let Ok(part) = h.join() {
                out.extend(part);
            }
        }
    });
    // Merge order decides chunk ids, and a stable index is worth more than the
    // microsecond: the cache, the vector rows and every test compare by id.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Index {
    /// Merge a prepared file into the postings. Cheap and serial by design; the
    /// work is in `prepare_file`.
    fn merge(&mut self, p: Prepared) {
        for (chunk, (tf, total, structural)) in p.chunks.into_iter().zip(p.scored) {
            let cid = self.chunks.len() as u32;
            for t in structural {
                self.symbols.entry(t).or_default().push(cid);
            }
            for (term, count) in tf {
                let next = self.vocab.len() as u32;
                let tid = *self.vocab.entry(term).or_insert(next);
                if self.post.len() <= tid as usize {
                    self.post.resize(tid as usize + 1, Vec::new());
                }
                self.post[tid as usize].push((cid, count));
            }
            self.len.push(total);
            self.chunks.push(chunk);
            if self.dead.len() < self.chunks.len() {
                self.dead.resize(self.chunks.len(), false);
            }
        }
    }

    /// Index one file. The incremental path; the build uses `merge` directly
    /// after tokenising every file in parallel.
    fn add_file(&mut self, path: &str, lang: &'static str, text: &str) {
        let p = prepare_file(path, lang, text);
        self.merge(p);
    }

    fn finish(&mut self) {
        let total: u64 = self.len.iter().map(|l| *l as u64).sum();
        self.avgdl = if self.len.is_empty() {
            1.0
        } else {
            total as f32 / self.len.len() as f32
        };
    }
}

/// A path as searchable words: its directories and file stem, without the
/// extension. The extension is dropped because it is the same for most of a
/// project — `rs` in 68 of koda's 68 indexed files — so it carries no signal and,
/// in the structural channel below, actively harms: a term every chunk shares
/// makes every chunk look like a match.
fn path_words(path: &str) -> String {
    let stem = path.rsplit_once('.').map(|(a, _)| a).unwrap_or(path);
    stem.replace(['/', '\\', '-'], " ")
}

/// The prose inside a chunk: its doc comment and any comment line in the body.
fn doc_lines(lang: &str, lines: &[&str], c: &Chunk) -> String {
    let markers: &[&str] = match lang {
        "python" | "ruby" | "shell" | "perl" | "r" | "elixir" | "julia" => &["#"],
        "lua" | "haskell" => &["--"],
        _ => &["//", "/*", "*", "///", "//!"],
    };
    lines
        .iter()
        .skip(c.start.saturating_sub(1))
        .take(c.end + 1 - c.start)
        .filter(|l| {
            let t = l.trim_start();
            markers.iter().any(|m| t.starts_with(m))
        })
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text an embedding model sees for a chunk: the file, what it defines, and
/// the span itself. The path and symbol names carry real signal for an
/// intent-shaped question and cost almost nothing.
pub fn embed_text(root: &Path, c: &Chunk) -> Option<String> {
    let text = std::fs::read_to_string(root.join(&c.path)).ok()?;
    let body: Vec<&str> = text
        .lines()
        .skip(c.start.saturating_sub(1))
        .take(c.end + 1 - c.start)
        .collect();
    if body.is_empty() {
        return None;
    }
    Some(format!(
        "{} — {}\n{}",
        c.path,
        c.names.join(", "),
        body.join("\n")
    ))
}

/// Chunks per embedding request. Small enough that one failure loses little,
/// large enough that a thousand chunks is tens of round trips rather than a
/// thousand.
pub const EMBED_BATCH: usize = 32;

/// Attach a completed set of vectors, if it matches the corpus.
///
/// The dimension is read from the model's own answer rather than configured,
/// and a set that does not cover every chunk is refused outright: a partially
/// embedded corpus would rank the embedded half above the rest for reasons
/// that have nothing to do with the query.
pub fn attach_vectors(idx: &mut Index, model: &str, rows: Vec<Vec<f32>>) -> Result<(), String> {
    if rows.len() != idx.chunks.len() {
        return Err(format!(
            "embedded {} of {} chunks",
            rows.len(),
            idx.chunks.len()
        ));
    }
    let Some(dim) = rows.first().map(|r| r.len()).filter(|d| *d > 0) else {
        return Err("the embedding model returned no dimensions".into());
    };
    if rows.iter().any(|r| r.len() != dim) {
        return Err("the embedding model returned rows of different lengths".into());
    }
    let mut v = Vectors {
        dim,
        data: Vec::with_capacity(rows.len() * dim),
        rows_of: Vec::with_capacity(rows.len()),
        model: model.to_string(),
    };
    for (cid, row) in rows.iter().enumerate() {
        v.push(cid as u32, row);
    }
    idx.vectors = Some(v);
    Ok(())
}

/// The one traversal policy, shared by the build, the refresh sweep and the
/// cache validation, so those three can never disagree about what is in the
/// corpus.
fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .filter_entry(|e| !graph::is_vendor_dir(&e.file_name().to_string_lossy()))
        .build()
}

/// A path relative to the workspace root, with separators normalised to `/`.
///
/// Normalising is not cosmetic. `is_test` looks for `tests/` and `/tests/`, so
/// on Windows — where the walk yields `tests\probe.rs` — every test file in the
/// project silently stopped being recognised as one and kept its full rank. The
/// cache is portable for the same reason: a repository on a synced folder is
/// read by whichever machine opens it.
fn relative(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).unwrap_or(abs).to_string_lossy();
    (!rel.is_empty()).then(|| normalise(&rel))
}

fn normalise(path: &str) -> String {
    path.replace('\\', "/")
}

/// Build the index by walking the workspace, under the same limits and the same
/// ignore rules the graph uses — one traversal policy, not two.
pub fn build(root: &Path) -> Index {
    let mut idx = Index::default();
    // Read first, tokenise second. The walk is I/O bound and the tokeniser is
    // CPU bound, so they are separated: one pass collects the bytes, then every
    // core works on them at once. Reading is left serial on purpose — a spinning
    // disk and a small SSD both do worse with eight threads seeking at once, and
    // the machines that would gain are the ones already fast enough.
    let mut inputs: Vec<(String, &'static str, String)> = Vec::new();
    for entry in walker(root).flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(lang) = graph::language_of(entry.path()) else {
            continue;
        };
        if idx.files >= graph::MAX_FILES {
            break;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.len() > graph::MAX_FILE_BYTES || bytes.iter().take(4000).any(|b| *b == 0) {
            continue;
        }
        let Some(rel) = relative(root, entry.path()) else {
            continue;
        };
        if let Ok(meta) = entry.metadata() {
            idx.stamps.insert(rel.clone(), stamp_of(&meta));
        }
        inputs.push((rel, lang, String::from_utf8_lossy(&bytes).into_owned()));
        idx.files += 1;
    }
    for (_, prepared) in prepare_in_parallel(inputs) {
        idx.merge(prepared);
    }
    idx.finish();
    idx
}

// -------------------------------------------------------------- persistence

/// Where a project's index cache lives, next to the other `.koda/` residents.
///
/// The whole directory is a cache and nothing else: deleting it must cost a
/// rebuild and nothing more. Nothing in here is authoritative — chunk *bodies*
/// are deliberately not stored, so a hit always shows the file as it is now
/// rather than as it was when indexed.
pub fn cache_dir(root: &Path) -> std::path::PathBuf {
    crate::config::project_state_dir(root, "index")
}

/// Bumped whenever the on-disk layout, the tokeniser, or the ranking's inputs
/// change. A cache written by a different version is deleted, not migrated:
/// migration code for a cache is a liability with no upside, and a rebuild is
/// under a second.
const CACHE_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"KODAIDX\x00";

/// What the manifest has to agree with before the binary half is even opened.
///
/// Kept as JSON, and small, so validating a cache costs one short read: on a
/// slow disk that is the difference between "the index is ready" and "the index
/// is ready in a moment", which is the entire point of persisting it.
#[derive(Serialize, Deserialize)]
struct Meta {
    version: u32,
    /// Per-file (mtime, size), the same identity `graph::Stamp` uses. What lets
    /// a load decide, without reading a single source file, whether the cache
    /// still describes this working tree.
    files: BTreeMap<String, (u64, u64)>,
    chunks: usize,
    /// The embedding model the vectors came from, if any. A different model
    /// invalidates the vectors and nothing else.
    embed_model: String,
    embed_dim: usize,
}

/// Take a file's (mtime, size) identity.
fn stamp_of(meta: &std::fs::Metadata) -> (u64, u64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (mtime, meta.len())
}

/// A little-endian writer. Hand-rolled rather than reached for from a crate:
/// the format is ours, it is a cache, and the alternative is a dependency whose
/// own format version becomes a thing to track.
struct Out(Vec<u8>);

impl Out {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        let b = s.as_bytes();
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
    }
}

/// The reader half. Every read is bounds-checked and returns `None` rather than
/// panicking: this file can be truncated by a full disk, a killed process, or a
/// synced folder resolving a conflict, and none of those may take the session
/// down. A `None` anywhere means "rebuild", which is always correct.
struct In<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        // A length field is the one place a corrupt file can ask for a huge
        // allocation, so it is checked against what is actually left.
        if n > self.b.len() - self.at {
            return None;
        }
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    /// A count that is about to size a `Vec`. Same reasoning as `str`, with a
    /// per-element floor so the bound is not merely "fits in the file".
    fn count(&mut self, min_bytes_each: usize) -> Option<usize> {
        let n = self.u32()? as usize;
        (n.saturating_mul(min_bytes_each.max(1)) <= self.b.len() - self.at).then_some(n)
    }
}

impl Index {
    /// Write the index to `<root>/.koda/index/`.
    ///
    /// Both files are written to a temporary name and renamed into place, so a
    /// crash mid-write leaves the previous cache intact rather than a truncated
    /// one. The manifest is renamed *last*: it is what a load validates against,
    /// so until it lands the new binary is simply an orphan the next save
    /// overwrites.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = cache_dir(root);
        std::fs::create_dir_all(&dir)?;

        let mut o = Out(Vec::with_capacity(1 << 20));
        o.0.extend_from_slice(MAGIC);
        o.u32(CACHE_VERSION);

        // Paths are interned: 1,685 chunks over 68 files here, so storing the
        // path per chunk would be 25 copies of every string.
        let mut paths: Vec<&str> = Vec::new();
        let mut path_id: HashMap<&str, u32> = HashMap::new();
        for c in &self.chunks {
            if !path_id.contains_key(c.path.as_str()) {
                path_id.insert(c.path.as_str(), paths.len() as u32);
                paths.push(c.path.as_str());
            }
        }
        o.u32(paths.len() as u32);
        for p in &paths {
            o.str(p);
        }

        o.u32(self.chunks.len() as u32);
        for c in &self.chunks {
            o.u32(*path_id.get(c.path.as_str()).unwrap_or(&0));
            o.u32(c.start as u32);
            o.u32(c.end as u32);
            o.u32(c.names.len() as u32);
            for n in &c.names {
                o.str(n);
            }
        }

        o.u32(self.len.len() as u32);
        for l in &self.len {
            o.u32(*l);
        }
        o.f32(self.avgdl);
        o.u32(self.files as u32);

        // The postings, term by term. `vocab` maps term -> id and `post` is
        // indexed by that id, so they are written together and the ids are
        // implicit in the order.
        let mut vocab: Vec<(&str, u32)> =
            self.vocab.iter().map(|(t, i)| (t.as_str(), *i)).collect();
        vocab.sort_by_key(|(_, i)| *i);
        o.u32(vocab.len() as u32);
        for (term, id) in &vocab {
            o.str(term);
            let empty = Vec::new();
            let post = self.post.get(*id as usize).unwrap_or(&empty);
            o.u32(post.len() as u32);
            for (cid, tf) in post {
                o.u32(*cid);
                o.u16(*tf);
            }
        }

        o.u32(self.symbols.len() as u32);
        for (term, cids) in &self.symbols {
            o.str(term);
            o.u32(cids.len() as u32);
            for c in cids {
                o.u32(*c);
            }
        }

        match &self.vectors {
            Some(v) if v.dim > 0 => {
                o.u32(1);
                o.u32(v.dim as u32);
                o.u32(v.rows_of.len() as u32);
                for c in &v.rows_of {
                    o.u32(*c);
                }
                o.u32(v.data.len() as u32);
                for h in &v.data {
                    o.u16(*h);
                }
            }
            _ => o.u32(0),
        }

        write_atomic(&dir.join("index.bin"), &o.0)?;

        let meta = Meta {
            version: CACHE_VERSION,
            files: self.stamps.clone(),
            chunks: self.chunks.len(),
            embed_model: self
                .vectors
                .as_ref()
                .map(|v| v.model.clone())
                .unwrap_or_default(),
            embed_dim: self.vectors.as_ref().map(|v| v.dim).unwrap_or(0),
        };
        let json = serde_json::to_vec(&meta).unwrap_or_default();
        write_atomic(&dir.join("meta.json"), &json)
    }

    /// Load the cache, or `None` if there is nothing usable there.
    ///
    /// Usable means: the manifest parses, its version matches, and every file it
    /// claims still has the same (mtime, size). A cache that describes a
    /// different tree is discarded rather than repaired here — `refresh` does
    /// the repairing, and it needs a loaded index to do it to.
    pub fn load(root: &Path) -> Option<Index> {
        let dir = cache_dir(root);
        let meta: Meta =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        if meta.version != CACHE_VERSION {
            return None;
        }
        let bytes = std::fs::read(dir.join("index.bin")).ok()?;
        let mut i = In { b: &bytes, at: 0 };
        if i.take(8)? != MAGIC || i.u32()? != CACHE_VERSION {
            return None;
        }

        let n_paths = i.count(4)?;
        let mut paths = Vec::with_capacity(n_paths);
        for _ in 0..n_paths {
            paths.push(i.str()?);
        }

        let n_chunks = i.count(16)?;
        let mut chunks = Vec::with_capacity(n_chunks);
        for _ in 0..n_chunks {
            let pid = i.u32()? as usize;
            let start = i.u32()? as usize;
            let end = i.u32()? as usize;
            let n_names = i.count(4)?;
            let mut names = Vec::with_capacity(n_names);
            for _ in 0..n_names {
                names.push(i.str()?);
            }
            chunks.push(Chunk {
                path: paths.get(pid)?.clone(),
                start,
                end,
                names,
            });
        }

        let n_len = i.count(4)?;
        let mut len = Vec::with_capacity(n_len);
        for _ in 0..n_len {
            len.push(i.u32()?);
        }
        let avgdl = i.f32()?;
        let files = i.u32()? as usize;

        let n_vocab = i.count(8)?;
        let mut vocab = HashMap::with_capacity(n_vocab);
        let mut post = Vec::with_capacity(n_vocab);
        for id in 0..n_vocab {
            let term = i.str()?;
            let n_post = i.count(6)?;
            let mut list = Vec::with_capacity(n_post);
            for _ in 0..n_post {
                list.push((i.u32()?, i.u16()?));
            }
            vocab.insert(term, id as u32);
            post.push(list);
        }

        let n_sym = i.count(8)?;
        let mut symbols = HashMap::with_capacity(n_sym);
        for _ in 0..n_sym {
            let term = i.str()?;
            let n = i.count(4)?;
            let mut cids = Vec::with_capacity(n);
            for _ in 0..n {
                cids.push(i.u32()?);
            }
            symbols.insert(term, cids);
        }

        let vectors = if i.u32()? == 1 {
            let dim = i.u32()? as usize;
            let n_rows = i.count(4)?;
            let mut rows_of = Vec::with_capacity(n_rows);
            for _ in 0..n_rows {
                rows_of.push(i.u32()?);
            }
            let n_data = i.count(2)?;
            let mut data = Vec::with_capacity(n_data);
            for _ in 0..n_data {
                data.push(i.u16()?);
            }
            // A vector store that does not describe itself consistently is
            // dropped, and only it: the lexical half never depended on it.
            (dim > 0 && n_data == n_rows * dim).then_some(Vectors {
                dim,
                data,
                rows_of,
                model: meta.embed_model.clone(),
            })
        } else {
            None
        };

        // Cross-checks. Every one of these is a corrupt-file signature rather
        // than a state the code can produce, so any of them means rebuild.
        if len.len() != chunks.len() || meta.chunks != chunks.len() {
            return None;
        }
        if post
            .iter()
            .flatten()
            .any(|(c, _)| *c as usize >= chunks.len())
        {
            return None;
        }
        if symbols
            .values()
            .flatten()
            .any(|c| *c as usize >= chunks.len())
        {
            return None;
        }

        Some(Index {
            chunks,
            vectors,
            vocab,
            post,
            len,
            avgdl,
            files,
            symbols,
            stamps: meta.files,
            dead: Vec::new(),
        })
    }
}

/// Write bytes so that a reader sees either the old file or the new one.
///
/// The temporary name carries the process id so two koda sessions in one
/// working tree cannot write over each other's half-written file. The rename is
/// retried a few times because on Windows a replace fails outright while
/// another process — an indexer, a virus scanner, the other koda — has the
/// destination open, and that window is milliseconds.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    let mut last = None;
    for attempt in 0..5 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

// -------------------------------------------------------- incremental update

/// The share of dead chunks past which the index is rebuilt rather than swept
/// again. Tombstones cost a branch per posting; a rebuild costs the whole file
/// walk. A fifth is roughly where the second becomes cheaper than living with
/// the first, and it is reached only by a session that rewrites a lot of the
/// tree — which is exactly the session that can afford one rebuild.
const COMPACT_AT: f32 = 0.20;

impl Index {
    /// Whether this chunk is still part of the corpus.
    fn live(&self, cid: usize) -> bool {
        !self.dead.get(cid).copied().unwrap_or(false)
    }

    /// Retire every chunk belonging to `rel`.
    ///
    /// Tombstones rather than surgery: removing a chunk id from the middle of
    /// the postings would renumber every id after it, and the vector rows and
    /// the structural index are keyed by those ids too. Marking is O(chunks of
    /// one file); renumbering is O(corpus).
    pub fn remove_file(&mut self, rel: &str) {
        let rel = normalise(rel);
        if self.dead.len() < self.chunks.len() {
            self.dead.resize(self.chunks.len(), false);
        }
        for (cid, c) in self.chunks.iter().enumerate() {
            if c.path == rel {
                self.dead[cid] = true;
            }
        }
        self.stamps.remove(&rel);
    }

    /// Re-index one file: retire what it had, add what it has now.
    ///
    /// This is the hook koda's own writes ride. A `write_file` that the model
    /// just made must be findable by the search it runs next; an index that is
    /// only correct at startup is worse than no index, because the agent
    /// believes it.
    pub fn update_file(&mut self, root: &Path, abs: &Path) {
        let Some(rel) = relative(root, abs) else {
            return;
        };
        self.remove_file(&rel);
        let Some(lang) = graph::language_of(abs) else {
            return;
        };
        let Ok(bytes) = std::fs::read(abs) else {
            return;
        };
        if bytes.len() > graph::MAX_FILE_BYTES || bytes.iter().take(4000).any(|b| *b == 0) {
            return;
        }
        if let Ok(meta) = std::fs::metadata(abs) {
            self.stamps.insert(rel.clone(), stamp_of(&meta));
        }
        self.add_file(&rel, lang, &String::from_utf8_lossy(&bytes));
        self.finish();
    }

    /// Sweep the tree for edits made outside koda and re-index what changed.
    ///
    /// The same shape as `graph::refresh`, and for the same reason: an editor,
    /// a `git checkout` or a code generator changes files constantly, and a
    /// search index that quietly describes the tree as it was will send the
    /// agent to a line number that has moved. Returns how many files changed,
    /// so the caller can back off on a tree big enough that the walk itself is
    /// the cost.
    pub fn refresh(&mut self, root: &Path) -> usize {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut changed = 0usize;
        for entry in walker(root).flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                || graph::language_of(entry.path()).is_none()
            {
                continue;
            }
            let Some(rel) = relative(root, entry.path()) else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            let now = stamp_of(&meta);
            seen.insert(rel.clone());
            if self.stamps.get(&rel) == Some(&now) {
                continue;
            }
            self.update_file(root, entry.path());
            changed += 1;
        }
        // Files that have gone. Collected first because removing mutates the
        // map being walked.
        let gone: Vec<String> = self
            .stamps
            .keys()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        for rel in gone {
            self.remove_file(&rel);
            changed += 1;
        }
        if changed > 0 {
            self.compact_if_needed();
        }
        changed
    }

    /// Rebuild the postings without the tombstoned chunks, once there are
    /// enough of them to be worth the pass.
    fn compact_if_needed(&mut self) {
        let dead = self.dead.iter().filter(|d| **d).count();
        if self.chunks.is_empty() || (dead as f32) < self.chunks.len() as f32 * COMPACT_AT {
            return;
        }
        // Old id -> new id, with the dead left out.
        let mut next = 0u32;
        let remap: Vec<u32> = (0..self.chunks.len())
            .map(|cid| {
                if !self.live(cid) {
                    return u32::MAX;
                }
                let id = next;
                next += 1;
                id
            })
            .collect();
        let keep = |cid: &u32| remap.get(*cid as usize).copied().unwrap_or(u32::MAX);
        for list in &mut self.post {
            list.retain(|(c, _)| keep(c) != u32::MAX);
            for (c, _) in list.iter_mut() {
                *c = keep(c);
            }
        }
        for list in self.symbols.values_mut() {
            list.retain(|c| keep(c) != u32::MAX);
            for c in list.iter_mut() {
                *c = keep(c);
            }
        }
        self.symbols.retain(|_, v| !v.is_empty());
        if let Some(v) = self.vectors.as_mut() {
            v.retain(|cid| keep(&cid));
        }
        let mut i = 0usize;
        self.chunks.retain(|_| {
            let live = remap[i] != u32::MAX;
            i += 1;
            live
        });
        let mut i = 0usize;
        self.len.retain(|_| {
            let live = remap[i] != u32::MAX;
            i += 1;
            live
        });
        self.dead.clear();
        self.finish();
    }

    /// Chunks with no embedding yet, so a background fill can top up rather
    /// than start over. Empty when there are no vectors at all: with nothing to
    /// extend, the fill is a first fill and takes the other path.
    pub fn unembedded(&self) -> Vec<usize> {
        let Some(v) = self.vectors.as_ref() else {
            return Vec::new();
        };
        let have: std::collections::HashSet<u32> = v.rows_of.iter().copied().collect();
        (0..self.chunks.len())
            .filter(|c| self.live(*c) && !have.contains(&(*c as u32)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch workspace, removed on drop.
    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "koda-idx-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("src")).expect("scratch dir");
            Self(dir)
        }
        fn write(&self, rel: &str, text: &str) {
            let p = self.0.join(rel);
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(p, text).expect("write");
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The cache has to come back as the same index, ranking included — that is
    /// the only property that matters, and it is stronger than field equality.
    #[test]
    fn a_saved_index_reloads_and_ranks_identically() {
        let t = Tmp::new("save");
        t.write(
            "src/retry.rs",
            "/// Retry a failing request with backoff.\npub fn stream_with_retry() {}\n",
        );
        t.write(
            "src/theme.rs",
            "/// Colours for the terminal.\npub fn palette() {}\n",
        );
        let built = build(&t.0);
        assert!(built.chunks.len() >= 2, "{}", built.chunks.len());
        built.save(&t.0).expect("save");
        assert!(cache_dir(&t.0).join("index.bin").is_file());

        let loaded = Index::load(&t.0).expect("load");
        assert_eq!(loaded.chunks.len(), built.chunks.len());
        assert_eq!(loaded.files, built.files);
        for q in ["retry backoff", "terminal colours", "palette"] {
            let a: Vec<_> = built
                .hybrid_search(q, None, 5)
                .iter()
                .map(|h| built.chunks[h.chunk].path.clone())
                .collect();
            let b: Vec<_> = loaded
                .hybrid_search(q, None, 5)
                .iter()
                .map(|h| loaded.chunks[h.chunk].path.clone())
                .collect();
            assert_eq!(a, b, "ranking for {q:?} changed across a save/load");
        }
        // And it agrees the tree has not moved: a sweep finds nothing to do.
        let mut loaded = loaded;
        assert_eq!(loaded.refresh(&t.0), 0);
    }

    /// A cache that describes a different tree must be rejected, not used. This
    /// is the failure that would be invisible: stale line numbers look exactly
    /// like fresh ones.
    #[test]
    fn a_cache_is_rejected_once_the_tree_moves_on() {
        let t = Tmp::new("stale");
        t.write("src/a.rs", "pub fn alpha() {}\n");
        let idx = build(&t.0);
        idx.save(&t.0).expect("save");
        assert_eq!(Index::load(&t.0).expect("load").refresh(&t.0), 0);

        // A new file the cache has never seen is picked up by the sweep.
        t.write("src/b.rs", "pub fn beta() {}\n");
        let mut reloaded = Index::load(&t.0).expect("load");
        assert_eq!(reloaded.refresh(&t.0), 1);
        assert!(!reloaded.hybrid_search("beta", None, 5).is_empty());

        // A wrong version is refused outright rather than parsed.
        std::fs::write(
            cache_dir(&t.0).join("meta.json"),
            r#"{"version":999,"files":{},"chunks":0,"embed_model":"","embed_dim":0}"#,
        )
        .expect("write meta");
        assert!(Index::load(&t.0).is_none());
    }

    /// Truncation is the realistic corruption — a full disk, a killed process,
    /// a synced folder. Every one of them must cost a rebuild and nothing else.
    #[test]
    fn a_truncated_cache_is_refused_rather_than_trusted() {
        let t = Tmp::new("trunc");
        t.write(
            "src/a.rs",
            "/// Doc.\npub fn alpha() {}\npub fn beta() {}\n",
        );
        build(&t.0).save(&t.0).expect("save");
        let path = cache_dir(&t.0).join("index.bin");
        let full = std::fs::read(&path).expect("read");
        for cut in [0, 4, 12, full.len() / 3, full.len() / 2, full.len() - 1] {
            std::fs::write(&path, &full[..cut]).expect("truncate");
            assert!(Index::load(&t.0).is_none(), "accepted a {cut}-byte cache");
        }
        // Garbage of the right length is refused too: the magic guards it.
        std::fs::write(&path, vec![0xABu8; full.len()]).expect("garbage");
        assert!(Index::load(&t.0).is_none());
    }

    /// The hook koda's own writes ride: what the agent just wrote has to be
    /// findable by the search it runs next.
    #[test]
    fn an_edited_file_is_searchable_immediately() {
        let t = Tmp::new("update");
        t.write("src/a.rs", "/// Nothing to see.\npub fn alpha() {}\n");
        let mut idx = build(&t.0);
        assert!(idx.hybrid_search("quantum tunnelling", None, 5).is_empty());

        t.write(
            "src/a.rs",
            "/// Quantum tunnelling through the barrier.\npub fn tunnel() {}\n",
        );
        idx.update_file(&t.0, &t.0.join("src/a.rs"));
        let hits = idx.hybrid_search("quantum tunnelling", None, 5);
        assert!(!hits.is_empty(), "the new text is not indexed");
        assert_eq!(idx.chunks[hits[0].chunk].path, "src/a.rs");
        // The old chunk is gone rather than shadowed.
        assert!(idx.hybrid_search("nothing to see", None, 5).is_empty());
    }

    /// A deleted file must stop being an answer, and a sweep has to notice an
    /// edit koda did not make.
    #[test]
    fn refresh_catches_outside_edits_and_deletions() {
        let t = Tmp::new("refresh");
        t.write("src/keep.rs", "/// Kept.\npub fn keep() {}\n");
        t.write(
            "src/gone.rs",
            "/// Ephemeral marker text.\npub fn gone() {}\n",
        );
        let mut idx = build(&t.0);
        assert!(!idx.hybrid_search("ephemeral marker", None, 5).is_empty());

        std::fs::remove_file(t.0.join("src/gone.rs")).expect("remove");
        t.write(
            "src/outside.rs",
            "/// Written by somebody else entirely.\npub fn outside() {}\n",
        );
        assert!(idx.refresh(&t.0) >= 2, "the sweep missed a change");
        assert!(idx.hybrid_search("ephemeral marker", None, 5).is_empty());
        assert!(!idx
            .hybrid_search("written by somebody else", None, 5)
            .is_empty());
        // A second sweep with nothing changed must be a no-op.
        assert_eq!(idx.refresh(&t.0), 0);
    }

    /// Tombstones accumulate; past a threshold the index is rebuilt in place.
    /// The observable property is that it stays correct across the transition.
    #[test]
    fn compaction_preserves_the_corpus() {
        let t = Tmp::new("compact");
        // A distinctive token per file, so "did this chunk survive" is a
        // question with one answer rather than a shared-vocabulary guess.
        for i in 0..12 {
            t.write(
                &format!("src/f{i}.rs"),
                &format!("/// Module holding marker rhinoceros{i}.\npub fn f{i}() {{}}\n"),
            );
        }
        let mut idx = build(&t.0);
        let before = idx.chunks.len();
        for i in 0..6 {
            t.write(
                &format!("src/f{i}.rs"),
                &format!("/// Rewritten module {i} entirely.\npub fn g{i}() {{}}\n"),
            );
            idx.update_file(&t.0, &t.0.join(format!("src/f{i}.rs")));
        }
        idx.compact_if_needed();
        assert!(
            idx.dead.iter().all(|d| !d),
            "tombstones survived compaction"
        );
        assert_eq!(idx.chunks.len(), before, "chunk count changed");
        // Every posting still points at a real chunk, and search still works.
        assert!(idx
            .post
            .iter()
            .flatten()
            .all(|(c, _)| (*c as usize) < idx.chunks.len()));
        assert!(!idx
            .hybrid_search("rewritten module entirely", None, 5)
            .is_empty());
        // The rewritten files lost their old markers; the untouched ones kept
        // theirs, which is what says compaction renumbered rather than dropped.
        assert!(idx.hybrid_search("rhinoceros3", None, 5).is_empty());
        assert!(!idx.hybrid_search("rhinoceros9", None, 5).is_empty());
    }

    /// Windows hands the walk `src\\a.rs`. Everything downstream — the test
    /// demotion, the path field, the cache read on another machine — keys on
    /// `/`, so the separator is normalised once, at the edge.
    #[test]
    fn paths_are_normalised_so_windows_behaves_like_the_others() {
        assert_eq!(normalise("src\\index.rs"), "src/index.rs");
        assert_eq!(normalise("tests\\unit\\a.py"), "tests/unit/a.py");
        assert_eq!(normalise("src/index.rs"), "src/index.rs");
        // The consequence that actually mattered: test demotion works either way.
        assert!(is_test(&normalise("tests\\probe.py")));
        assert!(is_test(&normalise("src\\foo\\test_thing.rs")));
    }

    /// Vectors survive an edit now: only the chunks that changed lose theirs.
    /// Re-embedding a corpus costs minutes on a machine without a GPU, so "one
    /// file changed" must not mean "embed everything again".
    #[test]
    fn an_edit_costs_only_the_edited_chunks_their_embeddings() {
        let t = Tmp::new("vec");
        t.write("src/a.rs", "/// Alpha.\npub fn alpha() {}\n");
        t.write("src/b.rs", "/// Beta.\npub fn beta() {}\n");
        let mut idx = build(&t.0);
        let n = idx.chunks.len();
        let rows: Vec<Vec<f32>> = (0..n).map(|i| vec![i as f32 + 1.0, 1.0]).collect();
        attach_vectors(&mut idx, "m", rows).expect("attached");
        assert_eq!(idx.unembedded(), Vec::<usize>::new());

        t.write("src/a.rs", "/// Alpha, revised.\npub fn alpha() {}\n");
        idx.update_file(&t.0, &t.0.join("src/a.rs"));
        // The untouched file keeps its embeddings; only the new chunks want one.
        let want = idx.unembedded();
        assert!(!want.is_empty(), "new chunks should want embedding");
        assert!(
            idx.vectors.as_ref().expect("vectors").rows() >= n,
            "surviving rows were thrown away"
        );
        // And a top-up fills exactly those, leaving nothing outstanding.
        let dim = idx.vectors.as_ref().expect("vectors").dim;
        for cid in want {
            idx.vectors
                .as_mut()
                .expect("vectors")
                .upsert(cid as u32, &vec![0.5; dim]);
        }
        assert_eq!(idx.unembedded(), Vec::<usize>::new());
    }

    /// The query the whole module exists for, against koda's own source. It
    /// names no symbol, so the graph cannot answer it.
    #[test]
    fn an_intent_shaped_query_finds_the_code_that_serves_it() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let idx = build(root);
        assert!(
            idx.chunks.len() > 200,
            "indexed {} chunks",
            idx.chunks.len()
        );

        let hits = idx.hybrid_search("where is retry handled", None, 8);
        assert!(!hits.is_empty(), "no hits at all");
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| idx.chunks[h.chunk].path.as_str())
            .collect();
        assert!(
            paths.iter().any(|p| p.ends_with("llm.rs")),
            "retry lives in llm.rs; got {paths:?}"
        );

        // And an exact symbol still wins on its own name, so the fuzzy path
        // does not cost precision.
        // An exact symbol still finds its definition, so the fuzzy path costs
        // no precision. (Top-3 rather than top-1: this very test file mentions
        // the name, and BM25 has no way to know a test is not the answer.)
        let hits = idx.hybrid_search("stream_with_retry", None, 3);
        let paths: Vec<&str> = hits
            .iter()
            .map(|h| idx.chunks[h.chunk].path.as_str())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("llm.rs")), "{paths:?}");
    }

    /// Recall@10 and MRR@10 over `tests/retrieval_gold.txt`.
    ///
    /// The point of §7 of `docs/research-hybrid-retrieval.md`: four of the
    /// constants in this module are judgement calls, and reading more papers
    /// cannot settle them because the corpus is one specific repository. A
    /// labelled query set can. The floors below are set just under the measured
    /// score, so a change that makes retrieval worse fails the build instead of
    /// being discovered months later by a user who cannot describe it.
    ///
    /// Run `cargo test retrieval_quality -- --nocapture` to see the per-query
    /// breakdown, including which queries currently miss.
    #[test]
    fn retrieval_quality_holds() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let gold = gold_queries(root);
        assert!(gold.len() >= 30, "gold set shrank to {}", gold.len());
        let idx = build(root);
        let s = score_gold(&idx, &gold);
        eprintln!(
            "\nretrieval over {} queries:  P@1 {:.3}  R@3 {:.3}  R@10 {:.3}  MRR@10 {:.3}",
            gold.len(),
            s.p1,
            s.r3,
            s.r10,
            s.mrr
        );
        for (q, got) in &s.misses {
            eprintln!("  MISS  {q:?}\n        got {got:?}");
        }
        // Floors with a few points of headroom under the measured scores. The
        // headroom is not slack: the corpus *is* this repository, so editing it
        // moves the numbers a little, and a floor set flush against the current
        // run would fail on an unrelated commit. They are still far above the
        // pre-hybrid measurements (P@1 0.429, MRR 0.617), which is what they
        // exist to catch.
        assert!(s.r10 >= 0.94, "Recall@10 fell to {:.3}", s.r10);
        assert!(s.r3 >= 0.71, "Recall@3 fell to {:.3}", s.r3);
        assert!(s.p1 >= 0.54, "P@1 fell to {:.3}", s.p1);
        assert!(s.mrr >= 0.67, "MRR@10 fell to {:.3}", s.mrr);
    }

    /// The labelled set. Kept as a data file rather than a table in here so it
    /// can be extended without touching code.
    fn gold_queries(root: &Path) -> Vec<(String, Vec<String>)> {
        let text = std::fs::read_to_string(root.join("tests/retrieval_gold.txt"))
            .expect("tests/retrieval_gold.txt");
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .map(|(q, paths)| {
                (
                    q.trim().to_string(),
                    paths.split(',').map(|p| p.trim().to_string()).collect(),
                )
            })
            .collect()
    }

    /// What one run of the gold set scored.
    struct Scores {
        /// The right file was the first hit.
        p1: f32,
        r3: f32,
        r10: f32,
        mrr: f32,
        misses: Vec<(String, Vec<String>)>,
    }

    fn score_gold(idx: &Index, gold: &[(String, Vec<String>)]) -> Scores {
        let (mut p1, mut r3, mut r10, mut mrr) = (0f32, 0f32, 0f32, 0f32);
        let mut misses = Vec::new();
        for (q, want) in gold {
            let paths: Vec<String> = idx
                .hybrid_search(q, None, 10)
                .iter()
                .map(|h| idx.chunks[h.chunk].path.clone())
                .collect();
            match paths
                .iter()
                .position(|p| want.iter().any(|w| p.ends_with(w)))
            {
                Some(i) => {
                    r10 += 1.0;
                    r3 += (i < 3) as u8 as f32;
                    p1 += (i == 0) as u8 as f32;
                    mrr += 1.0 / (i as f32 + 1.0);
                }
                None => misses.push((q.clone(), paths)),
            }
        }
        let n = gold.len().max(1) as f32;
        Scores {
            p1: p1 / n,
            r3: r3 / n,
            r10: r10 / n,
            mrr: mrr / n,
            misses,
        }
    }

    /// Sweeps the structural channel's fusion weight over the gold set.
    ///
    /// The only honest way to pick `W_SYMBOLIC`. Run with
    /// `cargo test --release sweep_structural_weight -- --ignored --nocapture`.
    #[test]
    #[ignore = "diagnostic, not an assertion"]
    fn sweep_structural_weight() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let gold = gold_queries(root);
        let idx = build(root);
        eprintln!("\n  w_sym  depth   cov    P@1    R@3   R@10   MRR@10");
        for (w, d, cov) in [0.0f32, 0.7, 1.0, 1.5]
            .into_iter()
            .flat_map(|w| [5usize, 8, 20].into_iter().map(move |d| (w, d)))
            .flat_map(|(w, d)| [0.34f32, 0.5, 0.7, 1.0].into_iter().map(move |c| (w, d, c)))
        {
            let (mut p1, mut r3, mut r10, mut mrr) = (0f32, 0f32, 0f32, 0f32);
            for (q, want) in &gold {
                let paths: Vec<String> = idx
                    .fuse(q, None, 10, w, d, cov)
                    .iter()
                    .map(|h| idx.chunks[h.chunk].path.clone())
                    .collect();
                if let Some(i) = paths
                    .iter()
                    .position(|p| want.iter().any(|x| p.ends_with(x)))
                {
                    r10 += 1.0;
                    r3 += (i < 3) as u8 as f32;
                    p1 += (i == 0) as u8 as f32;
                    mrr += 1.0 / (i as f32 + 1.0);
                }
            }
            let n = gold.len() as f32;
            eprintln!(
                "  {w:5.1}  {d:5}  {cov:.2}  {:.3}  {:.3}  {:.3}   {:.3}",
                p1 / n,
                r3 / n,
                r10 / n,
                mrr / n
            );
        }
    }

    /// Prints each retrieval channel separately for a few gold queries, so a
    /// fusion result that looks wrong can be traced to the channel that caused
    /// it rather than to the fusion.
    #[test]
    #[ignore = "diagnostic, not an assertion"]
    fn show_channels() {
        let idx = build(Path::new(env!("CARGO_MANIFEST_DIR")));
        for q in [
            "how is markdown rendered to the terminal",
            "where do we write the session file",
            "discovering skills on disk",
            "how is the context window trimmed",
        ] {
            eprintln!("\n  {q:?}");
            for (label, hits) in [
                ("lexical  ", idx.rank_lexical(q, 6)),
                ("symbolic ", idx.rank_symbolic(q, 6, SYMBOLIC_COVERAGE)),
            ] {
                for (r, h) in hits.iter().enumerate() {
                    let c = &idx.chunks[h.chunk];
                    eprintln!(
                        "    {label} {r}. {:6.2}  {}:{}  {}",
                        h.score,
                        c.path,
                        c.start,
                        c.names.join(",")
                    );
                }
            }
        }
    }

    /// The numbers that decide whether this is worth having on a slow machine:
    /// cold build, cache write, cache load, and one query on each path.
    #[test]
    #[ignore = "diagnostic, not an assertion"]
    fn bench_index() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let t = std::time::Instant::now();
        let idx = build(root);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;

        // What the same work costs on one core, which is the machine this
        // feature has to stay usable on — measured, not extrapolated.
        let inputs: Vec<(String, &'static str, String)> = walker(root)
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| {
                let lang = graph::language_of(e.path())?;
                let rel = relative(root, e.path())?;
                let text = std::fs::read_to_string(e.path()).ok()?;
                Some((rel, lang, text))
            })
            .collect();
        let t = std::time::Instant::now();
        let one = prepare_with(inputs, 1);
        let serial_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(!one.is_empty());

        let dir = std::env::temp_dir().join(format!("koda-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let t = std::time::Instant::now();
        idx.save(&dir).expect("save");
        let save_ms = t.elapsed().as_secs_f64() * 1000.0;
        let bytes: u64 = std::fs::read_dir(cache_dir(&dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum();

        let t = std::time::Instant::now();
        let loaded = Index::load(&dir).expect("load");
        let load_ms = t.elapsed().as_secs_f64() * 1000.0;

        let queries = gold_queries(root);
        let t = std::time::Instant::now();
        for (q, _) in &queries {
            let _ = loaded.hybrid_search(q, None, 8);
        }
        let query_us = t.elapsed().as_secs_f64() * 1e6 / queries.len() as f64;

        // The same tree again, but with the cache warm: what a second session
        // actually pays.
        // What a second session actually pays: load the cache, then sweep the
        // tree to confirm it is still current.
        let t = std::time::Instant::now();
        let mut warm = Index::load(&dir).expect("load");
        let changed = warm.refresh(root);
        let warm_ms = t.elapsed().as_secs_f64() * 1000.0;

        // A resident-size estimate: the structures that actually stay in memory.
        // Deliberately an estimate and labelled as one — an exact figure would
        // need an allocator hook, and the point is the order of magnitude.
        let resident = idx.chunks.len() * std::mem::size_of::<Chunk>()
            + idx
                .chunks
                .iter()
                .map(|c| c.path.len() + c.names.iter().map(|n| n.len() + 24).sum::<usize>())
                .sum::<usize>()
            + idx.vocab.keys().map(|k| k.len() + 40).sum::<usize>()
            + idx.post.iter().map(|p| p.len() * 6 + 24).sum::<usize>()
            + idx
                .symbols
                .iter()
                .map(|(k, v)| k.len() + 40 + v.len() * 4)
                .sum::<usize>()
            + idx.len.len() * 4
            + idx
                .vectors
                .as_ref()
                .map(|v| v.rows() * v.dim * 2)
                .unwrap_or(0);

        eprintln!(
            "\n  files {}  chunks {}  terms {}  postings {}\n  \
             resident ~{:.2} MB\n  \
             cold build {build_ms:.1} ms  (tokenise only, one core: {serial_ms:.1} ms)\n  \
             cache save {save_ms:.1} ms   size {:.2} MB\n  \
             cache load {load_ms:.1} ms   (warm start, load + sweep: {warm_ms:.1} ms, {changed} changed)\n  \
             query {query_us:.0} us mean over {} gold queries\n  \
             cores {}",
            idx.files,
            idx.chunks.len(),
            idx.vocab.len(),
            idx.post.iter().map(|p| p.len()).sum::<usize>(),
            resident as f64 / 1e6,
            bytes as f64 / 1e6,
            queries.len(),
            std::thread::available_parallelism().map(|p| p.get()).unwrap_or(0),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same measurements against a corpus far larger than koda, to see
    /// where the numbers go. Point `KODA_BENCH_ROOT` at any tree:
    ///
    /// ```text
    /// KODA_BENCH_ROOT=~/.cargo/registry/src/index.crates.io-*/ \
    ///   cargo test --release bench_large -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "diagnostic, needs KODA_BENCH_ROOT"]
    fn bench_large() {
        let Ok(root) = std::env::var("KODA_BENCH_ROOT") else {
            eprintln!("set KODA_BENCH_ROOT to a directory to measure");
            return;
        };
        let root = Path::new(&root);
        let t = std::time::Instant::now();
        let idx = build(root);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;

        let dir = std::env::temp_dir().join(format!("koda-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let t = std::time::Instant::now();
        idx.save(&dir).expect("save");
        let save_ms = t.elapsed().as_secs_f64() * 1000.0;
        let size: u64 = std::fs::read_dir(cache_dir(&dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum();
        let t = std::time::Instant::now();
        let loaded = Index::load(&dir).expect("load");
        let load_ms = t.elapsed().as_secs_f64() * 1000.0;

        let qs = gold_queries(Path::new(env!("CARGO_MANIFEST_DIR")));
        let t = std::time::Instant::now();
        for (q, _) in &qs {
            let _ = loaded.hybrid_search(q, None, 8);
        }
        let query_us = t.elapsed().as_secs_f64() * 1e6 / qs.len() as f64;

        let resident = idx.chunks.len() * std::mem::size_of::<Chunk>()
            + idx
                .chunks
                .iter()
                .map(|c| c.path.len() + c.names.iter().map(|n| n.len() + 24).sum::<usize>())
                .sum::<usize>()
            + idx.vocab.keys().map(|k| k.len() + 40).sum::<usize>()
            + idx.post.iter().map(|p| p.len() * 6 + 24).sum::<usize>()
            + idx
                .symbols
                .iter()
                .map(|(k, v)| k.len() + 40 + v.len() * 4)
                .sum::<usize>()
            + idx.len.len() * 4;
        eprintln!(
            "\n  {}\n  files {}  chunks {}  terms {}  postings {}\n  \
             cold build {build_ms:.0} ms · save {save_ms:.0} ms · load {load_ms:.0} ms · \
             cache {:.1} MB · resident ~{:.0} MB · query {query_us:.0} us",
            root.display(),
            idx.files,
            idx.chunks.len(),
            idx.vocab.len(),
            idx.post.iter().map(|p| p.len()).sum::<usize>(),
            size as f64 / 1e6,
            resident as f64 / 1e6,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Prints the ranking for a few real questions, for eyeballing quality.
    #[test]
    #[ignore = "diagnostic, not an assertion"]
    fn show_rankings() {
        let t0 = std::time::Instant::now();
        let idx = build(Path::new(env!("CARGO_MANIFEST_DIR")));
        let build_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        let _ = idx.hybrid_search("where is retry handled", None, 8);
        eprintln!(
            "{} chunks over {} files · build {build_ms}ms · query {}us",
            idx.chunks.len(),
            idx.files,
            t1.elapsed().as_micros()
        );
        for q in [
            "where is retry handled",
            "how is the context window trimmed",
            "rate limiting throttle",
            "where do we write the session file",
            "how does approval work for commands",
        ] {
            eprintln!("\n  {q:?}");
            for h in idx.hybrid_search(q, None, 5) {
                let c = &idx.chunks[h.chunk];
                eprintln!(
                    "    {:6.2}  {}:{}-{}  {}",
                    h.score,
                    c.path,
                    c.start,
                    c.end,
                    c.names.join(",")
                );
            }
        }
    }

    /// f16 costs precision on purpose; it must not cost the ranking. Round-trip
    /// error has to stay far below the gaps between cosine scores.
    #[test]
    fn f16_keeps_enough_precision_for_a_ranking() {
        for x in [0.0f32, 1.0, -1.0, 0.5, -0.03125, 0.123_45, -0.987_65, 1e-3] {
            let back = f16_to(f16_from(x));
            assert!((back - x).abs() <= 0.001 + x.abs() * 0.001, "{x} -> {back}");
        }
        // Subnormals flush to zero rather than producing garbage.
        assert_eq!(f16_to(f16_from(1e-9)), 0.0);
        // And a value out of range saturates instead of becoming an infinity.
        assert!(f16_to(f16_from(1e9)).is_finite());
    }

    /// Cosine over the stored rows, and the vectors being rejected rather than
    /// half-applied when they do not cover the corpus.
    #[test]
    fn vectors_rank_by_direction_and_are_all_or_nothing() {
        let mut idx = Index {
            chunks: vec![
                Chunk {
                    path: "a.rs".into(),
                    start: 1,
                    end: 2,
                    names: vec!["a".into()],
                },
                Chunk {
                    path: "b.rs".into(),
                    start: 1,
                    end: 2,
                    names: vec!["b".into()],
                },
            ],
            ..Default::default()
        };
        // A short set is refused: a partly embedded corpus would rank the
        // embedded half first for reasons unrelated to the query.
        assert!(attach_vectors(&mut idx, "m", vec![vec![1.0, 0.0]]).is_err());
        assert!(idx.vectors.is_none());
        // Ragged rows are refused too.
        assert!(attach_vectors(&mut idx, "m", vec![vec![1.0, 0.0], vec![1.0]]).is_err());

        attach_vectors(&mut idx, "m", vec![vec![1.0, 0.0], vec![0.0, 2.0]]).expect("attached");
        let v = idx.vectors.as_ref().expect("vectors");
        assert_eq!(v.dim, 2);
        assert_eq!(v.rows(), 2);
        // Magnitude is normalised away, so only direction ranks.
        let hits = v.search(&[0.0, 9.0], 2);
        assert_eq!(hits[0].chunk, 1, "{hits:?}");
        assert!((hits[0].score - 1.0).abs() < 0.01, "{hits:?}");
    }

    /// Fusion has to lift what both halves like, without either being able to
    /// veto the other.
    #[test]
    fn fusion_lifts_what_both_paths_agree_on() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut idx = build(root);

        let lexical = idx.hybrid_search("retry backoff", None, 5);
        assert!(!lexical.is_empty());

        // Point every vector at the second lexical hit; fusion should promote
        // it above the first, since it now has support from both paths.
        let target = lexical[1].chunk;
        let dim = 4;
        let rows: Vec<Vec<f32>> = (0..idx.chunks.len())
            .map(|i| {
                if i == target {
                    vec![1.0, 0.0, 0.0, 0.0]
                } else {
                    vec![0.0, 0.0, 0.0, 1.0]
                }
            })
            .collect();
        attach_vectors(&mut idx, "fake", rows).expect("attached");
        assert_eq!(idx.vectors.as_ref().map(|v| v.dim), Some(dim));

        let fused = idx.hybrid_search("retry backoff", Some(&[1.0, 0.0, 0.0, 0.0]), 5);
        assert_eq!(fused[0].chunk, target, "both paths agree on it");
        // The lexical leader is still on the page: one path cannot veto.
        assert!(
            fused.iter().any(|h| h.chunk == lexical[0].chunk),
            "lexical top-1 must survive fusion"
        );
    }

    #[test]
    fn tests_rank_below_implementation_unless_asked_for() {
        assert!(is_test("tests/probe.py"));
        assert!(is_test("src/foo/test_thing.rs"));
        assert!(is_test("web-ui/app.test.js"));
        assert!(!is_test("src/agent.rs"));
        assert!(!is_test("src/latest.rs"), "substring must not catch this");
    }

    #[test]
    fn identifiers_split_but_survive_whole() {
        let t = terms(Some("rust"), "fn stream_with_retry() {}");
        assert!(t.contains(&"stream_with_retry".to_string()), "{t:?}");
        assert!(t.contains(&"stream".to_string()), "{t:?}");
        assert!(
            t.contains(&"retri".to_string()) || t.contains(&"retry".to_string()),
            "{t:?}"
        );
        // `fn` is a keyword, `with` a stopword: neither is a search term.
        assert!(!t.contains(&"fn".to_string()), "{t:?}");
        assert!(!t.contains(&"with".to_string()), "{t:?}");

        // camelCase and runs of capitals both split sensibly.
        let t = terms(None, "HTTPServer streamWithRetry");
        assert!(t.contains(&"http".to_string()), "{t:?}");
        assert!(t.contains(&"server".to_string()), "{t:?}");

        // Code-ambient English is dropped — measured: without this, "where is
        // retry handled" ranks a test asserting on "handles" first.
        assert!(terms(None, "handled handles handler").is_empty());
    }

    #[test]
    fn a_definition_is_chunked_with_its_doc_comment() {
        let src =
            "//! module doc\nuse std::io;\n\n/// What it does.\n#[inline]\nfn work() {\n    1\n}\n";
        let chunks = chunk_file("a.rs", "rust", src);
        assert!(!chunks.is_empty(), "{chunks:?}");
        // The header comes first, then the definition starting at its doc.
        assert_eq!(chunks[0].start, 1, "{chunks:?}");
        let def = chunks
            .iter()
            .find(|c| c.names.contains(&"work".to_string()));
        let def = def.expect("the definition is a chunk");
        assert_eq!(def.start, 4, "the span starts at the doc comment: {def:?}");
    }
}
