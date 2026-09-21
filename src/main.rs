//! koda — a small terminal coding agent for local, OpenAI-compatible LLMs.

mod agent;
mod anim;
mod config;
mod context;
mod curtain;
mod dap;
mod debug;
mod detailhelp;
mod editor;
mod engine;
mod fuzzy;
mod fx;
mod graph;
mod index;
mod learning;
mod llm;
mod log;
mod lsp;
mod mcp;
mod md;
mod memory;
mod panel;
mod prompt;
mod session;
mod settings;
mod setup;
mod skills;
mod theme;
mod tools;
mod trace;
mod tui;
mod view;
mod watch;
mod web;
mod webui;

use agent::{Agent, Approval, Command, Event};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::{Config, ToolProtocol};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

#[derive(Parser, Debug)]
#[command(
    name = "koda",
    version,
    about = "Terminal coding agent for local OpenAI-compatible LLMs",
    after_help = "Examples:\n  \
        koda                                  start the TUI in the current directory\n  \
        koda \"add tests for parse_args\"       start with a first message\n  \
        koda -p \"what does src/main.rs do?\"   headless, print to stdout\n  \
        koda models                           list models on the endpoint\n  \
        koda config --init                    write a default config file"
)]
struct Cli {
    /// First message. Without -p this seeds the TUI.
    prompt: Vec<String>,

    /// Headless: stream the answer to stdout and exit.
    #[arg(short = 'p', long = "print")]
    print: bool,

    /// Model name, e.g. qwen2.5-coder:14b.
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// OpenAI-compatible base URL, e.g. http://localhost:1234/v1.
    #[arg(short = 'u', long = "url")]
    base_url: Option<String>,

    /// API key, if the server needs one.
    #[arg(long)]
    api_key: Option<String>,

    /// Workspace root. Defaults to the current directory.
    #[arg(short = 'C', long = "dir")]
    dir: Option<PathBuf>,

    /// Approve file writes and commands without asking.
    #[arg(short = 'y', long)]
    yolo: bool,

    /// Tool-call protocol: auto, native or text.
    #[arg(long)]
    protocol: Option<ToolProtocol>,

    /// Fast mode: a terse prompt and a lean tool set, for small local models.
    #[arg(long)]
    fast: bool,

    /// Allow file tools outside the workspace root.
    #[arg(long)]
    no_sandbox: bool,

    /// Sampling temperature.
    #[arg(short = 't', long)]
    temperature: Option<f64>,

    /// Palette: auto, catppuccin-mocha, tokyo-night, gruvbox-dark, nord,
    /// dracula, rose-pine, solarized-light, mono.
    #[arg(long)]
    theme: Option<String>,

    /// Glyphs: auto, unicode, ascii.
    #[arg(long)]
    icons: Option<String>,

    /// Start in plan (read-only), execute, or vibe mode.
    #[arg(long)]
    mode: Option<config::Mode>,

    /// Reopen the most recent conversation in this project.
    #[arg(short = 'c', long = "continue", visible_alias = "resume")]
    resume: bool,

    /// Name this conversation, so /resume shows it instead of the first prompt.
    #[arg(long)]
    name: Option<String>,

    #[command(subcommand)]
    cmd: Option<Sub>,
}

#[derive(Subcommand, Debug)]
enum Sub {
    /// List models reported by the endpoint.
    Models,
    /// List skills, or write a starter one.
    Skills {
        /// Write a commented example into <project>/.koda/skills/.
        #[arg(long)]
        init: bool,
    },
    /// Show the effective configuration.
    Config {
        /// Write a starter config file if none exists.
        #[arg(long)]
        init: bool,
        /// Print where koda keeps this project's files instead of the config.
        #[arg(long)]
        paths: bool,
    },
    /// Manage the browse tool's engine, which koda ships and installs itself.
    Browser {
        #[command(subcommand)]
        cmd: BrowserCmd,
    },
    /// Show the configured MCP servers, connecting to each to list what it offers.
    Mcp {
        #[command(subcommand)]
        cmd: Option<McpCmd>,
    },
    /// Show which language servers koda knows, and which are usable here.
    Lsp,
}

#[derive(Subcommand, Debug)]
enum McpCmd {
    /// Connect to every configured server and list its tools, resources and prompts.
    List,
}

#[derive(Subcommand, Debug)]
enum BrowserCmd {
    /// Download the browse engine into koda's own directory. The installer runs
    /// this; a `browse` call with no engine does it on its own.
    Install {
        /// Re-download even if it is already installed.
        #[arg(long)]
        force: bool,
    },
    /// Say which engine koda would use, and where it came from.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    let result = runtime.block_on(async_main(cli));
    if let Err(e) = &result {
        // The TUI restores the terminal itself; this keeps errors readable.
        eprintln!("koda: {e:#}");
        std::process::exit(1);
    }
    // Exit the process explicitly rather than falling through to dropping the
    // runtime. After the agent has spawned a child process (e.g. a `!command`
    // or run_command), tokio's process reaper plus the blocking stdin reader
    // thread can keep runtime teardown from completing, which manifested as
    // koda hanging on ctrl+d. A clean run has already restored the terminal, so
    // terminating here is safe and immediate.
    std::process::exit(0);
}

/// Reap koda's children when the process is told to end.
///
/// koda starts two kinds of child that outlive it: a debug adapter holding a
/// stopped program, and the browse engine, which is a daemon holding a whole
/// headless Chrome. Both were only ever cleaned up on koda's own exit paths, so
/// anything that ended the process from outside -- closing the terminal window,
/// `kill`, a `systemctl stop` -- left them running. Three of those in an
/// afternoon is several gigabytes of orphaned Chrome and a laptop that swaps.
///
/// SIGKILL cannot be caught by anything, which is what `reap_orphaned_browsers`
/// is for: it cleans up on the *next* start what this could not clean up on the
/// last exit.
fn install_signal_handlers() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // SIGHUP is the terminal window closing or an ssh session dropping;
        // SIGTERM is a `kill` or a supervisor; SIGINT is ctrl+c, which only
        // reaches us in a headless run -- the TUI puts the terminal in raw
        // mode, where ctrl+c arrives as a key event instead.
        for (kind, code) in [
            (SignalKind::hangup(), 129),
            (SignalKind::interrupt(), 130),
            (SignalKind::terminate(), 143),
        ] {
            tokio::spawn(async move {
                let Ok(mut sig) = signal(kind) else { return };
                if sig.recv().await.is_none() {
                    return;
                }
                // `restore` is the one teardown: terminal, debugger, browser.
                // It runs at most once however koda is ending.
                tui::restore();
                std::process::exit(code);
            });
        }
    }
}

async fn async_main(cli: Cli) -> Result<()> {
    let root = match &cli.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;

    // Installed before anything can start a child, so a signal arriving during
    // startup still finds a teardown to run.
    install_signal_handlers();

    let mut cfg = Config::load(&root)?;
    if let Some(v) = cli.base_url.clone() {
        cfg.base_url = v;
    }
    if let Some(v) = cli.model.clone() {
        cfg.model = v;
    }
    if let Some(v) = cli.api_key.clone() {
        cfg.api_key = v;
    }
    if let Some(v) = cli.protocol {
        cfg.tool_protocol = v;
    }
    if let Some(v) = cli.temperature {
        cfg.temperature = v;
    }
    if cli.yolo {
        cfg.auto_approve = true;
    }
    if cli.fast {
        cfg.fast = true;
    }
    if cli.no_sandbox {
        cfg.sandbox = false;
    }
    if let Some(v) = cli.theme.clone() {
        cfg.theme = v;
    }
    if let Some(v) = cli.icons.clone() {
        cfg.icons = v;
    }
    if let Some(v) = cli.mode {
        cfg.mode = v;
    }

    match cli.cmd {
        Some(Sub::Models) => return list_models(&cfg).await,
        Some(Sub::Mcp { .. }) => return show_mcp(&cfg, &root).await,
        Some(Sub::Lsp) => {
            print!("{}", lsp::status_report(&root));
            return Ok(());
        }
        Some(Sub::Skills { init }) => return show_skills(&root, init),
        Some(Sub::Config { init, paths }) => {
            if paths {
                return show_paths(&root);
            }
            return show_config(&cfg, init);
        }
        Some(Sub::Browser { cmd }) => return browser_cmd(&cfg, cmd).await,
        None => {}
    }

    // An empty model is the common case on first run: ask the server.
    if cfg.model.trim().is_empty() {
        match resolve_model(&cfg).await {
            Ok(m) => cfg.model = m,
            Err(e) => {
                if cli.print {
                    bail!("no model configured and the endpoint is unreachable: {e:#}");
                }
                eprintln!("koda: {e:#}");
                eprintln!(
                    "koda: set a model with -m or in {}",
                    config::config_path().display()
                );
            }
        }
    }

    log::init(&cfg.log_level, cfg.log_to_file);
    debug::set_enabled(cfg.debug);
    // Optional local web UI for live logs and debugging (127.0.0.1 only).
    if cfg.web_ui && !cli.print {
        // The trace ring only pays for itself when something can display it.
        trace::set_enabled(true);
        match webui::start(root.clone(), cfg.web_ui_port, cfg.ui_detail.clone()).await {
            Ok(addr) => eprintln!("koda: web UI at http://{addr}"),
            // Not fatal — koda runs fine without it — but not silent either.
            Err(why) => eprintln!("koda: {why}"),
        }
    }
    tel_info!(
        "agent",
        "session start",
        "model" => cfg.model,
        "endpoint" => cfg.endpoint(),
        "mode" => cfg.mode,
    );

    let prompt = cli.prompt.join(" ");

    let resume = if cli.resume {
        let found = session::latest(&root);
        if found.is_none() {
            eprintln!("koda: no saved session in {}", root.display());
        }
        found
    } else {
        None
    };

    let cfg = Arc::new(cfg);

    if cli.print {
        if prompt.trim().is_empty() {
            bail!("-p needs a prompt: koda -p \"your question\"");
        }
        return headless(cfg, root, prompt, resume, cli.name).await;
    }

    tui::run(cfg, root, Some(prompt), resume, cli.name).await
}

async fn resolve_model(cfg: &Config) -> Result<String> {
    let client = llm::Client::with_tls(cfg.endpoint(), cfg.api_key.clone(), cfg.insecure_tls)?;
    let models = client.models().await?;
    models
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{} reports no models", cfg.endpoint()))
}

/// `koda browser install|status`: provision or report the browse engine.
async fn browser_cmd(cfg: &Config, cmd: BrowserCmd) -> Result<()> {
    match cmd {
        BrowserCmd::Install { force } => {
            if !force {
                if let Some(p) = engine::installed() {
                    println!("agent-browser {} already installed", engine::VERSION);
                    println!("  {}", p.display());
                    return Ok(());
                }
            }
            println!("downloading agent-browser {}…", engine::VERSION);
            let path = engine::install(force).await?;
            println!("installed {}", path.display());
            Ok(())
        }
        BrowserCmd::Status => {
            match tools::find_agent_browser(&cfg.browser_path) {
                Some(p) => {
                    let ours = engine::engine_path().is_some_and(|own| own == p);
                    println!("{}", p.display());
                    println!(
                        "  source: {}",
                        if ours {
                            "shipped with koda"
                        } else if !cfg.browser_path.trim().is_empty() {
                            "browser_path in your config"
                        } else {
                            "found on your system"
                        }
                    );
                }
                None => {
                    println!("no browse engine found — run `koda browser install`");
                }
            }
            Ok(())
        }
    }
}

/// `koda mcp`: connect to every configured server and report what it offers.
///
/// Synchronous where the TUI is not: run from a terminal the user is waiting
/// for an answer, so this waits for the handshakes instead of reporting
/// "connecting…" and exiting.
async fn show_mcp(cfg: &Config, root: &Path) -> Result<()> {
    if !cfg.mcp || cfg.mcp_servers.is_empty() {
        print!("{}", mcp::status_report(cfg));
        return Ok(());
    }
    mcp::connect_all(cfg, root);
    // Poll until every server has either answered or failed, with a ceiling so
    // one wedged server cannot hold the command open for ever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let want = cfg.mcp_servers.iter().filter(|s| s.enabled).count();
    loop {
        let settled = mcp::catalog()
            .iter()
            .filter(|s| s.connected || s.error.is_some())
            .count();
        if settled >= want || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    print!("{}", mcp::status_report(cfg));
    mcp::shutdown().await;
    Ok(())
}

async fn list_models(cfg: &Config) -> Result<()> {
    let client = llm::Client::with_tls(cfg.endpoint(), cfg.api_key.clone(), cfg.insecure_tls)?;
    let models = client.models().await?;
    if models.is_empty() {
        println!("{} reports no models", cfg.endpoint());
        return Ok(());
    }
    for m in models {
        if m == cfg.model {
            println!("* {m}");
        } else {
            println!("  {m}");
        }
    }
    Ok(())
}

fn show_skills(root: &Path, init: bool) -> Result<()> {
    if init {
        let path = skills::write_example(root)?;
        println!("wrote {}", path.display());
    }
    let found = skills::load(root);
    println!("searched:");
    for d in skills::dirs(root) {
        println!("  {}", d.display());
    }
    if found.is_empty() {
        println!("\nno skills found — `koda skills --init` writes an example");
        return Ok(());
    }
    println!("\n{} skill(s):", found.len());
    for s in found {
        println!("  {:<16} {}", s.name, s.when);
    }
    Ok(())
}

/// Where this project's files live. Transcripts, the index and learned rules
/// moved out of `<project>/.koda` into koda's data directory, so "where did my
/// sessions go?" needs an answer that is not "read the source".
fn show_paths(root: &Path) -> Result<()> {
    println!("project    {}", root.display());
    println!("key        {}", config::project_key(root));
    println!("config     {}", config::config_path().display());
    println!("sessions   {}", session::dir(root).display());
    println!(
        "index      {}",
        config::project_state_dir(root, "index").display()
    );
    println!(
        "learning   {}",
        config::project_state_dir(root, "learning").display()
    );
    println!();
    println!("in the project (they are project content, and yours to commit):");
    println!("  skills   {}", root.join(".koda").join("skills").display());
    println!(
        "  memory   {}",
        root.join(".koda").join("memory.md").display()
    );
    Ok(())
}

fn show_config(cfg: &Config, init: bool) -> Result<()> {
    if init {
        let path = Config::write_default_file()?;
        println!("config: {}", path.display());
    }
    println!("# path: {}", config::config_path().display());
    print!("{}", toml::to_string_pretty(cfg)?);
    Ok(())
}

/// Non-interactive run: stream text to stdout, tool activity to stderr.
async fn headless(
    cfg: Arc<Config>,
    root: PathBuf,
    prompt: String,
    resume: Option<session::Summary>,
    name: Option<String>,
) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let auto = cfg.auto_approve;
    let mut agent = Agent::new(cfg, root, cancel, notify)?;
    if let Some(name) = name {
        agent.set_session_name(name);
    }
    if let Some(s) = resume {
        match session::read(&s.path) {
            Ok((_, messages)) => {
                eprintln!("· resumed {} — {} message(s)", s.header.id, messages.len());
                agent.resume(s.path, messages);
            }
            Err(e) => eprintln!("· could not resume: {e}"),
        }
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    // Captured before the agent moves into the task below.
    let cfg_browser = agent.cfg.browser;
    let browser_path = agent.cfg.browser_path.clone();

    let task = tokio::spawn(async move {
        agent.handle(Command::User(prompt), &tx).await;
    });

    let mut failed = false;
    let mut stdout = std::io::stdout();
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Text(chunk) => {
                print!("{chunk}");
                let _ = stdout.flush();
            }
            Event::ToolStart { label, .. } => eprintln!("· {label}"),
            // Headless: the card that would show streamed progress isn't there,
            // and neither is the status row a draft would update.
            Event::ToolProgress { .. } | Event::ToolDraft { .. } => {}
            Event::ToolEnd { ok, summary, .. } => {
                if !ok {
                    eprintln!("✗ {summary}");
                }
            }
            Event::ToolPending { name, reply, .. } => {
                // Nothing can answer a prompt here, so require --yolo up front.
                let decision = if auto { Approval::Once } else { Approval::Deny };
                if !auto {
                    eprintln!("✗ {name} needs approval; re-run with --yolo to allow it");
                    failed = true;
                }
                let _ = reply.send(decision);
            }
            Event::AskUser {
                question,
                options: _,
                reply,
            } => {
                // Headless has nobody to ask; dropping the sender makes the tool
                // return its "no answer, proceed" result.
                eprintln!("· agent asked: {question} (no user in headless mode)");
                drop(reply);
            }
            Event::Notice(msg) => eprintln!("· {msg}"),
            Event::Compacting => eprintln!("· compacting context…"),
            Event::Compacted { before, after } => {
                eprintln!("· compacted {before} → {after} tokens")
            }
            Event::SubActivity(_) => {}
            Event::Error(msg) => {
                eprintln!("✗ {msg}");
                failed = true;
            }
            Event::Models(list) => eprintln!("· models: {}", list.join(", ")),
            Event::Skills(list) => {
                for (n, w) in &list {
                    eprintln!("· skill {n}: {w}");
                }
            }
            Event::Todos(items) => {
                let done = items
                    .iter()
                    .filter(|i| i.status == tools::TodoStatus::Done)
                    .count();
                eprintln!("· plan {done}/{}", items.len());
                for it in &items {
                    let mark = match it.status {
                        tools::TodoStatus::Done => "x",
                        tools::TodoStatus::Active => ">",
                        tools::TodoStatus::Pending => " ",
                    };
                    eprintln!("  [{mark}] {}", it.text);
                }
            }
            Event::NeedsExecuteMode(tool) => {
                eprintln!("· plan mode blocked {tool}; re-run with --mode execute");
            }
            Event::Reasoning(_) | Event::TurnStart | Event::Tokens(_) => {}
            Event::TurnEnd { .. } => break,
        }
    }
    let _ = task.await;
    println!();
    // The TUI tears its children down in `restore`; this path has no terminal
    // to restore and so had no teardown at all -- a headless run that browsed
    // left the engine and its Chrome tree running, and the `exit(2)` below
    // skipped even a `Drop`. Same children, same rule.
    crate::dap::shutdown();
    crate::tools::shutdown_browser();
    // Reap here as well as at startup. The startup reap runs on a background
    // thread so it cannot delay the first prompt, and a headless run is often
    // over before that thread has finished closing anything -- so a `koda -p`
    // would find an orphan and then exit before it had dealt with it. By this
    // point koda's own sessions are already closed above and their records
    // still name this live process, so they are skipped and only genuinely
    // dead ones are touched.
    if cfg_browser {
        crate::tools::reap_orphaned_browsers(&browser_path);
    }
    if failed {
        std::process::exit(2);
    }
    Ok(())
}
