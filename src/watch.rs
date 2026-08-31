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
use std::collections::HashSet;
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
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a trigger dispatched so the next scan skips it.
    pub fn mark(&mut self, t: &Trigger) {
        self.seen.insert(Self::key(t));
    }

    fn key(t: &Trigger) -> String {
        format!("{}::{}", t.path.display(), t.raw.trim())
    }

    /// Scan the workspace and return the first not-yet-dispatched trigger, if
    /// any. One at a time keeps turns ordered and the transcript readable.
    pub fn scan(&self, root: &Path) -> Option<Trigger> {
        for t in scan_all(root) {
            if !self.seen.contains(&Self::key(&t)) {
                return Some(t);
            }
        }
        None
    }
}

/// Detect a trigger token at the end of a line. Returns the kind and the
/// instruction (line with comment markers and the token removed), or None.
pub fn detect(line: &str) -> Option<(Kind, String)> {
    let trimmed = line.trim_end();
    let (kind, without_token) = if let Some(rest) = strip_token(trimmed, "AI!") {
        (Kind::Do, rest)
    } else if let Some(rest) = strip_token(trimmed, "AI?") {
        (Kind::Ask, rest)
    } else {
        return None;
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
    if rest.chars().next_back().map(|c| !c.is_whitespace()).unwrap_or(false) {
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
    out.trim().trim_end_matches("-->").trim_end_matches("*/").trim().to_string()
}

/// Walk the workspace (respecting .gitignore) and collect every trigger.
fn scan_all(root: &Path) -> Vec<Trigger> {
    let mut out = Vec::new();
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .max_filesize(Some(512 * 1024))
        .build();
    for dent in walk.flatten() {
        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = dent.path();
        if !is_texty(path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
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
        assert_eq!(detect("<!-- fix the layout AI!").unwrap().1, "fix the layout");
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
}
