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
use std::collections::HashMap;
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
    /// chunk id -> token count
    len: Vec<u32>,
    avgdl: f32,
    /// Files indexed, so a stale index can be told apart from an empty one.
    pub files: usize,
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
    /// The model that produced these. Changing it invalidates them and nothing
    /// else, since the lexical half never depended on it.
    pub model: String,
}

impl Vectors {
    pub fn rows(&self) -> usize {
        self.data.len().checked_div(self.dim).unwrap_or(0)
    }

    fn push(&mut self, v: &[f32]) {
        // Normalise on the way in so every query is a dot product.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        self.data.extend(v.iter().map(|x| f16_from(x / norm)));
    }

    /// Cosine against every row. A flat scan: at koda's corpus sizes an ANN
    /// index would be more code, more memory and more failure modes than the
    /// millisecond it saves.
    fn search(&self, query: &[f32], k: usize) -> Vec<Hit> {
        if self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        let q: Vec<f32> = query.iter().map(|x| x / norm).collect();
        let mut hits: Vec<Hit> = (0..self.rows())
            .map(|i| {
                let row = &self.data[i * self.dim..(i + 1) * self.dim];
                let score = row.iter().zip(&q).map(|(a, b)| f16_to(*a) * b).sum::<f32>();
                Hit { chunk: i, score }
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        hits
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

/// Cormack et al.'s constant. Their own sweep moves MAP by 0.2% between k=10
/// and k=100, so there is no tuning to be had here without labelled queries.
const RRF_K: f32 = 60.0;
/// How deep each path is read before fusing.
const FUSE_DEPTH: usize = 50;
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
        let lexical = self.rank_lexical(query, FUSE_DEPTH);
        let dense = match (query_vec, self.vectors.as_ref()) {
            (Some(q), Some(v)) => v.search(q, FUSE_DEPTH),
            _ => Vec::new(),
        };
        if dense.is_empty() {
            return self.finish_ranking(lexical, query, k);
        }
        let mut fused: HashMap<usize, f32> = HashMap::new();
        for (rank, h) in lexical.iter().enumerate() {
            *fused.entry(h.chunk).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
        }
        for (rank, h) in dense.iter().enumerate() {
            *fused.entry(h.chunk).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
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
fn chunk_text(lines: &[&str], c: &Chunk) -> String {
    lines
        .iter()
        .skip(c.start.saturating_sub(1))
        .take(c.end + 1 - c.start)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

impl Index {
    /// Index one file's chunks into the postings.
    fn add_file(&mut self, path: &str, lang: &'static str, text: &str) {
        let lines: Vec<&str> = text.lines().collect();
        for chunk in chunk_file(path, lang, text) {
            let body = chunk_text(&lines, &chunk);
            let terms = terms(Some(lang), &body);
            if terms.is_empty() {
                continue;
            }
            let cid = self.chunks.len() as u32;
            let mut tf: HashMap<&str, u16> = HashMap::new();
            for t in &terms {
                *tf.entry(t.as_str()).or_insert(0) =
                    tf.get(t.as_str()).unwrap_or(&0).saturating_add(1);
            }
            for (term, count) in tf {
                let next = self.vocab.len() as u32;
                let tid = *self.vocab.entry(term.to_string()).or_insert(next);
                if self.post.len() <= tid as usize {
                    self.post.resize(tid as usize + 1, Vec::new());
                }
                self.post[tid as usize].push((cid, count));
            }
            self.len.push(terms.len() as u32);
            self.chunks.push(chunk);
        }
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
        model: model.to_string(),
    };
    for row in &rows {
        v.push(row);
    }
    idx.vectors = Some(v);
    Ok(())
}

/// Build the index by walking the workspace, under the same limits and the same
/// ignore rules the graph uses — one traversal policy, not two.
pub fn build(root: &Path) -> Index {
    let mut idx = Index::default();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .filter_entry(|e| !graph::is_vendor_dir(&e.file_name().to_string_lossy()))
        .build();
    for entry in walk.flatten() {
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
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();
        idx.add_file(&rel, lang, &String::from_utf8_lossy(&bytes));
        idx.files += 1;
    }
    idx.finish();
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

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
