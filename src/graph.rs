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

/// Remove leading visibility/modifier tokens from a definition line so the core
/// keyword (`fn`, `struct`, `class`, …) reaches the prefix table. Conservative:
/// it only strips a known set of modifier words and never consumes the keyword
/// itself. Language-aware so, e.g., Go's `func` is never mistaken for a modifier.
fn strip_def_modifiers(lang: &str, line: &str) -> String {
    // Modifiers that may precede a definition keyword, per language family.
    // Kept as whole words; `extern "C"`/`pub(crate)`/annotations are handled below.
    let words: &[&str] = match lang {
        "rust" => &["pub", "async", "unsafe", "extern", "default"],
        "javascript" | "typescript" => &[
            "export", "default", "public", "private", "protected", "static",
            "readonly", "abstract", "async", "declare",
        ],
        "java" | "kotlin" | "swift" | "csharp" | "scala" | "dart" => &[
            "public", "private", "protected", "internal", "static", "final",
            "abstract", "sealed", "open", "override", "suspend", "inline",
            "virtual", "async", "partial",
        ],
        "python" => &["async"],
        _ => &[],
    };
    let mut s = line.trim_start();
    // Drop a leading decorator/annotation (`@Override`, `@dataclass`, …): keep
    // only what follows on the same line if the annotation is inline.
    if s.starts_with('@') {
        // A decorator on its own line has no following keyword; leave it be so
        // it simply doesn't match. When it's `@Ann def foo`, skip the token.
        if let Some(rest) = s.split_whitespace().nth(1) {
            // Reconstruct from the first non-annotation token onward.
            if let Some(idx) = s.find(rest) {
                s = &s[idx..];
            }
        }
    }
    // Special-case `pub(crate)` / `pub(super)` / `pub(in ...)`.
    if lang == "rust" {
        if let Some(rest) = s.strip_prefix("pub(") {
            if let Some(close) = rest.find(')') {
                s = rest[close + 1..].trim_start();
            }
        }
    }
    loop {
        let word: String = s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if word.is_empty() || !words.contains(&word.as_str()) {
            break;
        }
        let after = &s[word.len()..];
        // Only strip when the word is followed by whitespace (it's a real
        // modifier, not the start of the name/keyword).
        let Some(next) = after.strip_prefix(|c: char| c.is_whitespace()) else {
            break;
        };
        // `extern "C"` — also drop the following string literal.
        if lang == "rust" && word == "extern" {
            let n = next.trim_start();
            if let Some(after_quote) = n.strip_prefix('"') {
                if let Some(end) = after_quote.find('"') {
                    s = after_quote[end + 1..].trim_start();
                    continue;
                }
            }
        }
        s = next.trim_start();
    }
    // After general modifiers are gone, handle Rust's `const fn` / `static`:
    // these words are modifiers ONLY before `fn`/`async`/`unsafe`; otherwise
    // they are themselves definition keywords, so leave them in place.
    if lang == "rust" {
        for kw in ["const ", "static "] {
            if let Some(rest) = s.strip_prefix(kw) {
                let r = rest.trim_start();
                if r.starts_with("fn ") || r.starts_with("async ") || r.starts_with("unsafe ") {
                    s = r;
                }
            }
        }
    }
    s.to_string()
}

/// Definition patterns per language. Each returns (kind, name) for a line.
fn definitions(lang: &str, line: &str) -> Option<(&'static str, String)> {
    let t = line.trim_start();
    // Strip leading modifier/visibility tokens so a decorated definition still
    // matches the keyword table. Without this, `pub async fn f`, `pub(crate) fn`,
    // `unsafe fn`, `pub extern "C" fn`, or an annotated Java/TS method are all
    // missed just because the prefix isn't verbatim. We normalise once here and
    // match the core keyword against the same table as before.
    let t = strip_def_modifiers(lang, t);
    let t = t.as_str();
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

/// Identifiers mentioned in a line, for the reference pass. Strips string/char
/// literals and comments first, and skips language keywords, so a reference set
/// isn't inflated by `"run"` in a string, a `// comment`, or keywords like
/// `return`/`func` that happen to be ≥3 chars.
fn identifiers(lang: &str, line: &str, out: &mut BTreeSet<String>) {
    let code = strip_literals_and_comments(lang, line);
    let kw = keywords(lang);
    let mut cur = String::new();
    for c in code.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            if cur.len() > 2 && !cur.chars().all(|c| c.is_ascii_digit()) && !kw.contains(&cur.as_str()) {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() > 2 && !kw.contains(&cur.as_str()) {
        out.insert(cur);
    }
}

/// Blank out string/char literals and strip line/block comments from a single
/// line so identifier extraction only sees code. Line-local (no multi-line
/// block-comment state), which is enough to kill the common false positives.
fn strip_literals_and_comments(lang: &str, line: &str) -> String {
    let line_comment: &[&str] = match lang {
        "python" | "ruby" | "shell" | "perl" | "r" | "elixir" | "julia" => &["#"],
        "lua" | "haskell" => &["--"],
        _ => &["//"],
    };
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // Line comment: drop the rest.
        if line_comment.iter().any(|p| line[i..].starts_with(p)) {
            break;
        }
        // Block comment start `/* … */` (C-family): drop to a same-line close.
        if matches!(lang, "rust" | "c" | "cpp" | "java" | "javascript" | "typescript" | "csharp" | "go" | "swift" | "kotlin" | "scala" | "dart" | "php")
            && line[i..].starts_with("/*")
        {
            if let Some(end) = line[i..].find("*/") {
                for _ in 0..end + 1 {
                    chars.next();
                }
                continue;
            }
            break;
        }
        // String / char literal: skip to its close, keeping a placeholder space.
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

/// Language keywords that must never be counted as symbol references.
fn keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" => &[
            "let", "mut", "fn", "pub", "use", "mod", "struct", "enum", "impl",
            "trait", "for", "while", "loop", "match", "return", "self", "super",
            "crate", "async", "await", "move", "ref", "dyn", "where", "const",
            "static", "type", "unsafe", "extern", "continue", "break", "else",
        ],
        "python" => &[
            "def", "class", "return", "import", "from", "for", "while", "with",
            "and", "not", "elif", "else", "None", "True", "False", "self",
            "lambda", "yield", "async", "await", "pass", "raise", "except",
            "finally", "global", "nonlocal", "assert",
        ],
        "javascript" | "typescript" => &[
            "let", "var", "const", "function", "return", "import", "export",
            "from", "for", "while", "class", "extends", "async", "await",
            "this", "new", "typeof", "instanceof", "else", "case", "switch",
            "interface", "type", "public", "private", "readonly", "static",
        ],
        "go" => &[
            "func", "var", "type", "struct", "interface", "return", "import",
            "package", "for", "range", "chan", "defer", "else", "map", "const",
        ],
        "java" | "kotlin" | "csharp" | "swift" | "scala" => &[
            "public", "private", "protected", "class", "interface", "return",
            "import", "static", "final", "void", "new", "this", "else", "for",
            "while", "func", "fun", "val", "var", "override",
        ],
        _ => &[],
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
    // Method/receiver association: remember the enclosing type so a method is
    // also recorded as `Type::method`. For brace languages we track a stack by
    // brace depth; for Python we track by indentation. Conservative — when we
    // aren't sure of the owner we just record the bare name as before.
    let brace_lang = matches!(
        lang,
        "rust" | "javascript" | "typescript" | "go" | "java" | "kotlin"
            | "swift" | "csharp" | "scala" | "cpp" | "c" | "php" | "dart"
    );
    // (type_name, brace_depth_at_open) for brace langs.
    let mut type_stack: Vec<(String, i32)> = Vec::new();
    let mut depth: i32 = 0;
    // (type_name, indent_cols) for Python.
    let mut py_class: Option<(String, usize)> = None;

    for (i, line) in text.lines().enumerate() {
        let def = definitions(lang, line);

        // Maintain the Python class context by indentation before recording.
        if lang == "python" {
            let indent = line.len() - line.trim_start().len();
            if let Some((_, cindent)) = &py_class {
                // Dedented out of the class body: pop it.
                if !line.trim().is_empty() && indent <= *cindent {
                    py_class = None;
                }
            }
        }

        if let Some((kind, name)) = def {
            let owner = if lang == "python" {
                py_class.as_ref().map(|(t, _)| t.clone())
            } else if brace_lang {
                type_stack.last().map(|(t, _)| t.clone())
            } else {
                None
            };
            // A method (fn inside a type) is also recorded as `Type::method`,
            // so both `symbol(method)` and `symbol(Type::method)` resolve.
            if kind == "fn" {
                if let Some(t) = &owner {
                    defs.push((format!("{t}::{name}"), "method", i + 1));
                }
            }
            defs.push((name.clone(), kind, i + 1));

            // Opening a type scope: remember it as the current owner.
            let opens_type = matches!(kind, "class" | "struct" | "trait" | "interface" | "enum");
            if opens_type {
                if lang == "python" {
                    let indent = line.len() - line.trim_start().len();
                    py_class = Some((name.clone(), indent));
                } else if brace_lang {
                    type_stack.push((name.clone(), depth));
                }
            }
            // Rust `impl Type` / `impl Trait for Type` also sets the owner.
            if lang == "rust" {
                if let Some(t) = rust_impl_target(line) {
                    type_stack.push((t, depth));
                }
            }
        } else if lang == "rust" {
            // `impl` blocks aren't definitions but establish the owner.
            if let Some(t) = rust_impl_target(line) {
                type_stack.push((t, depth));
            }
        }

        if let Some(imp) = import_of(lang, line) {
            imports.push(imp);
        }
        identifiers(lang, line, &mut ids);

        // Update brace depth and pop any type scope whose block closed.
        if brace_lang {
            let stripped = strip_literals_and_comments(lang, line);
            for c in stripped.chars() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        while type_stack.last().map(|(_, d)| *d >= depth).unwrap_or(false) {
                            type_stack.pop();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    FileParse { rel, lang, defs, imports, ids }
}

/// Extract the target type of a Rust `impl` line: `impl Foo`, `impl<T> Foo<T>`,
/// `impl Trait for Foo` → `Foo`. Returns None for non-impl lines.
fn rust_impl_target(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("impl")?;
    // Must be `impl` as a word (followed by space or `<`).
    if !rest.starts_with([' ', '<']) {
        return None;
    }
    // `impl Trait for Foo` → take what's after `for`; else the first type token.
    let target = if let Some(idx) = rest.find(" for ") {
        &rest[idx + 5..]
    } else {
        rest
    };
    // Skip generics `<...>` then read the type name.
    let target = target.trim_start();
    let target = target.strip_prefix('<').map_or(target, |r| {
        r.find('>').map(|i| target[i + 2..].trim_start()).unwrap_or(target)
    });
    let name: String = target
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
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

    /// Project idioms: internal symbols and modules that are load-bearing here,
    /// so the agent prefers them over reinventing generic equivalents. Returns
    /// `(name, kind, cross_file_uses)` for symbols DEFINED in this project and
    /// referenced across at least `min_files` other files. Sorted by reach.
    ///
    /// This is the raw material for Phase 3 idiom rules — deterministic, drawn
    /// straight from the graph, no model involved.
    pub fn idioms(&self, min_files: usize) -> Vec<(String, &'static str, usize)> {
        let mut out: Vec<(String, &'static str, usize)> = Vec::new();
        for (name, files) in &self.refs {
            let reach = files.len();
            if reach < min_files {
                continue;
            }
            // Must be defined in THIS project (not a stdlib/third-party name we
            // merely saw referenced), and have a single clear definition kind.
            let Some(defs) = self.defs.get(name) else { continue };
            let Some(first) = defs.first() else { continue };
            // Skip trivially short names — too generic to be a useful idiom.
            if name.len() < 3 {
                continue;
            }
            out.push((name.clone(), first.kind, reach));
        }
        out.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        out
    }

    /// Internal modules imported across many files — a project convention worth
    /// following. Returns `(module, times_imported)` sorted by frequency.
    pub fn common_imports(&self, min_files: usize) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for imps in self.imports.values() {
            // Dedup within a file so one file counts once per module.
            let mut seen: BTreeSet<&String> = BTreeSet::new();
            for m in imps {
                if seen.insert(m) {
                    *counts.entry(m.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<(String, usize)> = counts
            .into_iter()
            .filter(|(m, n)| *n >= min_files && m.len() >= 3)
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
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
    fn detects_definitions_behind_modifiers() {
        // Rust: modifiers that used to defeat the verbatim prefix match.
        assert_eq!(definitions("rust", "pub async fn fetch() {}"), Some(("fn", "fetch".into())));
        assert_eq!(definitions("rust", "pub(crate) fn helper() {}"), Some(("fn", "helper".into())));
        assert_eq!(definitions("rust", "    pub unsafe fn raw() {}"), Some(("fn", "raw".into())));
        assert_eq!(definitions("rust", "pub const fn zero() -> u8 { 0 }"), Some(("fn", "zero".into())));
        assert_eq!(definitions("rust", "pub extern \"C\" fn c_abi() {}"), Some(("fn", "c_abi".into())));
        // But `const`/`static` as real definition keywords are still recognised.
        assert_eq!(definitions("rust", "pub const MAX: usize = 8;"), Some(("const", "MAX".into())));
        assert_eq!(definitions("rust", "static REG: u32 = 0;"), Some(("static", "REG".into())));
        // TS/JS: export/async/decorated.
        assert_eq!(definitions("typescript", "export async function load() {}"), Some(("fn", "load".into())));
        assert_eq!(definitions("typescript", "export abstract class Base {"), Some(("class", "Base".into())));
        // Java-family: multi-modifier and annotated.
        assert_eq!(definitions("java", "public static final class Config {"), Some(("class", "Config".into())));
        assert_eq!(definitions("kotlin", "override suspend fun run() {}"), Some(("fn", "run".into())));
        assert_eq!(definitions("python", "async def handler():"), Some(("fn", "handler".into())));
    }

    #[test]
    fn references_ignore_literals_comments_and_keywords() {
        let mut ids = BTreeSet::new();
        // `Widget` in code counts; `Widget` inside a string does not; the
        // keyword `return` never counts.
        identifiers("rust", r#"    return Widget::new("Widget in a string");"#, &mut ids);
        assert!(ids.contains("Widget"), "real reference should be kept: {ids:?}");
        assert!(!ids.contains("return"), "keyword must be dropped: {ids:?}");
        assert!(!ids.contains("string"), "string-literal word must be dropped: {ids:?}");

        // A line-comment's words are ignored.
        let mut ids2 = BTreeSet::new();
        identifiers("rust", "let x = 1; // TokenInComment matters not", &mut ids2);
        assert!(!ids2.contains("TokenInComment"), "comment word must be dropped: {ids2:?}");
        assert!(!ids2.contains("let"), "keyword dropped: {ids2:?}");

        // Python uses # comments.
        let mut ids3 = BTreeSet::new();
        identifiers("python", "value = compute()  # HiddenName here", &mut ids3);
        assert!(ids3.contains("compute"));
        assert!(!ids3.contains("HiddenName"), "py comment dropped: {ids3:?}");
    }

    #[test]
    fn methods_are_associated_with_their_type() {
        let dir = std::env::temp_dir().join("koda-graph-methods");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub struct Cart;\nimpl Cart {\n    pub fn total(&self) -> u32 { 0 }\n    fn helper(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("app.py"),
            "class Order:\n    def submit(self):\n        pass\n\ndef free_function():\n    pass\n",
        )
        .unwrap();
        let g = scan(&dir);
        // Bare names still resolve (backward compatible).
        assert!(g.defs.contains_key("total"), "{:?}", g.defs.keys());
        assert!(g.defs.contains_key("submit"));
        // And now the qualified method name resolves too.
        assert!(g.defs.contains_key("Cart::total"), "impl method not associated: {:?}", g.defs.keys());
        assert!(g.defs.contains_key("Order::submit"), "py method not associated: {:?}", g.defs.keys());
        // A module-level function is NOT falsely attributed to the class.
        assert!(g.defs.contains_key("free_function"));
        assert!(!g.defs.contains_key("Order::free_function"), "dedented fn wrongly attributed");
        std::fs::remove_dir_all(&dir).ok();
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

    #[test]
    fn idioms_surface_internal_symbols_used_across_files() {
        // A helper defined once and referenced from several files is an idiom.
        let dir = fixture("idioms");
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn build_widget() {}\n",
        )
        .unwrap();
        // Three files that all call build_widget -> reach = 3.
        for f in ["a", "b", "c"] {
            std::fs::write(
                dir.join(format!("src/{f}.rs")),
                "fn use_it() { build_widget(); }\n",
            )
            .unwrap();
        }
        let g = scan(&dir);
        let idioms = g.idioms(3);
        assert!(
            idioms.iter().any(|(n, _, reach)| n == "build_widget" && *reach >= 3),
            "expected build_widget as an idiom, got {idioms:?}"
        );
        // A high threshold excludes it.
        assert!(g.idioms(99).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn common_imports_counts_modules_across_files() {
        let dir = fixture("common-imports-idiom");
        for f in ["a", "b", "c"] {
            std::fs::write(
                dir.join(format!("{f}.py")),
                "from internal_kit import helper\n",
            )
            .unwrap();
        }
        let g = scan(&dir);
        let common = g.common_imports(3);
        assert!(
            common.iter().any(|(m, n)| m.contains("internal_kit") && *n >= 3),
            "expected internal_kit imported across files, got {common:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
