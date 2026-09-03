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
- Use search and find_files to locate free text or files; do not guess at paths.
- When you discover a durable fact about THIS project — the build/test command, \
  where a subsystem lives, a naming or library convention, a project-specific \
  idiom — call `remember` with one plain sentence so the next session starts \
  knowing it. Only durable facts, not what you are doing right now.
- When you work out a *procedure* that was not obvious and will come up again — \
  how to run this repo's integration tests, how to add a subsystem end to end, a \
  release checklist — call `manage_skill` to write it down as a skill: the steps, \
  the exact commands, and what to check. Do it once the procedure has actually \
  worked, not while you are still guessing. A fact is `remember`; a procedure is a \
  skill. If a skill already covers the situation, read it and update it rather \
  than adding a second one.
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

/// Functional guardrail layered onto every prompt while the tool is enabled.
/// Keep this outside `BASE`: a custom system prompt replaces the base, but must
/// not accidentally remove the code-analysis workflow that makes koda precise.
const CODEGRAPH_GUIDANCE: &str = "\n\nCODE ANALYSIS WORKFLOW (mandatory): for questions about where a symbol is \
defined or used, what calls it, file dependencies, project structure, or where \
to make a change, call `codegraph` FIRST (`symbol`, `file`, or `overview`). Use \
its file/line result to choose what to read. Use `search`/`find_files` first only \
for free text or literals, and as a fallback when codegraph has no match.";

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

Investigate, then produce a short plan the user can approve:
- What you understood the goal to be, in one sentence.
- The files involved and what needs to change in each, described in prose.
- The order to do it in.
- How the result will be verified (which test, which command).
- Anything you are unsure about, stated as a question.

Describe the work; do not hand it over. No code blocks, no diffs, no patches, \
no numbered instructions for the user to carry out by hand. They did not ask to \
do this themselves -- they asked you, in a mode that cannot write yet. Writing \
the change out for them to paste is slower, drops the diff preview and the undo \
that koda gives an edit, and quietly makes them do your job.

End the turn by asking them to press ctrl+p for execute mode, and say you will \
carry out the plan yourself once they do. Do not pretend to have made changes.";

const EXECUTE_MODE: &str = "\
MODE: EXECUTE. You can change files and run commands. The write, edit and \
command tools are available to you right now, subject to the user's approval \
settings — so call them. Proposing an edit is not the same as making it.

If earlier in this conversation you said you could not act because you were in \
plan mode, that restriction is lifted and no longer applies. Do not repeat it, \
and do not ask the user to switch to execute mode — you are already in it. Get \
on with the work that was planned.";

const VIBE_MODE: &str = "\
MODE: VIBE — autonomous spec-driven delivery. Work end-to-end with minimal \
check-ins, and hold yourself to the spec.

1. SPEC first (briefly, but explicitly):
   - Goal: what the user actually wants.
   - Done when: the concrete, checkable conditions for success.
   - Files to inspect and what you expect in each.
   - Changes to make.
   - Validation: the exact command or test that proves it works.

2. PLAN with the `todo` tool: lay out the steps, then keep it updated as you go.

3. DO the work. For a large or many-part task, ORCHESTRATE: hand self-contained \
   subtasks to `delegate` (pass a `role` like dev, qa, or tester when a matching \
   role skill exists; otherwise delegate without a role). Keep the hands-on work \
   yourself when a task is small. You are the integrator — you own the result.

4. VERIFY before you finish: re-read what you changed, run the validation you \
   named, and check every 'Done when' condition. If you delegated, verify each \
   report against the actual files — a subagent's claim is not evidence. Fix any \
   gap before replying.

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
    if cfg.codegraph {
        p.push_str(CODEGRAPH_GUIDANCE);
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_prompt_keeps_codegraph_workflow_when_enabled() {
        let cfg = Config {
            system_prompt: "Custom concise reviewer.".into(),
            codegraph: true,
            ..Config::default()
        };
        let prompt = build(&cfg, Path::new("/tmp/koda-prompt-test"), false, Mode::Execute);
        assert!(prompt.starts_with("Custom concise reviewer."));
        assert!(prompt.contains("CODE ANALYSIS WORKFLOW"), "{prompt}");
        assert!(prompt.contains("call `codegraph` FIRST"), "{prompt}");
    }

    #[test]
    fn disabled_codegraph_is_not_advertised_in_prompt() {
        let cfg = Config {
            system_prompt: "Custom concise reviewer.".into(),
            codegraph: false,
            ..Config::default()
        };
        let prompt = build(&cfg, Path::new("/tmp/koda-prompt-test"), false, Mode::Execute);
        assert!(!prompt.contains("CODE ANALYSIS WORKFLOW"), "{prompt}");
        assert!(!prompt.contains("`codegraph`"), "{prompt}");
    }
}
