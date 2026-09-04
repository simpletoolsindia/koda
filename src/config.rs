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
            other => Err(format!(
                "unknown tool protocol `{other}` (auto|native|text)"
            )),
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

/// One named endpoint: where to talk, what to say you are, and which model.
///
/// A provider is exactly the four things you have to get right before koda can
/// say anything, kept together and given a name so switching between a local
/// server and a hosted one is one word rather than three edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Provider {
    /// What you call it. Shown in the status bar and used by `/provider`.
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Per-provider image support: "auto" | "on" | "off". Empty means fall back
    /// to the global setting, since most providers do not need their own.
    #[serde(default)]
    pub vision: String,
    /// History budget for this provider. 0 inherits the global setting, since
    /// context size is a property of the model behind the endpoint and differs
    /// wildly between a local 8k model and a hosted 200k one.
    #[serde(default)]
    pub context_tokens: usize,
    /// Skip TLS certificate verification for this endpoint only.
    ///
    /// For an internal server behind a corporate proxy that re-signs traffic
    /// with a private CA. Deliberately per-provider and one-way: a provider can
    /// turn it on for itself, and can never turn it off for anything else, so
    /// trusting one internal host cannot quietly weaken the connection to a
    /// public API in the same config.
    #[serde(default)]
    pub insecure_tls: bool,
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
    /// Cap on the raw byte size of a document (CSV/XLSX/DOCX/PDF) read through
    /// `read_file` before it is parsed. Guards against a huge binary being
    /// decompressed in memory. Extracted text is still capped by
    /// `max_file_bytes` on output.
    #[serde(default = "default_max_document_bytes")]
    pub max_document_bytes: usize,
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
    /// Capture the mouse so the wheel scrolls the transcript — what most people
    /// expect from a scrollable pane. On by default. Turn it off (`/mouse`, or
    /// here) to hand click-drag back to the terminal for native text selection
    /// and copy; you then scroll with the wheel-free keys (PageUp/PageDown).
    pub mouse_capture: bool,

    /// Which provider is in use, by name. Empty (or a name that is not in the
    /// list) means the top-level fields.
    #[serde(default)]
    pub active_provider: String,

    /// Accept TLS certificates koda cannot verify.
    ///
    /// Off by default and it should stay off. It exists for one situation: an
    /// internal endpoint behind a proxy that re-signs TLS with a private CA
    /// your machine does not trust, where the alternative is not using koda at
    /// all. It disables the check that the server is who it claims to be, so
    /// anything able to sit in the path can read and alter the traffic --
    /// including your API key and your source. Prefer installing the CA
    /// certificate; use this when you cannot.
    #[serde(default)]
    pub insecure_tls: bool,

    /// Let the agent open pages in a real browser (Playwright).
    ///
    /// Off by default: it needs Node and Playwright installed, launches a real
    /// Chromium, and is far slower than `web_fetch`. Worth it when the page only
    /// exists after its JavaScript has run, which is most of the modern web.
    #[serde(default)]
    pub browser: bool,
    /// Where Playwright's `node_modules` lives, when koda cannot find it.
    ///
    /// Empty means look in the usual places: the project, the global npm root,
    /// then the npx cache. Set it when Playwright is somewhere else.
    #[serde(default)]
    pub browser_path: String,
    /// Which browser `browse` should drive.
    ///
    /// A Playwright channel name (`chrome`, `msedge`, `chrome-beta`) uses the
    /// copy already installed on the machine; an absolute path drives any other
    /// Chromium build, which is how Brave or Arc get used. Empty falls back to
    /// the Chromium Playwright downloads for itself.
    ///
    /// Defaults to `chrome`, because a browser you already have is the one you
    /// meant. If it is not there, koda quietly uses the bundled Chromium rather
    /// than failing -- the point is to read the page.
    #[serde(default = "default_browser_channel")]
    pub browser_channel: String,
    /// Run the browser without a window. On by default, because that is what
    /// you want when an agent is doing the browsing. Turn it off to watch the
    /// page as it works, or to sign in somewhere by hand first -- headed runs
    /// share the same profile, so a session survives into later ones.
    #[serde(default = "yes")]
    pub browser_headless: bool,

    /// Whether the model accepts images: "auto" | "on" | "off".
    ///
    /// "auto" guesses from the model name, which is all you can do for a plain
    /// endpoint — and is exactly wrong behind a router, where the model is an
    /// alias like `auto` or `best-coding` that no name heuristic can classify
    /// and that `/v1/models` may not even list. Set it explicitly there.
    pub vision: String,

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

    /// Self-improvement (Phase 1): watch how you work — the edits you make to
    /// koda's output, command outcomes — and distil deterministic, inspectable
    /// rules into <project>/.koda/learning/rules.md. Candidates await `/learn`
    /// before they enter the prompt. Off by default while it settles. No model,
    /// no network.
    #[serde(default)]
    pub learning: bool,

    /// Scan the project into a symbol graph on open, so the model can ask where
    /// something lives instead of grepping for it.
    pub codegraph: bool,

    /// Allow the `web_search` tool. Off unless a SearXNG URL is set.
    pub web_search: bool,
    /// Which backend web search uses: "duckduckgo" (no setup) or "searxng"
    /// (uses `searx_url`). Change live in the settings page.
    #[serde(default = "default_backend")]
    pub search_backend: String,
    /// Base URL of a SearXNG instance with the JSON format enabled.
    pub searx_url: String,
    /// Results per search.
    pub search_results: usize,

    /// Allow the `web_fetch` tool: GET a URL and read it as text. Off by
    /// default (a model-supplied URL becomes a request from your machine).
    #[serde(default)]
    pub web_fetch: bool,

    /// When an image is attached but the model isn't vision-capable, extract its
    /// text with the `tesseract` CLI and send that instead. Off by default;
    /// requires tesseract to be installed. Toggle in `/settings`.
    #[serde(default)]
    pub ocr: bool,

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

    /// User-defined tools, each backed by a shell command. Declared in config as
    /// `[[tools]]` tables; the agent can call them like any built-in.
    #[serde(default, rename = "tools")]
    pub custom_tools: Vec<CustomTool>,

    /// Reasoning effort hint for thinking models: "off" | "low" | "medium" |
    /// "high". Sent as `reasoning_effort` in the request (servers that ignore it
    /// are unaffected). "off" omits the field. Change live with `/reason`.
    #[serde(default = "default_reasoning")]
    pub reasoning_effort: String,

    /// Developer debug mode. When on, koda dumps each raw LLM request body and
    /// the raw streamed response to <state>/koda/debug/rr-session-N.{json,res.log}
    /// for inspection, and surfaces a `/debug` report. `KODA_DEBUG=1` also
    /// enables it. Off by default. Toggle live with `/debug`.
    #[serde(default)]
    pub debug: bool,

    /// Override the built-in main system prompt entirely. Empty = use the
    /// built-in. Edit it in the settings page (`/settings`). `instructions` is
    /// still appended either way.
    #[serde(default)]
    pub system_prompt: String,

    /// Per-tool system-prompt overrides, keyed by tool name. Empty/absent means
    /// use the built-in guidance for that tool. Edit in the settings page.
    #[serde(default)]
    pub tool_prompts: std::collections::BTreeMap<String, String>,

    /// Serve a local web UI (React) for live logs and debugging. Off by
    /// default. Enable in `/settings`. Binds to 127.0.0.1 only.
    #[serde(default)]
    pub web_ui: bool,
    /// Port for the web UI server.
    #[serde(default = "default_web_ui_port")]
    pub web_ui_port: u16,
    /// Detail level surfaced to the web UI log stream: "simple" | "medium" |
    /// "high". simple = info+, medium = adds tool/timing debug, high = everything.
    #[serde(default = "default_detail")]
    pub ui_detail: String,

    /// Watch mode (aider-style): scan the workspace for `AI!`/`AI?` comment
    /// triggers and act on them automatically. `AI!` implements the request in
    /// that file; `AI?` answers the question. Off by default; toggle with
    /// `/watch`. Only fires when the agent is idle.
    #[serde(default)]
    pub watch: bool,
    /// How often (ms) watch mode rescans for triggers.
    #[serde(default = "default_watch_ms")]
    pub watch_interval_ms: u64,
    /// Named endpoints, added through the setup page or by hand.
    ///
    /// Declared last on purpose: serde writes fields in declaration order and
    /// TOML binds a bare key to whatever table precedes it, so a scalar emitted
    /// after `[[provider]]` is read back as a field *of that provider*. With
    /// this in the middle, koda wrote a config it then misread -- `vision` and
    /// everything after it silently became part of the last provider.
    #[serde(default, rename = "provider")]
    pub providers: Vec<Provider>,
}

fn default_watch_ms() -> u64 {
    1500
}

fn default_web_ui_port() -> u16 {
    7717
}

fn default_detail() -> String {
    "medium".into()
}

fn default_reasoning() -> String {
    "off".into()
}

fn default_backend() -> String {
    "duckduckgo".into()
}

/// ~8 MiB: room for a real spreadsheet or a text PDF, but not a scanned tome.
fn default_max_document_bytes() -> usize {
    8 * 1024 * 1024
}

/// A user-defined tool: a named, described shell command the agent may call.
/// `{arg}` placeholders in `command` are filled from the call's arguments (and
/// shell-quoted), so a user can teach koda a project-specific action without
/// touching Rust. Runs through the same approval + shell path as run_command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomTool {
    /// Tool name the model calls (letters, digits, underscores).
    pub name: String,
    /// One line telling the model what it does and when to use it.
    pub description: String,
    /// Shell command with `{arg_name}` placeholders.
    pub command: String,
    /// Parameter names the command expects; each becomes a string argument.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether calling it needs approval (defaults to true — it runs a command).
    #[serde(default = "default_true")]
    pub mutating: bool,
}

fn default_true() -> bool {
    true
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
            // Room for a real conversation on a modern model. Small local
            // models still trim to whatever they actually accept; the budget
            // only decides when koda starts dropping history.
            context_tokens: 110_000,
            tool_protocol: ToolProtocol::Auto,
            max_steps: 24,
            auto_approve: false,
            auto_tier: AutoTier::Ask,
            sandbox: true,
            shell: default_shell(),
            command_timeout_ms: 120_000,
            max_file_bytes: 256 * 1024,
            max_tool_output_bytes: 24 * 1024,
            max_document_bytes: default_max_document_bytes(),
            instructions: String::new(),
            sync_output: true,
            motion: true,
            reveal: true,
            mouse_capture: true,
            vision: "auto".into(),
            providers: Vec::new(),
            active_provider: String::new(),
            insecure_tls: false,
            browser: false,
            browser_path: String::new(),
            browser_headless: true,
            browser_channel: default_browser_channel(),
            theme: "auto".into(),
            icons: "auto".into(),
            sessions: true,
            memory: true,
            learning: false,
            codegraph: true,
            web_search: false,
            search_backend: default_backend(),
            searx_url: String::new(),
            search_results: 6,
            web_fetch: false,
            ocr: false,
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
            custom_tools: Vec::new(),
            reasoning_effort: default_reasoning(),
            debug: false,
            system_prompt: String::new(),
            tool_prompts: std::collections::BTreeMap::new(),
            web_ui: false,
            web_ui_port: default_web_ui_port(),
            ui_detail: default_detail(),
            watch: false,
            watch_interval_ms: default_watch_ms(),
        }
    }
}

fn default_browser_channel() -> String {
    "chrome".into()
}

/// serde default for a flag that should be on unless someone says otherwise.
/// Needed because `#[serde(default)]` on a bool gives false, and a config
/// written before this field existed would then open a visible browser window.
fn yes() -> bool {
    true
}

/// The default command shell for the current OS: `cmd` on Windows, the user's
/// `$SHELL` or `/bin/sh` elsewhere.
pub fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".into())
    } else {
        "/bin/sh".into()
    }
}

/// The flag that tells a shell "run this command string": `/C` for a Windows
/// `cmd`/`powershell`, `-c` for POSIX shells.
pub fn shell_flag(shell: &str) -> &'static str {
    // Split on both separators so a Windows path parses correctly even when the
    // binary runs on unix (and vice versa).
    let base = shell
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(shell)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    match base.as_str() {
        "cmd" | "powershell" | "pwsh" => "/C",
        _ => "-c",
    }
}

/// Where the config lives. Honours `$XDG_CONFIG_HOME` first (Linux/macOS
/// convention); on unix falls back to `~/.config/koda`; on Windows uses the
/// platform config dir (`%APPDATA%\koda`) via the `dirs` crate.
pub fn config_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("koda");
        }
    }
    if cfg!(windows) {
        if let Some(d) = dirs::config_dir() {
            return d.join("koda");
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
            let t: toml::Table =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
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
        let cfg: Config = toml::Value::Table(table)
            .try_into()
            .context("invalid koda config")?;
        // Fold the active provider in here, once, so nothing downstream has to
        // remember to ask -- a caller reading cfg.model directly while another
        // used a getter is how two halves of the app end up on different
        // servers. The list and the choice are carried over intact, so saving
        // this config back does not flatten the providers away.
        let providers = cfg.providers.clone();
        let active = cfg.active_provider.clone();
        let mut cfg = cfg.resolved();
        cfg.providers = providers;
        cfg.active_provider = active;
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

    /// The provider in use, when one is named and exists.
    pub fn active(&self) -> Option<&Provider> {
        if self.active_provider.trim().is_empty() {
            return None;
        }
        self.providers
            .iter()
            .find(|p| p.name == self.active_provider)
    }

    /// Fold the active provider's settings over the top-level ones.
    ///
    /// Returned as a whole config rather than field-by-field getters so every
    /// caller sees the same resolved view; a caller that read `cfg.model`
    /// directly while another used a getter is how two halves of the app end up
    /// talking to different servers.
    pub fn resolved(&self) -> Config {
        let Some(p) = self.active() else {
            return self.clone();
        };
        let mut out = self.clone();
        if !p.base_url.trim().is_empty() {
            out.base_url = p.base_url.clone();
        }
        if !p.api_key.trim().is_empty() {
            out.api_key = p.api_key.clone();
        }
        if !p.model.trim().is_empty() {
            out.model = p.model.clone();
        }
        if !p.vision.trim().is_empty() {
            out.vision = p.vision.clone();
        }
        if p.context_tokens > 0 {
            out.context_tokens = p.context_tokens;
        }
        // One-way on purpose: a provider may relax TLS for itself, never
        // tighten it away from a global setting and never relax it for others.
        if p.insecure_tls {
            out.insecure_tls = true;
        }
        out
    }

    /// Add or replace a named provider, and make it the active one.
    pub fn upsert_provider(&mut self, p: Provider) {
        match self.providers.iter_mut().find(|x| x.name == p.name) {
            Some(slot) => *slot = p.clone(),
            None => self.providers.push(p.clone()),
        }
        self.active_provider = p.name;
    }

    /// What to show for the endpoint: the provider's name when there is one,
    /// since "omniroute" tells you more at a glance than "localhost:20128".
    pub fn provider_label(&self) -> String {
        match self.active() {
            Some(p) => p.name.clone(),
            None => self
                .endpoint()
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_end_matches("/v1")
                .to_string(),
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

# Max raw size of a CSV/XLSX/DOCX/PDF read through read_file, before parsing.
# XLSX/DOCX/PDF need `koda` built with --features docs (or pdf) to be parsed.
max_document_bytes = 8388608

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

# Self-improvement: watch how you work (edits to koda's output, command
# outcomes) and distil inspectable rules into .koda/learning/rules.md. Review
# and accept candidates with /learn. Off by default. Fully local, no model.
learning = false

# Scan the project on open into a symbol graph (definitions, references,
# imports) and expose it to the model via the `codegraph` tool.
codegraph = true

# Web search through your own SearXNG instance. That instance needs `json` in
# `search.formats` in its settings.yml. Toggle live with /websearch.
# Web search. On, koda uses your SearXNG instance if searx_url is set (private,
# self-hosted); otherwise it falls back to DuckDuckGo, which needs no setup.
web_search = false
searx_url = ""          # optional, e.g. "http://localhost:8888"
# Backend: "duckduckgo" (keyless, no setup) or "searxng" (uses searx_url).
# Pick it interactively in /settings: enable web search, choose the backend,
# then esc to confirm.
search_backend = "duckduckgo"
search_results = 6

# Let the agent GET a URL and read it as text (the web_fetch tool). Off by
# default — a model-supplied URL becomes a request from your machine. Toggle in
# /settings.
web_fetch = false

# OCR fallback for images: when you attach an image (@shot.png) but the model
# isn't vision-capable, extract the image's text with the `tesseract` CLI and
# send that instead. Off by default; needs tesseract installed
# (brew install tesseract / apt install tesseract-ocr). Toggle in /settings.
ocr = false

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

# Reasoning effort for thinking models: off | low | medium | high.
# Sent as `reasoning_effort`; servers that don't support it ignore it.
# "off" omits the field. Change live with /reason.
reasoning_effort = "off"

# Developer debug mode. On, koda writes each raw request body and the raw
# streamed response to <state>/koda/debug/rr-session-N.{json,res.log} and adds a
# /debug report. KODA_DEBUG=1 also turns it on. Toggle live with /debug.
debug = false

# Override the built-in main system prompt entirely (empty = built-in). The
# `instructions` value above is still appended. Easiest to edit via /settings.
system_prompt = ""

# Per-tool prompt overrides, e.g.:
# [tool_prompts]
# run_command = "Prefer ripgrep over grep. Never run destructive commands."

# Local web UI (React) for live logs and debugging. On, koda serves it on
# 127.0.0.1:<web_ui_port>. Enable in /settings. ui_detail sets how much the log
# stream shows: simple (info+), medium, or high (everything).
web_ui = false
web_ui_port = 7717
ui_detail = "medium"

# Watch mode (aider-style). On, koda scans the workspace when idle for comment
# lines ending in `AI!` (implement it here) or `AI?` (answer the question) and
# acts on them, removing the trigger. Toggle with /watch.
watch = false
watch_interval_ms = 1500

# Your own tools. Each [[tools]] entry adds a command the agent can call like a
# built-in; {arg} placeholders are filled from the call and shell-quoted. Runs
# through the normal approval + shell path.
#
# [[tools]]
# name = "typecheck"
# description = "Type-check the project and report errors."
# command = "npm run -s typecheck"
#
# [[tools]]
# name = "grep_todos"
# description = "Find TODO comments matching a term."
# command = "rg -n 'TODO.*{term}' ."
# args = ["term"]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// TLS trust is per-endpoint on purpose: relaxing it for an internal host
    /// must never relax it for a public API sitting in the same config.
    #[test]
    fn a_provider_may_relax_tls_for_itself_only() {
        let mut cfg = Config::default();
        assert!(!cfg.insecure_tls, "off by default, and it stays off");
        cfg.upsert_provider(Provider {
            name: "internal".into(),
            base_url: "https://internal.corp/v1".into(),
            insecure_tls: true,
            ..Provider::default()
        });
        assert!(cfg.resolved().insecure_tls, "the internal host may opt in");

        // A provider that says nothing cannot switch it back off globally.
        let mut strict = Config {
            insecure_tls: true,
            ..Config::default()
        };
        strict.upsert_provider(Provider {
            name: "public".into(),
            insecure_tls: false,
            ..Provider::default()
        });
        assert!(
            strict.resolved().insecure_tls,
            "a provider cannot tighten away from the global setting either -- \
             the flag is one-way, so the direction is always explicit"
        );
    }

    /// Context is a property of the model behind the endpoint, so a provider
    /// carries its own; 0 means "whatever the global budget is".
    #[test]
    fn a_provider_may_set_its_own_context_budget() {
        assert_eq!(
            Config::default().context_tokens,
            110_000,
            "a modern default"
        );
        let mut cfg = Config::default();
        cfg.upsert_provider(Provider {
            name: "tiny".into(),
            context_tokens: 8_000,
            ..Provider::default()
        });
        assert_eq!(cfg.resolved().context_tokens, 8_000);

        cfg.upsert_provider(Provider {
            name: "inherit".into(),
            context_tokens: 0,
            ..Provider::default()
        });
        assert_eq!(cfg.resolved().context_tokens, 110_000, "0 inherits");
    }

    /// TOML puts bare keys under whatever table precedes them, so a scalar
    /// written after `[[provider]]` is read back as a field *of that provider*.
    /// If koda ever emits its config in that order it writes a file it cannot
    /// read -- the endpoint and model would silently revert on next start.
    #[test]
    fn saved_config_keeps_scalars_above_the_provider_tables() {
        let mut cfg = Config {
            base_url: "http://top-level/v1".into(),
            ..Config::default()
        };
        cfg.upsert_provider(Provider {
            name: "omni".into(),
            base_url: "http://provider/v1".into(),
            model: "auto".into(),
            ..Provider::default()
        });
        let text = toml::to_string_pretty(&cfg).unwrap();

        let first_table = text.find("[[provider]]").expect("providers were written");
        let head = &text[..first_table];
        // Every top-level scalar must be in the part above the first table
        // array, where TOML will still read it as top-level.
        for key in ["base_url", "model", "active_provider", "vision", "theme"] {
            assert!(
                head.lines().any(|l| l.starts_with(&format!("{key} ="))),
                "{key} is written after [[provider]] and would be read back as \
                 part of that provider:\n{text}"
            );
        }
        // And the whole thing survives the round trip.
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.active_provider, "omni");
        assert_eq!(back.base_url, "http://top-level/v1");
        assert_eq!(back.resolved().base_url, "http://provider/v1");
    }

    /// A config written before providers existed must keep working exactly as
    /// it did: the top-level fields stay authoritative until something is
    /// actually in the list.
    #[test]
    fn a_config_without_providers_is_unchanged() {
        let cfg = Config {
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen".into(),
            api_key: "local".into(),
            ..Config::default()
        };
        let r = cfg.resolved();
        assert_eq!(r.base_url, "http://localhost:11434/v1");
        assert_eq!(r.model, "qwen");
        assert!(cfg.active().is_none());
        assert_eq!(cfg.provider_label(), "localhost:11434");
    }

    /// The active provider wins field by field, and an empty field falls
    /// through -- so a provider can name just a model and inherit the rest.
    #[test]
    fn the_active_provider_overrides_field_by_field() {
        let mut cfg = Config {
            base_url: "http://fallback/v1".into(),
            api_key: "fallback-key".into(),
            model: "fallback-model".into(),
            ..Config::default()
        };
        cfg.upsert_provider(Provider {
            name: "omni".into(),
            base_url: "http://localhost:20128/v1".into(),
            api_key: String::new(), // inherit
            model: "auto".into(),
            vision: "on".into(),
            context_tokens: 0,
            insecure_tls: false,
        });
        let r = cfg.resolved();
        assert_eq!(r.base_url, "http://localhost:20128/v1");
        assert_eq!(r.model, "auto");
        assert_eq!(r.api_key, "fallback-key", "an empty field inherits");
        assert_eq!(r.vision, "on");
        assert_eq!(cfg.provider_label(), "omni", "the bar shows the name");
    }

    /// Adding a provider twice replaces it rather than growing a duplicate.
    #[test]
    fn upsert_replaces_and_activates() {
        let mut cfg = Config::default();
        cfg.upsert_provider(Provider {
            name: "a".into(),
            model: "m1".into(),
            ..Provider::default()
        });
        cfg.upsert_provider(Provider {
            name: "b".into(),
            model: "m2".into(),
            ..Provider::default()
        });
        cfg.upsert_provider(Provider {
            name: "a".into(),
            model: "m3".into(),
            ..Provider::default()
        });
        assert_eq!(cfg.providers.len(), 2, "two names, not three entries");
        assert_eq!(cfg.active_provider, "a", "the last one added is active");
        assert_eq!(cfg.resolved().model, "m3", "and it was replaced, not kept");
    }

    /// Round-tripping through TOML has to preserve the list and the choice.
    #[test]
    fn providers_survive_a_toml_round_trip() {
        let mut cfg = Config::default();
        cfg.upsert_provider(Provider {
            name: "omni".into(),
            base_url: "http://localhost:20128/v1".into(),
            api_key: "sk-x".into(),
            model: "auto".into(),
            vision: String::new(),
            context_tokens: 0,
            insecure_tls: false,
        });
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            text.contains("[[provider]]"),
            "written as a table array:\n{text}"
        );
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.providers, cfg.providers);
        assert_eq!(back.active_provider, "omni");
        assert_eq!(back.resolved().base_url, "http://localhost:20128/v1");
    }

    #[test]
    fn shell_flag_matches_the_shell() {
        assert_eq!(shell_flag("/bin/sh"), "-c");
        assert_eq!(shell_flag("/usr/bin/zsh"), "-c");
        assert_eq!(shell_flag("bash"), "-c");
        assert_eq!(shell_flag("cmd"), "/C");
        assert_eq!(shell_flag("C:\\Windows\\System32\\cmd.exe"), "/C");
        assert_eq!(shell_flag("powershell"), "/C");
        assert_eq!(shell_flag("pwsh.exe"), "/C");
    }

    #[test]
    fn default_shell_is_nonempty() {
        assert!(!default_shell().is_empty());
    }
}
