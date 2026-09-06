//! Watch mode: aider-style inline triggers.
//!
//! When enabled, koda periodically scans the workspace's text files for comment
//! lines that end with a trigger token:
//!
//!   `AI!` — implement the request right here (a code-writing turn on this file)
//!   `AI?` — answer the question (a read-only explanation)
//!
//! Example (Python):
//!
//!   # implement binary search over `items` returning the index or -1  AI!
//!
//! On the next scan koda picks that up, sends a turn scoped to the file with the
//! instruction, and the agent edits the file — including removing the trigger
//! comment so it does not fire again. A trigger is remembered by (path, line
//! text) so an unchanged file is never re-processed.

use ignore::WalkBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What kind of action a trigger asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `AI!` — make the change.
    Do,
    /// `AI?` — answer the question.
    Ask,
}

#[derive(Debug, Clone)]
pub struct Trigger {
    pub path: PathBuf,
    pub kind: Kind,
    /// The instruction text (the comment with the trigger token stripped).
    pub instruction: String,
    /// 1-based line the trigger was found on.
    pub line: usize,
    /// The full raw line, used to de-duplicate across scans.
    pub raw: String,
}

/// Remembers which triggers have already been dispatched, so the same comment
/// isn't acted on twice while it still sits in the file (the agent removes it,
/// but there's a window between dispatch and the edit landing).
#[derive(Default)]
pub struct Watcher {
    seen: HashSet<String>,
    /// When non-empty, only these files are watched (set via `/watch @file`).
    /// Empty means watch the whole workspace (the original behaviour).
    watched: Vec<PathBuf>,
    /// (mtime, size) per file at the last sweep. A file whose stamp is
    /// unchanged cannot have gained a trigger, so it is never re-read.
    ///
    /// This is what makes watch mode affordable: the scan runs every few
    /// hundred milliseconds, and it used to read *every* text file in the
    /// workspace each time — the whole project, several times a minute.
    stamps: HashMap<PathBuf, Stamp>,
    /// How many files this watcher has actually read, for the test that keeps
    /// the sweep incremental.
    #[cfg(test)]
    reads: std::cell::Cell<usize>,
    /// Triggers found per file, refreshed only when that file changes. Keeping
    /// them means skipping a file does not lose the triggers it holds: a sweep
    /// still considers every known trigger, it just does not re-read the file
    /// to rediscover them.
    found: HashMap<PathBuf, Vec<Trigger>>,
}

/// A file's identity for change detection: modification time and size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime: std::time::SystemTime,
    len: u64,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Files read so far. Test-only: the point of the stamp cache is that this
    /// stops growing once the tree is quiet.
    #[cfg(test)]
    pub fn reads(&self) -> usize {
        self.reads.get()
    }

    /// Add a specific file to the watch list. Returns false if already present.
    pub fn watch_path(&mut self, path: PathBuf) -> bool {
        if self.watched.contains(&path) {
            return false;
        }
        self.watched.push(path);
        true
    }

    /// Clear the scoped watch list (`/unwatch`). After this, scanning falls back
    /// to the whole workspace (until the caller also disables watch mode).
    pub fn clear_paths(&mut self) {
        self.watched.clear();
    }

    /// How many specific files are being watched.
    #[allow(dead_code)]
    pub fn watched_count(&self) -> usize {
        self.watched.len()
    }

    /// Mark a trigger dispatched so the next scan skips it.
    pub fn mark(&mut self, t: &Trigger) {
        self.seen.insert(Self::key(t));
    }

    fn key(t: &Trigger) -> String {
        format!("{}::{}", t.path.display(), t.raw.trim())
    }

    /// Scan and return the first not-yet-dispatched trigger. When a scoped watch
    /// list is set, only those files are checked; otherwise the whole workspace.
    pub fn scan(&mut self, root: &Path) -> Option<Trigger> {
        let triggers = if self.watched.is_empty() {
            self.sweep(root)
        } else {
            let watched = self.watched.clone();
            let mut out = Vec::new();
            for p in &watched {
                out.extend(self.triggers_in(p));
            }
            out
        };
        triggers
            .into_iter()
            .find(|t| !self.seen.contains(&Self::key(t)))
    }

    /// Walk the workspace and return every known trigger, re-reading only the
    /// files that changed since the last sweep.
    fn sweep(&mut self, root: &Path) -> Vec<Trigger> {
        let walk = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .max_filesize(Some(512 * 1024))
            .build();
        let mut out = Vec::new();
        let mut live: HashSet<PathBuf> = HashSet::new();
        for dent in walk.flatten() {
            if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = dent.path();
            if !is_texty(path) {
                continue;
            }
            live.insert(path.to_path_buf());
            out.extend(self.triggers_in(path));
        }
        // Forget files that are gone, so the maps track the tree rather than
        // growing for the life of the session.
        self.stamps.retain(|p, _| live.contains(p));
        self.found.retain(|p, _| live.contains(p));
        out
    }

    /// The triggers in one file, reading it only if its stamp moved.
    fn triggers_in(&mut self, path: &Path) -> Vec<Trigger> {
        let stamp = std::fs::metadata(path).ok().and_then(|m| {
            Some(Stamp {
                mtime: m.modified().ok()?,
                len: m.len(),
            })
        });
        if let (Some(stamp), Some(cached)) = (stamp, self.found.get(path)) {
            if self.stamps.get(path) == Some(&stamp) {
                return cached.clone();
            }
        }
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        let triggers = scan_file(path);
        if let Some(stamp) = stamp {
            self.stamps.insert(path.to_path_buf(), stamp);
        }
        self.found.insert(path.to_path_buf(), triggers.clone());
        triggers
    }
}

/// Detect a trigger token at the end of a line. Returns the kind and the
/// instruction (line with comment markers and the token removed), or None.
pub fn detect(line: &str) -> Option<(Kind, String)> {
    let trimmed = line.trim_end();
    let (kind, without_token) = match strip_token(trimmed, "AI!") {
        Some(rest) => (Kind::Do, rest),
        // Neither token: `?` ends it here, which is what the else-return did.
        None => (Kind::Ask, strip_token(trimmed, "AI?")?),
    };
    let instruction = clean_comment(without_token);
    if instruction.is_empty() {
        return None;
    }
    Some((kind, instruction))
}

/// Strip a trailing trigger token (case-insensitive), requiring it to be its own
/// word at the very end so `HAIKU!` or `email?` never match.
fn strip_token<'a>(line: &'a str, token: &str) -> Option<&'a str> {
    let lower = line.to_ascii_lowercase();
    let tok = token.to_ascii_lowercase();
    let rest = lower.strip_suffix(&tok)?;
    // The char before the token must be whitespace (or start of line), so the
    // token stands alone.
    if rest
        .chars()
        .next_back()
        .map(|c| !c.is_whitespace())
        .unwrap_or(false)
    {
        return None;
    }
    Some(&line[..rest.len()])
}

/// Remove leading comment markers and surrounding punctuation so the model gets
/// a clean imperative instruction.
fn clean_comment(s: &str) -> String {
    let s = s.trim();
    // Common comment prefixes across languages.
    let markers = ["///", "//", "#", "--", ";", "/*", "*", "<!--"];
    let mut out = s;
    loop {
        let before = out;
        out = out.trim_start();
        for m in markers {
            if let Some(rest) = out.strip_prefix(m) {
                out = rest;
            }
        }
        if out == before {
            break;
        }
    }
    out.trim()
        .trim_end_matches("-->")
        .trim_end_matches("*/")
        .trim()
        .to_string()
}

/// Scan a single file for triggers (used by scoped `/watch @file`).
fn scan_file(path: &Path) -> Vec<Trigger> {
    let mut out = Vec::new();
    if !path.is_file() || !is_texty(path) {
        return out;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for (i, line) in text.lines().enumerate() {
        if let Some((kind, instruction)) = detect(line) {
            out.push(Trigger {
                path: path.to_path_buf(),
                kind,
                instruction,
                line: i + 1,
                raw: line.to_string(),
            });
        }
    }
    out
}

/// A conservative allowlist of source/text extensions worth scanning. Skips
/// binaries and lockfiles so a big repo scan stays cheap.
fn is_texty(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    const EXTS: &[&str] = &[
        "rs", "py", "pyi", "js", "mjs", "cjs", "jsx", "ts", "tsx", "mts", "cts", "go", "java",
        "kt", "kts", "swift", "rb", "c", "h", "cc", "cpp", "hpp", "cxx", "hxx", "cs", "scala",
        "dart", "ex", "exs", "erl", "hs", "zig", "jl", "r", "php", "lua", "sh", "bash", "sql",
        "html", "css", "scss", "vue", "svelte", "toml", "yaml", "yml", "json", "md", "txt",
    ];
    EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Compose the user-turn text sent to the agent for a trigger.
pub fn turn_text(t: &Trigger, root: &Path) -> String {
    let rel = t.path.strip_prefix(root).unwrap_or(&t.path).display();
    match t.kind {
        Kind::Do => format!(
            "Watch trigger in {rel} (line {}): {}\n\nImplement this in that file. \
             Read it first, make the change, and remove the `AI!` trigger comment \
             (line {}) as part of your edit so it does not fire again.",
            t.line, t.instruction, t.line
        ),
        Kind::Ask => format!(
            "Watch question in {rel} (line {}): {}\n\nAnswer it. You may read the \
             file for context. Do not edit files; this is a question (`AI?`).",
            t.line, t.instruction
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_do_trigger_and_strips_comment() {
        let (k, ins) = detect("# implement binary search  AI!").unwrap();
        assert_eq!(k, Kind::Do);
        assert_eq!(ins, "implement binary search");
    }

    #[test]
    fn detects_ask_trigger() {
        let (k, ins) = detect("// what does this function return? AI?").unwrap();
        assert_eq!(k, Kind::Ask);
        assert_eq!(ins, "what does this function return?");
    }

    #[test]
    fn strips_various_comment_markers() {
        assert_eq!(detect("/// add docs AI!").unwrap().1, "add docs");
        assert_eq!(detect("-- add a column AI!").unwrap().1, "add a column");
        // The trigger token must be the last token on the line (aider's rule),
        // so a trailing block-comment close means no match.
        assert!(detect("<!-- fix the layout AI! -->").is_none());
        assert_eq!(
            detect("<!-- fix the layout AI!").unwrap().1,
            "fix the layout"
        );
    }

    #[test]
    fn ignores_lines_without_a_standalone_token() {
        assert!(detect("let x = HAIKU!").is_none());
        assert!(detect("send the email?").is_none());
        assert!(detect("just a normal comment").is_none());
    }

    #[test]
    fn empty_instruction_is_not_a_trigger() {
        assert!(detect("# AI!").is_none());
    }

    /// Watch mode polls every few hundred milliseconds. Re-reading the whole
    /// workspace each time is what made it expensive, so an unchanged file must
    /// not be read twice — while a trigger in it is still found.
    #[test]
    fn an_unchanged_file_is_not_read_again() {
        let dir = std::env::temp_dir().join(format!("koda-watch-perf-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..40 {
            std::fs::write(dir.join(format!("f{i}.rs")), "fn main() {}\n").unwrap();
        }
        std::fs::write(dir.join("hot.rs"), "// add a docstring here  AI!\n").unwrap();

        let mut w = Watcher::new();
        let first = w
            .scan(&dir)
            .expect("the trigger is found on the first sweep");
        assert_eq!(first.kind, Kind::Do);
        w.mark(&first);

        // Second sweep: nothing changed, so nothing is re-read — and the
        // trigger is still known (it is simply already seen).
        let before = w.reads();
        assert!(
            w.scan(&dir).is_none(),
            "an already-dispatched trigger fired twice"
        );
        assert_eq!(w.reads(), before, "an unchanged sweep read files again");

        // A file that actually changes is picked up.
        std::fs::write(dir.join("f3.rs"), "// explain this function  AI?\n").unwrap();
        let next = w.scan(&dir).expect("a changed file is re-read");
        assert_eq!(next.kind, Kind::Ask);
        assert!(w.reads() > before, "the changed file was not read");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watcher_dedups_seen_triggers() {
        let mut w = Watcher::new();
        let t = Trigger {
            path: PathBuf::from("a.rs"),
            kind: Kind::Do,
            instruction: "do it".into(),
            line: 1,
            raw: "// do it AI!".into(),
        };
        assert!(!w.seen.contains(&Watcher::key(&t)));
        w.mark(&t);
        assert!(w.seen.contains(&Watcher::key(&t)));
    }

    #[test]
    fn scoped_watch_only_scans_listed_files() {
        let dir = std::env::temp_dir().join("koda-watch-scoped");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("watched.py"), "# implement foo  AI!\n").unwrap();
        std::fs::write(dir.join("other.py"), "# implement bar  AI!\n").unwrap();

        let mut w = Watcher::new();
        // No scope: whole-workspace scan finds a trigger.
        assert!(w.scan(&dir).is_some());

        // Scope to just watched.py: only its trigger is seen.
        assert!(w.watch_path(dir.join("watched.py")));
        assert!(!w.watch_path(dir.join("watched.py")), "dedup");
        assert_eq!(w.watched_count(), 1);
        let hit = w.scan(&dir).expect("scoped scan finds the watched file");
        assert!(hit.path.ends_with("watched.py"), "{:?}", hit.path);

        // Clearing the scope returns to whole-workspace behaviour.
        w.clear_paths();
        assert_eq!(w.watched_count(), 0);
        assert!(w.scan(&dir).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
