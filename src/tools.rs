//! Built-in tools. All filesystem work happens in-process (no shelling out)
//! so tool latency stays in the sub-millisecond range for typical repos.

use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub struct Spec {
    pub name: &'static str,
    pub desc: &'static str,
    pub params: Value,
    /// Mutating tools require approval unless auto-approve is on.
    pub mutating: bool,
}

#[derive(Clone)]
pub struct ToolCtx {
    pub root: PathBuf,
    pub cfg: Arc<Config>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub ok: bool,
    /// Text handed back to the model. Kept verbatim so the wire protocol is
    /// independent of how the TUI chooses to draw the result.
    pub content: String,
    /// Short human-facing summary for the transcript.
    pub summary: String,
    /// Structured result for rendering. The transcript draws from this; the model
    /// never sees it.
    pub view: ToolView,
}

/// What a tool produced, in a shape the renderer can lay out properly.
///
/// The model still gets `Outcome::content`; this exists purely so the TUI can
/// draw a grep hit differently from a directory listing instead of printing one
/// generic blob for every tool.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ToolView {
    /// Nothing worth structuring — render the text detail as-is.
    #[default]
    Plain,
    /// File contents with a starting line number, for a numbered gutter.
    Read {
        path: String,
        lang: String,
        lines: Vec<String>,
        start: usize,
        total: usize,
        truncated: bool,
    },
    Listing {
        path: String,
        entries: Vec<DirEntry>,
        truncated: bool,
    },
    Files {
        pattern: String,
        files: Vec<String>,
        truncated: bool,
    },
    /// Grep hits grouped by file, which is how they are useful to read.
    Matches {
        pattern: String,
        groups: Vec<MatchGroup>,
        hits: usize,
        truncated: bool,
    },
    /// A write or an edit: the diff plus its stats.
    Diff {
        path: String,
        diff: String,
        added: usize,
        removed: usize,
        created: bool,
    },
    Run {
        command: String,
        stdout: String,
        stderr: String,
        code: i32,
    },
}

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Every hit inside a single file.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchGroup {
    pub file: String,
    pub lines: Vec<(usize, String)>,
}

impl Outcome {
    fn ok(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
            summary: summary.into(),
            view: ToolView::Plain,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        Self {
            ok: false,
            content: format!("ERROR: {msg}"),
            summary: msg,
            view: ToolView::Plain,
        }
    }

    /// Attach structured data for the renderer.
    fn with(mut self, view: ToolView) -> Self {
        self.view = view;
        self
    }
}

fn str_prop(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

pub fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "read_file",
            desc: "Read a UTF-8 text file. Returns numbered lines. Use offset/limit for large files.",
            params: json!({
                "type": "object",
                "properties": {
                    "path": str_prop("File path, relative to the workspace root."),
                    "offset": { "type": "integer", "description": "1-based first line to read." },
                    "limit": { "type": "integer", "description": "Max lines to read." }
                },
                "required": ["path"]
            }),
            mutating: false,
        },
        Spec {
            name: "list_dir",
            desc: "List directory entries. Respects .gitignore. depth>1 recurses.",
            params: json!({
                "type": "object",
                "properties": {
                    "path": str_prop("Directory path. Defaults to the workspace root."),
                    "depth": { "type": "integer", "description": "Recursion depth, default 1." }
                }
            }),
            mutating: false,
        },
        Spec {
            name: "find_files",
            desc: "Find files by glob, e.g. `**/*.rs` or `Cargo.toml`. Respects .gitignore.",
            params: json!({
                "type": "object",
                "properties": {
                    "glob": str_prop("Glob pattern to match against paths or file names."),
                    "path": str_prop("Directory to search from."),
                    "limit": { "type": "integer", "description": "Max results, default 200." }
                },
                "required": ["glob"]
            }),
            mutating: false,
        },
        Spec {
            name: "search",
            desc: "Regex search across file contents. Respects .gitignore. Returns path:line:text.",
            params: json!({
                "type": "object",
                "properties": {
                    "pattern": str_prop("Rust regex pattern."),
                    "path": str_prop("Directory or file to search."),
                    "glob": str_prop("Only search files matching this glob."),
                    "limit": { "type": "integer", "description": "Max matches, default 80." }
                },
                "required": ["pattern"]
            }),
            mutating: false,
        },
        Spec {
            name: "write_file",
            desc: "Create or overwrite a file with the given content. Parent dirs are created.",
            params: json!({
                "type": "object",
                "properties": {
                    "path": str_prop("File path."),
                    "content": str_prop("Full file content.")
                },
                "required": ["path", "content"]
            }),
            mutating: true,
        },
        Spec {
            name: "edit_file",
            desc: "Replace an exact substring in a file. `old` must appear exactly once unless \
                   replace_all is true. Include surrounding lines to make it unique.",
            params: json!({
                "type": "object",
                "properties": {
                    "path": str_prop("File path."),
                    "old": str_prop("Exact text to replace, copied verbatim from the file."),
                    "new": str_prop("Replacement text."),
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence." }
                },
                "required": ["path", "old", "new"]
            }),
            mutating: true,
        },
        Spec {
            name: "remember",
            desc: "Record something about this project that will still be true next session: \
                   the test command, where a subsystem lives, a convention you had to \
                   discover. Only durable facts — not what you are doing right now. Say \
                   `forget` with a phrase to drop a note that turned out wrong.",
            params: json!({
                "type": "object",
                "properties": {
                    "note": str_prop("One sentence, stated as a fact."),
                    "forget": str_prop("Instead of adding, remove notes containing this text.")
                }
            }),
            mutating: false,
        },
        Spec {
            name: "codegraph",
            desc: "Ask the project's symbol graph instead of grepping. `overview` maps the \
                   project; `symbol` says where a name is defined and which files use it; \
                   `file` lists what a file defines, imports, and who depends on it. Start \
                   here, then read the files it points at.",
            params: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "enum": ["overview", "symbol", "file"],
                        "description": "Which question to ask."
                    },
                    "name": str_prop("Symbol name, for query=symbol."),
                    "path": str_prop("File path, for query=file.")
                },
                "required": ["query"]
            }),
            mutating: false,
        },
        Spec {
            name: "skill",
            desc: "Read a project skill: conventions and rules for a kind of work. The \
                   available skills are listed in your instructions. Read the relevant one \
                   before starting that kind of work, not after.",
            params: json!({
                "type": "object",
                "properties": {
                    "name": str_prop("Skill name, as listed in your instructions.")
                },
                "required": ["name"]
            }),
            mutating: false,
        },
        Spec {
            name: "web_search",
            desc: "Search the web for things outside the codebase: library docs, error \
                   messages, API changes, current versions. Returns titles, URLs and \
                   snippets — not full pages. Do not use it for questions the repo itself \
                   can answer.",
            params: json!({
                "type": "object",
                "properties": {
                    "query": str_prop("Search terms. Keywords work better than a sentence."),
                    "limit": { "type": "integer", "description": "Max results, default 6." }
                },
                "required": ["query"]
            }),
            mutating: false,
        },
        Spec {
            name: "todo",
            desc: "Track a multi-step task so the user can see the plan and the progress. \
                   Send the whole list every time, with one item marked in_progress. Use it \
                   for work with three or more steps; skip it for single edits.",
            params: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "The full list, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "Short imperative step." },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "done"],
                                    "description": "Exactly one item should be in_progress."
                                }
                            },
                            "required": ["text", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
            mutating: false,
        },
        Spec {
            name: "delegate",
            desc: "Hand a self-contained investigation to a subagent that has its own fresh \
                   context. Use it for wide searches so only the findings come back to you, \
                   not every file it had to read. The subagent can read, list, find and \
                   search, but cannot modify files or run commands. It returns a written \
                   report. Give it one clear question and everything it needs to start.",
            params: json!({
                "type": "object",
                "properties": {
                    "task": str_prop("The question or investigation, stated so it makes sense \
                                      with no other context."),
                    "context": str_prop("Optional facts the subagent should start from: paths \
                                         already known, findings so far.")
                },
                "required": ["task"]
            }),
            mutating: false,
        },
        Spec {
            name: "run_command",
            desc: "Run a shell command in the workspace root. Use for builds, tests, git, and \
                   package managers. Returns exit code, stdout and stderr.",
            params: json!({
                "type": "object",
                "properties": {
                    "command": str_prop("Shell command line."),
                    "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds." }
                },
                "required": ["command"]
            }),
            mutating: true,
        },
    ]
}

/// Tools available in plan mode: everything that cannot change the workspace.
pub const PLAN_TOOLS: &[&str] = &[
    "read_file", "list_dir", "find_files", "search", "delegate", "todo", "skill", "web_search",
    "codegraph", "remember",
];

/// One tracked step of a multi-step task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    Active,
    Done,
}

impl TodoStatus {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "done" | "completed" | "complete" | "finished" => Self::Done,
            "in_progress" | "active" | "doing" | "current" => Self::Active,
            _ => Self::Pending,
        }
    }
}

/// Parse a `todo` call. Tolerant: local models send strings, or bare arrays.
pub fn parse_todos(args: &Value) -> Vec<Todo> {
    let items = args
        .get("items")
        .or_else(|| args.get("todos"))
        .or_else(|| args.get("tasks"));
    let Some(arr) = items.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|it| match it {
            Value::String(s) => Some(Todo {
                text: s.clone(),
                status: TodoStatus::Pending,
            }),
            Value::Object(_) => {
                let text = it
                    .get("text")
                    .or_else(|| it.get("task"))
                    .or_else(|| it.get("title"))
                    .and_then(|t| t.as_str())?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    return None;
                }
                let status = it
                    .get("status")
                    .or_else(|| it.get("state"))
                    .and_then(|s| s.as_str())
                    .map(TodoStatus::parse)
                    .unwrap_or(TodoStatus::Pending);
                Some(Todo { text, status })
            }
            _ => None,
        })
        .collect()
}

/// Tools a subagent may call: read-only, and no further delegation.
pub const SUBAGENT_TOOLS: &[&str] =
    &["read_file", "list_dir", "find_files", "search", "skill", "codegraph"];

pub fn spec(name: &str) -> Option<Spec> {
    specs().into_iter().find(|s| s.name == name)
}

pub fn is_mutating(name: &str) -> bool {
    spec(name).map(|s| s.mutating).unwrap_or(true)
}

/// OpenAI `tools` array. `allow` restricts it to a named subset.
pub fn openai_schema_for(allow: Option<&[&str]>) -> Vec<Value> {
    specs()
        .into_iter()
        .filter(|s| allow.map(|a| a.contains(&s.name)).unwrap_or(true))
        .map(|s| {
            json!({
                "type": "function",
                "function": {
                    "name": s.name,
                    "description": s.desc,
                    "parameters": s.params,
                }
            })
        })
        .collect()
}

/// Compact listing injected into the system prompt for the text protocol.
pub fn text_protocol_help_for(allow: Option<&[&str]>) -> String {
    let mut out = String::new();
    for s in specs()
        .into_iter()
        .filter(|s| allow.map(|a| a.contains(&s.name)).unwrap_or(true))
    {
        let params = s.params.get("properties").and_then(|p| p.as_object());
        let required: Vec<&str> = s
            .params
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut arg_list = Vec::new();
        if let Some(params) = params {
            for (k, _) in params {
                if required.contains(&k.as_str()) {
                    arg_list.push(k.clone());
                } else {
                    arg_list.push(format!("{k}?"));
                }
            }
        }
        let _ = writeln!(out, "- {}({}): {}", s.name, arg_list.join(", "), s.desc);
    }
    out
}

// ---------------------------------------------------------------- path handling

/// Lexical normalization: no filesystem access, so it works for paths that
/// don't exist yet.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolve(ctx: &ToolCtx, raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("empty path");
    }
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("no home directory"))?
            .join(rest)
    } else {
        PathBuf::from(raw)
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        ctx.root.join(expanded)
    };
    let norm = normalize(&joined);
    if ctx.cfg.sandbox && !norm.starts_with(&ctx.root) {
        bail!(
            "path `{}` is outside the workspace ({}); sandbox is enabled",
            raw,
            ctx.root.display()
        );
    }
    Ok(norm)
}

pub fn rel(ctx: &ToolCtx, p: &Path) -> String {
    p.strip_prefix(&ctx.root)
        .unwrap_or(p)
        .to_string_lossy()
        .to_string()
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("missing required string argument `{key}`"))
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| match v {
        Value::Number(n) => n.as_u64().map(|n| n as usize),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key)
        .and_then(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(false)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = s.len() - end;
    format!("{}\n[... {omitted} bytes truncated ...]", &s[..end])
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

// ---------------------------------------------------------------- previews

/// Human-readable preview shown in the approval dialog.
pub fn preview(name: &str, args: &Value, ctx: &ToolCtx) -> Option<String> {
    match name {
        "write_file" => {
            let path = args.get("path")?.as_str()?;
            let new = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let full = resolve(ctx, path).ok()?;
            let old = std::fs::read_to_string(&full).unwrap_or_default();
            Some(unified_diff(&old, new, &rel(ctx, &full)))
        }
        "edit_file" => {
            let path = args.get("path")?.as_str()?;
            let old_s = args.get("old").and_then(|c| c.as_str()).unwrap_or("");
            let new_s = args.get("new").and_then(|c| c.as_str()).unwrap_or("");
            let full = resolve(ctx, path).ok()?;
            let content = std::fs::read_to_string(&full).ok()?;
            let replaced = if arg_bool(args, "replace_all") {
                content.replace(old_s, new_s)
            } else {
                content.replacen(old_s, new_s, 1)
            };
            Some(unified_diff(&content, &replaced, &rel(ctx, &full)))
        }
        "run_command" => {
            let cmd = args.get("command")?.as_str()?;
            Some(format!("$ {cmd}"))
        }
        _ => None,
    }
}

pub fn unified_diff(old: &str, new: &str, label: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let _ = writeln!(out, "--- {label}");
    let _ = writeln!(out, "+++ {label}");
    let mut any = false;
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start + 1..last.old_range().end;
        let new_range = first.new_range().start + 1..last.new_range().end;
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            old_range.start,
            old_range.end.saturating_sub(old_range.start - 1),
            new_range.start,
            new_range.end.saturating_sub(new_range.start - 1),
        );
        for op in group {
            for change in diff.iter_changes(&op) {
                any = true;
                let sign = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal => ' ',
                };
                let value = change.value();
                out.push(sign);
                out.push_str(value.trim_end_matches('\n'));
                out.push('\n');
            }
        }
    }
    if !any {
        return format!("{label}: no changes");
    }
    out
}

// ---------------------------------------------------------------- execution

pub async fn run(name: &str, args: Value, ctx: &ToolCtx) -> Outcome {
    if name == "run_command" {
        return run_command(&args, ctx).await;
    }
    let name = name.to_string();
    let ctx = ctx.clone();
    let res = tokio::task::spawn_blocking(move || run_sync(&name, &args, &ctx)).await;
    match res {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => Outcome::err(format!("{e:#}")),
        Err(e) => Outcome::err(format!("tool task failed: {e}")),
    }
}

fn run_sync(name: &str, args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    match name {
        "read_file" => read_file(args, ctx),
        "list_dir" => list_dir(args, ctx),
        "find_files" => find_files(args, ctx),
        "search" => search(args, ctx),
        "write_file" => write_file(args, ctx),
        "edit_file" => edit_file(args, ctx),
        other => Ok(Outcome::err(format!("unknown tool `{other}`"))),
    }
}

fn read_file(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let path = arg_str(args, "path")?;
    let full = resolve(ctx, &path)?;
    let meta = match std::fs::metadata(&full) {
        Ok(m) => m,
        Err(_) => return Ok(Outcome::err(format!("no such file: {path}"))),
    };
    if meta.is_dir() {
        return Ok(Outcome::err(format!("{path} is a directory; use list_dir")));
    }
    let bytes = std::fs::read(&full).with_context(|| format!("reading {path}"))?;
    if looks_binary(&bytes) {
        return Ok(Outcome::err(format!(
            "{path} looks like a binary file ({} bytes)",
            bytes.len()
        )));
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = truncate(&text, ctx.cfg.max_file_bytes);

    let offset = arg_usize(args, "offset").unwrap_or(1).max(1);
    let limit = arg_usize(args, "limit").unwrap_or(usize::MAX);
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = offset - 1;
    if start >= total && total > 0 {
        return Ok(Outcome::err(format!(
            "offset {offset} is past end of file ({total} lines)"
        )));
    }
    let end = start.saturating_add(limit).min(total);
    let width = end.to_string().len().max(3);
    let mut out = String::new();
    for (i, line) in all[start..end].iter().enumerate() {
        let _ = writeln!(out, "{:>width$}| {line}", start + i + 1, width = width);
    }
    if end < total {
        let _ = writeln!(out, "[... {} more lines; use offset={} ...]", total - end, end + 1);
    }
    if out.is_empty() {
        out.push_str("(empty file)\n");
    }
    Ok(Outcome::ok(
        out,
        format!("read {} ({} lines)", rel(ctx, &full), total),
    )
    .with(ToolView::Read {
        path: rel(ctx, &full),
        lang: lang_of(&full),
        lines: all[start..end].iter().map(|l| l.to_string()).collect(),
        start: start + 1,
        total,
        truncated: end < total,
    }))
}

/// Language tag for a path, used to pick a syntax highlighter.
fn lang_of(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn walker(root: &Path, depth: Option<usize>) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        // Respect .gitignore even when the directory is not a git repo, so
        // node_modules / target stay out of results either way.
        .require_git(false)
        .git_global(false)
        .git_exclude(true)
        .follow_links(false)
        .max_depth(depth)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

fn list_dir(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let path = args
        .get("path")
        .and_then(|p| p.as_str())
        .unwrap_or(".")
        .to_string();
    let full = resolve(ctx, &path)?;
    if !full.is_dir() {
        return Ok(Outcome::err(format!("not a directory: {path}")));
    }
    let depth = arg_usize(args, "depth").unwrap_or(1).clamp(1, 8);
    let mut entries: Vec<(bool, String, u64)> = Vec::new();
    for e in walker(&full, Some(depth)).flatten() {
        if e.path() == full {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        let name = e
            .path()
            .strip_prefix(&full)
            .unwrap_or(e.path())
            .to_string_lossy()
            .to_string();
        entries.push((is_dir, name, size));
        if entries.len() >= 2000 {
            break;
        }
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let count = entries.len();
    let mut out = format!("{}/\n", rel(ctx, &full));
    // Tree connectors: the shape of the listing is the information.
    for (i, (is_dir, name, size)) in entries.iter().enumerate() {
        let connector = if i + 1 == entries.len() {
            "└─"
        } else {
            "├─"
        };
        if *is_dir {
            let _ = writeln!(out, "{connector} {name}/");
        } else {
            let _ = writeln!(out, "{connector} {name} ({})", human_size(*size));
        }
    }
    if count == 0 {
        out.push_str("  (empty)\n");
    }
    Ok(Outcome::ok(
        truncate(&out, ctx.cfg.max_tool_output_bytes),
        format!("list {} ({count} entries)", rel(ctx, &full)),
    )
    .with(ToolView::Listing {
        path: rel(ctx, &full),
        entries: entries
            .iter()
            .map(|(is_dir, name, size)| DirEntry {
                name: name.clone(),
                is_dir: *is_dir,
                size: *size,
            })
            .collect(),
        truncated: false,
    }))
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "K", "M", "G"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

fn build_matcher(pattern: &str) -> Result<globset::GlobMatcher> {
    Ok(globset::GlobBuilder::new(pattern)
        .literal_separator(pattern.contains('/'))
        .build()
        .with_context(|| format!("invalid glob `{pattern}`"))?
        .compile_matcher())
}

fn glob_hit(m: &globset::GlobMatcher, relative: &Path) -> bool {
    m.is_match(relative)
        || relative
            .file_name()
            .map(|n| m.is_match(Path::new(n)))
            .unwrap_or(false)
}

fn find_files(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let pattern = arg_str(args, "glob")?;
    let base = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let root = resolve(ctx, base)?;
    let limit = arg_usize(args, "limit").unwrap_or(200).min(2000);
    let matcher = build_matcher(&pattern)?;

    let mut hits = Vec::new();
    for e in walker(&root, None).flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let relative = e.path().strip_prefix(&root).unwrap_or(e.path());
        if glob_hit(&matcher, relative) {
            hits.push(rel(ctx, e.path()));
            if hits.len() >= limit {
                break;
            }
        }
    }
    hits.sort();
    let n = hits.len();
    let body = if n == 0 {
        format!("no files matching `{pattern}`")
    } else {
        hits.join("\n")
    };
    Ok(Outcome::ok(
        truncate(&body, ctx.cfg.max_tool_output_bytes),
        format!("find {pattern} ({n} matches)"),
    )
    .with(ToolView::Files {
        pattern: pattern.clone(),
        files: hits.clone(),
        truncated: false,
    }))
}

fn search(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let pattern = arg_str(args, "pattern")?;
    let base = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let root = resolve(ctx, base)?;
    let limit = arg_usize(args, "limit").unwrap_or(80).min(1000);
    let re = regex::RegexBuilder::new(&pattern)
        .case_insensitive(false)
        .build()
        .with_context(|| format!("invalid regex `{pattern}`"))?;
    let file_glob = match args.get("glob").and_then(|g| g.as_str()) {
        Some(g) if !g.trim().is_empty() => Some(build_matcher(g)?),
        _ => None,
    };

    let mut out = String::new();
    let mut hits = 0usize;
    let mut files = 0usize;
    // Hits grouped by file, so the transcript can show them under a file heading
    // instead of repeating the path on every line.
    let mut groups: Vec<MatchGroup> = Vec::new();
    let single_file = root.is_file();
    let walk_root = if single_file {
        root.parent().unwrap_or(&ctx.root).to_path_buf()
    } else {
        root.clone()
    };

    'outer: for e in walker(&walk_root, None).flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if single_file && e.path() != root {
            continue;
        }
        if let Some(m) = &file_glob {
            let relative = e.path().strip_prefix(&walk_root).unwrap_or(e.path());
            if !glob_hit(m, relative) {
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(e.path()) else {
            continue;
        };
        if looks_binary(&bytes) || bytes.len() > 4 * 1024 * 1024 {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut file_hit = false;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if !file_hit {
                    groups.push(MatchGroup {
                        file: rel(ctx, e.path()),
                        lines: Vec::new(),
                    });
                }
                file_hit = true;
                hits += 1;
                let shown: String = line.trim_end().chars().take(240).collect();
                if let Some(g) = groups.last_mut() {
                    g.lines.push((i + 1, shown.clone()));
                }
                let _ = writeln!(out, "{}:{}: {}", rel(ctx, e.path()), i + 1, shown);
                if hits >= limit {
                    let _ = writeln!(out, "[... result limit {limit} reached ...]");
                    break 'outer;
                }
            }
        }
        if file_hit {
            files += 1;
        }
    }
    if hits == 0 {
        out = format!("no matches for `{pattern}`");
    }
    Ok(Outcome::ok(
        truncate(&out, ctx.cfg.max_tool_output_bytes),
        format!("search {pattern} ({hits} hits in {files} files)"),
    )
    .with(ToolView::Matches {
        pattern: pattern.clone(),
        groups: groups.clone(),
        hits,
        truncated: hits >= limit,
    }))
}

fn write_file(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let path = arg_str(args, "path")?;
    let content = arg_str(args, "content").unwrap_or_default();
    let full = resolve(ctx, &path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let existed = full.exists();
    let old = if existed {
        std::fs::read_to_string(&full).unwrap_or_default()
    } else {
        String::new()
    };
    std::fs::write(&full, &content).with_context(|| format!("writing {path}"))?;
    let lines = content.lines().count();
    let verb = if existed { "overwrote" } else { "created" };
    let diff = unified_diff(&old, &content, &rel(ctx, &full));
    let (added, removed) = diff_stats(&diff);
    Ok(Outcome::ok(
        format!("{verb} {} ({lines} lines)\n{}", rel(ctx, &full), truncate(&diff, 4000)),
        format!("{verb} {} ({lines} lines)", rel(ctx, &full)),
    )
    .with(ToolView::Diff {
        path: rel(ctx, &full),
        diff,
        added,
        removed,
        created: !existed,
    }))
}

/// Count added and removed lines in a unified diff, ignoring the header.
fn diff_stats(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        match l.as_bytes().first() {
            Some(b'+') => added += 1,
            Some(b'-') => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

fn edit_file(args: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let path = arg_str(args, "path")?;
    let old_s = arg_str(args, "old")?;
    let new_s = arg_str(args, "new").unwrap_or_default();
    let replace_all = arg_bool(args, "replace_all");
    let full = resolve(ctx, &path)?;

    let content = match std::fs::read_to_string(&full) {
        Ok(c) => c,
        Err(_) => return Ok(Outcome::err(format!("cannot read {path}"))),
    };
    if old_s.is_empty() {
        return Ok(Outcome::err("`old` must not be empty; use write_file to create files"));
    }
    let count = content.matches(old_s.as_str()).count();
    if count == 0 {
        return Ok(Outcome::err(format!(
            "`old` text not found in {path}. Re-read the file and copy the exact text, \
             including indentation."
        )));
    }
    if count > 1 && !replace_all {
        return Ok(Outcome::err(format!(
            "`old` text appears {count} times in {path}. Add surrounding lines to make it \
             unique, or pass replace_all=true."
        )));
    }
    let updated = if replace_all {
        content.replace(old_s.as_str(), &new_s)
    } else {
        content.replacen(old_s.as_str(), &new_s, 1)
    };
    std::fs::write(&full, &updated).with_context(|| format!("writing {path}"))?;
    let diff = unified_diff(&content, &updated, &rel(ctx, &full));
    let (added, removed) = diff_stats(&diff);
    Ok(Outcome::ok(
        format!("edited {} ({count} replacement(s))\n{}", rel(ctx, &full), truncate(&diff, 4000)),
        format!("edit {} ({count} replacement(s))", rel(ctx, &full)),
    )
    .with(ToolView::Diff {
        path: rel(ctx, &full),
        diff,
        added,
        removed,
        created: false,
    }))
}

async fn run_command(args: &Value, ctx: &ToolCtx) -> Outcome {
    let cmd = match arg_str(args, "command") {
        Ok(c) => c,
        Err(e) => return Outcome::err(format!("{e:#}")),
    };
    let timeout = arg_usize(args, "timeout_ms")
        .map(|v| v as u64)
        .unwrap_or(ctx.cfg.command_timeout_ms)
        .clamp(100, 30 * 60_000);

    let child = match tokio::process::Command::new(&ctx.cfg.shell)
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ctx.root)
        .env("KODA", "1")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::err(format!("spawning `{}`: {e}", ctx.cfg.shell)),
    };

    let wait = child.wait_with_output();
    let result = tokio::time::timeout(std::time::Duration::from_millis(timeout), wait).await;

    let (code, stdout, stderr, timed_out) = match result {
        Ok(Ok(out)) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            false,
        ),
        Ok(Err(e)) => return Outcome::err(format!("running command: {e}")),
        Err(_) => (-1, String::new(), String::new(), true),
    };

    if timed_out {
        return Outcome::err(format!("command timed out after {timeout}ms: {cmd}"));
    }

    let cap = ctx.cfg.max_tool_output_bytes;
    let mut body = format!("$ {cmd}\nexit code: {code}\n");
    if !stdout.trim().is_empty() {
        let _ = write!(body, "--- stdout ---\n{}\n", truncate(stdout.trim_end(), cap));
    }
    if !stderr.trim().is_empty() {
        let _ = write!(body, "--- stderr ---\n{}\n", truncate(stderr.trim_end(), cap / 2));
    }
    if stdout.trim().is_empty() && stderr.trim().is_empty() {
        body.push_str("(no output)\n");
    }
    Outcome {
        ok: code == 0,
        content: body,
        summary: format!("$ {} → exit {code}", first_line(&cmd)),
        view: ToolView::Run {
            command: cmd.clone(),
            stdout: stdout.trim_end().to_string(),
            stderr: stderr.trim_end().to_string(),
            code,
        },
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(80).collect()
}

/// The image extensions koda will attach to a vision request, mapped to their
/// MIME type. Anything else is treated as a normal file.
pub fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// True if the path looks like an image koda can attach to a vision request.
pub fn is_image_path(path: &Path) -> bool {
    image_mime(path).is_some()
}

/// Read an image file and encode it as a `data:` URL suitable for the OpenAI
/// `image_url` content part. Fails loudly on an unsupported extension or a file
/// too large, so the caller can fall back to treating the path as text.
pub fn image_data_url(path: &Path, max_bytes: usize) -> Result<String> {
    let mime = image_mime(path).ok_or_else(|| anyhow!("not a supported image type"))?;
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > max_bytes {
        bail!(
            "image is {} but the limit is {}",
            human_size(bytes.len() as u64),
            human_size(max_bytes as u64)
        );
    }
    Ok(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

/// Standard base64 (RFC 4648), for image data URLs.
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = u32::from_be_bytes([0, b0, b1, b2]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(root: &Path) -> ToolCtx {
        ToolCtx {
            root: root.to_path_buf(),
            cfg: Arc::new(Config::default()),
        }
    }

    #[test]
    fn sandbox_blocks_escape() {
        let c = ctx(Path::new("/tmp/koda-test"));
        assert!(resolve(&c, "../../etc/passwd").is_err());
        assert!(resolve(&c, "src/main.rs").is_ok());
    }

    #[test]
    fn base64_encodes_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn image_paths_are_recognised_by_extension() {
        assert!(is_image_path(Path::new("shot.png")));
        assert!(is_image_path(Path::new("a/b/c.JPEG")));
        assert!(!is_image_path(Path::new("main.rs")));
        assert!(!is_image_path(Path::new("notes.txt")));
    }

    #[test]
    fn image_data_url_encodes_and_size_checks() {
        let dir = std::env::temp_dir().join("koda-image-test");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("x.png");
        std::fs::write(&png, b"foo").unwrap();

        let url = image_data_url(&png, 1024).unwrap();
        assert_eq!(url, "data:image/png;base64,Zm9v");

        // Over the byte limit fails loudly rather than sending a giant payload.
        assert!(image_data_url(&png, 2).is_err());
        // A non-image extension is rejected.
        let txt = dir.join("x.txt");
        std::fs::write(&txt, b"foo").unwrap();
        assert!(image_data_url(&txt, 1024).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn diff_reports_no_changes() {
        assert!(unified_diff("a\n", "a\n", "f").contains("no changes"));
        assert!(unified_diff("a\n", "b\n", "f").contains("+b"));
    }

    #[test]
    fn edit_requires_unique_match() {
        let dir = std::env::temp_dir().join("koda-edit-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "x\nx\n").unwrap();
        let c = ctx(&dir);
        let out = edit_file(&json!({"path": "a.txt", "old": "x", "new": "y"}), &c).unwrap();
        assert!(!out.ok, "{}", out.content);
        let out = edit_file(
            &json!({"path": "a.txt", "old": "x", "new": "y", "replace_all": true}),
            &c,
        )
        .unwrap();
        assert!(out.ok);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "y\ny\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Small fixture tree used by the read-only tools.
    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("koda-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    todo!();\n}\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn helper() {}\n").unwrap();
        std::fs::write(dir.join("README.md"), "# demo\ntodo: write docs\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir_all(dir.join("ignored")).unwrap();
        std::fs::write(dir.join("ignored/secret.rs"), "todo!()\n").unwrap();
        dir
    }

    #[test]
    fn read_file_numbers_lines_and_paginates() {
        let dir = fixture("read");
        let c = ctx(&dir);
        let out = read_file(&json!({"path": "src/main.rs"}), &c).unwrap();
        assert!(out.ok);
        assert!(out.content.contains("  1| fn main() {"), "{}", out.content);

        let out = read_file(&json!({"path": "src/main.rs", "offset": 2, "limit": 1}), &c).unwrap();
        assert!(out.content.contains("todo!();"));
        assert!(!out.content.contains("fn main"));
        assert!(out.content.contains("more lines"));

        let missing = read_file(&json!({"path": "nope.rs"}), &c).unwrap();
        assert!(!missing.ok);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_respects_gitignore_and_globs() {
        let dir = fixture("search");
        let c = ctx(&dir);
        let out = search(&json!({"pattern": "todo"}), &c).unwrap();
        assert!(out.ok);
        assert!(out.content.contains("src/main.rs:2"), "{}", out.content);
        assert!(out.content.contains("README.md:2"), "{}", out.content);
        assert!(
            !out.content.contains("secret.rs"),
            "gitignored file leaked: {}",
            out.content
        );

        let scoped = search(&json!({"pattern": "todo", "glob": "*.md"}), &c).unwrap();
        assert!(scoped.content.contains("README.md"));
        assert!(!scoped.content.contains("main.rs"));

        let none = search(&json!({"pattern": "zzz-not-here"}), &c).unwrap();
        assert!(none.content.contains("no matches"));

        let bad = search(&json!({"pattern": "("}), &c);
        assert!(bad.is_err(), "invalid regex should error");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_files_matches_globs() {
        let dir = fixture("find");
        let c = ctx(&dir);
        let out = find_files(&json!({"glob": "**/*.rs"}), &c).unwrap();
        assert!(out.content.contains("src/main.rs"), "{}", out.content);
        assert!(out.content.contains("src/lib.rs"));
        assert!(!out.content.contains("ignored/secret.rs"));

        let by_name = find_files(&json!({"glob": "README.md"}), &c).unwrap();
        assert!(by_name.content.contains("README.md"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_dir_reports_entries() {
        let dir = fixture("list");
        let c = ctx(&dir);
        let out = list_dir(&json!({"path": "."}), &c).unwrap();
        assert!(out.content.contains("src/"), "{}", out.content);
        assert!(out.content.contains("README.md"));
        // Connectors, and exactly one closing branch.
        assert!(out.content.contains("├─"), "{}", out.content);
        assert_eq!(out.content.matches("└─").count(), 1, "{}", out.content);

        let deep = list_dir(&json!({"path": ".", "depth": 2}), &c).unwrap();
        assert!(deep.content.contains("main.rs"), "{}", deep.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_command_captures_output_and_status() {
        let dir = fixture("cmd");
        let c = ctx(&dir);
        let out = run_command(&json!({"command": "echo hi && ls src"}), &c).await;
        assert!(out.ok, "{}", out.content);
        assert!(out.content.contains("hi"));
        assert!(out.content.contains("main.rs"));
        assert!(out.content.contains("exit code: 0"));

        let bad = run_command(&json!({"command": "exit 3"}), &c).await;
        assert!(!bad.ok);
        assert!(bad.content.contains("exit code: 3"));

        let slow = run_command(&json!({"command": "sleep 5", "timeout_ms": 200}), &c).await;
        assert!(!slow.ok);
        assert!(slow.content.contains("timed out"), "{}", slow.content);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_dispatches_unknown_tool() {
        let dir = fixture("dispatch");
        let c = ctx(&dir);
        let out = run("nope", json!({}), &c).await;
        assert!(!out.ok);
        assert!(out.content.contains("unknown tool"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncates_large_output_on_a_char_boundary() {
        let s = "é".repeat(100);
        let t = truncate(&s, 51);
        assert!(t.contains("truncated"));
        assert!(t.starts_with("é"));
    }
}
