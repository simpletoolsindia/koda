//! System prompt. Kept deliberately short: local models have small contexts and
//! degrade quickly when the instructions crowd out the actual task.

use crate::config::{Config, Mode, ToolProtocol};
use crate::tools;
use std::fmt::Write as _;
use std::path::Path;

const BASE: &str = "\
You are koda, an autonomous coding agent. You work directly in the user's terminal to inspect and modify their codebase.

Rules:
- Read files before editing. Do not guess file contents.
- `edit_file` requires an exact substring match. Copy the target text verbatim from `read_file` results.
- Prefer `edit_file` over `write_file` for existing files.
- Verify changes by running builds, tests, or linters via `run_command`.
- Never run destructive commands without explicit request.
- Do not delete what you set up for the user (clones, builds, outputs) unless they ask.
- Make one write or command at a time, and wait for its result before the next step.
- Long jobs (large downloads, builds, servers): run them in the background with output to a log (`nohup CMD > job.log 2>&1 &`), then check the log, rather than waiting on a command that may time out.
- When a task takes >2 steps, call `todo` to lay out the plan, then call it again \
as each step finishes — done for what you completed, in_progress for what you are on. \
A plan you never update tells the user less than no plan at all. Mark a step done only when a tool result shows it happened; a step you skipped or could not do is reported as such, never as tested or finished.
- Users type fast: read a misspelled word by its context, and if a key word is still unclear, ask before acting on a guess.
- Web research: use `web_search`/`web_fetch` for static text. Use `browse` for dynamic sites, forms, tabs, media downloads, and any page `web_fetch` reports is behind a Cloudflare challenge (browse runs a real browser that clears it — after navigating, `wait` a few seconds if you see \"Just a moment…\", then read).
- Store durable facts (build commands, architecture) with `remember`.
- Store repeatable procedures (release checklists, setup steps) with `manage_skill`.

Style:
- Be terse and direct.
- No preamble, no narration, no restating the request.
- Reply in plain text. Use fenced blocks only for writing code.
- Stop calling tools and reply with a brief summary when finished.";

/// The base for fast mode: the load-bearing rules a small model needs and
/// nothing it does not.
///
/// The full BASE plus its guidance sections runs ~1.3k tokens, re-sent every
/// request. A small local model is bottlenecked on exactly that, and reads the
/// last few load-bearing lines more reliably than twenty. This keeps the rules
/// that prevent real damage or rework (read before edit, verify, one write at a
/// time, ask when a command is destructive) and drops the rest. The terseness
/// line is stern because the failure it addresses is real: a coder model given
/// this task wrote its summary twice.
const FAST_BASE: &str = "\
You are koda, an autonomous coding agent working in the user's terminal.

Rules:
- Read a file before editing it. `edit_file` needs an exact substring copied verbatim from `read_file`.
- Prefer `edit_file` over `write_file` for existing files.
- Verify with `run_command` (build, tests, linter) before you finish. If a check still fails, say so plainly — never call failing or unfinished work done.
- One write or command at a time; wait for the result before the next.
- Do not run destructive commands, or delete what you set up, unless asked.

Be terse. No preamble, no narration, no restating the task. When done, stop and give ONE short summary — never repeat it. Reply in plain text; fenced blocks only for code.";

/// The built-in base system prompt, exposed so the settings editor can
/// pre-populate its textarea when the user has no custom prompt yet — editing
/// from the real text is far easier than starting from a blank field.
pub fn base_prompt() -> &'static str {
    BASE
}

/// Only for the native tool protocol, where one message may carry several
/// calls. koda runs independent read-only calls concurrently, so a model that
/// asks for three files at once gets them in the time of the slowest one — but
/// only if it knows it is allowed to ask. The text protocol carries one call
/// per message, so this must not be said there.
const PARALLEL_READS: &str = "\n\n\
GATHERING CONTEXT: when several read-only lookups are independent — reading three files, a search plus a listing, two codegraph queries — ask for them in ONE step. They run concurrently. Writes, edits and commands stay one at a time, each verified before the next.";

/// Functional guardrail layered onto every prompt while the tool is enabled.
/// Keep this outside `BASE`: a custom system prompt replaces the base, but must
/// not accidentally remove the code-analysis workflow that makes koda precise.
const CODEGRAPH_GUIDANCE: &str = "\n\n\
CODE ANALYSIS — `codegraph` is how you look at code you did not write.

Your FIRST call on any task that touches existing code is `codegraph`. It reads a \
symbol graph of the project and answers in one call what search-and-read takes five \
calls to guess at:
- Where is X defined, and what breaks if I change it? -> codegraph query=symbol name=X
- What does this file define and import, and who depends on it? -> codegraph query=file path=...
- Unfamiliar project, or \"where does this live\"? -> codegraph query=overview
- A question that names no symbol (\"where is retry handled\")? -> codegraph query=search text=...

Always:
- Before you edit an existing symbol, call codegraph query=symbol on it. The graph names \
every file that uses it — that is the difference between a complete change and a local one.
- Do not grep for a name you could look up. `search`/`find_files` are for free text (a \
message, a TODO, a config value) or for when the graph has no answer.
- The graph is current, including files changed outside koda. It never needs rebuilding.";

/// What a language server adds that the graph cannot, stated as the rule for
/// when to reach past the graph.
///
/// Only added when a server for this project is actually installed: telling a
/// model about a tool it does not have is how a turn gets spent on a refusal.
const LSP_GUIDANCE: &str = "\n\nPRECISE ANSWERS — `lsp` asks this project's real language server, the same one an editor uses. The code graph matches names; the language server resolves them.

Reach for it when a name match is not good enough:
- Two things share a name, or the symbol is a trait/interface method -> lsp action=definition
- You need a TYPE, a signature, or what a value actually is -> lsp action=hover
- \"Who calls this, really\" before changing a signature -> lsp action=references
- What the compiler or type checker says is wrong with a file -> lsp action=diagnostics
- Follow a type or find implementations -> lsp action=type_definition / implementation

Give `file`, `line` (1-based) and `symbol` — the name as it appears on that line. You never have to work out a column. Keep using codegraph first for orientation; `lsp` is for when the answer has to be exact.";

/// The same job when there is no graph to ask. Kept parallel to
/// `CODEGRAPH_GUIDANCE` so the base rules never have to name either tool: a
/// rule in `BASE` telling the model to grep is a rule it follows, and it
/// outranks anything further down the prompt.
const FIND_GUIDANCE: &str = "\n\n\
FINDING CODE: `search` for text inside files (regex), `find_files` for paths by glob, \
`list_dir` to see what is there. Locate before you read; do not guess at paths.";

/// Delegation, stated as a working rule rather than a footnote.
///
/// It used to be one sentence appended after the tool schema, and sessions
/// show the result: `delegate` was called zero times, ever. A capability the
/// model never reaches for is the same as one that does not exist, so this
/// says what to send, what comes back, and what not to send.
const DELEGATION: &str = "\n\n\
DELEGATION — `delegate` is a second agent with its own context window.

Send it any investigation whose intermediate reading you do not need:
- \"which files touch X, and how?\"
- \"how does Y flow through this repo?\"
- \"does this project already have something that does Z?\"
- anything that means opening more than a handful of files to answer one question

You get back a written report; the files it opened never enter your context. \
That is the whole point — your context window is the scarce thing, and a wide \
search you run yourself spends it on files you will never look at again.

Always:
- Ask one self-contained question. The subagent cannot see this conversation, \
so give it the whole question in one go.
- It is read-only. Do the edits yourself, from what it reports.
- Do not delegate a single file you could just read, or something you already know.";

const TEXT_PROTOCOL: &str = "\
Tool calls use this exact JSON format, one per message, at the very end of your reply:

<tool_call>
{\"name\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}
</tool_call>

The JSON must be valid and on a single line. Write nothing after the closing tag. You will receive the tool result in the next message.

Available tools:
";

const SUBAGENT: &str = "\
You are a research subagent. Another agent delegated an investigation to you.

Rules:
- You have read-only access. You cannot modify files or run commands.
- Do the research, then write a standalone report for the caller.
- Answer the question directly in the very first sentence.
- Cite exact file paths and line numbers.
- Quote only the relevant lines, never entire files.
- State plainly what you could not determine. Do not guess.
- No preamble or narration of your process. Keep it under 300 words.

Stop calling tools as soon as you have the answer.";

/// System prompt for a delegated subagent.
pub fn subagent(root: &Path) -> String {
    let mut p = String::from(SUBAGENT);
    let _ = write!(p, "\n\n{}", now_line());
    let _ = write!(p, "\nWorkspace: {}", root.display());
    if let Some(ctx) = environment(root) {
        let _ = write!(p, "\n{ctx}");
    }
    p
}

const PLAN_MODE: &str = "\
MODE: PLAN. You cannot change files or run commands yet.

Investigate, then output a short plan for user approval:
1. Goal in one sentence.
2. Files involved and prose description of changes.
3. Step-by-step execution order.
4. How to verify the result (exact test/command).
5. Any clarifying questions.

Rules for Plan Mode:
- Describe the work, do not do it.
- No code blocks, no diffs, no patches.
- Do NOT output instructions for the user to apply manually. You will apply them later.
- End by asking the user to press ctrl+p to switch to execute mode so you can do the work.";

const EXECUTE_MODE: &str = "\
MODE: EXECUTE. You can now modify files and run commands. The write, edit, and command tools are fully available.

If you previously made a plan, carry it out now. Do not ask the user to switch to execute mode again. You are in it.
Do the work, verify it, and tell the user when you are done.";

const VIBE_MODE: &str = "\
MODE: VIBE. You operate autonomously end-to-end with minimal check-ins.

1. SPEC: Briefly state the goal, acceptance criteria, files to change, and verification command.
2. PLAN: Call the `todo` tool to lay out steps. Keep it updated.
3. EXECUTE: Call tools to make changes. Use `delegate` for side investigations to keep your context clean. You own the final result.
4. VERIFY: Re-read your changes, run the verification command, and check all acceptance criteria. Fix issues before finishing.

Report your actions and proof of success when fully done.";

pub fn build_with_skills(
    cfg: &Config,
    root: &Path,
    use_text_protocol: bool,
    mode: Mode,
    skills: &[crate::skills::Skill],
    memory: &crate::memory::Memory,
    learned: &str,
) -> String {
    let mut p = build(cfg, root, use_text_protocol, mode);
    // Fast mode also drops the per-request context that is nice-to-have rather
    // than load-bearing: the skill catalogue, remembered notes and learned
    // rules. Each is a paragraph or more re-sent every turn; a small model on a
    // quick task moves faster without them, and they return the moment fast is
    // off.
    if cfg.fast {
        return p;
    }
    p.push_str(&crate::skills::catalogue(skills));
    if cfg.memory {
        // One note per ~1k of window, between four and twenty: enough to be
        // useful on a small model without crowding out the request.
        let notes = (cfg.context_tokens / 1_000).clamp(4, 20);
        p.push_str(&memory.brief(notes));
        if !memory.is_empty() {
            p.push_str(
                "Use `remember` when you discover another durable fact about this project.\n",
            );
        }
    }
    // Learned, user-accepted conventions (Phase 1 self-improvement). Empty
    // unless learning is on and rules have been accepted, so it costs nothing
    // otherwise.
    if cfg.learning {
        p.push_str(learned);
    }
    p
}

pub fn build(cfg: &Config, root: &Path, use_text_protocol: bool, mode: Mode) -> String {
    let mut p = String::with_capacity(2048);
    // A user-supplied system prompt (set in /settings) fully replaces the
    // built-in base; everything else (mode notes, workspace, tools, skills,
    // instructions) is still layered on so the agent stays functional.
    if cfg.system_prompt.trim().is_empty() {
        p.push_str(if cfg.fast { FAST_BASE } else { BASE });
    } else {
        p.push_str(cfg.system_prompt.trim());
    }
    // Fast mode drops the guidance sections a small local model pays for on
    // every request: the codegraph workflow, the language-server note, the
    // delegation explainer, the MORE TOOLS list and the parallel-reads note.
    // The tools they describe are still reachable — `load_tools` and a direct
    // call both work — so this costs discoverability, not capability.
    if cfg.fast {
        // One line, not the full workflow: a small model still needs to know
        // codegraph is the fast way to locate a symbol, or it greps and reads
        // whole files across several round-trips. Only when it is enabled.
        if cfg.codegraph {
            p.push_str(
                "\n\nTo locate code, use `codegraph` (query=symbol name=X for where a symbol is \
                 defined and who calls it; query=search text=… when you have no name) rather than \
                 grepping and reading whole files.",
            );
        }
    } else {
        if cfg.codegraph {
            p.push_str(CODEGRAPH_GUIDANCE);
        } else {
            p.push_str(FIND_GUIDANCE);
        }
        if cfg.lsp && !crate::lsp::available(root).is_empty() {
            p.push_str(LSP_GUIDANCE);
        }
        if cfg.subagents {
            p.push_str(DELEGATION);
        }
    }
    // Servers lending tools are named once, so the model knows that a
    // `mcp__…` name in its list reaches out of the project — and that `mcp`
    // itself reaches the resources and prompts those servers publish.
    if cfg.mcp && crate::mcp::any_tools() {
        let names: Vec<String> = crate::mcp::catalog()
            .into_iter()
            .filter(|s| s.connected && !s.tools.is_empty())
            .map(|s| format!("{} ({} tools)", s.name, s.tools.len()))
            .collect();
        if !names.is_empty() {
            let _ = write!(
                p,
                "\n\nCONNECTED SERVICES (MCP): {}. Their tools are in your list as \
                 `mcp__<server>__<tool>` and reach systems outside this workspace — \
                 use them when the answer is not in the code. `mcp` lists what each \
                 one also publishes as resources and prompts.",
                names.join(", ")
            );
        }
    }
    // Name what is not in the schema. A tool the model cannot see and is not
    // told about is a tool that does not exist — which is the one way this
    // could cost accuracy, so it is spelled out rather than implied. Fast mode
    // hides more tools but says so once, in one line, rather than a paragraph.
    if cfg.fast {
        p.push_str(
            "\n\nMore tools (delegate, remember, view_image, browse, …) are not \
             listed to keep this small. Call `load_tools` for a group, or just call the tool \
             by name — it loads automatically.",
        );
    } else {
        let groups = crate::tools::deferred_summary(|t| match t {
            "browse" => cfg.browser,
            _ => true,
        });
        if !groups.trim().is_empty() {
            let _ = write!(
                p,
                "\n\nMORE TOOLS — these exist but are not in your tool list yet, so that the \
                 list stays small:\n{groups}\
                 Call `load_tools` with the group name to bring one in. You may also just call \
                 the tool you want by name — it is loaded for you automatically, so a guess \
                 costs nothing."
            );
        }
    }
    if !cfg.fast && !(use_text_protocol || cfg.tool_protocol == ToolProtocol::Text) {
        p.push_str(PARALLEL_READS);
    }
    match mode {
        Mode::Plan => {
            p.push_str("\n\n");
            p.push_str(PLAN_MODE);
        }
        Mode::Vibe => {
            p.push_str("\n\n");
            p.push_str(VIBE_MODE);
        }
        Mode::Execute => {
            p.push_str("\n\n");
            p.push_str(EXECUTE_MODE);
        }
    }

    let _ = write!(p, "\n\n{}", now_line());
    let _ = write!(p, "\nWorkspace: {}", root.display());
    if let Some(ctx) = environment(root) {
        let _ = write!(p, "\n{ctx}");
    }

    if use_text_protocol || cfg.tool_protocol == ToolProtocol::Text {
        let allow = if mode.read_only() {
            Some(tools::PLAN_TOOLS)
        } else {
            None
        };
        p.push_str("\n\n");
        p.push_str(TEXT_PROTOCOL);
        p.push_str(&tools::text_protocol_help_for(allow));
        // Tools lent by MCP servers are not in the built-in table, so the text
        // protocol has to be told about them separately or a model without
        // native tool calling can never reach them.
        if cfg.mcp {
            p.push_str(&crate::mcp::text_protocol_help(mode.read_only()));
        }
    }

    if !cfg.instructions.trim().is_empty() {
        let _ = write!(p, "\n\nProject instructions:\n{}", cfg.instructions.trim());
    }

    // Project-level agent rules, if the repo has them.
    // Standing instructions, general first and specific second, so a project
    // can override a habit rather than merely restate it.
    //
    // The user-level file is the half koda was missing. A preference that is
    // true of everything you write -- the test runner you use, that you want
    // no comments unless you asked for them, which spelling -- belonged in
    // every project's AGENTS.md, copied by hand, or nowhere. Both Gemini CLI
    // (`~/.gemini/GEMINI.md`) and Claude Code settled on a user-level file
    // above the project one; this is the same shape.
    for (label, text) in user_instructions()
        .into_iter()
        .chain(project_instructions(root))
    {
        let _ = write!(p, "\n\nFrom {label}:\n{text}");
    }
    p
}

/// How much of one instruction file reaches the prompt.
///
/// Bounded because this text sits in the cached preamble of every request: on a
/// local model each thousand tokens here is a couple of seconds of one-time
/// prefill and a permanent slice of the context window. Generous enough for a
/// real set of house rules, small enough that a README pasted in by mistake
/// cannot cost the user their window.
const MAX_INSTRUCTION_CHARS: usize = 4000;

fn read_clipped(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(MAX_INSTRUCTION_CHARS).collect())
}

/// The user's own standing instructions, applying to every project.
fn user_instructions() -> Option<(String, String)> {
    let dir = crate::config::config_dir();
    // `KODA.md` first since it is koda's own name; `AGENTS.md` accepted in the
    // same place so a user who already keeps one there is not asked to
    // maintain a second copy under a different name.
    for name in ["KODA.md", "AGENTS.md"] {
        if let Some(text) = read_clipped(&dir.join(name)) {
            return Some((format!("your {name} (applies to every project)"), text));
        }
    }
    None
}

/// The project's instructions. First match wins, deliberately: `AGENTS.md` and
/// `CLAUDE.md` are usually the same content under two names, and sending both
/// would pay for it twice.
fn project_instructions(root: &Path) -> Option<(String, String)> {
    for name in ["AGENTS.md", "CLAUDE.md", ".koda.md"] {
        if let Some(text) = read_clipped(&root.join(name)) {
            return Some((name.to_string(), text));
        }
    }
    None
}

/// A few cheap facts that stop the model from guessing about the project.
fn environment(root: &Path) -> Option<String> {
    let mut bits: Vec<String> = Vec::new();

    let markers = [
        ("Cargo.toml", "Rust/Cargo"),
        ("package.json", "Node"),
        ("pyproject.toml", "Python"),
        ("requirements.txt", "Python"),
        ("go.mod", "Go"),
        ("pom.xml", "Maven"),
        ("build.gradle", "Gradle"),
        ("Makefile", "Make"),
        ("CMakeLists.txt", "CMake"),
    ];
    let found: Vec<&str> = markers
        .iter()
        .filter(|(f, _)| root.join(f).exists())
        .map(|(_, label)| *label)
        .collect();
    if !found.is_empty() {
        bits.push(format!("Project type: {}", dedup(&found).join(", ")));
    }

    // Top-level entries give the model a cheap map of the repo.
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    return None;
                }
                let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(if dir { format!("{name}/") } else { name })
            })
            .take(40)
            .collect();
        names.sort();
        if !names.is_empty() {
            bits.push(format!("Top level: {}", names.join(" ")));
        }
    }

    if root.join(".git").exists() {
        bits.push("Git repository.".into());
    }

    if let Some(py) = python_toolchain() {
        bits.push(py);
    }

    if bits.is_empty() {
        None
    } else {
        Some(bits.join("\n"))
    }
}

/// Which interpreter `python3` runs and whether `pip` installs into it.
///
/// They can disagree, and nothing in a failing import says so: a real session
/// had `python3` → Homebrew 3.14 while `pip` → the Command Line Tools' 3.9, and
/// spent a dozen steps installing packages the interpreter never saw. One
/// process spawn, once per process.
fn python_toolchain() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let out = std::process::Command::new("python3")
                .args([
                    "-c",
                    "import sys; print(sys.version.split()[0]); print(sys.executable)",
                ])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let mut lines = text.lines();
            let version = lines.next()?.trim().to_string();
            let exe = lines.next()?.trim().to_string();
            Some(python_line(&version, &exe, pip_interpreter().as_deref()))
        })
        .clone()
}

fn python_line(version: &str, exe: &str, pip_python: Option<&str>) -> String {
    let mut line = format!("Python: `python3` is {version} ({exe}).");
    let same = |a: &str, b: &str| {
        let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
        a == b || canon(a) == canon(b)
    };
    if let Some(pip) = pip_python.filter(|p| !same(p, exe)) {
        let _ = write!(
            line,
            " `pip` installs into a different interpreter ({pip}): use `python3 -m pip`, or a venv."
        );
    }
    line
}

/// The interpreter named in the shebang of the `pip` found on PATH. `None` when
/// there is no `pip`, or it defers to PATH itself (`#!/usr/bin/env python3`).
fn pip_interpreter() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let pip = std::env::split_paths(&path)
        .map(|d| d.join("pip"))
        .find(|p| p.is_file())?;
    let head = std::fs::read(&pip).ok()?;
    let first = String::from_utf8_lossy(&head[..head.len().min(512)])
        .lines()
        .next()?
        .to_string();
    let interp = first
        .strip_prefix("#!")?
        .split_whitespace()
        .next()?
        .to_string();
    (!interp.ends_with("/env")).then_some(interp)
}

fn dedup(v: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        if !out.iter().any(|o| o == s) {
            out.push((*s).to_string());
        }
    }
    out
}

/// The current date, for the system prompt.
///
/// A model with no clock guesses the year from its training data, and then
/// dates a changelog entry or a copyright header wrong. So the date is worth
/// its tokens.
///
/// The *time* is not, and used to be here at minute resolution. The system
/// prompt is the cached KV prefix for a local model, and that cache is
/// invalidated by any change at all -- so a minute hand nobody reads made every
/// launch, and every rebuild of the prompt, a full re-prefill of the preamble.
/// Measured against a local 30B that is about eleven seconds, paid whenever the
/// clock ticked over. At day resolution the prompt is byte-identical from one
/// run to the next, and a restart answers immediately.
///
/// A model that genuinely needs the wall clock can run `date`, and gets a
/// precise answer instead of a stamp that was stale the moment it was taken.
fn now_line() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let offset = local_offset_seconds();
    let local = secs + offset as i64;

    let days = local.div_euclid(86_400);
    // The calendar arithmetic already exists, for dating learned rules.
    let date = crate::learning::ymd(days.max(0) as u32);
    // 1970-01-01 was a Thursday.
    const DAY: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let weekday = DAY[(days + 3).rem_euclid(7) as usize];

    let (sign, off) = if offset < 0 {
        ('-', -offset)
    } else {
        ('+', offset)
    };
    format!(
        "Current date: {weekday} {date} (local time, UTC{sign}{:02}:{:02}). \
         Run `date` if you need the time of day.",
        off / 3600,
        (off % 3600) / 60,
    )
}

/// The machine's UTC offset in seconds, probed once per process.
///
/// std has no timezone database and koda takes no dependency for one, so the
/// offset comes from the platform's own date command -- one cheap spawn for the
/// life of the process, cached. If that fails we report UTC, which is wrong by
/// hours but never wrong about what it is: the line says which zone it is in.
fn local_offset_seconds() -> i32 {
    static OFFSET: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| probe_offset().unwrap_or(0))
}

#[cfg(not(windows))]
fn probe_offset() -> Option<i32> {
    let out = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()?;
    parse_offset(std::str::from_utf8(&out.stdout).ok()?)
}

#[cfg(windows)]
fn probe_offset() -> Option<i32> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-Date).ToString('zzz')",
        ])
        .output()
        .ok()?;
    parse_offset(std::str::from_utf8(&out.stdout).ok()?)
}

/// `+0530`, `-08:00` -> seconds east of UTC.
fn parse_offset(raw: &str) -> Option<i32> {
    let s: String = raw.trim().chars().filter(|c| *c != ':').collect();
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, &s[..]),
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[..2].parse().ok()?;
    let m: i32 = digits[2..].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

#[cfg(test)]
mod tests {

    /// A `pip` that installs somewhere other than `python3` is named in the
    /// prompt; a matching one adds nothing but the version.
    #[test]
    fn a_pip_for_another_python_is_called_out() {
        let split = super::python_line(
            "3.14.7",
            "/opt/homebrew/bin/python3",
            Some("/Library/Developer/CommandLineTools/usr/bin/python3"),
        );
        assert!(split.contains("3.14.7"), "{split}");
        assert!(split.contains("different interpreter"), "{split}");
        assert!(split.contains("python3 -m pip"), "{split}");

        let same = super::python_line("3.12.1", "/usr/bin/python3", Some("/usr/bin/python3"));
        assert!(!same.contains("different"), "{same}");
        let none = super::python_line("3.12.1", "/usr/bin/python3", None);
        assert!(!none.contains("different"), "{none}");
    }

    /// A preference that is true of everything you write had nowhere to live:
    /// it went into every project's AGENTS.md by hand, or nowhere.
    #[test]
    fn user_instructions_apply_to_every_project_and_the_project_can_override() {
        let home = crate::config::test_root("prompt-user-instructions");
        let cfgdir = home.join("cfg");
        std::fs::create_dir_all(&cfgdir).unwrap();
        // `config_dir()` honours XDG_CONFIG_HOME, which is how this reaches a
        // scratch directory instead of the real one.
        std::env::set_var("XDG_CONFIG_HOME", &cfgdir);
        std::fs::write(cfgdir.join("koda/KODA.md"), "")
            .or_else(|_| {
                std::fs::create_dir_all(cfgdir.join("koda"))?;
                std::fs::write(cfgdir.join("koda/KODA.md"), "Always use British spelling.")
            })
            .unwrap();

        let root = crate::config::test_root("prompt-project-instructions");
        let cfg = Config::default();

        // With only the user file, it is present.
        let p = build(&cfg, &root, false, Mode::Execute);
        assert!(p.contains("British spelling"), "user instructions missing");
        assert!(p.contains("applies to every project"), "{p}");

        // A project file is added *after* it, so the specific one is read last
        // and wins where they disagree.
        std::fs::write(
            root.join("AGENTS.md"),
            "This project uses American spelling.",
        )
        .unwrap();
        let p = build(&cfg, &root, false, Mode::Execute);
        let u = p.find("British spelling").expect("user rules kept");
        let a = p.find("American spelling").expect("project rules added");
        assert!(
            u < a,
            "the project's rules must come after the user's:\n{p}"
        );

        // The two names for a project file are the same content under two
        // names in most repos, so only one is sent.
        std::fs::write(root.join("CLAUDE.md"), "DUPLICATE").unwrap();
        let p = build(&cfg, &root, false, Mode::Execute);
        assert!(
            !p.contains("DUPLICATE"),
            "both project files were sent:\n{p}"
        );

        // Nothing here may grow without bound: this text is in the cached
        // preamble of every single request.
        std::fs::write(root.join("AGENTS.md"), "x".repeat(50_000)).unwrap();
        let p = build(&cfg, &root, false, Mode::Execute);
        // The longest run of `x`, not every `x` in the prompt — the base
        // instructions contain the letter too.
        let longest = p
            .split(|c| c != 'x')
            .map(|run| run.len())
            .max()
            .unwrap_or(0);
        assert_eq!(
            longest, MAX_INSTRUCTION_CHARS,
            "an oversized instruction file was not clipped to the cap"
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    use super::*;

    #[test]
    fn custom_prompt_keeps_codegraph_workflow_when_enabled() {
        let cfg = Config {
            system_prompt: "Custom concise reviewer.".into(),
            codegraph: true,
            ..Config::default()
        };
        let prompt = build(
            &cfg,
            Path::new("/tmp/koda-prompt-test"),
            false,
            Mode::Execute,
        );
        assert!(prompt.starts_with("Custom concise reviewer."));
        assert!(prompt.contains("CODE ANALYSIS"), "{prompt}");
        assert!(prompt.contains("FIRST call"), "{prompt}");
        // The workflow has to name the calls, not just the tool: a local model
        // that is told "use codegraph" without a shape reaches for grep.
        assert!(prompt.contains("query=symbol"), "{prompt}");
    }

    /// Fast mode exists to shrink what a local model prefills every request.
    /// The prompt must come out far smaller, keep the load-bearing rules, and
    /// drop the guidance sections whose tools are still reachable.
    #[test]
    fn fast_mode_ships_a_much_smaller_prompt() {
        let root = Path::new("/tmp/koda-fast-prompt");
        let full = build(&Config::default(), root, false, Mode::Execute);
        let fast = build(
            &Config {
                fast: true,
                ..Config::default()
            },
            root,
            false,
            Mode::Execute,
        );
        assert!(
            fast.len() * 2 < full.len(),
            "fast prompt should be well under half of full: {} vs {}",
            fast.len(),
            full.len()
        );
        // The rules that prevent damage or rework stay.
        assert!(fast.contains("Read a file before editing"), "{fast}");
        assert!(fast.contains("Verify"), "{fast}");
        assert!(fast.contains("One write or command at a time"), "{fast}");
        // The heavy guidance sections go, replaced by a one-line codegraph hint.
        assert!(!fast.contains("CODE ANALYSIS"), "{fast}");
        assert!(!fast.contains("DELEGATION"), "{fast}");
        assert!(!fast.contains("in ONE step"), "{fast}");
        assert!(
            fast.contains("codegraph"),
            "fast keeps a codegraph hint: {fast}"
        );
        // But the model is still told the hidden tools exist and how to reach them.
        assert!(fast.contains("load_tools"), "{fast}");
        // And the terseness rule that stops the double-summary is stern.
        assert!(fast.contains("never repeat"), "{fast}");
    }

    /// koda runs independent read-only calls concurrently, but only if the
    /// model batches them — and the text protocol cannot carry a batch, so the
    /// invitation must not appear there.
    #[test]
    fn batched_reads_are_invited_only_on_the_native_protocol() {
        let cfg = Config {
            tool_protocol: ToolProtocol::Native,
            ..Config::default()
        };
        let native = build(&cfg, Path::new("/tmp"), false, Mode::Execute);
        assert!(native.contains("in ONE step"), "{native}");
        assert!(
            !native.contains("One tool call per turn"),
            "the blanket rule keeps every batch serial: {native}"
        );

        let text = build(&cfg, Path::new("/tmp"), true, Mode::Execute);
        assert!(
            !text.contains("in ONE step"),
            "the text protocol carries one call per message: {text}"
        );
    }

    /// The base rules are what a small model actually follows. If they say
    /// "use search to locate code", it greps -- whatever the codegraph section
    /// further down asks for. So the rules must not name a locating tool at
    /// all; that job belongs to whichever guidance block is in force.
    /// Sessions show `delegate` called zero times, ever. A capability the
    /// model never reaches for is the same as one that does not exist, so the
    /// prompt has to say what to send it and what comes back — not mention it.
    #[test]
    fn delegation_is_a_working_rule_not_a_footnote() {
        let cfg = Config {
            subagents: true,
            ..Config::default()
        };
        let with = build(&cfg, Path::new("/tmp"), false, Mode::Execute);
        assert!(with.contains("DELEGATION"), "{with}");
        assert!(with.contains("its own context window"));
        // The shape of the ask matters: a subagent cannot see this conversation.
        assert!(with.contains("self-contained question"), "{with}");
        // It sits with the other working rules, not after the tool schema.
        let delegation = with.find("DELEGATION").expect("present");
        let workspace = with.find("Workspace:").expect("present");
        assert!(delegation < workspace, "delegation belongs with the rules");

        let off = Config {
            subagents: false,
            ..Config::default()
        };
        let without = build(&off, Path::new("/tmp"), false, Mode::Execute);
        assert!(!without.contains("DELEGATION"), "{without}");
    }

    #[test]
    fn base_rules_do_not_pick_a_locating_tool() {
        assert!(!base_prompt().contains("find_files"), "{}", base_prompt());
        assert!(!base_prompt().contains("locate code"), "{}", base_prompt());
        let cfg = Config {
            codegraph: true,
            ..Config::default()
        };
        let with = build(&cfg, Path::new("/tmp"), false, Mode::Execute);
        assert!(with.contains("CODE ANALYSIS"));
        assert!(!with.contains("FINDING CODE"), "one rule, not two: {with}");
    }

    #[test]
    fn offsets_parse_in_both_shapes_and_signs() {
        assert_eq!(parse_offset("+0530\n"), Some(19_800));
        assert_eq!(parse_offset("-08:00"), Some(-28_800));
        assert_eq!(parse_offset("+0000"), Some(0));
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("UTC"), None);
    }

    /// The whole point of the line: a model that asks "what year is it" must
    /// find the answer in its prompt, in both the main and subagent prompts.
    #[test]
    fn prompts_state_the_current_date() {
        let cfg = Config::default();
        let main = build(&cfg, Path::new("/tmp"), false, Mode::Execute);
        assert!(main.contains("Current date:"), "{main}");
        let sub = subagent(Path::new("/tmp"));
        assert!(sub.contains("Current date:"), "{sub}");
    }

    /// The preamble is the model server's cached KV prefix, and that cache is
    /// invalidated by *any* change to it. A clock with a minute hand therefore
    /// cost a full re-prefill of the whole preamble every time the minute
    /// rolled over -- about eleven seconds on a local 30B, paid on every launch
    /// and every rebuild of the prompt, for a stamp that was stale the moment
    /// it was taken.
    #[test]
    fn the_preamble_does_not_change_with_the_clock() {
        let cfg = Config::default();
        let root = Path::new("/tmp");
        let first = build(&cfg, root, false, Mode::Execute);
        // Two builds a notional minute apart must be byte-identical. Building
        // twice in a row is the same test the old code failed roughly once a
        // minute, so the assertion is on the content, not on timing.
        let again = build(&cfg, root, false, Mode::Execute);
        assert_eq!(first, again, "the preamble is not stable between builds");

        // No time of day anywhere in it. `\d\d:\d\d` is what the old line
        // emitted; the UTC offset is written without one.
        let clockish = first
            .lines()
            .find(|l| l.contains("Current date:"))
            .expect("the date line");
        assert!(
            !clockish.contains("date and time"),
            "the minute hand is back: {clockish}"
        );
        // The date itself must still be there — a model with no calendar dates
        // a changelog entry from its training cutoff.
        assert!(
            clockish.contains("UTC"),
            "the offset should stay, so the date is unambiguous: {clockish}"
        );
    }

    #[test]
    fn disabled_codegraph_is_not_advertised_in_prompt() {
        let cfg = Config {
            system_prompt: "Custom concise reviewer.".into(),
            codegraph: false,
            ..Config::default()
        };
        let prompt = build(
            &cfg,
            Path::new("/tmp/koda-prompt-test"),
            false,
            Mode::Execute,
        );
        assert!(!prompt.contains("CODE ANALYSIS WORKFLOW"), "{prompt}");
        assert!(!prompt.contains("`codegraph`"), "{prompt}");
        // With no graph, the model still has to be told how to find code.
        assert!(prompt.contains("FINDING CODE"), "{prompt}");
    }
}
