//! System prompt. Kept deliberately short: local models have small contexts and
//! degrade quickly when the instructions crowd out the actual task.

use crate::config::{Config, Mode, ToolProtocol};
use crate::tools;
use std::fmt::Write as _;
use std::path::Path;

const BASE: &str = "\
You are koda, an autonomous coding agent. You work directly in the user's terminal to inspect and modify their codebase.

Rules:
- Read files before editing. Do not guess file contents. Use search and find_files to locate code.
- `edit_file` requires an exact substring match. Copy the target text verbatim from `read_file` results.
- Prefer `edit_file` over `write_file` for existing files.
- Verify changes by running builds, tests, or linters via `run_command`.
- Never run destructive commands without explicit request.
- One tool call per turn. Wait for the result before deciding the next step.
- When a task takes >2 steps, use `todo` to plan and track progress.
- Web research: use `web_search`/`web_fetch` for static text. Use `browse` for dynamic sites, forms, tabs, and media downloads.
- Store durable facts (build commands, architecture) with `remember`.
- Store repeatable procedures (release checklists, setup steps) with `manage_skill`.

Style:
- Be terse and direct.
- No preamble, no narration, no restating the request.
- Reply in plain text. Use fenced blocks only for writing code.
- Stop calling tools and reply with a brief summary when finished.";

/// The built-in base system prompt, exposed so the settings editor can
/// pre-populate its textarea when the user has no custom prompt yet — editing
/// from the real text is far easier than starting from a blank field.
pub fn base_prompt() -> &'static str {
    BASE
}

/// Functional guardrail layered onto every prompt while the tool is enabled.
/// Keep this outside `BASE`: a custom system prompt replaces the base, but must
/// not accidentally remove the code-analysis workflow that makes koda precise.
const CODEGRAPH_GUIDANCE: &str =
    "\n\nCODE ANALYSIS: For questions about where a symbol is defined/used, dependencies, or project structure, call `codegraph` FIRST. Use `search` or `find_files` only for free text or as a fallback.";

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
    let _ = write!(p, "\n\nWorkspace: {}", root.display());
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
        let prompt = build(
            &cfg,
            Path::new("/tmp/koda-prompt-test"),
            false,
            Mode::Execute,
        );
        assert!(prompt.starts_with("Custom concise reviewer."));
        assert!(prompt.contains("CODE ANALYSIS:"), "{prompt}");
        assert!(prompt.contains("call `codegraph` FIRST"), "{prompt}");
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
    }
}
