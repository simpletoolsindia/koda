//! A map of the code that matters for one request, ranked over the code graph
//! and cut to a token budget.
//!
//! The graph's overview ranks symbols by how widely they are used, which is
//! what matters for orientation and exactly wrong for a task: `log` and
//! `Error` top every list, and the function the request is about is nowhere.
//! This ranks for the request instead, the way Aider's repo map does:
//!
//! - **The graph.** A file points at another when it uses a symbol defined
//!   there. An edge's weight is how specific that symbol is (a name defined in
//!   many places says little) and how good the evidence is — a mention read by
//!   a parser counts double one matched by a line pattern.
//! - **Personalised PageRank.** The random walk restarts at what the request is
//!   about: files it names, files defining symbols it names, and — weaker —
//!   files whose paths or symbol names share its words. Rank flows from there
//!   to what those files use and what uses them.
//! - **Symbols, not files.** A file's rank is shared out to the definitions its
//!   edges land on; symbols the request names are boosted outright.
//! - **A budget.** The best definitions are printed as their signature lines,
//!   grouped by file, until the token budget is spent. Same inputs, same map.
//!
//! What this is not: semantic. Edges are name matches between files (see
//! `lsp` for resolution), which is why they are weighted rather than trusted.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use crate::graph::Graph;

/// What a request is about, as far as the graph can tell.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Anchors {
    /// Files the request names (`@src/cart.py`, `cart.py`).
    pub files: BTreeSet<String>,
    /// Symbols the request names exactly (`apply_discount`, `Cart::total`).
    pub symbols: BTreeSet<String>,
    /// The request's other words, lower-cased, for weaker matches.
    pub words: BTreeSet<String>,
}

impl Anchors {
    /// Whether the request points at anything in this project directly.
    pub fn strong(&self) -> bool {
        !self.files.is_empty() || !self.symbols.is_empty()
    }
}

/// Words too common in requests to say anything about code.
const STOP: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "what", "where", "when", "why",
    "how", "does", "please", "can", "you", "fix", "add", "make", "use", "not", "are", "was", "all",
    "any", "code", "file", "files", "function", "method", "class", "should", "would", "could",
];

fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            // camelCase boundary
            if c.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_lowercase();
            cur.extend(c.to_lowercase());
        } else {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|w| w.len() >= 3 && !STOP.contains(&w.as_str()));
    out
}

/// Read a request for what it points at in this graph.
pub fn anchors(g: &Graph, request: &str) -> Anchors {
    let mut a = Anchors::default();
    let files: BTreeSet<&String> = g.stamps.keys().chain(g.by_file.keys()).collect();
    for raw in request.split_whitespace() {
        let tok = raw
            .trim_start_matches('@')
            .trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | ':')))
            .trim_end_matches(['.', ':']);
        // Graph keys use `/`; a Windows user may write `src\cart.py`.
        let tok = if cfg!(windows) {
            tok.replace('\\', "/")
        } else {
            tok.to_string()
        };
        let tok = tok.as_str();
        if tok.is_empty() {
            continue;
        }
        // A path, or the tail of one (`cart.py` for `src/cart.py`).
        if tok.contains('.') || tok.contains('/') {
            for f in &files {
                if f.as_str() == tok || f.ends_with(&format!("/{tok}")) {
                    a.files.insert((*f).clone());
                }
            }
        }
        // A symbol, exactly as the graph knows it — or, stripped of call
        // parentheses and backticks, `apply_discount()`.
        let sym = tok.trim_end_matches("()");
        if g.defs.contains_key(sym) {
            a.symbols.insert(sym.to_string());
        }
    }
    a.words = split_words(request).into_iter().collect();
    a
}

/// One ranked definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub file: String,
    pub name: String,
    pub line: usize,
    pub score: f64,
}

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 30;
/// A symbol the request names outright.
const NAMED: f64 = 10.0;
/// Evidence read by a parser, against a line pattern's.
const SYNTAX: f64 = 1.0;
const LEXICAL: f64 = 0.5;

/// Rank the graph's definitions for `anchors`, best first. Deterministic:
/// every map is iterated in sorted order and ties break on (file, name).
pub fn rank(g: &Graph, anchors: &Anchors) -> Vec<Ranked> {
    // Nodes: every file the graph knows.
    let nodes: Vec<&String> = {
        let mut s: BTreeSet<&String> = g.by_file.keys().collect();
        for fs in g.refs.values() {
            s.extend(fs.iter());
        }
        s.into_iter().collect()
    };
    if nodes.is_empty() {
        return Vec::new();
    }
    let ix: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, f)| (f.as_str(), i))
        .collect();
    let n = nodes.len();

    // Edges: user file -> defining file, per symbol, weighted.
    // (from, to) -> [(symbol, weight)]
    let mut edges: BTreeMap<(usize, usize), Vec<(&str, f64)>> = BTreeMap::new();
    for (name, users) in &g.refs {
        let Some(defs) = g.defs.get(name) else {
            continue;
        };
        let homes: BTreeSet<&str> = defs.iter().map(|d| d.file.as_str()).collect();
        let specific = 1.0 / (homes.len() as f64).sqrt();
        let mut w = specific;
        if anchors.symbols.contains(name) {
            w *= NAMED;
        }
        // A private-looking or very short name is weak evidence either way.
        if name.starts_with('_') || name.len() <= 3 {
            w *= 0.3;
        }
        for u in users {
            let evidence = match crate::graph::language_of(Path::new(u)) {
                Some(l) if crate::graph::syntax_parsed(l) => SYNTAX,
                _ => LEXICAL,
            };
            for h in &homes {
                let (Some(&from), Some(&to)) = (ix.get(u.as_str()), ix.get(h)) else {
                    continue;
                };
                edges
                    .entry((from, to))
                    .or_default()
                    .push((name.as_str(), w * evidence));
            }
        }
    }
    let mut out_weight = vec![0.0f64; n];
    for ((from, _), syms) in &edges {
        out_weight[*from] += syms.iter().map(|(_, w)| w).sum::<f64>();
    }

    // Personalisation: where the walk restarts.
    let mut p = vec![0.0f64; n];
    for f in &anchors.files {
        if let Some(&i) = ix.get(f.as_str()) {
            p[i] += 1.0;
        }
    }
    for s in &anchors.symbols {
        for d in g.defs.get(s).into_iter().flatten() {
            if let Some(&i) = ix.get(d.file.as_str()) {
                p[i] += 1.0;
            }
        }
    }
    // Weaker: the request's words in a path or in the names a file defines.
    if !anchors.words.is_empty() {
        for (i, f) in nodes.iter().enumerate() {
            let path_words: BTreeSet<String> = split_words(f).into_iter().collect();
            let mut hits = anchors.words.intersection(&path_words).count() as f64;
            if let Some(names) = g.by_file.get(*f) {
                let defined: BTreeSet<String> = names.iter().flat_map(|n| split_words(n)).collect();
                hits += 0.5 * anchors.words.intersection(&defined).count() as f64;
            }
            p[i] += 0.2 * hits;
        }
    }
    let total: f64 = p.iter().sum();
    if total > 0.0 {
        for v in &mut p {
            *v /= total;
        }
    } else {
        // Nothing to go on: plain PageRank, i.e. global importance.
        p = vec![1.0 / n as f64; n];
    }

    // Power iteration.
    let mut r = p.clone();
    for _ in 0..ITERATIONS {
        let mut next: Vec<f64> = p.iter().map(|v| (1.0 - DAMPING) * v).collect();
        let mut dangling = 0.0;
        for (i, &ri) in r.iter().enumerate() {
            if out_weight[i] == 0.0 {
                dangling += ri;
            }
        }
        for ((from, to), syms) in &edges {
            let w: f64 = syms.iter().map(|(_, w)| w).sum();
            next[*to] += DAMPING * r[*from] * w / out_weight[*from];
        }
        for (i, v) in next.iter_mut().enumerate() {
            *v += DAMPING * dangling * p[i];
        }
        r = next;
    }

    // Share each file's rank out to the definitions its edges land on.
    let mut score: BTreeMap<(&str, &str), f64> = BTreeMap::new();
    for ((from, to), syms) in &edges {
        for (name, w) in syms {
            *score.entry((nodes[*to].as_str(), name)).or_default() +=
                r[*from] * w / out_weight[*from];
        }
    }
    // A file the request is about contributes its own definitions, even the
    // ones nothing else uses yet.
    for (i, f) in nodes.iter().enumerate() {
        if p[i] <= 0.0 || total == 0.0 {
            continue;
        }
        if let Some(names) = g.by_file.get(*f) {
            let each = r[i] / names.len().max(1) as f64;
            for name in names {
                *score.entry((f.as_str(), name.as_str())).or_default() += each;
            }
        }
    }
    for s in &anchors.symbols {
        for d in g.defs.get(s).into_iter().flatten() {
            if let Some(v) = score.get_mut(&(d.file.as_str(), s.as_str())) {
                *v *= NAMED;
            } else {
                score.insert(
                    (d.file.as_str(), s.as_str()),
                    NAMED * r[ix[d.file.as_str()]],
                );
            }
        }
    }

    let mut ranked: Vec<Ranked> = score
        .into_iter()
        .filter_map(|((file, name), s)| {
            let line = g.defs.get(name)?.iter().find(|d| d.file == file)?.line;
            Some(Ranked {
                file: file.to_string(),
                name: name.to_string(),
                line,
                score: s,
            })
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
    });
    ranked
}

/// Roughly how many tokens a piece of text costs: the same four characters a
/// token the rest of koda budgets with.
fn tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// The map for a request, as signature lines grouped by file, no longer than
/// `budget` tokens. Empty when nothing ranks.
pub fn render(root: &Path, g: &Graph, anchors: &Anchors, budget: usize) -> String {
    let ranked = rank(g, anchors);
    if ranked.is_empty() || budget == 0 {
        return String::new();
    }
    let header = "Code map for this request — ranked from the code graph (signatures only; \
                  read a file before editing it):\n";
    let mut used = tokens(header);
    // Pick definitions best-first until the budget is spent, then print them
    // grouped by file in the order each file first appeared.
    let mut picked: Vec<&Ranked> = Vec::new();
    let mut seen_lines: BTreeSet<(&str, usize)> = BTreeSet::new();
    let mut file_order: Vec<&str> = Vec::new();
    let mut sources: HashMap<&str, Vec<String>> = HashMap::new();
    for r in &ranked {
        // `m` and `Type::m` are the same line.
        if !seen_lines.insert((r.file.as_str(), r.line)) {
            continue;
        }
        let lines = sources.entry(r.file.as_str()).or_insert_with(|| {
            std::fs::read_to_string(root.join(&r.file))
                .map(|t| t.lines().map(str::to_string).collect())
                .unwrap_or_default()
        });
        let sig = lines
            .get(r.line.saturating_sub(1))
            .map(|l| l.trim())
            .unwrap_or("");
        if sig.is_empty() {
            continue;
        }
        let new_file = !file_order.contains(&r.file.as_str());
        let cost = tokens(sig) + 3 + if new_file { tokens(&r.file) + 1 } else { 0 };
        if used + cost > budget {
            continue;
        }
        used += cost;
        if new_file {
            file_order.push(r.file.as_str());
        }
        picked.push(r);
    }
    if picked.is_empty() {
        return String::new();
    }
    let mut out = String::from(header);
    for f in &file_order {
        let _ = writeln!(out, "{f}:");
        let mut rows: Vec<&&Ranked> = picked.iter().filter(|r| r.file == *f).collect();
        rows.sort_by_key(|r| r.line);
        for r in rows {
            let sig = sources[r.file.as_str()]
                .get(r.line - 1)
                .map(|l| l.trim())
                .unwrap_or("");
            let _ = writeln!(out, "  {:>4}│ {sig}", r.line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small project, in a directory of its own: tests run in parallel and
    /// must not rewrite each other's files mid-scan.
    fn project(tag: &str) -> (std::path::PathBuf, Graph) {
        let dir = std::env::temp_dir().join(format!("koda-repomap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let w = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        // A logging helper every file uses: globally the most important thing.
        w("util/log.py", "def log_event(msg):\n    print(msg)\n");
        for i in 0..8 {
            w(
                &format!("feature/f{i}.py"),
                &format!(
                    "from util.log import log_event\n\ndef feature_{i}():\n    log_event('x')\n"
                ),
            );
        }
        // The code a discount task is about.
        w(
            "shop/cart.py",
            "from util.log import log_event\n\ndef apply_discount(amount, percent):\n    return amount - percent\n\nclass Cart:\n    def total(self):\n        return apply_discount(10, 5)\n",
        );
        w(
            "shop/checkout.py",
            "from shop.cart import Cart, apply_discount\n\ndef checkout(cart):\n    return apply_discount(cart.total(), 10)\n",
        );
        let g = crate::graph::scan(&dir);
        (dir, g)
    }

    /// Without a task, the popular helper wins; with one, the task's code does.
    #[test]
    fn the_request_outranks_global_popularity() {
        let (dir, g) = project("t1");
        let global = rank(&g, &Anchors::default());
        assert_eq!(global[0].name, "log_event", "{global:?}");

        let a = anchors(&g, "apply_discount is wrong — it subtracts the percent");
        assert!(a.symbols.contains("apply_discount"), "{a:?}");
        let task = rank(&g, &a);
        assert_eq!(task[0].name, "apply_discount", "{task:?}");
        let pos = |n: &str| task.iter().position(|r| r.name == n).unwrap_or(usize::MAX);
        assert!(pos("apply_discount") < pos("log_event"), "{task:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A named file brings its neighbours: what it uses ranks in.
    #[test]
    fn a_named_file_brings_what_it_uses() {
        let (dir, g) = project("t2");
        let a = anchors(&g, "the total in @shop/checkout.py looks off");
        assert!(a.files.contains("shop/checkout.py"), "{a:?}");
        let map = render(&dir, &g, &a, 400);
        assert!(map.contains("shop/cart.py:"), "{map}");
        assert!(
            map.contains("def apply_discount(amount, percent):"),
            "{map}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_map_is_deterministic_and_within_budget() {
        let (dir, g) = project("t3");
        let a = anchors(&g, "fix apply_discount in cart.py");
        for budget in [40, 80, 200, 600] {
            let one = render(&dir, &g, &a, budget);
            let two = render(&dir, &g, &a, budget);
            assert_eq!(one, two, "deterministic at {budget}");
            assert!(
                tokens(&one) <= budget,
                "{} > {budget}:\n{one}",
                tokens(&one)
            );
        }
        assert!(render(&dir, &g, &a, 0).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Against koda's own source and its retrieval gold set: for each
    /// plain-language question, is the file that answers it in a 600-token
    /// map? Task-ranked against untargeted (global importance, what a
    /// project overview gives). Printed, and the task-ranked map must not do
    /// worse.
    #[test]
    fn a_task_ranked_map_finds_more_of_the_right_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let gold: Vec<(String, Vec<String>)> =
            std::fs::read_to_string(root.join("tests/retrieval_gold.txt"))
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .filter_map(|l| l.split_once('\t'))
                .map(|(q, p)| {
                    (
                        q.trim().into(),
                        p.split(',').map(|x| x.trim().into()).collect(),
                    )
                })
                .collect();
        let g = crate::graph::scan(root);
        let hit = |map: &str, want: &[String]| want.iter().any(|w| map.contains(&format!("{w}:")));
        let global = render(root, &g, &Anchors::default(), 600);
        let (mut task_hits, mut global_hits) = (0, 0);
        for (q, want) in &gold {
            let a = anchors(&g, q);
            if hit(&render(root, &g, &a, 600), want) {
                task_hits += 1;
            }
            if hit(&global, want) {
                global_hits += 1;
            }
        }
        eprintln!(
            "repomap recall@600 tokens over {} gold questions: task-ranked {task_hits}, untargeted {global_hits}",
            gold.len()
        );
        assert!(task_hits >= global_hits, "{task_hits} < {global_hits}");
    }

    #[test]
    fn words_split_the_way_code_is_named() {
        assert_eq!(
            split_words("applyDiscount to cart_total"),
            vec!["apply", "discount", "cart", "total"]
        );
    }
    #[test]
    fn an_empty_project_maps_to_nothing_without_panicking() {
        let dir = std::env::temp_dir().join(format!("koda-repomap-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = crate::graph::scan(&dir);
        let a = anchors(&g, "fix the checkout total");
        assert!(!a.strong());
        assert!(rank(&g, &a).is_empty());
        assert!(render(&dir, &g, &a, 600).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A general question names nothing here, so the request is not anchored
    /// -- which is what keeps the map off requests it would not help.
    #[test]
    fn a_general_question_is_not_anchored() {
        let (dir, g) = project("t5");
        let a = anchors(&g, "what is the capital of France?");
        assert!(!a.strong(), "{a:?}");
        let named = anchors(&g, "why does `Cart` round down? see cart.py");
        assert!(named.strong());
        assert!(named.files.contains("shop/cart.py"), "{named:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filler_and_short_words_are_not_anchor_words() {
        let words = split_words("how do I fix the UI of the checkoutFlow in a file?");
        assert_eq!(words, vec!["checkout", "flow"]);
    }
}
