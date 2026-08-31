//! Configuration: layered defaults -> user TOML -> project TOML -> env -> CLI flags.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How much latitude the agent has this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read and think only. Nothing on disk changes; the agent produces a plan.
    Plan,
    /// Normal operation: edits and commands, gated by approval.
    #[default]
    Execute,
    /// Execute, but the agent writes an explicit spec first and checks its own
    /// work (and its subagents' work) against it before finishing.
    Vibe,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Plan => "PLAN",
            Mode::Execute => "EXEC",
            Mode::Vibe => "VIBE",
        }
    }
    /// Plan mode cannot touch disk.
    pub fn read_only(&self) -> bool {
        matches!(self, Mode::Plan)
    }
    pub fn next(&self) -> Mode {
        match self {
            Mode::Plan => Mode::Execute,
            Mode::Execute => Mode::Vibe,
            Mode::Vibe => Mode::Plan,
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plan" | "p" => Ok(Mode::Plan),
            "execute" | "exec" | "e" => Ok(Mode::Execute),
            "vibe" | "v" => Ok(Mode::Vibe),
            other => Err(format!("unknown mode `{other}` (plan|execute|vibe)")),
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mode::Plan => "plan",
            Mode::Execute => "execute",
            Mode::Vibe => "vibe",
        })
    }
}

/// How much the agent may do without asking. Modelled on oh-my-pi's approval
/// tiers: read is always free; the tier decides whether writes and commands
/// need a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AutoTier {
    /// Ask before every mutating action (writes and commands). The default.
    #[default]
    Ask,
    /// Auto-approve file writes, but still ask before running commands.
    Write,
    /// Full-auto: approve everything, no prompts. Autonomous operation.
    Full,
}

impl AutoTier {
    pub fn label(&self) -> &'static str {
        match self {
            AutoTier::Ask => "ASK",
            AutoTier::Write => "AUTO-WRITE",
            AutoTier::Full => "FULL-AUTO",
        }
    }
    /// Cycle for a keybinding / `/auto`: ask → write → full → ask.
    pub fn next(&self) -> AutoTier {
        match self {
            AutoTier::Ask => AutoTier::Write,
            AutoTier::Write => AutoTier::Full,
            AutoTier::Full => AutoTier::Ask,
        }
    }
    /// Whether a mutating tool of this name may run without a prompt.
    pub fn auto_allows(&self, tool: &str) -> bool {
        match self {
            AutoTier::Ask => false,
            // Writes are pre-approved; commands (and anything exec-tier) still ask.
            AutoTier::Write => matches!(tool, "write_file" | "edit_file"),
            AutoTier::Full => true,
        }
    }
}

impl std::str::FromStr for AutoTier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" | "off" | "false" => Ok(AutoTier::Ask),
            "write" | "auto-write" | "writes" => Ok(AutoTier::Write),
            "full" | "full-auto" | "yolo" | "on" | "true" => Ok(AutoTier::Full),
            other => Err(format!("unknown auto tier `{other}` (ask|write|full)")),
        }
    }
}

impl std::fmt::Display for AutoTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AutoTier::Ask => "ask",
            AutoTier::Write => "write",
            AutoTier::Full => "full",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolProtocol {
    /// Advertise native OpenAI `tools`, but also parse the text protocol as a fallback.
    Auto,
    /// Native `tools` / `tool_calls` only.
    Native,
    /// No `tools` field; instruct the model to emit `<tool_call>` blocks.
    Text,
}

impl std::str::FromStr for ToolProtocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "native" => Ok(Self::Native),
            "text" => Ok(Self::Text),
            other => Err(format!("unknown tool protocol `{other}` (auto|native|text)")),
        }
    }
}

impl std::fmt::Display for ToolProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Text => "text",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// OpenAI-compatible base URL, e.g. `http://localhost:11434/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub temperature: f64,
    pub top_p: f64,
    /// 0 = let the server decide.
    pub max_tokens: u32,
    /// Soft budget used to trim history before each request.
    pub context_tokens: usize,
    pub tool_protocol: ToolProtocol,
    /// Max model<->tool round trips per user turn.
    pub max_steps: usize,
    /// Skip approval prompts for mutating tools.
    pub auto_approve: bool,
    /// Tiered autonomy: ask (default), write (auto-approve writes), or full
    /// (approve everything). `auto_approve = true` implies `full`.
    pub auto_tier: AutoTier,
    /// Confine file tools to the workspace root.
    pub sandbox: bool,
    pub shell: String,
    pub command_timeout_ms: u64,
    pub max_file_bytes: usize,
    pub max_tool_output_bytes: usize,
    /// Appended verbatim to the system prompt.
    pub instructions: String,

    /// Wrap each frame in DEC 2026 synchronized-update markers so the terminal
    /// presents it atomically. Terminals that do not support it ignore the
    /// sequence; tmux does not forward it.
    pub sync_output: bool,
    /// Whether to animate. Environment overrides such as NO_MOTION and a
    /// non-tty stdout still win over this.
    pub motion: bool,
    /// Whether streaming assistant text is *revealed* progressively (typed in)
    /// rather than appearing all at once. Independent of `motion`: some people
    /// want spinners and gauges but find the text reveal distracting. Requires
    /// `motion` to have any effect.
    pub reveal: bool,
    /// Capture the mouse for wheel-scrolling. On (default) means the wheel
    /// scrolls the transcript, but the terminal's own click-drag text selection
    /// is suppressed. Turn it off (`/mouse`) to select and copy text with the
    /// mouse the normal way; scroll then uses pgup/pgdn or the keyboard.
    pub mouse_capture: bool,

    /// Palette name, or "auto"/"" for the terminal's own 16 colours.
    /// `NO_COLOR` and `TERM=dumb` force monochrome regardless.
    pub theme: String,
    /// "auto" | "unicode" | "ascii" — glyph set for icons and box drawing.
    pub icons: String,

    /// Record each session to <project>/.koda/sessions so it can be resumed.
    pub sessions: bool,

    /// Carry notes and command outcomes between sessions in
    /// <project>/.koda/memory.md.
    pub memory: bool,

    /// Scan the project into a symbol graph on open, so the model can ask where
    /// something lives instead of grepping for it.
    pub codegraph: bool,

    /// Allow the `web_search` tool. Off unless a SearXNG URL is set.
    pub web_search: bool,
    /// Base URL of a SearXNG instance with the JSON format enabled.
    pub searx_url: String,
    /// Results per search.
    pub search_results: usize,

    /// Attempts per request before giving up (1 = no retry).
    pub max_retries: u32,
    /// "debug" | "info" | "warn" | "error"
    pub log_level: String,
    /// Mirror the event log to ~/.local/state/koda/koda.log.
    pub log_to_file: bool,
    /// Show detailed (debug-level) telemetry in the `/logs` view. Off keeps the
    /// view concise (info and above); on surfaces every request, tool arg, and
    /// timing. Toggle live in the settings page.
    pub log_detail: bool,

    /// Starting mode: plan, execute or vibe.
    pub mode: Mode,
    /// Compact automatically once the context passes this fraction of budget.
    /// 0 disables it.
    pub auto_compact_at: f64,

    /// Allow the agent to delegate read-only investigations to subagents.
    pub subagents: bool,
    /// Step budget for one subagent run.
    pub subagent_max_steps: usize,
    /// In vibe mode, how many times the parent may send a report back to the
    /// subagent for another pass. 0 disables review.
    pub subagent_review_rounds: u32,
    /// How deep delegation may nest. 1 = subagents cannot delegate further.
    pub max_subagent_depth: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".into(),
            api_key: "local".into(),
            model: String::new(),
            temperature: 0.2,
            top_p: 0.95,
            max_tokens: 0,
            context_tokens: 16_000,
            tool_protocol: ToolProtocol::Auto,
            max_steps: 24,
            auto_approve: false,
            auto_tier: AutoTier::Ask,
            sandbox: true,
            shell: "/bin/sh".into(),
            command_timeout_ms: 120_000,
            max_file_bytes: 256 * 1024,
            max_tool_output_bytes: 24 * 1024,
            instructions: String::new(),
            sync_output: true,
            motion: true,
            reveal: true,
            mouse_capture: true,
            theme: "auto".into(),
            icons: "auto".into(),
            sessions: true,
            memory: true,
            codegraph: true,
            web_search: false,
            searx_url: String::new(),
            search_results: 6,
            max_retries: 3,
            log_level: "info".into(),
            log_to_file: true,
            log_detail: false,
            mode: Mode::Execute,
            auto_compact_at: 0.85,
            subagents: true,
            subagent_max_steps: 12,
            subagent_review_rounds: 1,
            max_subagent_depth: 1,
        }
    }
}

/// `$XDG_CONFIG_HOME/koda`, else `~/.config/koda`. macOS CLI tools conventionally
/// use `~/.config` rather than `~/Library/Application Support`.
pub fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("koda");
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".config").join("koda"))
        .unwrap_or_else(|| PathBuf::from(".koda"))
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Deep-merge `src` into `dst` (tables merge, scalars overwrite).
fn merge(dst: &mut toml::Table, src: toml::Table) {
    for (k, v) in src {
        match (dst.get_mut(&k), v) {
            (Some(toml::Value::Table(d)), toml::Value::Table(s)) => merge(d, s),
            (_, v) => {
                dst.insert(k, v);
            }
        }
    }
}

fn read_table(path: &Path) -> Result<Option<toml::Table>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let t: toml::Table = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            Ok(Some(t))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Walk up from `root` looking for a project-level config.
fn project_table(root: &Path) -> Result<Option<toml::Table>> {
    let mut dir = Some(root);
    while let Some(d) = dir {
        for name in ["koda.toml", ".koda.toml"] {
            if let Some(t) = read_table(&d.join(name))? {
                return Ok(Some(t));
            }
        }
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    Ok(None)
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let mut table = toml::Table::new();
        if let Some(t) = read_table(&config_path())? {
            merge(&mut table, t);
        }
        if let Some(t) = project_table(root)? {
            merge(&mut table, t);
        }
        let mut cfg: Config =
            toml::Value::Table(table).try_into().context("invalid koda config")?;
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        let env = |a: &str, b: &str| -> Option<String> {
            std::env::var(a).ok().or_else(|| std::env::var(b).ok())
        };
        if let Some(v) = env("KODA_BASE_URL", "OPENAI_BASE_URL") {
            self.base_url = v;
        }
        if let Some(v) = env("KODA_API_KEY", "OPENAI_API_KEY") {
            self.api_key = v;
        }
        if let Some(v) = env("KODA_MODEL", "OPENAI_MODEL") {
            self.model = v;
        }
    }

    /// Normalized endpoint without a trailing slash.
    pub fn endpoint(&self) -> String {
        self.base_url.trim_end_matches('/').to_string()
    }

    pub fn write_default_file() -> Result<PathBuf> {
        let path = config_path();
        std::fs::create_dir_all(config_dir())?;
        if !path.exists() {
            std::fs::write(&path, DEFAULT_CONFIG_TEMPLATE)?;
        }
        Ok(path)
    }
}

/// Write the effective config to the user config file, preserving nothing else.
/// Called by the setup screen, so the values a user typed survive a restart.
pub fn save(cfg: &Config) -> Result<PathBuf> {
    let path = config_path();
    std::fs::create_dir_all(config_dir())
        .with_context(|| format!("creating {}", config_dir().display()))?;
    let body = toml::to_string_pretty(cfg).context("serializing config")?;
    let text = format!(
        "# koda configuration — written by the provider setup screen.\n\
         # Every key is documented in the README.\n\n{body}"
    );
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# koda configuration (~/.config/koda/config.toml)
# A project-local `koda.toml` overrides these values.

# Ollama:     http://localhost:11434/v1
# LM Studio:  http://localhost:1234/v1
# llama.cpp:  http://localhost:8080/v1
base_url = "http://localhost:11434/v1"
api_key = "local"

# Leave empty to auto-pick the first model the server reports.
model = ""

temperature = 0.2
top_p = 0.95
max_tokens = 0          # 0 = server default
context_tokens = 16000  # soft budget; history is trimmed to fit

# auto   = native tool calls + text-block fallback (best for local models)
# native = OpenAI tool_calls only
# text   = <tool_call> blocks only (for models without tool support)
tool_protocol = "auto"

max_steps = 24
auto_approve = false    # true = never ask before writes/commands (same as auto_tier=full)
# Tiered autonomy, cycled live with /auto: ask (prompt for every write/command),
# write (auto-approve writes, still ask before commands), full (approve everything).
auto_tier = "ask"
sandbox = true          # confine file tools to the workspace root

shell = "/bin/sh"
command_timeout_ms = 120000
max_file_bytes = 262144
max_tool_output_bytes = 24576

# Extra project rules appended to the system prompt.
instructions = ""

# Present each frame atomically (DEC 2026). Stops tearing mid-render. Harmless
# on terminals that do not support it; set false if yours misbehaves.
sync_output = true

# Animate the UI (spinners, gauges, text reveal). NO_MOTION/REDUCED_MOTION and
# a non-tty stdout still disable it. Toggle live with /motion.
motion = true
# Reveal streaming replies progressively (typed in) rather than all at once.
# Needs motion = true. Toggle live with /reveal.
reveal = true

# Palette: auto (your terminal's 16 colours), catppuccin-mocha, tokyo-night,
# gruvbox-dark, nord, dracula, rose-pine, solarized-light, mono.
# NO_COLOR=1 forces mono. Switch live with /theme.
theme = "auto"

# Glyphs: auto (unicode when your locale is UTF-8), unicode, ascii.
icons = "auto"

# Save each conversation to <project>/.koda/sessions as JSONL so /resume works.
sessions = true

# Remember facts and command outcomes between sessions, in
# <project>/.koda/memory.md. Plain markdown you can read, edit or delete.
memory = true

# Scan the project on open into a symbol graph (definitions, references,
# imports) and expose it to the model via the `codegraph` tool.
codegraph = true

# Web search through your own SearXNG instance. That instance needs `json` in
# `search.formats` in its settings.yml. Toggle live with /websearch.
web_search = false
searx_url = ""          # e.g. "http://localhost:8888"
search_results = 6

# Transient failures (connection reset, 429, 5xx, empty stream) are retried
# with backoff before the agent gives up.
max_retries = 3

# Event log, viewable with /logs in the TUI.
log_level = "info"
log_to_file = true

# Starting mode. plan = read and think only, no edits; execute = normal;
# vibe = write a spec first, then verify the work against it.
# Switch live with ctrl+p or /mode.
mode = "execute"

# Summarize the conversation automatically at this fraction of context_tokens.
# 0 turns it off.
auto_compact_at = 0.85

# Subagents: let the agent delegate read-only investigations to a child agent
# with its own context window, so wide searches don't fill the main context.
subagents = true
subagent_max_steps = 12

# In vibe mode the parent checks each subagent report against the files and can
# send it back for another pass this many times.
subagent_review_rounds = 1
max_subagent_depth = 1
"#;
