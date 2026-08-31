//! koda — a small terminal coding agent for local, OpenAI-compatible LLMs.

mod agent;
mod anim;
mod config;
mod debug;
mod detailhelp;
mod editor;
mod fuzzy;
mod graph;
mod learning;
mod llm;
mod log;
mod md;
mod panel;
mod memory;
mod prompt;
mod theme;
mod session;
mod settings;
mod setup;
mod skills;
mod tools;
mod tui;
mod watch;
mod web;
mod webui;
mod view;

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
    },
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

async fn async_main(cli: Cli) -> Result<()> {
    let root = match &cli.dir {
        Some(d) => d.clone(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;

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
        Some(Sub::Skills { init }) => return show_skills(&root, init),
        Some(Sub::Config { init }) => return show_config(&cfg, init),
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
                eprintln!("koda: set a model with -m or in {}", config::config_path().display());
            }
        }
    }

    log::init(&cfg.log_level, cfg.log_to_file);
    debug::set_enabled(cfg.debug);
    // Optional local web UI for live logs and debugging (127.0.0.1 only).
    if cfg.web_ui && !cli.print {
        if let Some(addr) =
            webui::start(root.clone(), cfg.web_ui_port, cfg.ui_detail.clone()).await
        {
            eprintln!("koda: web UI at http://{addr}");
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
        return headless(cfg, root, prompt, resume).await;
    }

    tui::run(cfg, root, Some(prompt), resume).await
}

async fn resolve_model(cfg: &Config) -> Result<String> {
    let client = llm::Client::new(cfg.endpoint(), cfg.api_key.clone())?;
    let models = client.models().await?;
    models
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{} reports no models", cfg.endpoint()))
}

async fn list_models(cfg: &Config) -> Result<()> {
    let client = llm::Client::new(cfg.endpoint(), cfg.api_key.clone())?;
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
) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let auto = cfg.auto_approve;
    let mut agent = Agent::new(cfg, root, cancel, notify)?;
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
            Event::AskUser { question, options: _, reply } => {
                // Headless has nobody to ask; dropping the sender makes the tool
                // return its "no answer, proceed" result.
                eprintln!("· agent asked: {question} (no user in headless mode)");
                drop(reply);
            }
            Event::Notice(msg) => eprintln!("· {msg}"),
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
    if failed {
        std::process::exit(2);
    }
    Ok(())
}
