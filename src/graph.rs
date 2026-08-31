//! A code graph built by scanning the project once on open.
//!
//! The point is to answer "where is X defined and who uses it" without the model
//! spending five tool calls and a few thousand tokens grepping for it. Two
//! passes: collect definitions, then collect the identifiers each file mentions
//! and join. That is O(files), not O(symbols × files).
//!
//! It is deliberately regex-based rather than a real parser. A parser per
//! language would be far heavier than the value here: the graph only needs to be
//! right often enough to point the model at the right file, which it then reads
//! properly.

use crate::tel_info;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

const MAX_FILES: usize = 4000;
const MAX_FILE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    /// "fn", "struct", "class", "type", ...
    pub kind: &'static str,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Graph {
    /// symbol name -> definitions (a name can be defined in several places)
    pub defs: BTreeMap<String, Vec<Def>>,
    /// file -> symbols it defines
    pub by_file: BTreeMap<String, Vec<String>>,
    /// symbol name -> files that mention it (excluding its own definition file)
    pub refs: BTreeMap<String, BTreeSet<String>>,
    /// file -> modules/paths it imports
    pub imports: BTreeMap<String, Vec<String>>,
    /// language -> file count
    pub languages: BTreeMap<String, usize>,
    pub files: usize,
    pub scanned_ms: u128,
    pub truncated: bool,
}

fn language_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" | "hxx" | "hh" => "cpp",
        "cs" => "csharp",
        "scala" | "sc" => "scala",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "hs" => "haskell",
        "zig" => "zig",
        "jl" => "julia",
        "r" => "r",
        "pl" | "pm" => "perl",
        "m" | "mm" => "objc",
        "sh" | "bash" | "zsh" => "shell",
        "lua" => "lua",
        "php" => "php",
        "vue" | "svelte" => "javascript",
        _ => return None,
    })
}

/// Definition patterns per language. Each returns (kind, name) for a line.
fn definitions(lang: &str, line: &str) -> Option<(&'static str, String)> {
    let t = line.trim_start();
    let take = |rest: &str| -> Option<String> {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    // Keyword-prefixed forms cover most of what matters, across languages.
    let table: &[(&str, &'static str)] = match lang {
        "rust" => &[
            ("pub fn ", "fn"), ("fn ", "fn"),
            ("pub struct ", "struct"), ("struct ", "struct"),
            ("pub enum ", "enum"), ("enum ", "enum"),
            ("pub trait ", "trait"), ("trait ", "trait"),
            ("pub type ", "type"), ("type ", "type"),
            ("pub const ", "const"), ("const ", "const"),
            ("pub static ", "static"), ("static ", "static"),
            ("macro_rules! ", "macro"),
        ],
        "python" => &[("def ", "fn"), ("async def ", "fn"), ("class ", "class")],
        "javascript" | "typescript" => &[
            ("export function ", "fn"), ("async function ", "fn"), ("function ", "fn"),
            ("export class ", "class"), ("class ", "class"),
            ("export interface ", "interface"), ("interface ", "interface"),
            ("export type ", "type"), ("type ", "type"),
            ("export const ", "const"), ("const ", "const"),
            ("export enum ", "enum"), ("enum ", "enum"),
        ],
        "go" => &[("func ", "fn"), ("type ", "type")],
        "java" | "kotlin" | "swift" => &[
            ("public class ", "class"), ("class ", "class"),
            ("public interface ", "interface"), ("interface ", "interface"),
            ("struct ", "struct"), ("enum ", "enum"),
            ("func ", "fn"), ("fun ", "fn"),
        ],
        "csharp" => &[
            ("public class ", "class"), ("internal class ", "class"), ("class ", "class"),
            ("public interface ", "interface"), ("interface ", "interface"),
            ("public struct ", "struct"), ("struct ", "struct"),
            ("public enum ", "enum"), ("enum ", "enum"),
            ("public record ", "record"), ("record ", "record"),
            ("namespace ", "module"),
        ],
        "scala" => &[
            ("def ", "fn"), ("class ", "class"), ("object ", "object"),
            ("trait ", "trait"), ("case class ", "class"), ("type ", "type"),
        ],
        "dart" => &[
            ("class ", "class"), ("mixin ", "mixin"), ("enum ", "enum"),
            ("abstract class ", "class"),
        ],
        "elixir" => &[
            ("def ", "fn"), ("defp ", "fn"), ("defmodule ", "module"),
            ("defmacro ", "macro"), ("defstruct ", "struct"),
        ],
        "erlang" => &[("-module(", "module"), ("-record(", "record")],
        "haskell" => &[("data ", "type"), ("newtype ", "type"), ("type ", "type"), ("class ", "class")],
        "zig" => &[("pub fn ", "fn"), ("fn ", "fn"), ("const ", "const")],
        "julia" => &[("function ", "fn"), ("struct ", "struct"), ("module ", "module"), ("abstract type ", "type")],
        "r" => &[],
        "perl" => &[("sub ", "fn"), ("package ", "module")],
        "objc" => &[("@interface ", "class"), ("@implementation ", "class"), ("@protocol ", "interface")],
        "ruby" => &[("def ", "fn"), ("class ", "class"), ("module ", "module")],
        "c" | "cpp" => &[
            ("class ", "class"), ("struct ", "struct"),
            ("typedef struct ", "struct"), ("enum ", "enum"),
        ],
        "shell" => &[("function ", "fn")],
        "lua" => &[("local function ", "fn"), ("function ", "fn")],
        "php" => &[("function ", "fn"), ("class ", "class"), ("interface ", "interface"), ("trait ", "trait")],
        _ => &[],
    };
    for (prefix, kind) in table {
        if let Some(rest) = t.strip_prefix(prefix) {
            if let Some(name) = take(rest) {
                return Some((kind, name));
            }
        }
    }
    // `name() {` style definitions in shell, and `name = function` in lua/js.
    if lang == "shell" {
        if let Some(idx) = t.find("()") {
            if let Some(name) = take(&t[..idx]) {
                if t[idx..].contains('{') {
                    return Some(("fn", name));
                }
            }
        }
    }
    None
}

fn import_of(lang: &str, line: &str) -> Option<String> {
    let t = line.trim();
    match lang {
        "rust" => t
            .strip_prefix("use ")
            .map(|r| r.trim_end_matches(';').trim().to_string()),
        "python" => {
            if let Some(r) = t.strip_prefix("from ") {
                r.split_whitespace().next().map(|s| s.to_string())
            } else {
                t.strip_prefix("import ")
                    .and_then(|r| r.split_whitespace().next())
                    .map(|s| s.trim_end_matches(',').to_string())
            }
        }
        "javascript" | "typescript" => {
            let quoted = |s: &str| -> Option<String> {
                let start = s.find(['"', '\''])?;
                let quote = s.as_bytes()[start] as char;
                let rest = &s[start + 1..];
                let end = rest.find(quote)?;
                Some(rest[..end].to_string())
            };
            if t.starts_with("import ") || t.contains("require(") {
                quoted(t)
            } else {
                None
            }
        }
        "go" => {
            let t = t.trim_start_matches("import ").trim();
            (t.starts_with('"') && t.len() > 2).then(|| t.trim_matches('"').to_string())
        }
        _ => None,
    }
}

/// Identifiers mentioned in a line, for the reference pass.
fn identifiers(line: &str, out: &mut BTreeSet<String>) {
    let mut cur = String::new();
    for c in line.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            if cur.len() > 2 && !cur.chars().all(|c| c.is_ascii_digit()) {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() > 2 {
        out.insert(cur);
    }
}

/// The per-file result of a parse, so files can be parsed in parallel and then
/// merged into the graph on one thread (merging is cheap; parsing is the cost).
struct FileParse {
    rel: String,
    lang: &'static str,
    defs: Vec<(String, &'static str, usize)>, // (name, kind, line)
    imports: Vec<String>,
    ids: BTreeSet<String>,
}

/// Parse one file's bytes into definitions, imports and mentioned identifiers.
/// Pure and self-contained, so it is safe to run on a worker thread.
fn parse_file(rel: String, lang: &'static str, text: &str) -> FileParse {
    let mut defs = Vec::new();
    let mut imports = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        if let Some((kind, name)) = definitions(lang, line) {
            defs.push((name, kind, i + 1));
        }
        if let Some(imp) = import_of(lang, line) {
            imports.push(imp);
        }
        identifiers(line, &mut ids);
    }
    FileParse { rel, lang, defs, imports, ids }
}

/// Parse many files across worker threads and return the results. Uses scoped
/// threads (no dependency) sized to the machine's parallelism; small inputs run
/// inline to avoid thread-spawn overhead.
fn parse_in_parallel(inputs: Vec<(String, &'static str, String)>) -> Vec<FileParse> {
    let n = inputs.len();
    let workers = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .min(8);
    if n < 32 || workers <= 1 {
        // Not worth the threads: parse inline.
        return inputs
            .into_iter()
            .map(|(rel, lang, text)| parse_file(rel, lang, &text))
            .collect();
    }
    // Split into `workers` contiguous chunks, parse each on its own thread.
    let chunk = n.div_ceil(workers);
    let chunks: Vec<Vec<(String, &'static str, String)>> = {
        let mut v = Vec::new();
        let mut it = inputs.into_iter();
        for _ in 0..workers {
            let c: Vec<_> = it.by_ref().take(chunk).collect();
            if c.is_empty() {
                break;
            }
            v.push(c);
        }
        v
    };
    let mut out = Vec::with_capacity(n);
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|c| {
                s.spawn(move || {
                    c.into_iter()
                        .map(|(rel, lang, text)| parse_file(rel, lang, &text))
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
    out
}

/// Walk the project and build the graph. Blocking: callers run it off-thread.
pub fn scan(root: &Path) -> Graph {
    let started = Instant::now();
    let mut g = Graph::default();

    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .git_global(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "target")
        .build();

    // Phase 1 (I/O, single thread): walk the tree and read eligible files.
    let mut inputs: Vec<(String, &'static str, String)> = Vec::new();
    for entry in walk.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(lang) = language_of(entry.path()) else {
            continue;
        };
        if inputs.len() >= MAX_FILES {
            g.truncated = true;
            break;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.len() > MAX_FILE_BYTES || bytes.iter().take(4000).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();
        inputs.push((rel, lang, text));
    }

    // Phase 2 (CPU, parallel): parse files across worker threads. Parsing is the
    // hot cost; splitting it over the cores turns a large repo scan from serial
    // into ~linear-speedup. Merging (phase 3) is cheap and stays single-threaded.
    let parsed: Vec<FileParse> = parse_in_parallel(inputs);

    // Phase 3 (merge): fold the per-file results into the graph in a stable order.
    let mut mentions: Vec<(String, BTreeSet<String>)> = Vec::with_capacity(parsed.len());
    for fp in parsed {
        g.files += 1;
        *g.languages.entry(fp.lang.to_string()).or_insert(0) += 1;
        for (name, kind, line) in fp.defs {
            g.defs.entry(name.clone()).or_default().push(Def {
                name: name.clone(),
                kind,
                file: fp.rel.clone(),
                line,
            });
            g.by_file.entry(fp.rel.clone()).or_default().push(name);
        }
        if !fp.imports.is_empty() {
            g.imports.entry(fp.rel.clone()).or_default().extend(fp.imports);
        }
        mentions.push((fp.rel, fp.ids));
    }

    // Second pass: join mentions against known definitions.
    let known: HashMap<&str, ()> = g.defs.keys().map(|k| (k.as_str(), ())).collect();
    for (file, ids) in &mentions {
        for id in ids {
            if known.contains_key(id.as_str()) {
                let defined_here = g
                    .defs
                    .get(id)
                    .map(|ds| ds.iter().any(|d| &d.file == file))
                    .unwrap_or(false);
                if !defined_here {
                    g.refs.entry(id.clone()).or_default().insert(file.clone());
                }
            }
        }
    }

    g.scanned_ms = started.elapsed().as_millis();
    tel_info!(
        "graph",
        "project scanned",
        "files" => g.files,
        "symbols" => g.defs.len(),
        "ms" => g.scanned_ms,
    );
    g
}

impl Graph {
    /// Drop everything the graph knows about one file — its definitions, its
    /// by-file and imports entries, and its contribution to the reference sets.
    /// The inverse of folding a `FileParse` in, used before re-adding an edited
    /// file so the graph stays consistent without a full rescan.
    pub fn remove_file(&mut self, rel: &str) {
        // Definitions defined in this file.
        if let Some(names) = self.by_file.remove(rel) {
            for name in names {
                if let Some(defs) = self.defs.get_mut(&name) {
                    defs.retain(|d| d.file != rel);
                    if defs.is_empty() {
                        self.defs.remove(&name);
                    }
                }
            }
        }
        self.imports.remove(rel);
        // This file's mentions of other symbols.
        for files in self.refs.values_mut() {
            files.remove(rel);
        }
        self.refs.retain(|_, files| !files.is_empty());
    }

    /// Re-index a single file after it changed on disk (koda's own edit, say),
    /// so `codegraph` answers stay current between full scans. Cheap: it removes
    /// the old entries and folds the freshly parsed ones back in. A deleted or
    /// unreadable file is just removed. Language must be recognised.
    pub fn update_file(&mut self, root: &Path, abs_path: &Path) {
        let rel = abs_path
            .strip_prefix(root)
            .unwrap_or(abs_path)
            .to_string_lossy()
            .to_string();
        self.remove_file(&rel);
        let Some(lang) = language_of(abs_path) else {
            return;
        };
        let Ok(bytes) = std::fs::read(abs_path) else {
            return; // deleted/unreadable: leave it removed
        };
        if bytes.len() > MAX_FILE_BYTES || bytes.iter().take(4000).any(|b| *b == 0) {
            return;
        }
        let text = String::from_utf8_lossy(&bytes);
        let fp = parse_file(rel.clone(), lang, &text);

        for (name, kind, line) in fp.defs {
            self.defs.entry(name.clone()).or_default().push(Def {
                name: name.clone(),
                kind,
                file: rel.clone(),
                line,
            });
            self.by_file.entry(rel.clone()).or_default().push(name);
        }
        if !fp.imports.is_empty() {
            self.imports.entry(rel.clone()).or_default().extend(fp.imports);
        }
        // Rebuild this file's references against the (updated) definition set.
        for id in &fp.ids {
            if self.defs.contains_key(id) {
                let defined_here = self
                    .defs
                    .get(id)
                    .map(|ds| ds.iter().any(|d| d.file == rel))
                    .unwrap_or(false);
                if !defined_here {
                    self.refs.entry(id.clone()).or_default().insert(rel.clone());
                }
            }
        }
    }

    /// A short map of the project for the model to orient itself.
    pub fn overview(&self) -> String {
        if self.files == 0 {
            return "The code graph is empty — no recognised source files.".into();
        }
        let mut out = format!(
            "{} files, {} symbols, scanned in {}ms.\n\nLanguages:\n",
            self.files,
            self.defs.len(),
            self.scanned_ms
        );
        let mut langs: Vec<(&String, &usize)> = self.languages.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1));
        for (lang, n) in langs {
            let _ = writeln!(out, "- {lang}: {n} files");
        }

        // Most-referenced symbols are the load-bearing ones.
        let mut hot: Vec<(&String, usize)> =
            self.refs.iter().map(|(k, v)| (k, v.len())).collect();
        hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        if !hot.is_empty() {
            out.push_str("\nMost referenced:\n");
            for (name, n) in hot.iter().take(12) {
                let where_ = self
                    .defs
                    .get(*name)
                    .and_then(|d| d.first())
                    .map(|d| format!("{}:{}", d.file, d.line))
                    .unwrap_or_default();
                let _ = writeln!(out, "- {name} ({n} files) {where_}");
            }
        }

        let mut biggest: Vec<(&String, usize)> =
            self.by_file.iter().map(|(k, v)| (k, v.len())).collect();
        biggest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        if !biggest.is_empty() {
            out.push_str("\nFiles defining the most:\n");
            for (file, n) in biggest.iter().take(10) {
                let _ = writeln!(out, "- {file} ({n} symbols)");
            }
        }
        if self.truncated {
            out.push_str("\n(Scan hit the file cap; the graph is partial.)\n");
        }
        out
    }

    /// Where a symbol is defined and which files mention it.
    pub fn symbol(&self, name: &str) -> String {
        let name = name.trim();
        let Some(defs) = self.defs.get(name) else {
            // Offer near matches: the model often guesses a name slightly wrong.
            let lower = name.to_ascii_lowercase();
            let mut near: Vec<&String> = self
                .defs
                .keys()
                .filter(|k| k.to_ascii_lowercase().contains(&lower))
                .take(12)
                .collect();
            near.sort();
            if near.is_empty() {
                return format!("`{name}` is not in the code graph.");
            }
            let list: Vec<&str> = near.iter().map(|s| s.as_str()).collect();
            return format!(
                "No symbol named `{name}`. Similar: {}",
                list.join(", ")
            );
        };
        let mut out = format!("`{name}` — defined in:\n");
        for d in defs {
            let _ = writeln!(out, "- {}:{} ({})", d.file, d.line, d.kind);
        }
        match self.refs.get(name) {
            Some(files) if !files.is_empty() => {
                let _ = writeln!(out, "\nMentioned in {} other file(s):", files.len());
                for f in files.iter().take(25) {
                    let _ = writeln!(out, "- {f}");
                }
                if files.len() > 25 {
                    let _ = writeln!(out, "- … {} more", files.len() - 25);
                }
            }
            _ => out.push_str("\nNot mentioned outside its own file.\n"),
        }
        out
    }

    /// What a file defines, imports, and who depends on it.
    pub fn file(&self, path: &str) -> String {
        let path = path.trim().trim_start_matches("./");
        let key = self
            .by_file
            .keys()
            .find(|k| k.as_str() == path)
            .or_else(|| self.by_file.keys().find(|k| k.ends_with(path)))
            .cloned();
        let Some(key) = key else {
            return format!("`{path}` has no entry in the code graph.");
        };
        let mut out = format!("{key}\n");
        if let Some(syms) = self.by_file.get(&key) {
            let _ = writeln!(out, "\nDefines ({}):", syms.len());
            for s in syms.iter().take(60) {
                let kind = self
                    .defs
                    .get(s)
                    .and_then(|d| d.iter().find(|d| d.file == key))
                    .map(|d| format!(" ({} line {})", d.kind, d.line))
                    .unwrap_or_default();
                let _ = writeln!(out, "- {s}{kind}");
            }
        }
        if let Some(imps) = self.imports.get(&key) {
            let mut uniq: Vec<&String> = imps.iter().collect();
            uniq.sort();
            uniq.dedup();
            let _ = writeln!(out, "\nImports ({}):", uniq.len());
            for i in uniq.iter().take(40) {
                let _ = writeln!(out, "- {i}");
            }
        }
        // Who uses what this file defines.
        let mut users: BTreeSet<&String> = BTreeSet::new();
        if let Some(syms) = self.by_file.get(&key) {
            for s in syms {
                if let Some(files) = self.refs.get(s) {
                    users.extend(files.iter());
                }
            }
        }
        if !users.is_empty() {
            let _ = writeln!(out, "\nUsed by ({}):", users.len());
            for u in users.iter().take(30) {
                let _ = writeln!(out, "- {u}");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test gets its own directory: they run in parallel.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("koda-graph-{tag}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "use std::io;\npub struct Widget;\npub fn build_widget() -> Widget { Widget }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "use crate::lib;\nfn main() {\n    let w = build_widget();\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("app.py"), "import os\n\nclass Thing:\n    def run(self):\n        pass\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(dir.join("vendor/skip.rs"), "pub fn hidden() {}\n").unwrap();
        dir
    }

    #[test]
    fn incremental_update_reindexes_one_file() {
        let dir = fixture("incremental");
        let mut g = scan(&dir);
        assert!(g.defs.contains_key("build_widget"));
        // Rewrite lib.rs: rename the function; the graph must forget the old
        // symbol and know the new one after a single-file update.
        std::fs::write(
            dir.join("src/lib.rs"),
            "use std::io;\npub struct Widget;\npub fn make_widget() -> Widget { Widget }\n",
        )
        .unwrap();
        g.update_file(&dir, &dir.join("src/lib.rs"));
        assert!(!g.defs.contains_key("build_widget"), "old symbol should be gone");
        assert!(g.defs.contains_key("make_widget"), "new symbol should be indexed");
        assert!(g.defs.contains_key("Widget"), "unchanged symbol kept");
        // Deleting a file removes its symbols.
        std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
        g.update_file(&dir, &dir.join("src/lib.rs"));
        assert!(!g.defs.contains_key("make_widget"), "deleted file's symbols gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_definitions_in_more_languages() {
        assert_eq!(definitions("csharp", "public record User(int Id)"), Some(("record", "User".into())));
        assert_eq!(definitions("scala", "object Main {"), Some(("object", "Main".into())));
        assert_eq!(definitions("elixir", "defmodule App do"), Some(("module", "App".into())));
        assert_eq!(definitions("dart", "class Widget {"), Some(("class", "Widget".into())));
        assert_eq!(definitions("zig", "pub fn main() void {"), Some(("fn", "main".into())));
        assert_eq!(definitions("perl", "sub run {"), Some(("fn", "run".into())));
        assert_eq!(definitions("php", "trait Loggable {"), Some(("trait", "Loggable".into())));
    }

    #[test]
    fn finds_definitions_across_languages() {
        let dir = fixture("defs");
        let g = scan(&dir);
        assert!(g.defs.contains_key("Widget"), "{:?}", g.defs.keys());
        assert!(g.defs.contains_key("build_widget"));
        assert!(g.defs.contains_key("Thing"));
        assert!(g.defs.contains_key("run"));
        // gitignored files stay out.
        assert!(!g.defs.contains_key("hidden"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_references_but_not_self_definitions() {
        let dir = fixture("refs");
        let g = scan(&dir);
        let refs = g.refs.get("build_widget").expect("should be referenced");
        assert!(refs.iter().any(|f| f.ends_with("main.rs")), "{refs:?}");
        assert!(
            !refs.iter().any(|f| f.ends_with("lib.rs")),
            "definition file must not count as a reference"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_imports() {
        let dir = fixture("imports");
        let g = scan(&dir);
        let lib = g
            .imports
            .iter()
            .find(|(k, _)| k.ends_with("lib.rs"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(lib.iter().any(|i| i == "std::io"), "{lib:?}");
        let py = g
            .imports
            .iter()
            .find(|(k, _)| k.ends_with("app.py"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(py.iter().any(|i| i == "os"), "{py:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn symbol_query_reports_definition_and_users() {
        let dir = fixture("symbol");
        let g = scan(&dir);
        let out = g.symbol("build_widget");
        assert!(out.contains("lib.rs"), "{out}");
        assert!(out.contains("main.rs"), "{out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_symbol_suggests_near_matches() {
        let dir = fixture("near");
        let g = scan(&dir);
        let out = g.symbol("build_widge");
        assert!(out.contains("Similar"), "{out}");
        assert!(out.contains("build_widget"), "{out}");
        assert!(g.symbol("zzzz").contains("not in the code graph"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_query_lists_definitions_and_imports() {
        let dir = fixture("file");
        let g = scan(&dir);
        let out = g.file("src/lib.rs");
        assert!(out.contains("Widget"), "{out}");
        assert!(out.contains("std::io"), "{out}");
        assert!(out.contains("Used by"), "{out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overview_summarises_languages() {
        let dir = fixture("overview");
        let g = scan(&dir);
        let out = g.overview();
        assert!(out.contains("rust"));
        assert!(out.contains("python"));
        assert!(out.contains("Most referenced"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shell_functions_are_detected() {
        assert_eq!(
            definitions("shell", "check() {"),
            Some(("fn", "check".to_string()))
        );
        assert_eq!(definitions("rust", "pub fn go() {}"), Some(("fn", "go".into())));
        assert_eq!(definitions("rust", "    let x = 1;"), None);
    }
}
