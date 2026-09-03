//! `/detailhelp` — a full, trendy feature & command guide rendered as a local
//! HTML page and opened in the browser.
//!
//! The slash-command reference is generated from the same `COMMANDS` table the
//! TUI uses, so it can never drift from what koda actually accepts. The page is
//! written to a temp file and handed to the platform's default opener.

use std::io::Write;
use std::path::PathBuf;

/// Feature blurbs shown as cards. Kept here (not in the prompt) purely for docs.
const FEATURES: &[(&str, &str)] = &[
    ("Local-first", "Talks to any OpenAI-compatible server — Ollama, LM Studio, llama.cpp, vLLM, MLX. One ~6 MB binary, no runtime deps."),
    ("Modes", "plan (read-only), execute (edits with approval), and vibe (spec-driven: plans, delegates, and verifies its own work). Cycle with ctrl+p or /mode."),
    ("Autonomy tiers", "ask → auto-write → full-auto, cycled live with /auto. Every write shows a diff first; commands ask before running."),
    ("Reasoning effort", "Tell thinking models how hard to think: /reason off | low | medium | high."),
    ("Code graph", "The project is scanned into a symbol graph so the model asks where a name lives instead of grepping. Ask overview / symbol / file."),
    ("Memory", "Durable facts and command outcomes are kept in .koda/memory.md, so the next session already knows your test runner."),
    ("Sessions", "Every conversation is saved. /resume reopens one, /search finds by text, /fork branches a copy."),
    ("Subagents & /orc", "Delegate wide read-only investigations to a child agent with its own context. /orc <task> runs it in vibe mode, which plans, delegates to role agents, and verifies the result."),
    ("Self-authored skills", "koda writes down procedures it worked out with manage_skill, so the next session starts with them — a fact goes to remember, a procedure becomes a skill. Add a role and the same skill becomes a delegatable agent (qa, reviewer, …)."),
    ("Web search", "DuckDuckGo out of the box, or your own SearXNG. Toggle with /websearch; pick the backend in /settings."),
    ("Vision", "Attach an image with @screenshot.png for a vision-capable model."),
    ("Watch mode", "Aider-style: end a comment with AI! to implement it, or AI? to ask. koda acts when idle. Toggle with /watch."),
    ("Debug capture", "/debug records the exact request and raw response of each turn to ~/.local/state/koda/debug for inspection."),
    ("Web UI", "A live browser control center: every turn traced end to end (model calls, tool calls, compaction) with the exact request, raw response and reasoning behind each step — plus live control of model, mode, autonomy, toggles, memory, learned rules and sessions. Enable in /settings."),
    ("Custom tools", "Add your own shell-backed tools in config with [[tools]] — the agent calls them like built-ins."),
    ("System prompt", "Override the built-in prompt (and per-tool prompts) right in /settings."),
    ("Themes", "neon, tokyo-night, dracula, gruvbox, nord and more — switch live with /theme."),
];

/// Key bindings, mirrored from the /help panel.
const KEYS: &[(&str, &str)] = &[
    ("enter", "send · ctrl+j newline"),
    ("ctrl+c", "interrupt · twice quits · ctrl+d quit"),
    ("ctrl+p", "cycle mode"),
    ("ctrl+r", "expand last tool output"),
    ("ctrl+t", "expand last reasoning"),
    ("pgup / pgdn", "scroll (wheel works)"),
    ("up / down", "input history"),
    ("tab", "complete · pick a mentioned file"),
    ("@", "mention a file or image"),
    ("ctrl+a/e/k/u/w", "line editing"),
    ("y / a / n", "at a prompt: once / always / deny"),
    ("esc", "close an overlay"),
];

/// Build the standalone HTML page from the command table and feature list.
pub fn html(commands: &[(&str, &str)]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };

    let feature_cards: String = FEATURES
        .iter()
        .map(|(title, body)| {
            format!(
                r#"<article class="group rounded-2xl border border-white/10 bg-white/[0.03] p-5 transition hover:-translate-y-1 hover:border-emerald-400/40 hover:bg-white/[0.06]">
  <h3 class="text-emerald-300 font-semibold tracking-tight">{}</h3>
  <p class="mt-2 text-sm leading-relaxed text-slate-400">{}</p>
</article>"#,
                esc(title),
                esc(body)
            )
        })
        .collect();

    let command_rows: String = commands
        .iter()
        .map(|(cmd, what)| {
            format!(
                r#"<tr class="border-b border-white/5 hover:bg-white/[0.04]">
  <td class="py-2.5 pr-6 font-mono text-emerald-300 whitespace-nowrap">{}</td>
  <td class="py-2.5 text-slate-400">{}</td>
</tr>"#,
                esc(cmd),
                esc(what)
            )
        })
        .collect();

    let key_rows: String = KEYS
        .iter()
        .map(|(k, v)| {
            format!(
                r#"<tr class="border-b border-white/5">
  <td class="py-2 pr-6 font-mono text-cyan-300 whitespace-nowrap">{}</td>
  <td class="py-2 text-slate-400">{}</td>
</tr>"#,
                esc(k),
                esc(v)
            )
        })
        .collect();

    format!(
        r##"<!doctype html>
<html lang="en" class="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>koda — feature guide</title>
<script src="https://cdn.tailwindcss.com"></script>
<style>
  @keyframes floaty {{ 0%,100%{{transform:translateY(0)}} 50%{{transform:translateY(-8px)}} }}
  @keyframes shimmer {{ 0%{{background-position:0% 50%}} 100%{{background-position:200% 50%}} }}
  .aurora {{ background:radial-gradient(60rem 30rem at 20% -10%, rgba(16,185,129,.18), transparent),
                       radial-gradient(50rem 30rem at 90% 0%, rgba(56,189,248,.15), transparent); }}
  .grad-text {{ background:linear-gradient(90deg,#34d399,#22d3ee,#a78bfa,#34d399); background-size:200% auto;
                -webkit-background-clip:text; background-clip:text; color:transparent; animation:shimmer 6s linear infinite; }}
  @media (prefers-reduced-motion: reduce) {{ *{{animation:none!important}} .group:hover{{transform:none}} }}
  html{{scroll-behavior:smooth}}
</style>
</head>
<body class="bg-slate-950 text-slate-200 antialiased selection:bg-emerald-400/30">
<div class="aurora min-h-screen">
  <!-- nav -->
  <header class="sticky top-0 z-10 backdrop-blur bg-slate-950/70 border-b border-white/10">
    <nav class="max-w-6xl mx-auto flex items-center justify-between px-6 py-4">
      <a href="#top" class="font-black text-xl tracking-tight grad-text">koda</a>
      <div class="hidden sm:flex gap-6 text-sm text-slate-400">
        <a href="#features" class="hover:text-emerald-300">Features</a>
        <a href="#commands" class="hover:text-emerald-300">Commands</a>
        <a href="#keys" class="hover:text-emerald-300">Keys</a>
        <a href="#start" class="hover:text-emerald-300">Get started</a>
      </div>
    </nav>
  </header>

  <!-- hero -->
  <section id="top" class="max-w-6xl mx-auto px-6 pt-20 pb-14 text-center">
    <div style="animation:floaty 7s ease-in-out infinite" class="inline-block text-6xl mb-6">⌘</div>
    <h1 class="text-5xl sm:text-6xl font-black tracking-tighter">The <span class="grad-text">koda</span> guide</h1>
    <p class="mt-5 text-lg text-slate-400 max-w-2xl mx-auto">Everything koda can do, how to use it, and every slash command — a terminal coding agent for your local LLM.</p>
    <div class="mt-8 inline-flex gap-3">
      <a href="#features" class="rounded-full bg-emerald-400 text-slate-950 font-semibold px-6 py-2.5 hover:bg-emerald-300 transition">Explore features</a>
      <a href="#commands" class="rounded-full border border-white/15 px-6 py-2.5 hover:border-emerald-400/50 transition">Command reference</a>
    </div>
  </section>

  <!-- features -->
  <section id="features" class="max-w-6xl mx-auto px-6 py-12">
    <h2 class="text-2xl font-bold tracking-tight mb-6">Features</h2>
    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
      {feature_cards}
    </div>
  </section>

  <!-- commands -->
  <section id="commands" class="max-w-6xl mx-auto px-6 py-12">
    <h2 class="text-2xl font-bold tracking-tight mb-2">Slash commands</h2>
    <p class="text-slate-500 text-sm mb-6">Type <span class="font-mono text-emerald-300">/</span> in koda to autocomplete these; <span class="font-mono">↑↓</span> to pick, <span class="font-mono">tab</span> to complete.</p>
    <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5 overflow-x-auto">
      <table class="w-full text-sm"><tbody>{command_rows}</tbody></table>
    </div>
  </section>

  <!-- keys -->
  <section id="keys" class="max-w-6xl mx-auto px-6 py-12">
    <h2 class="text-2xl font-bold tracking-tight mb-6">Keyboard shortcuts</h2>
    <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5 overflow-x-auto max-w-2xl">
      <table class="w-full text-sm"><tbody>{key_rows}</tbody></table>
    </div>
  </section>

  <!-- get started -->
  <section id="start" class="max-w-6xl mx-auto px-6 py-12 pb-24">
    <h2 class="text-2xl font-bold tracking-tight mb-6">Get started</h2>
    <div class="grid gap-4 sm:grid-cols-3 text-sm">
      <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5">
        <div class="text-emerald-300 font-semibold mb-2">1 · Point it at a model</div>
        <p class="text-slate-400">Run <span class="font-mono">/setup</span>, or start with <span class="font-mono">koda -u http://localhost:11434/v1 -m qwen2.5-coder:14b</span>.</p>
      </div>
      <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5">
        <div class="text-emerald-300 font-semibold mb-2">2 · Pick a mode & autonomy</div>
        <p class="text-slate-400"><span class="font-mono">ctrl+p</span> cycles plan/execute/vibe; <span class="font-mono">/auto</span> sets how much runs without asking.</p>
      </div>
      <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5">
        <div class="text-emerald-300 font-semibold mb-2">3 · Turn on the extras</div>
        <p class="text-slate-400">Open <span class="font-mono">/settings</span> for web search, the web UI, watch mode, reasoning effort and the system prompt.</p>
      </div>
    </div>
    <p class="mt-10 text-center text-slate-600 text-xs">koda · MIT · generated by /detailhelp</p>
  </section>
</div>
</body>
</html>"##,
        feature_cards = feature_cards,
        command_rows = command_rows,
        key_rows = key_rows,
    )
}

/// Write the guide to a temp file and open it in the default browser. Returns
/// the path written so the caller can tell the user where it is.
pub fn open(commands: &[(&str, &str)]) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join("koda-help.html");
    let mut f = std::fs::File::create(&path)?;
    f.write_all(html(commands).as_bytes())?;
    open_in_browser(&path);
    Ok(path)
}

/// Best-effort launch of the OS default browser. Failure is non-fatal — the
/// caller still reports the file path so the user can open it manually.
fn open_in_browser(path: &std::path::Path) {
    let p = path.to_string_lossy().to_string();
    let (prog, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("open", vec![p])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C".into(), "start".into(), "".into(), p])
    } else {
        ("xdg-open", vec![p])
    };
    let _ = std::process::Command::new(prog).args(args).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_is_wellformed_and_lists_commands() {
        let cmds = &[("/help", "keys and commands"), ("/watch", "watch triggers")];
        let out = html(cmds);
        assert!(out.starts_with("<!doctype html>"));
        assert!(out.contains("</html>"));
        assert!(out.contains("/help"));
        assert!(out.contains("/watch"));
        // Features and keys sections are present.
        assert!(out.contains("Features"));
        assert!(out.contains("Slash commands"));
        assert!(out.contains("Watch mode"));
    }

    #[test]
    fn html_escapes_angle_brackets() {
        let cmds = &[("/orc <task>", "split it")];
        let out = html(cmds);
        assert!(out.contains("/orc &lt;task&gt;"));
        assert!(!out.contains("/orc <task>"));
    }
}
