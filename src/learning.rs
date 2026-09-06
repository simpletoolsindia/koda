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
    Edit {
        path: String,
        before: String,
        after: String,
    },
    /// A command koda ran and whether it succeeded.
    Command { command: String, ok: bool },
    /// The user denied an approval for a tool.
    Denied { tool: String },
    /// The user changed a file *after* koda wrote it. `koda_wrote` is what koda
    /// left; `user_has` is what the file holds now. The delta is the user's
    /// correction of koda's output — the richest "vibe" signal there is.
    Correction {
        path: String,
        koda_wrote: String,
        user_has: String,
    },
}

/// A distilled rule. Candidate rules await the user's nod; accepted rules are
/// injected into the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rule {
    /// Stable key for dedup (e.g. "naming.fn.case"). Never shown.
    pub key: String,
    /// The human-readable rule, stated as a fact.
    pub text: String,
    /// How many observations back this rule.
    pub support: u32,
    pub accepted: bool,
    /// Distinct days this rule has been re-derived from the log. A habit shows
    /// up again tomorrow; a coincidence does not — which is what lets koda
    /// promote a rule on its own without asking the user to run `/learn`.
    pub days: u32,
    /// Day (days since the epoch) this rule was last re-derived, so a rule that
    /// stops being true can be retired instead of lingering in the prompt.
    pub last_day: u32,
    /// Promoted by the daily pass rather than by the user. Only these are ever
    /// retired automatically.
    pub auto: bool,
}

/// What one daily consolidation changed. Empty is the normal case.
#[derive(Debug, Default, Clone)]
pub struct DailyReport {
    /// Day the pass ran, as days since the epoch.
    pub day: u32,
    /// New candidates mined today.
    pub found: usize,
    /// Rules promoted into the prompt on their own.
    pub promoted: Vec<String>,
    /// Auto-promoted rules that went quiet and dropped back to candidates.
    pub retired: Vec<String>,
}

impl DailyReport {
    /// A one-line summary for the user, or None when nothing changed.
    pub fn notice(&self) -> Option<String> {
        if self.promoted.is_empty() && self.retired.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !self.promoted.is_empty() {
            parts.push(format!("learned {} new rule(s)", self.promoted.len()));
        }
        if !self.retired.is_empty() {
            parts.push(format!("retired {} stale one(s)", self.retired.len()));
        }
        Some(format!(
            "daily learning: {} — see /learn or .koda/learning/journal.md",
            parts.join(", ")
        ))
    }
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

fn obsolete_generic_idiom(key: &str) -> bool {
    let Some(name) = key.strip_prefix("idiom.symbol.") else {
        return false;
    };
    matches!(
        name,
        "body"
            | "build"
            | "chars"
            | "clone"
            | "collect"
            | "contains"
            | "count"
            | "default"
            | "filter"
            | "find"
            | "first"
            | "from"
            | "get"
            | "insert"
            | "into"
            | "is_empty"
            | "iter"
            | "last"
            | "len"
            | "line"
            | "load"
            | "main"
            | "map"
            | "name"
            | "new"
            | "next"
            | "open"
            | "parse"
            | "path"
            | "push"
            | "read"
            | "remove"
            | "run"
            | "save"
            | "skip"
            | "sort"
            | "take"
            | "text"
            | "to_string"
            | "trim"
            | "unwrap"
            | "update"
            | "value"
            | "write"
    )
}

fn dir(root: &Path) -> PathBuf {
    root.join(".koda").join("learning")
}
fn rules_path(root: &Path) -> PathBuf {
    dir(root).join("rules.md")
}
fn obs_path(root: &Path) -> PathBuf {
    dir(root).join("observations.jsonl")
}
/// Day number of the last daily consolidation, so it runs once a day even
/// across restarts.
fn pass_path(root: &Path) -> PathBuf {
    dir(root).join("last-pass")
}
/// Human-readable record of what was learned and when.
fn journal_path(root: &Path) -> PathBuf {
    dir(root).join("journal.md")
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
                        // Older graph mining could mistake ubiquitous collection
                        // methods (`len`, `map`, `push`, …) for project idioms.
                        // Never discard an explicitly accepted rule, but retire
                        // those low-signal pending candidates automatically.
                        if !rule.accepted && obsolete_generic_idiom(&rule.key) {
                            l.dirty = true;
                            continue;
                        }
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
        self.rotate_observations();
    }

    /// Keep the observation log to a working set.
    ///
    /// It had no bound at all, and `induce` re-reads and re-parses the whole
    /// file at the end of every turn — so a project accumulated a file that both
    /// grew for ever on disk and made every turn end more slowly than the last.
    /// Mining only looks for repetition, so the recent past is what carries the
    /// signal; the oldest entries are the ones worth dropping.
    fn rotate_observations(&self) {
        const MAX_OBSERVATIONS: usize = 4_000;
        let path = obs_path(&self.root);
        // Checking length by lines would mean reading the file on every
        // observation, which is the cost being avoided. Size is a cheap proxy;
        // only then is the trim worth doing.
        const SIZE_TRIGGER: u64 = 4 * 1024 * 1024;
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if meta.len() < SIZE_TRIGGER {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= MAX_OBSERVATIONS {
            return;
        }
        let kept = lines[lines.len() - MAX_OBSERVATIONS..].join("\n");
        // Write beside and rename, so a crash mid-write cannot leave a
        // half-truncated log where a whole one used to be.
        let tmp = path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, format!("{kept}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        } else {
            let _ = std::fs::remove_file(&tmp);
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
        // Same reasoning as induce_idioms: command rules are re-derived from the
        // full observation log every time, so a candidate this run did not
        // produce is one the current rules no longer justify.
        self.retire_stale(&["cmd.use.", "cmd.avoid."], &mined);
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
        for (name, kind, reach) in idioms.iter().take(8) {
            mined.push(Rule {
                key: format!("idiom.symbol.{}", slug(name)),
                text: format!(
                    "`{name}` is a load-bearing {kind} in this project (used across {reach} files) \
                     — prefer it over reinventing an equivalent."
                ),
                support: *reach as u32,
                accepted: false,
                ..Default::default()
            });
        }
        for (module, n) in imports.iter().take(6) {
            mined.push(Rule {
                key: format!("idiom.import.{}", slug(module)),
                text: format!(
                    "This project commonly imports `{module}` ({n} files) — use it rather than an alternative."
                ),
                support: *n as u32,
                accepted: false,
                ..Default::default()
            });
        }
        // Idiom mining is deterministic and re-runs from the whole graph, so
        // this run's output is the complete truth about idioms. Anything left
        // over under the same prefix was mined by an older, worse version of
        // these heuristics and would otherwise sit in /learn for ever: the
        // filters improve, the garbage never leaves. Accepted rules are the
        // user's, and survive regardless.
        self.retire_stale(&["idiom.symbol.", "idiom.import."], &mined);
        self.merge_candidates(mined)
    }

    /// Drop unaccepted candidates under `prefixes` that this mining run no
    /// longer produces.
    ///
    /// Without it a rule is permanent once mined. Every fix to the miner leaves
    /// its previous mistakes behind, and a user who has seen the same nonsense
    /// in /learn twice stops reading the list — which costs far more than the
    /// bad rules themselves.
    fn retire_stale(&mut self, prefixes: &[&str], mined: &[Rule]) {
        let before = self.rules.len();
        self.rules.retain(|r| {
            let ours = prefixes.iter().any(|p| r.key.starts_with(p));
            !ours || r.accepted || mined.iter().any(|m| m.key == r.key)
        });
        if self.rules.len() != before {
            self.dirty = true;
        }
    }

    /// Fold freshly-mined candidates into the rule set: refresh known ones,
    /// add new ones. Accepted rules keep their acceptance. Returns new count.
    fn merge_candidates(&mut self, mined: Vec<Rule>) -> usize {
        let mut added = 0;
        let today = today();
        for mut m in mined {
            match self.rules.iter_mut().find(|r| r.key == m.key) {
                Some(existing) => {
                    // Refresh support/text; keep acceptance state.
                    existing.support = m.support;
                    if !existing.accepted {
                        existing.text = m.text;
                    }
                    // A rule re-derived on a new calendar day is a rule that
                    // held up overnight. That count — not raw support, which one
                    // busy afternoon can inflate — is what promotion rests on.
                    if existing.last_day != today {
                        existing.days = existing.days.saturating_add(1);
                        existing.last_day = today;
                        self.dirty = true;
                    }
                }
                None => {
                    m.days = 1;
                    m.last_day = today;
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

    /// Consolidate a day's learning: mine the log, promote what has held up
    /// across several days, and retire what stopped being true.
    ///
    /// Learning that only happens when someone remembers to run `/learn` mostly
    /// does not happen. This runs at most once per calendar day (the marker is
    /// a file, so it survives restarts), and it only ever promotes a rule that
    /// the log has re-derived on `promote_days` *different* days — a habit, not
    /// a busy afternoon. Anything it promotes is marked `auto` in rules.md and
    /// can be dropped with `/learn reject`; rules the user accepted by hand are
    /// never touched.
    pub fn daily_pass(&mut self, promote_days: u32, retire_days: u32) -> Option<DailyReport> {
        let today = today();
        if self.last_pass() == Some(today) {
            return None;
        }
        self.mark_pass(today);

        let found = self.induce();
        let promote_days = promote_days.max(1);
        let mut report = DailyReport {
            day: today,
            found,
            ..Default::default()
        };

        for r in self.rules.iter_mut() {
            if !r.accepted && r.days >= promote_days {
                r.accepted = true;
                r.auto = true;
                report.promoted.push(r.text.clone());
            }
        }
        // Retire only what koda promoted itself, and only once it has gone
        // quiet for long enough that it is probably about a part of the project
        // that changed. It drops back to a candidate rather than vanishing, so
        // the evidence is still there to read.
        if retire_days > 0 {
            for r in self.rules.iter_mut() {
                if r.accepted
                    && r.auto
                    && r.last_day > 0
                    && today.saturating_sub(r.last_day) >= retire_days
                {
                    r.accepted = false;
                    r.auto = false;
                    r.days = 0;
                    report.retired.push(r.text.clone());
                }
            }
        }
        if !report.promoted.is_empty() || !report.retired.is_empty() {
            self.dirty = true;
        }
        self.write_journal(&report);
        tel_info!(
            "learning", "daily pass",
            "found" => report.found,
            "promoted" => report.promoted.len(),
            "retired" => report.retired.len(),
        );
        Some(report)
    }

    /// The day the last consolidation ran, if any.
    fn last_pass(&self) -> Option<u32> {
        std::fs::read_to_string(pass_path(&self.root))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn mark_pass(&self, day: u32) {
        let _ = std::fs::create_dir_all(dir(&self.root));
        let _ = std::fs::write(pass_path(&self.root), day.to_string());
    }

    /// Append the day's changes to a plain-text journal, so "what has koda
    /// learned about this project" has an answer you can read without koda.
    fn write_journal(&self, report: &DailyReport) {
        if report.promoted.is_empty() && report.retired.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(dir(&self.root));
        let mut entry = format!("\n## {}\n", ymd(report.day));
        for t in &report.promoted {
            let _ = writeln!(entry, "- learned: {t}");
        }
        for t in &report.retired {
            let _ = writeln!(entry, "- retired: {t}");
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(journal_path(&self.root))
        {
            let _ = write!(f, "{entry}");
        }
    }

    /// One line for the status/`/learn` output: what the daily pass has been
    /// doing lately.
    pub fn daily_status(&self) -> String {
        let auto = self.rules.iter().filter(|r| r.auto && r.accepted).count();
        let pending = self.candidates().len();
        match self.last_pass() {
            Some(d) => format!(
                "last consolidated {} · {auto} rule(s) learned on their own · {pending} awaiting review",
                ymd(d)
            ),
            None => format!("not consolidated yet · {pending} candidate(s) awaiting review"),
        }
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
        let mut out = String::from(
            "\n\nMANDATORY PROJECT CONVENTIONS (learned from this user's corrections and workflow):\n",
        );
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
                let _ = writeln!(text, "- {}", rule_line(r));
            }
        }
        if !candidates.is_empty() {
            text.push_str("\n## Candidates\n");
            for r in &candidates {
                let _ = writeln!(text, "- {}", rule_line(r));
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
                ..Default::default()
            });
        }
    }
    for (cmd, oks) in &ok {
        if is_generic_command(cmd) {
            continue;
        }
        if *oks >= MIN_SUPPORT {
            out.push(Rule {
                key: format!("cmd.use.{}", slug(cmd)),
                text: format!("`{cmd}` is the command that works here for that task."),
                support: *oks,
                accepted: false,
                ..Default::default()
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
            ..Default::default()
        }]
    } else if camel as f32 / total as f32 >= 0.7 {
        vec![Rule {
            key: "naming.fn.camel".into(),
            text: "Functions in this project use camelCase.".into(),
            support: camel,
            accepted: false,
            ..Default::default()
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
                ..Default::default()
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
    // Structural habits the user imposed on koda's code: (key, text) -> count.
    let mut habits: BTreeMap<&'static str, (u32, &'static str)> = BTreeMap::new();
    // Also: a whole import line the user swapped, mined as a library preference.
    for o in obs {
        if let Observation::Correction {
            koda_wrote,
            user_has,
            path,
        } = o
        {
            if !is_code_file(path) {
                continue;
            }
            for (k_line, u_line) in aligned_changed_lines(koda_wrote, user_has) {
                // A rewrite is rarely a single token: renaming a function and its
                // parameters in one go is normal. Take every identifier the user
                // swapped in that line, up to a small cap — beyond that the line
                // was rewritten wholesale and no crisp rule follows from it.
                for (removed, added) in token_swaps(&k_line, &u_line, 3) {
                    // Ignore trivial/short tokens and pure whitespace churn.
                    if removed.len() >= 2 && added.len() >= 2 && removed != added {
                        *subs.entry((removed, added)).or_insert(0) += 1;
                    }
                }
            }
            for (key, text) in style_habits(koda_wrote, user_has) {
                let e = habits.entry(key).or_insert((0, text));
                e.0 += 1;
            }
        }
    }
    let mut out = Vec::new();
    for ((removed, added), n) in subs {
        // A user editing koda's code to swap one token for another is the
        // strongest, most deliberate signal there is — learn it from a single
        // occurrence rather than making the user repeat the same correction.
        if n >= 1 {
            let times = if n == 1 {
                "changed it".to_string()
            } else {
                format!("changed it {n} times")
            };
            out.push(Rule {
                key: format!("correction.sub.{}.{}", slug(&removed), slug(&added)),
                text: format!(
                    "In this project, prefer `{added}` over `{removed}` — the user {times}."
                ),
                support: n,
                accepted: false,
                ..Default::default()
            });
        }
    }
    for (key, (n, text)) in habits {
        out.push(Rule {
            key: format!("correction.style.{key}"),
            text: text.to_string(),
            support: n,
            accepted: false,
            ..Default::default()
        });
    }
    out
}

/// Every identifier the user swapped between two versions of a line, in order,
/// as long as the line was edited rather than replaced: the two lines must still
/// have the same token count and share most of their tokens. Capped at `max`
/// substitutions, because a line that changed more than that was rewritten and
/// no single preference can be read out of it.
fn token_swaps(a: &str, b: &str, max: usize) -> Vec<(String, String)> {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.len() != tb.len() || ta.is_empty() {
        return Vec::new();
    }
    let mut diff: Vec<(String, String)> = Vec::new();
    for (x, y) in ta.iter().zip(tb.iter()) {
        if x != y {
            diff.push((x.clone(), y.clone()));
        }
    }
    if diff.is_empty() || diff.len() > max {
        return Vec::new();
    }
    // At least half the line must be untouched, so "same shape, renamed thing"
    // is learnable but a wholesale rewrite is not.
    if diff.len() * 2 > ta.len() {
        return Vec::new();
    }
    diff
}

/// Structural conventions the user added to koda's code. Deterministic and
/// conservative: each one is a shape that is either present or absent, so the
/// rule states an observed fact rather than an inference.
fn style_habits(koda: &str, user: &str) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let k_defs: Vec<&str> = def_lines(koda);
    let u_defs: Vec<&str> = def_lines(user);
    if !k_defs.is_empty() && !u_defs.is_empty() {
        // Type annotations: the user annotated a signature koda left bare.
        let k_annotated = k_defs.iter().any(|l| is_annotated_def(l));
        let u_annotated = u_defs.iter().any(|l| is_annotated_def(l));
        if !k_annotated && u_annotated {
            out.push((
                "annotate_signatures",
                "Annotate function signatures with types — the user added type \
                 annotations to code koda wrote without them.",
            ));
        }
    }
    // Docstrings: the user documented a function koda left undocumented.
    if !has_docstring(koda) && has_docstring(user) {
        out.push((
            "docstrings",
            "Give new functions a docstring — the user added one to code koda \
             wrote without it.",
        ));
    }
    out
}

fn def_lines(src: &str) -> Vec<&str> {
    src.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with("def ")
                || l.starts_with("async def ")
                || l.starts_with("fn ")
                || l.starts_with("pub fn ")
                || l.starts_with("function ")
        })
        .collect()
}

/// A signature that names types: `name: T` inside the parameter list, or a
/// `-> T` return type after it. The parameter list is bounded by its own
/// parentheses — a Python `def f(a):` ends in a colon that says nothing about
/// annotations, and counting it would make every signature look annotated.
fn is_annotated_def(line: &str) -> bool {
    let Some(open) = line.find('(') else {
        return false;
    };
    let Some(close) = line.rfind(')') else {
        return false;
    };
    if close <= open {
        return false;
    }
    let params = &line[open + 1..close];
    let tail = &line[close..];
    params.contains(':') || tail.contains("->")
}

fn has_docstring(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim();
        t.starts_with("\"\"\"") || t.starts_with("'''") || t.starts_with("///")
    })
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
    let common: std::collections::HashSet<&str> =
        k.iter().filter(|l| u.contains(l)).copied().collect();
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
    k_changed.into_iter().zip(u_changed).collect()
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

/// Whether a command is ambient shell vocabulary rather than project knowledge.
///
/// "`cd /tmp` is the command that works here for that task" is not a lesson --
/// it is a note that changing directory changes directory. The rule earns its
/// place only when the command encodes something a newcomer to *this* project
/// could not have guessed: a task runner, a test invocation, a build script.
/// Navigation and inspection verbs never carry that, and every one of them that
/// gets mined pushes a real rule further down the /learn list.
fn is_generic_command(cmd: &str) -> bool {
    const AMBIENT: &[&str] = &[
        "awk", "cat", "cd", "chmod", "cp", "cut", "date", "df", "diff", "du", "echo", "env",
        "export", "file", "find", "grep", "head", "hostname", "kill", "less", "ln", "ls", "mkdir",
        "mv", "nano", "open", "printf", "ps", "pwd", "rg", "rm", "sed", "sleep", "sort", "tail",
        "tee", "touch", "tr", "uniq", "vim", "wc", "which", "who", "xargs",
    ];
    let Some(head) = cmd.split_whitespace().next() else {
        return true;
    };
    if AMBIENT.binary_search(&head).is_ok() {
        return true;
    }
    // A fetcher or VCS is only interesting together with the rest of the
    // invocation; the bare verb plus a flag ("curl -s", "git status") is still
    // the shell's vocabulary rather than this project's.
    cmd.split_whitespace().count() <= 2 && matches!(head, "curl" | "wget" | "git")
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
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
    let has_inner_upper = name.chars().skip(1).any(|c| c.is_ascii_uppercase());
    let first_lower = name
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase())
        .unwrap_or(false);
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

/// Days since the Unix epoch, in UTC. Calendar-day granularity is all the
/// daily pass needs, and an integer is what makes "three distinct days" a
/// comparison rather than a date library.
pub fn today() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as u32)
        .unwrap_or(0)
}

/// `2026-09-06` for a day number, so the file a user reads shows a date.
/// Hinnant's civil-from-days, which is exact for every date koda will see.
pub fn ymd(day: u32) -> String {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The inverse of `ymd`, for reading a date back out of rules.md.
fn day_from_ymd(s: &str) -> Option<u32> {
    let mut parts = s.split('-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.trim().parse().ok()?;
    let d: i64 = parts.next()?.trim().parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u32::try_from(era * 146_097 + doe - 719_468).ok()
}

/// One rules.md line. The suffix is only written when there is something to
/// say, so a hand-written file stays hand-written.
fn rule_line(r: &Rule) -> String {
    let mut line = format!("[{}] {} — ({})", r.key, r.text, r.support);
    if r.days > 0 {
        let _ = write!(line, " · seen {}d", r.days);
    }
    if r.last_day > 0 {
        let _ = write!(line, " · last {}", ymd(r.last_day));
    }
    if r.auto {
        line.push_str(" · auto");
    }
    line
}

fn parse_rule_line(item: &str, accepted: bool) -> Option<Rule> {
    // `[key] text — (support) · seen 3d · last 2026-09-06 · auto`
    // Everything after the support count is optional, so a rules.md written by
    // an older koda (or edited by hand) still loads.
    let rest = item.strip_prefix('[')?;
    let (key, rest) = rest.split_once(']')?;
    let rest = rest.trim();
    let (rest, trailer) = match rest.split_once(" · ") {
        Some((head, tail)) => (head.trim(), tail),
        None => (rest, ""),
    };
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
    let mut days = 0;
    let mut last_day = 0;
    let mut auto = false;
    for part in trailer.split(" · ") {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("seen ") {
            days = n.trim_end_matches('d').trim().parse().unwrap_or(0);
        } else if let Some(d) = part.strip_prefix("last ") {
            last_day = day_from_ymd(d.trim()).unwrap_or(0);
        } else if part == "auto" {
            auto = true;
        }
    }
    Some(Rule {
        key: key.to_string(),
        text,
        support,
        accepted,
        days,
        last_day,
        auto,
    })
}

fn encode(obs: &Observation) -> Option<String> {
    // Minimal JSONL by hand — no serde dependency on this hot path, and the
    // format stays greppable. Strings are escaped for newlines and quotes.
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let line = match obs {
        Observation::Edit {
            path,
            before,
            after,
        } => {
            if path.trim().is_empty() {
                return None;
            }
            // Cap payloads: we only need the shape of the change, not megabytes.
            let b: String = before.chars().take(4000).collect();
            let a: String = after.chars().take(4000).collect();
            format!(
                r#"{{"t":"edit","path":"{}","before":"{}","after":"{}"}}"#,
                esc(path),
                esc(&b),
                esc(&a)
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
        Observation::Correction {
            path,
            koda_wrote,
            user_has,
        } => {
            if path.trim().is_empty() {
                return None;
            }
            let k: String = koda_wrote.chars().take(4000).collect();
            let u: String = user_has.chars().take(4000).collect();
            format!(
                r#"{{"t":"correction","path":"{}","koda_wrote":"{}","user_has":"{}"}}"#,
                esc(path),
                esc(&k),
                esc(&u)
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
            before: v
                .get("before")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            after: v
                .get("after")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
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
            koda_wrote: v
                .get("koda_wrote")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            user_has: v
                .get("user_has")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported /learn list included "`cd /tmp` is the command that works
    /// here for that task" -- a note that cd changes directory, taking up a slot
    /// a real rule could have used.
    #[test]
    fn ambient_shell_verbs_do_not_become_project_rules() {
        for noise in [
            "cd /tmp",
            "cd /Users/sridhar/research/OmniRoute",
            "curl -s",
            "grep -rE",
            "ls -la",
            "cat README.md",
            "git status",
        ] {
            assert!(
                is_generic_command(noise),
                "{noise} teaches nothing about the project"
            );
        }
        // Project-specific invocations are exactly what this should keep.
        for real in [
            "cargo test --all",
            "npm run build:release",
            "just migrate",
            "./install.sh --system",
            "curl -s localhost:20128/v1/models",
        ] {
            assert!(!is_generic_command(real), "{real} is worth remembering");
        }
    }

    /// The structural gap: a candidate was permanent once mined, so every
    /// improvement to the miner left its old mistakes sitting in /learn for
    /// ever. Re-mining must retire what it no longer produces -- except what the
    /// user has accepted, which is theirs.
    #[test]
    fn re_mining_retires_candidates_it_no_longer_produces() {
        let dir = std::env::temp_dir().join(format!("koda-learn-stale-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut l = Learning::load(&dir);

        let idioms = vec![("undefined".to_string(), "fn", 2026_usize)];
        let imports: Vec<(String, usize)> = vec![];
        assert_eq!(l.induce_idioms(&idioms, &imports), 1, "mined once");
        assert!(l.candidates().iter().any(|r| r.text.contains("undefined")));

        // A later run of a better miner no longer produces it.
        assert_eq!(l.induce_idioms(&[], &imports), 0);
        assert!(
            !l.candidates().iter().any(|r| r.text.contains("undefined")),
            "the stale candidate is gone: {:?}",
            l.candidates().iter().map(|r| &r.text).collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An accepted rule is a decision the user made; re-mining must not undo it.
    #[test]
    fn retiring_stale_candidates_leaves_accepted_rules_alone() {
        let dir = std::env::temp_dir().join(format!("koda-learn-keep-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut l = Learning::load(&dir);

        l.induce_idioms(&[("log_audit".to_string(), "fn", 9)], &[]);
        l.accept(0).expect("accepted");
        l.induce_idioms(&[], &[]);
        assert!(
            l.rules
                .iter()
                .any(|r| r.accepted && r.text.contains("log_audit")),
            "an accepted rule survives a run that would not re-mine it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

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
        l.observe(&Observation::Command {
            command: "just test".into(),
            ok: true,
        });
        l.observe(&Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn do_thing() {}".into(),
        });
        let got = l.observations();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            Observation::Command {
                command: "just test".into(),
                ok: true
            }
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// The daily pass is what makes learning happen without being asked, so the
    /// promotion rule has to be exactly "held up on N different days" — not N
    /// observations, which one busy afternoon produces on its own.
    #[test]
    fn a_rule_is_promoted_only_after_several_distinct_days() {
        let dir = std::env::temp_dir().join("koda-daily-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut l = Learning::load(&dir);
        // Evidence that a habit exists: the same command failing, its fix
        // working — repeated, which is what induce_rules needs.
        for _ in 0..3 {
            l.observe(&Observation::Command {
                command: "pytest".into(),
                ok: false,
            });
            l.observe(&Observation::Command {
                command: "python -m pytest".into(),
                ok: true,
            });
        }

        // Day one: mined, but nothing is promoted on a single day's evidence.
        let day = today();
        let r = l.daily_pass(3, 30).expect("first pass runs");
        assert!(
            r.promoted.is_empty(),
            "promoted too early: {:?}",
            r.promoted
        );
        assert!(!l.candidates().is_empty(), "nothing was mined at all");
        // Twice in one day is still one day.
        assert!(l.daily_pass(3, 30).is_none(), "the pass must be once a day");

        // Days two and three: the same evidence keeps holding up.
        for extra in 1..=2 {
            for r in l.rules.iter_mut() {
                r.last_day = day - 1;
            }
            l.mark_pass(day - 1);
            let report = l.daily_pass(3, 30).expect("a new day runs the pass");
            if extra < 2 {
                assert!(report.promoted.is_empty(), "promoted on day {}", extra + 1);
            }
        }
        assert!(
            l.rules.iter().any(|r| r.accepted && r.auto),
            "a rule seen on three days should be promoted: {:?}",
            l.rules
        );
        assert!(
            l.brief().contains("pytest"),
            "a promoted rule must reach the prompt: {}",
            l.brief()
        );
        // The journal records it in a form a person can read.
        let journal = std::fs::read_to_string(dir.join(".koda/learning/journal.md")).unwrap();
        assert!(journal.contains("learned:"), "{journal}");

        // Going quiet retires it — back to a candidate, never deleted. "Quiet"
        // means the log no longer supports it, so clear the evidence first;
        // while the observations still re-derive it, the rule is still true.
        std::fs::write(obs_path(&dir), "").unwrap();
        for r in l.rules.iter_mut() {
            r.last_day = today() - 40;
        }
        l.mark_pass(today() - 1);
        let report = l.daily_pass(3, 30).expect("runs again the next day");
        assert!(!report.retired.is_empty(), "{report:?}");
        assert!(l.rules.iter().all(|r| !r.accepted));
        assert!(
            !l.rules.is_empty(),
            "a retired rule drops to a candidate, it is not deleted"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Day tracking survives a reload, or every restart would look like day one
    /// and nothing would ever be promoted.
    #[test]
    fn day_tracking_round_trips_through_rules_md() {
        let dir = std::env::temp_dir().join("koda-daily-roundtrip");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut l = Learning::load(&dir);
        l.rules.push(Rule {
            key: "cmd.use.pytest".into(),
            text: "run tests with `python -m pytest`".into(),
            support: 4,
            accepted: true,
            days: 3,
            last_day: today(),
            auto: true,
        });
        l.dirty = true;
        l.save().unwrap();

        let back = Learning::load(&dir);
        let r = back.rules.first().expect("rule survived the round trip");
        assert_eq!(
            (r.days, r.last_day, r.auto, r.accepted),
            (3, today(), true, true)
        );
        assert_eq!(r.support, 4);
        assert_eq!(r.text, "run tests with `python -m pytest`");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Dates are written for people to read and parsed back exactly.
    #[test]
    fn day_numbers_and_dates_convert_both_ways() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(19_000), "2022-01-08");
        for day in [0u32, 1, 11_000, 19_000, 20_338, 25_000] {
            assert_eq!(day_from_ymd(&ymd(day)), Some(day), "round trip for {day}");
        }
        assert_eq!(day_from_ymd("not-a-date"), None);
    }

    #[test]
    fn induces_command_that_works_and_command_to_avoid() {
        let obs = vec![
            Observation::Command {
                command: "npm test".into(),
                ok: false,
            },
            Observation::Command {
                command: "npm test".into(),
                ok: false,
            },
            Observation::Command {
                command: "just test".into(),
                ok: true,
            },
            Observation::Command {
                command: "just test".into(),
                ok: true,
            },
        ];
        let rules = induce_rules(&obs);
        assert!(
            rules.iter().any(|r| r.key == "cmd.avoid.npm_test"),
            "{rules:?}"
        );
        assert!(
            rules.iter().any(|r| r.key == "cmd.use.just_test"),
            "{rules:?}"
        );
    }

    #[test]
    fn induces_snake_case_convention_with_a_clear_majority() {
        let obs = vec![Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn apply_discount() {}\nfn read_file() {}\nfn write_out() {}".into(),
        }];
        let rules = induce_rules(&obs);
        assert!(
            rules.iter().any(|r| r.key == "naming.fn.snake"),
            "{rules:?}"
        );
    }

    #[test]
    fn mixed_casing_asserts_no_naming_rule() {
        let obs = vec![Observation::Edit {
            path: "src/a.rs".into(),
            before: "".into(),
            after: "fn one_two() {}\nfn threeFour() {}".into(),
        }];
        let rules = induce_rules(&obs);
        assert!(
            !rules.iter().any(|r| r.key.starts_with("naming.fn")),
            "{rules:?}"
        );
    }

    #[test]
    fn induces_import_preference_above_threshold() {
        let after = "import httpx\nx = 1";
        let obs = vec![
            Observation::Edit {
                path: "a.py".into(),
                before: "".into(),
                after: after.into(),
            },
            Observation::Edit {
                path: "b.py".into(),
                before: "".into(),
                after: after.into(),
            },
            Observation::Edit {
                path: "c.py".into(),
                before: "".into(),
                after: after.into(),
            },
        ];
        let rules = induce_rules(&obs);
        assert!(
            rules.iter().any(|r| r.key == "import.prefer.httpx"),
            "{rules:?}"
        );
    }

    #[test]
    fn accept_promotes_a_candidate_into_the_brief() {
        let d = tmp("accept");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command {
            command: "just test".into(),
            ok: true,
        });
        l.observe(&Observation::Command {
            command: "just test".into(),
            ok: true,
        });
        assert!(l.induce() >= 1);
        assert!(l.brief().is_empty(), "candidates must not enter the prompt");
        assert!(l.accept(0).is_some());
        assert!(
            l.brief().contains("just test"),
            "accepted rule enters the brief"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rules_round_trip_through_the_file() {
        let d = tmp("rulefile");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command {
            command: "just build".into(),
            ok: true,
        });
        l.observe(&Observation::Command {
            command: "just build".into(),
            ok: true,
        });
        l.induce();
        l.accept_all();
        assert!(l.save().unwrap());
        let reloaded = Learning::load(&d);
        assert!(
            reloaded.brief().contains("just build"),
            "{:?}",
            reloaded.rules
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn reject_drops_a_candidate() {
        let d = tmp("reject");
        let mut l = Learning::load(&d);
        l.observe(&Observation::Command {
            command: "just test".into(),
            ok: true,
        });
        l.observe(&Observation::Command {
            command: "just test".into(),
            ok: true,
        });
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
    fn load_prunes_generic_idiom_candidates_but_keeps_accepted_rules() {
        let d = tmp("prune-generic");
        std::fs::create_dir_all(dir(&d)).unwrap();
        std::fs::write(
            rules_path(&d),
            "# koda learned rules\n\n## Accepted\n- [idiom.symbol.len] Keep accepted len. — (9)\n\n## Candidates\n- [idiom.symbol.map] noisy map — (12)\n- [idiom.symbol.log_audit] useful helper — (5)\n",
        )
        .unwrap();
        let l = Learning::load(&d);
        assert!(l
            .rules
            .iter()
            .any(|r| r.key == "idiom.symbol.len" && r.accepted));
        assert!(!l.rules.iter().any(|r| r.key == "idiom.symbol.map"));
        assert!(l.rules.iter().any(|r| r.key == "idiom.symbol.log_audit"));
        assert!(
            l.dirty,
            "pruning should rewrite the rules file on the next save"
        );
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
        assert!(
            cands
                .iter()
                .any(|t| t.contains("log_audit") && t.contains("load-bearing")),
            "{cands:?}"
        );
        assert!(
            cands.iter().any(|t| t.contains("internal_kit")),
            "{cands:?}"
        );
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
            rules
                .iter()
                .any(|r| r.text.contains("prefer `log.audit` over `logging`")),
            "{rules:?}"
        );
    }

    #[test]
    fn a_single_correction_is_learned() {
        // A deliberate user correction is the strongest signal there is, so one
        // occurrence is enough to propose a preference rule.
        let obs = vec![Observation::Correction {
            path: "a.py".into(),
            koda_wrote: "x = requests".into(),
            user_has: "x = httpx".into(),
        }];
        let rules = induce_rules(&obs);
        let rule = rules
            .iter()
            .find(|r| r.key.starts_with("correction.sub"))
            .expect("a single correction should become a candidate rule");
        assert!(
            rule.text.contains("httpx") && rule.text.contains("requests"),
            "{}",
            rule.text
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
    fn token_swaps_reports_the_identifiers_the_user_changed() {
        // One token changed: the classic library/identifier preference.
        assert_eq!(
            token_swaps("return logging.info", "return log.audit", 3),
            vec![("logging.info".to_string(), "log.audit".to_string())]
        );
        // Nothing changed.
        assert!(token_swaps("a = b", "a = b", 3).is_empty());
        // A rename that touches the name and one argument is still a shape the
        // user edited, not a rewrite: both swaps are reported.
        assert_eq!(
            token_swaps(
                "def total_price(items, tax_rate, currency, region):",
                "def invoice_total(items, tax_rate, currency, region):",
                3
            ),
            vec![("total_price".to_string(), "invoice_total".to_string())]
        );
        // A wholesale rewrite yields nothing: more than half the line changed.
        assert!(token_swaps("let a = foo(bar)", "let b = baz(qux)", 3).is_empty());
        // Different token counts mean the line's shape changed, not one name.
        assert!(token_swaps("def f(a)", "def f(a: int) -> int", 3).is_empty());
    }

    #[test]
    fn a_realistic_rewrite_still_teaches_the_users_conventions() {
        // What a correction actually looks like: the user renamed things, added
        // type annotations and a docstring. The old one-token rule learned
        // nothing from this; the conventions are the learnable part.
        let obs = vec![Observation::Correction {
            path: "src/discount.py".into(),
            koda_wrote: "def apply_percent_discount(price, percent):\n    \
                         return price * (1 - percent / 100)\n"
                .into(),
            user_has: "def apply_discount(price_cents: int, percent_off: int) -> int:\n    \
                       \"\"\"Prices are integer cents in this project.\"\"\"\n    \
                       return price_cents - (price_cents * percent_off) // 100\n"
                .into(),
        }];
        let rules = induce_rules(&obs);
        assert!(
            rules
                .iter()
                .any(|r| r.key == "correction.style.annotate_signatures"),
            "the added type annotations should be learned: {rules:?}"
        );
        assert!(
            rules.iter().any(|r| r.key == "correction.style.docstrings"),
            "the added docstring should be learned: {rules:?}"
        );
    }

    #[test]
    fn style_habits_need_evidence_and_do_not_fire_backwards() {
        let annotated = "def f(a: int) -> int:\n    return a\n";
        let bare = "def f(a):\n    return a\n";
        // koda bare -> user annotated: learn it.
        assert!(style_habits(bare, annotated)
            .iter()
            .any(|(k, _)| *k == "annotate_signatures"));
        // The reverse (the user removing annotations) is not this rule.
        assert!(!style_habits(annotated, bare)
            .iter()
            .any(|(k, _)| *k == "annotate_signatures"));
        // No docstring either way: no rule.
        assert!(!style_habits(bare, annotated)
            .iter()
            .any(|(k, _)| *k == "docstrings"));
        // Rust signatures count as annotated too, so the rule never fires there.
        let rs = "pub fn f(a: u8) -> u8 {\n    a\n}\n";
        assert!(!style_habits(rs, rs)
            .iter()
            .any(|(k, _)| *k == "annotate_signatures"));
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
