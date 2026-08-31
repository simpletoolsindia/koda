//! System prompt. Kept deliberately short: local models have small contexts and
//! degrade quickly when the instructions crowd out the actual task.

use crate::config::{Config, Mode, ToolProtocol};
use crate::tools;
use std::fmt::Write as _;
use std::path::Path;

const BASE: &str = "\
You are koda, a coding agent running in a macOS terminal. You work on the user's \
codebase by calling tools.

Rules:
- Investigate before you edit. Read the relevant file before changing it.
- edit_file matches an exact substring: copy the text verbatim from a read_file result, \
  including indentation. Prefer edit_file over write_file for existing files.
- Use search and find_files to locate code; do not guess at paths.
- To find where a symbol is defined or used, call `codegraph` first (query \"symbol\" \
  for a name, \"file\" for a path, \"overview\" to map the project) — it is faster and \
  more precise than grep. If codegraph returns nothing useful or the symbol is not in \
  the graph, fall back to `search` and `find_files`.
- For anything outside the codebase — library docs, an unfamiliar error, an API or \
  version question you cannot answer from the repo — use `web_search` to find pages, \
  then `web_fetch` to read the most relevant one. If web search is unavailable or \
  returns nothing, say what you could not verify rather than guessing. (These are off \
  unless enabled; if a call reports the tool is disabled, do not retry it.)
- Make the smallest change that solves the task. Match the project's existing style.
- After changing code, verify it: run the project's build, tests or linter with run_command.
- Never run destructive commands (rm -rf, git reset --hard, force push) unless the user \
  explicitly asks.
- For work with three or more steps, call `todo` first with the whole plan, then \
  update it as you go so the user can follow along. Skip it for one-off edits.
- One tool call at a time, then read the result before deciding the next step.
- When the task is done, stop calling tools and reply with a short summary.

Style: terse. No preamble, no restating the request, no summaries of what you are about \
to do. Reply in plain text; use fenced code blocks only for code. The user sees tool \
calls and diffs already, so do not repeat them.";

/// The built-in base system prompt, exposed so the settings editor can
/// pre-populate its textarea when the user has no custom prompt yet — editing
/// from the real text is far easier than starting from a blank field.
pub fn base_prompt() -> &'static str {
    BASE
}

const TEXT_PROTOCOL: &str = "\
Tool calls use this exact format, one per message, at the end of your reply:

<tool_call>
{\"name\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}
</tool_call>

The JSON must be valid and on a single object. Write nothing after the closing tag. \
You will receive the tool result in the next message.

Available tools:
";

const SUBAGENT: &str = "\
You are a research subagent. Another agent delegated one investigation to you \
because your context is separate from theirs.

You can read, list, find and search files. You cannot modify anything or run \
commands — do not attempt to.

Work the question, then write a report. The report is the only thing that gets \
back to the caller, so it must stand alone:
- Answer the question directly in the first sentence.
- Cite exact paths and line numbers for anything you claim.
- Quote the few lines that matter, not whole files.
- Say plainly what you could not determine.
- No preamble, no description of your search process. Under 300 words.

Stop calling tools as soon as you can answer.";

/// System prompt for a delegated subagent.
pub fn subagent(root: &Path) -> String {
    let mut p = String::from(SUBAGENT);
    let _ = write!(p, "\n\nWorkspace: {}", root.display());
    if let Some(ctx) = environment(root) {
        let _ = write!(p, "\n{ctx}");
    }
    p
}

const PLAN_MODE: &str = "\
MODE: PLAN. Nothing on disk may change yet. The write and command tools are \
unavailable on purpose.

Investigate, then produce a plan the user can approve:
- What you understood the goal to be, in one sentence.
- The files involved, with the specific change each one needs.
- The order to do it in.
- How the result will be verified (which test, which command).
- Anything you are unsure about, stated as a question.

End your turn there. Ask the user to press ctrl+p to switch to execute mode. Do \
not pretend to have made changes.";

const VIBE_MODE: &str = "\
MODE: VIBE. Before doing any work, write the spec — briefly, but explicitly:
- Goal: what the user actually wants.
- Done when: the concrete, checkable conditions for success.
- Files to inspect, and what you expect to find in each.
- Changes to make.
- Validation: the exact command or test that proves it works.

Then do the work. When you believe you are finished, check your own result \
against the 'Done when' list — re-read what you changed and run the validation \
you named. If something does not hold, fix it before replying. If you delegated \
part of the work, verify the report against the files yourself; a subagent's \
claim is not evidence.

Report what you did and the evidence that it works.";

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
    p.push_str(&crate::skills::catalogue(skills));
    if cfg.memory {
        p.push_str(&memory.brief());
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
        p.push_str(BASE);
    } else {
        p.push_str(cfg.system_prompt.trim());
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
        Mode::Execute => {}
    }

    let _ = write!(p, "\n\nWorkspace: {}", root.display());
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
    }

    if cfg.subagents {
        p.push_str(
            "\n\nDelegation: for a wide search whose intermediate reading you do not need \
             (\"which files touch X?\", \"how does Y flow through this repo?\"), call \
             `delegate` instead of reading dozens of files yourself. You get back a report \
             and your context stays clean. Do the actual edits yourself.",
        );
    }

    if !cfg.instructions.trim().is_empty() {
        let _ = write!(p, "\n\nProject instructions:\n{}", cfg.instructions.trim());
    }

    // Project-level agent rules, if the repo has them.
    for name in ["AGENTS.md", "CLAUDE.md", ".koda.md"] {
        let path = root.join(name);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim();
            if !text.is_empty() {
                let clipped: String = text.chars().take(4000).collect();
                let _ = write!(p, "\n\nFrom {name}:\n{clipped}");
                break;
            }
        }
    }
    p
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

    if bits.is_empty() {
        None
    } else {
        Some(bits.join("\n"))
    }
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
