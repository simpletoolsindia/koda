//! Local, no-remote self-improvement (Phase 1: observation + deterministic rules).
//!
//! koda watches how you actually work — the edits you make to its output, the
//! commands that succeed or fail, the files you revert — and distils that into
//! **explicit, inspectable rules** it can follow next time. Nothing here uses a
//! model, nothing leaves the machine, and every artifact is a plain file you can
//! read, edit, or delete:
//!
//! - `.koda/learning/observations.jsonl` — an append-only log of raw signals.
//! - `.koda/learning/rules.md` — candidate and accepted rules. You promote a
//!   candidate to accepted with `/learn`; only accepted rules enter the prompt.
//!
//! The design mirrors `memory.rs`: narrow scope, verifiable facts, no hidden
//! inference. See `docs/research-self-improvement.md` for the full rationale.

use crate::{tel_debug, tel_info};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A single observed signal, appended to observations.jsonl. Kept as a tagged
/// line so the file is greppable and the user can read exactly what koda saw.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// koda edited/created a file: (path, before, after). `before` empty = new.
    Edit { path: String, before: String, after: String },
    /// A command koda ran and whether it succeeded.
    Command { command: String, ok: bool },
    /// The user denied an approval for a tool.
    Denied { tool: String },
    /// The user changed a file *after* koda wrote it. `koda_wrote` is what koda
    /// left; `user_has` is what the file holds now. The delta is the user's
    /// correction of koda's output — the richest "vibe" signal there is.
    Correction { path: String, koda_wrote: String, user_has: String },
}

/// A distilled rule. Candidate rules await the user's nod; accepted rules are
/// injected into the system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Stable key for dedup (e.g. "naming.fn.case"). Never shown.
    pub key: String,
    /// The human-readable rule, stated as a fact.
    pub text: String,
    /// How many observations back this rule.
    pub support: u32,
    pub accepted: bool,
}

#[derive(Debug, Default)]
pub struct Learning {
    pub rules: Vec<Rule>,
    root: PathBuf,
    dirty: bool,
}

const MAX_RULES: usize = 80;
/// A rule needs at least this much repeated evidence before koda proposes it.
/// One-off edits are noise; a habit repeats.
const MIN_SUPPORT: u32 = 2;

fn dir(root: &Path) -> PathBuf {
    root.join(".koda").join("learning")
}
fn rules_path(root: &Path) -> PathBuf {
    dir(root).join("rules.md")
}
fn obs_path(root: &Path) -> PathBuf {
    dir(root).join("observations.jsonl")
}
/// Directory holding koda's last-written content per file, so a correction can
/// be detected even across sessions (koda writes today, you edit in your
/// editor, koda notices tomorrow). Files are named by a hash of the path.
fn writes_dir(root: &Path) -> PathBuf {
    dir(root).join("last_writes")
}

/// Record what koda just wrote to `rel_path`, keyed by a hash of the path.
/// Survives across sessions so a later divergence is attributable to the user.
pub fn record_write(root: &Path, rel_path: &str, content: &str) {
    let d = writes_dir(root);
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    let f = d.join(format!("{}.txt", path_key(rel_path)));
    let _ = std::fs::write(f, content);
}

/// Return what koda last wrote to `rel_path`, if tracked.
pub fn last_write(root: &Path, rel_path: &str) -> Option<String> {
    std::fs::read_to_string(writes_dir(root).join(format!("{}.txt", path_key(rel_path)))).ok()
}

/// Forget the tracked write for `rel_path` (after a correction is counted).
pub fn clear_write(root: &Path, rel_path: &str) {
    let _ = std::fs::remove_file(writes_dir(root).join(format!("{}.txt", path_key(rel_path))));
}

/// A filesystem-safe, collision-resistant key for a path (FNV-1a hex).
fn path_key(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

impl Learning {
    /// Load accepted + candidate rules from rules.md. Observations are not held
    /// in memory — they are mined on demand in `induce`.
    pub fn load(root: &Path) -> Self {
        let mut l = Self {
            root: root.to_path_buf(),
            ..Default::default()
        };
        if let Ok(text) = std::fs::read_to_string(rules_path(root)) {
            let mut section_accepted = false;
            for line in text.lines() {
                let t = line.trim();
                if let Some(h) = t.strip_prefix("## ") {
                    section_accepted = h.trim().eq_ignore_ascii_case("accepted");
                    continue;
                }
                // `- [key] the rule text — (3)`
                if let Some(item) = t.strip_prefix("- ") {
                    if let Some(rule) = parse_rule_line(item, section_accepted) {
                        l.rules.push(rule);
                    }
                }
            }
        }
        tel_debug!("learning", "loaded", "rules" => l.rules.len());
        l
    }

    #[allow(dead_code)] // public API parity with Memory::is_empty
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn accepted(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|r| r.accepted)
    }

    pub fn candidates(&self) -> Vec<&Rule> {
        self.rules.iter().filter(|r| !r.accepted).collect()
    }

    /// Record a raw observation to the append-only log. Cheap and side-effect
    /// free beyond the file append — mining happens later in `induce`.
    pub fn observe(&self, obs: &Observation) {
        let line = match encode(obs) {
            Some(l) => l,
            None => return,
        };
        let d = dir(&self.root);
        if std::fs::create_dir_all(&d).is_err() {
            return;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(obs_path(&self.root))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Read the observation log back (for mining and tests).
    pub fn observations(&self) -> Vec<Observation> {
        let Ok(text) = std::fs::read_to_string(obs_path(&self.root)) else {
            return Vec::new();
        };
        text.lines().filter_map(decode).collect()
    }

    /// Mine the observation log into candidate rules. Deterministic, no model.
    /// New candidates that aren't already known (accepted or candidate) are
    /// added; returns how many new candidates were found.
    pub fn induce(&mut self) -> usize {
        let obs = self.observations();
        let mined = induce_rules(&obs);
        self.merge_candidates(mined)
    }

    /// Turn project idioms (from the code graph) into candidate rules: internal
    /// symbols that are load-bearing here, and modules imported across the
    /// project. Deterministic; the graph is the evidence. `idioms` is
    /// `(name, kind, cross_file_uses)`, `imports` is `(module, times_imported)`.
    pub fn induce_idioms(
        &mut self,
        idioms: &[(String, &'static str, usize)],
        imports: &[(String, usize)],
    ) -> usize {
        let mut mined = Vec::new();
        for (name, kind, reach) in idioms.iter().take(12) {
            mined.push(Rule {
                key: format!("idiom.symbol.{}", slug(name)),
                text: format!(
                    "`{name}` is a load-bearing {kind} in this project (used across {reach} files) \
                     — prefer it over reinventing an equivalent."
                ),
                support: *reach as u32,
                accepted: false,
            });
        }
        for (module, n) in imports.iter().take(8) {
            mined.push(Rule {
                key: format!("idiom.import.{}", slug(module)),
                text: format!(
                    "This project commonly imports `{module}` ({n} files) — use it rather than an alternative."
                ),
                support: *n as u32,
                accepted: false,
            });
        }
        self.merge_candidates(mined)
    }

    /// Fold freshly-mined candidates into the rule set: refresh known ones,
    /// add new ones. Accepted rules keep their acceptance. Returns new count.
    fn merge_candidates(&mut self, mined: Vec<Rule>) -> usize {
        let mut added = 0;
        for m in mined {
            match self.rules.iter_mut().find(|r| r.key == m.key) {
                Some(existing) => {
                    // Refresh support/text; keep acceptance state.
                    existing.support = m.support;
                    if !existing.accepted {
                        existing.text = m.text;
                    }
                }
                None => {
                    self.rules.push(m);
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.dirty = true;
            self.evict();
        }
        tel_info!("learning", "induced", "new" => added, "total" => self.rules.len());
        added
    }

    /// Accept a candidate rule by index into `candidates()`. Returns its text.
    pub fn accept(&mut self, idx: usize) -> Option<String> {
        let key = self.candidates().get(idx).map(|r| r.key.clone())?;
        let rule = self.rules.iter_mut().find(|r| r.key == key)?;
        rule.accepted = true;
        self.dirty = true;
        Some(rule.text.clone())
    }

    /// Accept every current candidate. Returns how many.
    pub fn accept_all(&mut self) -> usize {
        let mut n = 0;
        for r in self.rules.iter_mut().filter(|r| !r.accepted) {
            r.accepted = true;
            n += 1;
        }
        if n > 0 {
            self.dirty = true;
        }
        n
    }

    /// Reject (drop) a candidate rule by index into `candidates()`.
    pub fn reject(&mut self, idx: usize) -> Option<String> {
        let key = self.candidates().get(idx).map(|r| r.key.clone())?;
        let pos = self.rules.iter().position(|r| r.key == key)?;
        let removed = self.rules.remove(pos);
        self.dirty = true;
        Some(removed.text)
    }

    fn evict(&mut self) {
        if self.rules.len() <= MAX_RULES {
            return;
        }
        // Drop the least-supported candidates first; never drop accepted rules.
        self.rules.sort_by_key(|r| (r.accepted, r.support));
        while self.rules.len() > MAX_RULES {
            if let Some(pos) = self.rules.iter().position(|r| !r.accepted) {
                self.rules.remove(pos);
            } else {
                break;
            }
        }
    }

    /// The accepted rules, formatted for the system prompt. Empty if none — so
    /// it costs nothing until koda has actually learned something.
    pub fn brief(&self) -> String {
        let accepted: Vec<&Rule> = self.accepted().collect();
        if accepted.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\nConventions learned in this project (follow them):\n");
        for r in accepted {
            let _ = writeln!(out, "- {}", r.text);
        }
        out
    }

    pub fn save(&mut self) -> std::io::Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        let d = dir(&self.root);
        std::fs::create_dir_all(&d)?;
        let mut text = String::from(
            "# koda learned rules\n\nWritten by koda from watching how you work here. \
             Accepted rules are followed automatically; candidates await `/learn`. \
             Edit or delete freely — nothing else depends on this file.\n",
        );
        let accepted: Vec<&Rule> = self.rules.iter().filter(|r| r.accepted).collect();
        let candidates: Vec<&Rule> = self.rules.iter().filter(|r| !r.accepted).collect();
        if !accepted.is_empty() {
            text.push_str("\n## Accepted\n");
            for r in &accepted {
                let _ = writeln!(text, "- [{}] {} — ({})", r.key, r.text, r.support);
            }
        }
        if !candidates.is_empty() {
            text.push_str("\n## Candidates\n");
            for r in &candidates {
                let _ = writeln!(text, "- [{}] {} — ({})", r.key, r.text, r.support);
            }
        }
        std::fs::write(rules_path(&self.root), text)?;
        self.dirty = false;
        tel_info!("learning", "saved", "rules" => self.rules.len());
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Rule induction — the deterministic core. Pure functions over observations so
// they are trivially testable and hold no state.
// ---------------------------------------------------------------------------

/// Mine a set of observations into candidate rules.
pub fn induce_rules(obs: &[Observation]) -> Vec<Rule> {
    let mut rules = Vec::new();
    rules.extend(command_substitutions(obs));
    rules.extend(naming_convention(obs));
    rules.extend(import_preferences(obs));
    rules.extend(correction_rules(obs));
    rules
}

/// If a command fails and a *similar* command later succeeds, prefer the one
/// that works. Also surfaces commands that only ever failed here.
fn command_substitutions(obs: &[Observation]) -> Vec<Rule> {
    let mut ok: BTreeMap<String, u32> = BTreeMap::new();
    let mut fail: BTreeMap<String, u32> = BTreeMap::new();
    for o in obs {
        if let Observation::Command { command, ok: good } = o {
            let head = command_head(command);
            if head.is_empty() {
                continue;
            }
            if *good {
                *ok.entry(head).or_insert(0) += 1;
            } else {
                *fail.entry(head).or_insert(0) += 1;
            }
        }
    }
    let mut out = Vec::new();
    for (cmd, fails) in &fail {
        let succeeds = ok.get(cmd).copied().unwrap_or(0);
        // Only ever failed, with real evidence: warn koda off it.
        if succeeds == 0 && *fails >= MIN_SUPPORT {
            out.push(Rule {
                key: format!("cmd.avoid.{}", slug(cmd)),
                text: format!("`{cmd}` does not work here — it has only ever failed; find the right command instead."),
                support: *fails,
                accepted: false,
            });
        }
    }
    for (cmd, oks) in &ok {
        if *oks >= MIN_SUPPORT {
            out.push(Rule {
                key: format!("cmd.use.{}", slug(cmd)),
                text: format!("`{cmd}` is the command that works here for that task."),
                support: *oks,
                accepted: false,
            });
        }
    }
    out
}

/// Infer the dominant function-naming case from identifiers koda wrote, when
/// there is a clear majority. A coarse but honest signal.
fn naming_convention(obs: &[Observation]) -> Vec<Rule> {
    let mut snake = 0u32;
    let mut camel = 0u32;
    for o in obs {
        if let Observation::Edit { after, path, .. } = o {
            if !is_code_file(path) {
                continue;
            }
            for name in fn_names(after, path) {
                match casing(&name) {
                    Casing::Snake => snake += 1,
                    Casing::Camel => camel += 1,
                    Casing::Other => {}
                }
            }
        }
    }
    let total = snake + camel;
    if total < MIN_SUPPORT {
        return Vec::new();
    }
    // Require a clear (>=70%) majority before asserting a convention.
    if snake as f32 / total as f32 >= 0.7 {
        vec![Rule {
            key: "naming.fn.snake".into(),
            text: "Functions in this project use snake_case.".into(),
            support: snake,
            accepted: false,
        }]
    } else if camel as f32 / total as f32 >= 0.7 {
        vec![Rule {
            key: "naming.fn.camel".into(),
            text: "Functions in this project use camelCase.".into(),
            support: camel,
            accepted: false,
        }]
    } else {
        Vec::new()
    }
}

/// When the user's surviving code repeatedly imports one library, note the
/// preference. Detected from import lines present in edited files.
fn import_preferences(obs: &[Observation]) -> Vec<Rule> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for o in obs {
        if let Observation::Edit { after, path, .. } = o {
            if !is_code_file(path) {
                continue;
            }
            for lib in imported_libs(after) {
                *counts.entry(lib).or_insert(0) += 1;
            }
        }
    }
    let mut out = Vec::new();
    for (lib, n) in counts {
        // Imports want a touch more evidence than the base threshold.
        if n > MIN_SUPPORT {
            out.push(Rule {
                key: format!("import.prefer.{}", slug(&lib)),
                text: format!("This project uses `{lib}` — prefer it over alternatives."),
                support: n,
                accepted: false,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Correction mining (Phase 2). When the user changes a file after koda wrote
// it, the diff is their correction. We look for lines koda wrote that the user
// replaced by changing exactly one identifier/token, and count recurring
// substitutions across all corrections. A substitution seen MIN_SUPPORT times
// becomes a rule ("use X, not Y"). This is deterministic and inspectable — no
// model, no fuzzy matching beyond token equality.
// ---------------------------------------------------------------------------

fn correction_rules(obs: &[Observation]) -> Vec<Rule> {
    // (removed_token, added_token) -> count
    let mut subs: BTreeMap<(String, String), u32> = BTreeMap::new();
    // Also: a whole import line the user swapped, mined as a library preference.
    for o in obs {
        if let Observation::Correction { koda_wrote, user_has, path } = o {
            if !is_code_file(path) {
                continue;
            }
            for (k_line, u_line) in aligned_changed_lines(koda_wrote, user_has) {
                if let Some((removed, added)) = single_token_swap(&k_line, &u_line) {
                    // Ignore trivial/short tokens and pure whitespace churn.
                    if removed.len() >= 2 && added.len() >= 2 && removed != added {
                        *subs.entry((removed, added)).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for ((removed, added), n) in subs {
        if n >= MIN_SUPPORT {
            out.push(Rule {
                key: format!("correction.sub.{}.{}", slug(&removed), slug(&added)),
                text: format!(
                    "In this project, prefer `{added}` over `{removed}` — the user has changed that {n} time(s)."
                ),
                support: n,
                accepted: false,
            });
        }
    }
    out
}

/// Pair up lines that changed between two versions, in order. Lines present in
/// both (unchanged) are skipped. This is a coarse positional diff — good enough
/// to catch "koda wrote line X, the user replaced it with line Y" without a
/// full LCS. Returns (koda_line, user_line) pairs of the same 1:1 position
/// among the lines that differ.
fn aligned_changed_lines(koda: &str, user: &str) -> Vec<(String, String)> {
    let k: Vec<&str> = koda.lines().collect();
    let u: Vec<&str> = user.lines().collect();
    // Lines the user kept verbatim are not corrections; drop the common set.
    let common: std::collections::HashSet<&str> = k
        .iter()
        .filter(|l| u.contains(l))
        .copied()
        .collect();
    let k_changed: Vec<String> = k
        .iter()
        .filter(|l| !common.contains(**l) && !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect();
    let u_changed: Vec<String> = u
        .iter()
        .filter(|l| !common.contains(**l) && !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect();
    // Pair positionally; only the overlap length matters.
    k_changed
        .into_iter()
        .zip(u_changed)
        .collect()
}

/// If two lines differ by exactly one token (same tokens otherwise, in order),
/// return (removed, added). Tokens are identifier-ish runs.
fn single_token_swap(a: &str, b: &str) -> Option<(String, String)> {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.len() != tb.len() {
        return None;
    }
    let mut diff: Option<(String, String)> = None;
    for (x, y) in ta.iter().zip(tb.iter()) {
        if x != y {
            if diff.is_some() {
                return None; // more than one token changed — too ambiguous
            }
            diff = Some((x.clone(), y.clone()));
        }
    }
    diff
}

/// Split a line into identifier-ish tokens (letters, digits, underscore, dot),
/// dropping punctuation and whitespace.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in line.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------------------
// Small deterministic helpers.
// ---------------------------------------------------------------------------
fn command_head(cmd: &str) -> String {
    // First two words capture "npm test" / "just build" without arg noise.
    cmd.trim()
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

fn is_code_file(path: &str) -> bool {
    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str()),
        Some("rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "go" | "rb" | "java" | "c" | "cpp" | "h")
    )
}

enum Casing {
    Snake,
    Camel,
    Other,
}

fn casing(name: &str) -> Casing {
    let has_underscore = name.contains('_');
    let has_inner_upper = name
        .chars()
        .skip(1)
        .any(|c| c.is_ascii_uppercase());
    let first_lower = name.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false);
    if has_underscore && !has_inner_upper {
        Casing::Snake
    } else if !has_underscore && has_inner_upper && first_lower {
        Casing::Camel
    } else {
        Casing::Other
    }
}

/// Extract function names defined in `text` for the file's language. Regex-free,
/// coarse, and cheap — matches the code graph's philosophy of "good enough to
/// point at the truth."
fn fn_names(text: &str, path: &str) -> Vec<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let kw = match ext {
            "rs" => "fn ",
            "py" => "def ",
            "go" => "func ",
            "js" | "ts" | "jsx" | "tsx" => "function ",
            _ => "",
        };
        if kw.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix(kw) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.len() >= 2 {
                out.push(name);
            }
        }
    }
    out
}

/// Libraries imported in `text`, best-effort across a few languages.
fn imported_libs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        // Python: `import x` / `from x import ...`
        if let Some(rest) = t.strip_prefix("import ") {
            if let Some(first) = rest.split(['.', ' ', ',']).next() {
                push_lib(&mut out, first);
            }
        } else if let Some(rest) = t.strip_prefix("from ") {
            if let Some(first) = rest.split_whitespace().next() {
                push_lib(&mut out, first.split('.').next().unwrap_or(first));
            }
        } else if let Some(rest) = t.strip_prefix("use ") {
            // Rust: `use foo::bar;` -> foo (skip std/crate/self/super).
            if let Some(first) = rest.split("::").next() {
                let first = first.trim();
                if !matches!(first, "std" | "crate" | "self" | "super" | "core" | "alloc") {
                    push_lib(&mut out, first);
                }
            }
        }
    }
    out
}

fn push_lib(out: &mut Vec<String>, name: &str) {
    let name: String = name
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    // Ignore stdlib-ish and trivial names to keep the signal meaningful.
    if name.len() >= 3 && !matches!(name.as_str(), "os" | "sys" | "std" | "fmt") {
        out.push(name);
    }
}

fn parse_rule_line(item: &str, accepted: bool) -> Option<Rule> {
    // `[key] text — (support)`
    let rest = item.strip_prefix('[')?;
    let (key, rest) = rest.split_once(']')?;
    let rest = rest.trim();
    let (text, support) = match rest.rsplit_once(" — (") {
        Some((t, s)) => {
            let n = s.trim_end_matches(')').trim().parse().unwrap_or(1);
            (t.trim().to_string(), n)
        }
        None => (rest.to_string(), 1),
    };
    if key.is_empty() || text.is_empty() {
        return None;
    }
    Some(Rule {
        key: key.to_string(),
        text,
        support,
        accepted,
    })
}

fn encode(obs: &Observation) -> Option<String> {
    // Minimal JSONL by hand — no serde dependency on this hot path, and the
    // format stays greppable. Strings are escaped for newlines and quotes.
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let line = match obs {
        Observation::Edit { path, before, after } => {
            if path.trim().is_empty() {
                return None;
            }
            // Cap payloads: we only need the shape of the change, not megabytes.
            let b: String = before.chars().take(4000).collect();
            let a: String = after.chars().take(4000).collect();
            format!(
                r#"{{"t":"edit","path":"{}","before":"{}","after":"{}"}}"#,
                esc(path), esc(&b), esc(&a)
            )
        }
        Observation::Command { command, ok } => {
            if command.trim().is_empty() {
                return None;
            }
            format!(r#"{{"t":"cmd","command":"{}","ok":{}}}"#, esc(command), ok)
        }
        Observation::Denied { tool } => {
            format!(r#"{{"t":"denied","tool":"{}"}}"#, esc(tool))
        }
        Observation::Correction { path, koda_wrote, user_has } => {
            if path.trim().is_empty() {
                return None;
            }
            let k: String = koda_wrote.chars().take(4000).collect();
            let u: String = user_has.chars().take(4000).collect();
            format!(
                r#"{{"t":"correction","path":"{}","koda_wrote":"{}","user_has":"{}"}}"#,
                esc(path), esc(&k), esc(&u)
            )
        }
    };
    Some(line)
}

fn decode(line: &str) -> Option<Observation> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("t")?.as_str()? {
        "edit" => Some(Observation::Edit {
            path: v.get("path")?.as_str()?.to_string(),
            before: v.get("before").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            after: v.get("after").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }),
        "cmd" => Some(Observation::Command {
            command: v.get("command")?.as_str()?.to_string(),
            ok: v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
        }),
        "denied" => Some(Observation::Denied {
            tool: v.get("tool")?.as_str()?.to_string(),
        }),
        "correction" => Some(Observation::Correction {
            path: v.get("path")?.as_str()?.to_string(),
            koda_wrote: v.get("koda_wrote").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            user_has: v.get("user_has").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("koda-learn-{tag}"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn observations_round_trip_through_the_log() {
        let d = tmp("roundtrip");
        let l = Learning::load(&d);
        l.observe(&Observation::Command { command: "just test".into(), ok: true });
        l.observe(&Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn do_thing() {}".into(),
        });
        let got = l.observations();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Observation::Command { command: "just test".into(), ok: true });
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn induces_command_that_works_and_command_to_avoid() {
        let obs = vec![
            Observation::Command { command: "npm test".into(), ok: false },
            Observation::Command { command: "npm test".into(), ok: false },
            Observation::Command { command: "just test".into(), ok: true },
            Observation::Command { command: "just test".into(), ok: true },
        ];
        let rules = induce_rules(&obs);
        assert!(rules.iter().any(|r| r.key == "cmd.avoid.npm_test"), "{rules:?}");
        assert!(rules.iter().any(|r| r.key == "cmd.use.just_test"), "{rules:?}");
    }

    #[test]
    fn induces_snake_case_convention_with_a_clear_majority() {
        let obs = vec![Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn apply_discount() {}\nfn read_file() {}\nfn write_out() {}".into(),
        }];
        let rules = induce_rules(&obs);
        assert!(rules.iter().any(|r| r.key == "naming.fn.snake"), "{rules:?}");
    }

    #[test]
    fn mixed_casing_asserts_no_naming_rule() {
        let obs = vec![Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn one_two() {}\nfn threeFour() {}".into(),
        }];
        let rules = induce_rules(&obs);
        assert!(!rules.iter().any(|r| r.key.starts_with("naming.fn")), "{rules:?}");
    }

    #[test]
    fn induces_import_preference_above_threshold() {
        let after = "import httpx\nx = 1";
        let obs = vec![
            Observation::Edit { path: "a.py".into(), before: "".into(), after: after.into() },
            Observation::Edit { path: "b.py".into(), before: "".into(), after: after.into() },
            Observation::Edit { path: "c.py".into(), before: "".into(), after: after.into() },
        ];
        let rules = induce_rules(&obs);
        assert!(rules.iter().any(|r| r.key == "import.prefer.httpx"), "{rules:?}");
    }

    #[test]
    fn accept_promotes_a_candidate_into_the_brief() {
        let d = tmp("accept");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command { command: "just test".into(), ok: true });
        l.observe(&Observation::Command { command: "just test".into(), ok: true });
        assert!(l.induce() >= 1);
        assert!(l.brief().is_empty(), "candidates must not enter the prompt");
        assert!(l.accept(0).is_some());
        assert!(l.brief().contains("just test"), "accepted rule enters the brief");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rules_round_trip_through_the_file() {
        let d = tmp("rulefile");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command { command: "just build".into(), ok: true });
        l.observe(&Observation::Command { command: "just build".into(), ok: true });
        l.induce();
        l.accept_all();
        assert!(l.save().unwrap());
        let reloaded = Learning::load(&d);
        assert!(reloaded.brief().contains("just build"), "{:?}", reloaded.rules);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn reject_drops_a_candidate() {
        let d = tmp("reject");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command { command: "just test".into(), ok: true });
        l.observe(&Observation::Command { command: "just test".into(), ok: true });
        l.induce();
        let before = l.candidates().len();
        assert!(before >= 1);
        assert!(l.reject(0).is_some());
        assert_eq!(l.candidates().len(), before - 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn empty_learning_contributes_nothing_to_the_prompt() {
        let d = tmp("empty");
        assert!(Learning::load(&d).brief().is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn induce_idioms_creates_symbol_and_import_candidates() {
        let d = tmp("idioms");
        let mut l = Learning::load(&d);
        let idioms = vec![("log_audit".to_string(), "fn", 7usize)];
        let imports = vec![("internal_kit".to_string(), 5usize)];
        let n = l.induce_idioms(&idioms, &imports);
        assert_eq!(n, 2);
        let cands: Vec<String> = l.candidates().iter().map(|r| r.text.clone()).collect();
        assert!(cands.iter().any(|t| t.contains("log_audit") && t.contains("load-bearing")), "{cands:?}");
        assert!(cands.iter().any(|t| t.contains("internal_kit")), "{cands:?}");
        // Idempotent: re-mining the same idioms adds nothing new.
        assert_eq!(l.induce_idioms(&idioms, &imports), 0);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn induces_a_substitution_rule_from_repeated_corrections() {
        // koda wrote `logging`; the user changed it to the internal `log.audit`
        // twice across two files. That recurring swap should become a rule.
        let obs = vec![
            Observation::Correction {
                path: "a.py".into(),
                koda_wrote: "x = logging".into(),
                user_has: "x = log.audit".into(),
            },
            Observation::Correction {
                path: "b.py".into(),
                koda_wrote: "y = logging".into(),
                user_has: "y = log.audit".into(),
            },
        ];
        let rules = induce_rules(&obs);
        assert!(
            rules.iter().any(|r| r.text.contains("prefer `log.audit` over `logging`")),
            "{rules:?}"
        );
    }

    #[test]
    fn a_single_correction_is_not_enough() {
        let obs = vec![Observation::Correction {
            path: "a.py".into(),
            koda_wrote: "x = requests".into(),
            user_has: "x = httpx".into(),
        }];
        let rules = induce_rules(&obs);
        assert!(
            !rules.iter().any(|r| r.key.starts_with("correction.sub")),
            "one-off correction must not become a rule: {rules:?}"
        );
    }

    #[test]
    fn multi_token_changes_are_ignored_as_ambiguous() {
        // Two tokens changed on the line — too ambiguous to attribute a single swap.
        let obs = vec![
            Observation::Correction {
                path: "a.rs".into(),
                koda_wrote: "let a = foo(bar)".into(),
                user_has: "let b = baz(qux)".into(),
            },
            Observation::Correction {
                path: "b.rs".into(),
                koda_wrote: "let a = foo(bar)".into(),
                user_has: "let b = baz(qux)".into(),
            },
        ];
        let rules = induce_rules(&obs);
        assert!(
            !rules.iter().any(|r| r.key.starts_with("correction.sub")),
            "ambiguous multi-token change must not induce a rule: {rules:?}"
        );
    }

    #[test]
    fn single_token_swap_detects_one_changed_token() {
        assert_eq!(
            single_token_swap("return logging.info", "return log.audit"),
            Some(("logging.info".into(), "log.audit".into()))
        );
        assert_eq!(single_token_swap("a = b", "a = b"), None);
    }

    #[test]
    fn correction_round_trips_through_the_log() {
        let d = tmp("corr-rt");
        let l = Learning::load(&d);
        l.observe(&Observation::Correction {
            path: "a.py".into(),
            koda_wrote: "import requests".into(),
            user_has: "import httpx".into(),
        });
        let got = l.observations();
        assert_eq!(got.len(), 1);
        matches!(got[0], Observation::Correction { .. });
        std::fs::remove_dir_all(&d).ok();
    }
}
