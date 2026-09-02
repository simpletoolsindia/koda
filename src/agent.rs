//! The agent loop: stream a completion, run any tool calls, repeat until the
//! model stops asking for tools.

use crate::config::{Config, Mode, ToolProtocol};
use crate::llm::{ChatRequest, Client, Message, Role, StreamEvent, ToolCall};
use crate::prompt;
use crate::tools::{self, ToolCtx};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};

const TOOL_OPEN: &str = "<tool_call>";
const TOOL_CLOSE: &str = "</tool_call>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Once,
    AlwaysThisTool,
    Deny,
}

#[derive(Debug)]
pub enum Event {
    TurnStart,
    /// Assistant text delta.
    Text(String),
    /// Chain-of-thought delta from thinking models (displayed dimmed, not resent).
    Reasoning(String),
    ToolPending {
        name: String,
        args_pretty: String,
        preview: Option<String>,
        reply: oneshot::Sender<Approval>,
    },
    /// The agent is asking the user a question and waiting for a typed answer.
    /// The TUI routes the user's next message into `reply` instead of starting
    /// a new turn. When `options` is non-empty, the TUI shows a dropdown to pick
    /// from (plus a custom-answer entry).
    AskUser {
        question: String,
        options: Vec<String>,
        reply: oneshot::Sender<String>,
    },
    ToolStart {
        id: String,
        name: String,
        label: String,
        /// 0 = this agent, 1 = inside a delegated subagent.
        depth: u8,
    },
    ToolEnd {
        id: String,
        ok: bool,
        summary: String,
        detail: String,
        /// Structured result, so the transcript can lay it out per tool.
        view: tools::ToolView,
    },
    Notice(String),
    /// Compaction has started (either `/compact` or auto-compact). The TUI shows
    /// an animated "compacting…" status and holds/queues user input until it is
    /// done, so a slow summary call can't look like a frozen prompt.
    Compacting,
    /// Compaction finished: token counts before/after (after == before on a
    /// no-op or failure). The TUI clears the compacting status and flushes any
    /// input the user queued meanwhile.
    Compacted {
        before: usize,
        after: usize,
    },
    /// A lightweight, status-only note that a running subagent is doing
    /// something (thinking, or a short prose beat). The transcript is left
    /// clean — this only updates the working-status row so the user can see the
    /// subagent is alive and what it is up to.
    SubActivity(String),
    Error(String),
    Models(Vec<String>),
    Skills(Vec<(String, String)>),
    /// Running context size, so the status bar stays live mid-turn.
    Tokens(usize),
    /// Plan mode blocked a change the agent wanted to make.
    NeedsExecuteMode(String),
    /// The agent's task list, replacing whatever was shown before.
    Todos(Vec<tools::Todo>),
    TurnEnd {
        history_tokens: usize,
    },
}

#[derive(Debug)]
pub enum Command {
    User(String),
    /// Run a shell command directly (the `!cmd` prefix) — no LLM turn.
    Bang(String),
    Clear,
    Compact,
    ListModels,
    /// Fetch the model list from a URL without switching to it, for setup.
    ProbeModels(String),
    ListSkills,
    /// Load a saved session in place of the current one.
    Resume(std::path::PathBuf),
    WhichSession,
    Undo,
    ReloadSkills,
    SetModel(String),
    SetEndpoint(String),
    #[allow(dead_code)]
    SetAutoApprove(bool),
    SetAutoTier(crate::config::AutoTier),
    SetMode(Mode),
    SetWebSearch(bool),
    /// Push a full updated config from the settings page so live-editable
    /// fields (web search + backend, reasoning effort, system prompt, debug)
    /// take effect without a restart.
    UpdateConfig(Box<crate::config::Config>),
    /// Self-improvement: review/accept/reject learned rule candidates (`/learn`).
    Learn(LearnAction),
    /// Add a durable project note (the web control rail's memory editor). Goes
    /// through the agent so the live system prompt picks it up immediately.
    RememberNote(String),
    /// Drop notes matching a substring.
    ForgetNote(String),
    Quit,
}

/// What `/learn` should do this invocation.
#[derive(Debug, Clone)]
pub enum LearnAction {
    /// Show accepted rules + pending candidates.
    Review,
    /// Accept a candidate by 1-based index, or all of them.
    Accept(Option<usize>),
    /// Reject a candidate by 1-based index.
    Reject(usize),
}

/// Incrementally strips `<tool_call>` blocks out of a streaming text response.
#[derive(Default)]
struct TextScan {
    tail: String,
    in_block: bool,
    block: String,
}

impl TextScan {
    /// Returns (text to display, completed tool-call payloads).
    fn push(&mut self, chunk: &str) -> (String, Vec<String>) {
        let mut buf = std::mem::take(&mut self.tail);
        buf.push_str(chunk);
        let mut display = String::new();
        let mut blocks = Vec::new();

        loop {
            if self.in_block {
                if let Some(i) = buf.find(TOOL_CLOSE) {
                    self.block.push_str(&buf[..i]);
                    blocks.push(std::mem::take(&mut self.block));
                    buf = buf[i + TOOL_CLOSE.len()..].to_string();
                    self.in_block = false;
                    continue;
                }
                // Hold the partial block until the closing tag arrives, keeping
                // back any suffix that could be the start of that tag.
                let hold = partial_suffix(&buf, TOOL_CLOSE);
                let split = buf.len() - hold;
                self.block.push_str(&buf[..split]);
                buf = buf[split..].to_string();
                break;
            }
            if let Some(i) = buf.find(TOOL_OPEN) {
                display.push_str(&buf[..i]);
                buf = buf[i + TOOL_OPEN.len()..].to_string();
                self.in_block = true;
                continue;
            }
            // Hold back any suffix that could be the start of an opening tag.
            let hold = partial_suffix(&buf, TOOL_OPEN);
            let split = buf.len() - hold;
            display.push_str(&buf[..split]);
            buf = buf[split..].to_string();
            break;
        }
        self.tail = buf;
        (display, blocks)
    }

    /// Anything still buffered when the stream ends.
    fn finish(&mut self) -> String {
        if self.in_block {
            // Unterminated block: surface it rather than silently dropping text.
            let mut s = String::from(TOOL_OPEN);
            s.push_str(&std::mem::take(&mut self.block));
            s.push_str(&std::mem::take(&mut self.tail));
            s
        } else {
            std::mem::take(&mut self.tail)
        }
    }
}

/// Length of the longest suffix of `buf` that is a proper prefix of `tag`.
fn partial_suffix(buf: &str, tag: &str) -> usize {
    let max = (tag.len() - 1).min(buf.len());
    for k in (1..=max).rev() {
        let start = buf.len() - k;
        if buf.is_char_boundary(start) && tag.starts_with(&buf[start..]) {
            return k;
        }
    }
    0
}

/// Mutable state accumulated while consuming one streamed response.
#[derive(Default)]
struct StepAcc {
    scan: TextScan,
    text: String,
    reasoning_len: usize,
    /// The reasoning text itself, for the trace. Bounded by the trace's own cap
    /// when the step closes.
    reasoning: String,
    /// Why the model stopped, when the server says.
    finish_reason: Option<String>,
    /// index -> (id, name, partial arguments JSON)
    partials: BTreeMap<usize, (Option<String>, String, String)>,
    /// Completed `<tool_call>` payloads from the text protocol.
    text_calls: Vec<String>,
}

struct StreamResult {
    text: String,
    calls: Vec<ToolCall>,
    cancelled: bool,
    /// Bytes of `reasoning_content` seen; used to explain an empty reply.
    reasoning_len: usize,
}

pub struct Agent {
    pub cfg: Arc<Config>,
    pub model: String,
    pub endpoint: String,
    pub auto_approve: bool,
    pub auto_tier: crate::config::AutoTier,
    client: Client,
    ctx: ToolCtx,
    system: String,
    history: Vec<Message>,
    /// True once we've committed to the `<tool_call>` text protocol.
    text_mode: bool,
    always: HashSet<String>,
    cancel: Arc<AtomicBool>,
    notify: Arc<Notify>,
    call_seq: usize,
    /// 0 for the agent the user talks to; 1 for a delegated subagent.
    depth: u8,
    /// Subagents stream into their own context, not onto the user's screen.
    quiet: bool,
    /// None = every tool; Some = a restricted subset.
    allow: Option<&'static [&'static str]>,
    pub mode: Mode,
    skills: Vec<crate::skills::Skill>,
    /// Shared with the background scanner; None until the first scan finishes.
    graph: Arc<std::sync::RwLock<Option<crate::graph::Graph>>>,
    memory: crate::memory::Memory,
    learning: crate::learning::Learning,
    /// Whether the project-idiom miner (Phase 3) has run this session. It reads
    /// the code graph once it's ready and turns load-bearing internal symbols
    /// and common imports into candidate rules — done once, not every turn.
    mined_idioms: bool,
    session: Option<crate::session::Store>,
    /// File contents captured before each write, newest last.
    undo: Vec<UndoEntry>,
    /// Which turn we are on, so undo can revert a whole turn's edits at once
    /// rather than one file at a time. Bumped at the start of each top-level
    /// user turn.
    turn_seq: u32,
    /// The last tool call that failed and how many times in a row an identical
    /// call has failed — small models tend to re-issue the exact same invalid
    /// call, so we detect that and escalate the corrective feedback.
    last_failure: Option<(String, u32)>,
    /// How many times in a row the model has produced neither a tool call nor
    /// usable text this turn. Small models sometimes reply empty (or with only
    /// hidden reasoning); we nudge once with a concrete hint, then stop cleanly
    /// instead of looping. Reset at the start of each top-level turn.
    empty_replies: u32,
    /// Whether codegraph has already been called in this turn. Used to give a
    /// single corrective hint when a model greps for a bare symbol first.
    used_codegraph_this_turn: bool,
    /// Prevent repeated codegraph reminders from polluting tool results.
    codegraph_hint_sent: bool,
    /// Outcomes for read-only tools pre-computed concurrently for the current
    /// step, keyed by call id, so `execute` can reuse them instead of re-running
    /// the work serially. Drained as the step's calls are processed.
    prefetched: std::collections::HashMap<String, tools::Outcome>,
    /// The trace turn currently open (top-level agent only). `None` when
    /// tracing is off, which makes every trace call a no-op.
    trace_turn: Option<u64>,
    /// How the last tool call was approved, so the trace can show whether the
    /// user was asked. Set by `approve`, consumed by `execute`.
    last_approval: Option<crate::trace::Approval>,
}

/// One reversible file change. `before: None` means the file did not exist, so
/// undoing it removes the file again.
#[derive(Debug, Clone)]
struct UndoEntry {
    path: PathBuf,
    before: Option<String>,
    label: String,
    /// The turn that produced this change. Entries sharing a turn are undone
    /// together.
    turn: u32,
}

impl Agent {
    pub fn new(cfg: Arc<Config>, root: PathBuf, cancel: Arc<AtomicBool>, notify: Arc<Notify>) -> anyhow::Result<Self> {
        let endpoint = cfg.endpoint();
        let client = Client::new(endpoint.clone(), cfg.api_key.clone())?;
        let text_mode = cfg.tool_protocol == ToolProtocol::Text;
        let mode = cfg.mode;
        let skills = crate::skills::load(&root);
        let memory = if cfg.memory {
            crate::memory::Memory::load(&root)
        } else {
            crate::memory::Memory::default()
        };
        let learning = if cfg.learning {
            crate::learning::Learning::load(&root)
        } else {
            crate::learning::Learning::default()
        };

        // Scan off-thread: a large repo must not delay the first prompt.
        let graph: Arc<std::sync::RwLock<Option<crate::graph::Graph>>> =
            Arc::new(std::sync::RwLock::new(None));
        if cfg.codegraph {
            let slot = graph.clone();
            let scan_root = root.clone();
            std::thread::spawn(move || {
                let g = crate::graph::scan(&scan_root);
                if let Ok(mut w) = slot.write() {
                    *w = Some(g);
                }
            });
        }
        let system =
            prompt::build_with_skills(&cfg, &root, text_mode, mode, &skills, &memory, &learning.brief());
        let ctx = ToolCtx {
            root,
            cfg: cfg.clone(),
        };
        Ok(Self {
            model: cfg.model.clone(),
            auto_approve: cfg.auto_approve,
            auto_tier: if cfg.auto_approve {
                crate::config::AutoTier::Full
            } else {
                cfg.auto_tier
            },
            cfg,
            endpoint,
            client,
            ctx,
            system,
            history: Vec::new(),
            text_mode,
            always: HashSet::new(),
            cancel,
            notify,
            call_seq: 0,
            depth: 0,
            quiet: false,
            allow: None,
            mode,
            skills,
            graph: graph.clone(),
            memory,
            learning,
            mined_idioms: false,
            // Created on first persist: an eagerly-created file would be left
            // orphaned by `resume`, and a session with no exchange is noise.
            session: None,
            undo: Vec::new(),
            turn_seq: 0,
            last_failure: None,
            empty_replies: 0,
            used_codegraph_this_turn: false,
            codegraph_hint_sent: false,
            prefetched: std::collections::HashMap::new(),
            trace_turn: None,
            last_approval: None,
        })
    }
    /// Adopt a saved conversation. The transcript the user sees is rebuilt
    /// separately by the UI from the same messages.
    pub fn resume(&mut self, path: std::path::PathBuf, messages: Vec<Message>) {
        let written = messages.len();
        self.history = messages;
        self.session = crate::session::read(&path)
            .ok()
            .map(|(header, _)| crate::session::Store::reopen(path, header, written));
        crate::tel_info!("session", "resumed", "messages" => written);
    }

    /// Persist whatever is new. Cheap: an append of the tail.
    fn persist(&mut self) {
        if !self.cfg.sessions || self.history.is_empty() {
            return;
        }
        if self.session.is_none() {
            self.session = Some(crate::session::Store::create(
                &self.ctx.root,
                &self.model,
                &self.endpoint,
            ));
        }
        if let Some(s) = self.session.as_mut() {
            s.append(&self.history);
        }
    }

    /// Re-read skill files from disk. Cheap, so `/skills` can hot-reload.
    pub fn reload_skills(&mut self) -> usize {
        self.skills = crate::skills::load(&self.ctx.root);
        self.rebuild_system();
        self.skills.len()
    }

    pub fn skill_list(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|s| (s.name.clone(), s.when.clone()))
            .collect()
    }

    /// A subagent: same endpoint and workspace, fresh context, read-only tools.
    fn child(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            model: self.model.clone(),
            endpoint: self.endpoint.clone(),
            // Its tools cannot mutate anything, so there is nothing to approve.
            auto_approve: true,
            auto_tier: crate::config::AutoTier::Full,
            client: self.client.clone(),
            ctx: self.ctx.clone(),
            system: prompt::subagent(&self.ctx.root),
            history: Vec::new(),
            text_mode: self.text_mode,
            always: HashSet::new(),
            cancel: self.cancel.clone(),
            notify: self.notify.clone(),
            call_seq: 0,
            depth: self.depth + 1,
            skills: self.skills.clone(),
            graph: self.graph.clone(),
            memory: self.memory.clone(),
            learning: crate::learning::Learning::default(),
            mined_idioms: true,
            session: None,
            undo: Vec::new(),
            turn_seq: 0,
            last_failure: None,
            empty_replies: 0,
            used_codegraph_this_turn: false,
            codegraph_hint_sent: false,
            prefetched: std::collections::HashMap::new(),
            quiet: true,
            allow: Some(tools::SUBAGENT_TOOLS),
            mode: self.mode,
            // A subagent's work is attributed to the parent turn's tool step;
            // it never opens a turn of its own.
            trace_turn: None,
            last_approval: None,
        }
    }

    pub fn history_tokens(&self) -> usize {
        self.system.len() / 4 + self.history.iter().map(|m| m.approx_tokens()).sum::<usize>()
    }

    pub async fn models(&self) -> anyhow::Result<Vec<String>> {
        self.client.models().await
    }

    fn rebuild_system(&mut self) {
        self.system = prompt::build_with_skills(
            &self.cfg,
            &self.ctx.root.clone(),
            self.text_mode,
            self.mode,
            &self.skills,
            &self.memory,
            &self.learning.brief(),
        );
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.rebuild_system();
        }
    }

    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub fn set_endpoint(&mut self, endpoint: String) {
        self.endpoint = endpoint.trim_end_matches('/').to_string();
        self.client.set_endpoint(self.endpoint.clone());
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Handle one command; drives the whole turn including tool round trips.
    pub async fn handle(&mut self, cmd: Command, tx: &mpsc::UnboundedSender<Event>) {
        match cmd {
            Command::User(input) => self.turn(input, tx).await,
            Command::Clear => {
                self.clear();
                if let Some(s) = self.session.as_mut() {
                    s.rewrite(&[]);
                }
                let _ = tx.send(Event::Notice("context cleared".into()));
            }
            Command::Bang(cmd) => {
                // Run the shell command directly and render it as a tool block.
                // No history entry, no model call — this is a convenience shell,
                // so the conversation context is untouched.
                let id = format!("bang-{}", self.turn_seq.wrapping_add(1));
                let _ = tx.send(Event::ToolStart {
                    id: id.clone(),
                    name: "run_command".into(),
                    label: cmd.clone(),
                    depth: 0,
                });
                let args = serde_json::json!({ "command": cmd });
                let outcome = tools::run("run_command", args, &self.ctx).await;
                let _ = tx.send(Event::ToolEnd {
                    id,
                    ok: outcome.ok,
                    summary: outcome.summary,
                    detail: outcome.content,
                    view: outcome.view,
                });
            }
            Command::Compact => self.compact(tx).await,
            Command::ProbeModels(url) => {
                let probe = match Client::new(
                    url.trim_end_matches('/').to_string(),
                    self.cfg.api_key.clone(),
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Event::Error(user_message(&e)));
                        return;
                    }
                };
                match probe.models().await {
                    Ok(list) => {
                        let _ = tx.send(Event::Models(list));
                    }
                    Err(e) => {
                        crate::tel_warn!("http", "probe failed", "detail" => format!("{e:#}"));
                        let _ = tx.send(Event::Models(Vec::new()));
                        let _ = tx.send(Event::Notice(user_message(&e)));
                    }
                }
            }
            Command::Resume(path) => match crate::session::read(&path) {
                Ok((header, messages)) => {
                    let n = messages.len();
                    self.resume(path, messages);
                    let _ = tx.send(Event::Notice(format!(
                        "resumed {} — {n} message(s), {} tokens",
                        header.id,
                        self.history_tokens()
                    )));
                    let _ = tx.send(Event::Tokens(self.history_tokens()));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("could not read that session: {e}")));
                }
            },
            Command::Undo => {
                let msg = self.undo_last();
                let _ = tx.send(Event::Notice(msg));
            }
            Command::WhichSession => {
                let msg = match self.session.as_ref() {
                    Some(s) => format!("session {}", s.id()),
                    None => "sessions are off (sessions = false)".to_string(),
                };
                let _ = tx.send(Event::Notice(msg));
            }
            Command::ListSkills => {
                let _ = tx.send(Event::Skills(self.skill_list()));
            }
            Command::ReloadSkills => {
                let n = self.reload_skills();
                let _ = tx.send(Event::Notice(format!("reloaded {n} skill(s)")));
                let _ = tx.send(Event::Skills(self.skill_list()));
            }
            Command::ListModels => match self.models().await {
                Ok(list) => {
                    let _ = tx.send(Event::Models(list));
                }
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("{e:#}")));
                }
            },
            Command::SetModel(m) => {
                let _ = tx.send(Event::Notice(format!("model → {m}")));
                self.set_model(m);
            }
            Command::SetEndpoint(u) => {
                let _ = tx.send(Event::Notice(format!("endpoint → {u}")));
                self.set_endpoint(u);
            }
            Command::SetMode(m) => {
                self.set_mode(m);
                let _ = tx.send(Event::Notice(format!("mode → {}", m)));
            }
            Command::SetWebSearch(v) => {
                // cfg is shared immutably, so keep the runtime flag beside it.
                let mut cfg = (*self.cfg).clone();
                cfg.web_search = v;
                self.cfg = Arc::new(cfg);
                self.ctx.cfg = self.cfg.clone();
                self.rebuild_system();
                let _ = tx.send(Event::Notice(format!(
                    "web search {}",
                    if v { "on" } else { "off" }
                )));
            }
            Command::SetAutoApprove(v) => {
                self.auto_approve = v;
                let _ = tx.send(Event::Notice(format!(
                    "auto-approve {}",
                    if v { "on" } else { "off" }
                )));
            }
            Command::SetAutoTier(tier) => {
                self.auto_tier = tier;
                // `auto_approve` stays as the hard override; the tier is the
                // nuanced control the UI now drives.
                self.auto_approve = tier == crate::config::AutoTier::Full;
                let _ = tx.send(Event::Notice(format!("autonomy: {}", tier.label())));
            }
            Command::UpdateConfig(cfg) => {
                // Adopt the settings-edited config for the live-editable fields.
                crate::debug::set_enabled(cfg.debug);
                self.cfg = Arc::new(*cfg);
                self.ctx.cfg = self.cfg.clone();
                // System prompt / web-search availability may have changed.
                self.rebuild_system();
            }
            Command::Learn(action) => {
                if !self.cfg.learning {
                    let _ = tx.send(Event::Notice(
                        "self-improvement is off — set `learning = true` in config or /settings".into(),
                    ));
                    return;
                }
                // Make sure the latest observations are mined before showing.
                self.learning.induce();
                let msg = match action {
                    LearnAction::Review => {
                        let cands = self.learning.candidates();
                        let accepted = self.learning.brief();
                        let mut s = String::new();
                        if cands.is_empty() {
                            s.push_str("No new candidate rules. koda learns as you work — edit files, run commands, and check back.");
                        } else {
                            s.push_str(&format!(
                                "koda learned {} candidate rule(s) — /learn accept <n>, /learn all, or /learn reject <n>:\n",
                                cands.len()
                            ));
                            for (i, r) in cands.iter().enumerate() {
                                s.push_str(&format!("  {}. {}\n", i + 1, r.text));
                            }
                        }
                        if !accepted.trim().is_empty() {
                            s.push_str("\nAlready accepted:");
                            s.push_str(&accepted);
                        }
                        s
                    }
                    LearnAction::Accept(None) => {
                        let n = self.learning.accept_all();
                        self.rebuild_system();
                        format!("accepted {n} rule(s) — koda will follow them from now on")
                    }
                    LearnAction::Accept(Some(n)) => match self.learning.accept(n.saturating_sub(1)) {
                        Some(text) => {
                            self.rebuild_system();
                            format!("accepted: {text}")
                        }
                        None => format!("no candidate #{n}"),
                    },
                    LearnAction::Reject(n) => match self.learning.reject(n.saturating_sub(1)) {
                        Some(text) => format!("rejected: {text}"),
                        None => format!("no candidate #{n}"),
                    },
                };
                let _ = self.learning.save();
                let _ = tx.send(Event::Notice(msg));
            }
            Command::RememberNote(note) => {
                if !self.cfg.memory {
                    let _ = tx.send(Event::Notice(
                        "memory is off — set `memory = true` in config or /settings".into(),
                    ));
                    return;
                }
                let added = self.memory.remember(note.trim());
                let _ = self.memory.save(&self.ctx.root);
                // The prompt carries memory, so it has to be rebuilt for the
                // note to affect this turn rather than the next session.
                self.rebuild_system();
                let _ = tx.send(Event::Notice(if added {
                    format!("remembered: {}", note.trim())
                } else {
                    "already remembered".to_string()
                }));
            }
            Command::ForgetNote(needle) => {
                let n = self.memory.forget(needle.trim());
                let _ = self.memory.save(&self.ctx.root);
                self.rebuild_system();
                let _ = tx.send(Event::Notice(format!("forgot {n} note(s)")));
            }
            Command::Quit => {}
        }
    }

    async fn turn(&mut self, input: String, tx: &mpsc::UnboundedSender<Event>) {
        if self.depth == 0 {
            self.cancel.store(false, Ordering::Relaxed);
            // Each top-level turn is its own undo group.
            self.turn_seq = self.turn_seq.wrapping_add(1);
            self.empty_replies = 0;
            self.used_codegraph_this_turn = false;
            self.codegraph_hint_sent = false;
            self.trace_turn = crate::trace::begin_turn(
                &self.mode.to_string(),
                &self.model,
                &self.endpoint,
                &input,
            );
            let _ = tx.send(Event::TurnStart);
        }
        self.history.push(self.user_message(&input, tx));

        self.auto_compact(tx).await;

        // What the trace will record for this turn. Set at each exit so a
        // cancelled or failed turn is not reported as a clean one.
        let mut status = crate::trace::Status::Ok;
        let mut reply = String::new();
        let mut steps = 0usize;
        loop {
            if self.cancelled() {
                let _ = tx.send(Event::Notice("cancelled".into()));
                status = crate::trace::Status::Cancelled;
                break;
            }
            if steps >= self.cfg.max_steps {
                let _ = tx.send(Event::Notice(format!(
                    "stopped after {steps} steps (max_steps); send another message to continue"
                )));
                break;
            }
            steps += 1;

            self.trim();
            let result = match self.stream_step(tx).await {
                Ok(r) => r,
                Err(e) => {
                    let msg = user_message(&e);
                    crate::tel_error!("agent", "turn failed", "detail" => format!("{e:#}"));
                    // Servers that lack tool support reject the `tools` field; fall back.
                    if !self.text_mode
                        && self.cfg.tool_protocol == ToolProtocol::Auto
                        && looks_like_tool_rejection(&format!("{e:#}"))
                    {
                        self.text_mode = true;
                        self.rebuild_system();
                        let _ = tx.send(Event::Notice(
                            "server rejected native tools; switched to text protocol".into(),
                        ));
                        continue;
                    }
                    let _ = tx.send(Event::Error(msg));
                    status = crate::trace::Status::Error;
                    break;
                }
            };

            if result.cancelled {
                if !result.text.trim().is_empty() {
                    reply = result.text.clone();
                    self.history.push(Message::assistant(result.text));
                }
                let _ = tx.send(Event::Notice("cancelled".into()));
                status = crate::trace::Status::Cancelled;
                break;
            }

            if result.calls.is_empty() {
                if result.text.trim().is_empty() {
                    // Small-model reliability: an empty or reasoning-only reply
                    // is usually a stuck model, not a finished one. Nudge it
                    // once with a concrete hint, then stop cleanly rather than
                    // relaying the same non-answer over and over.
                    self.empty_replies += 1;
                    if self.empty_replies == 1 {
                        self.history.push(Message::user(
                            "You replied with no answer and no tool call. If you are done, \
                             say so in plain text. Otherwise take ONE concrete action now: \
                             call a tool (e.g. read_file or list_dir to gather context, or \
                             write_file/edit_file to make a change), or ask the user a \
                             specific question. Do not reply empty again.",
                        ));
                        let _ = tx.send(Event::Notice(
                            "empty reply — nudging the model to take one concrete action".into(),
                        ));
                        continue;
                    }
                    // Second empty reply in a row: stop the loop deterministically.
                    let _ = tx.send(Event::Error(self.empty_reply_hint(result.reasoning_len)));
                    status = crate::trace::Status::Error;
                    break;
                }
                self.empty_replies = 0;
                reply = result.text.clone();
                self.history.push(Message::assistant(result.text));
                break;
            }
            // Any usable progress resets the empty-reply guard.
            self.empty_replies = 0;

            // Record the assistant turn, then execute the requested tools.
            if self.text_mode {
                let mut raw = result.text.clone();
                for c in &result.calls {
                    raw.push_str(&format!(
                        "\n{TOOL_OPEN}\n{{\"name\": \"{}\", \"arguments\": {}}}\n{TOOL_CLOSE}",
                        c.function.name, c.function.arguments
                    ));
                }
                self.history.push(Message::assistant(raw));
            } else {
                let text = (!result.text.trim().is_empty()).then(|| result.text.clone());
                self.history
                    .push(Message::assistant_calls(text, result.calls.clone()));
            }

            // Parallel tool calls: when the model asks for several read-only
            // tools in one step, run their work concurrently and cache the
            // results. The loop below still processes calls in order (so events,
            // history and approvals stay sequential and predictable) but reuses
            // these outcomes instead of re-doing the I/O one at a time.
            self.prefetch_parallel(&result.calls).await;

            // Track which tool_calls got a response. In native mode every
            // tool_calls entry MUST be followed by a matching tool message or
            // the next request is rejected — so if we break early (cancel, or
            // the repeated-failure breaker) we backfill the rest afterward.
            let mut answered: HashSet<String> = HashSet::new();
            // Files already written by an earlier call in THIS step. A model
            // that batches two writes/edits to the same path would have the
            // second clobber or mis-match the first (its `old` was computed
            // against the pre-batch content); warn instead of silently corrupt.
            let mut written_this_step: HashSet<String> = HashSet::new();
            for call in &result.calls {
                if self.cancelled() {
                    break;
                }
                // Guard same-file conflicts within a batch.
                if matches!(call.function.name.as_str(), "write_file" | "edit_file") {
                    if let Some(p) = call.args().get("path").and_then(|p| p.as_str()) {
                        let key = p.to_string();
                        if !written_this_step.insert(key) {
                            self.history.push(Message::tool(
                                &call.id,
                                &call.function.name,
                                format!(
                                    "ERROR: {p} was already modified earlier in this same step. \
                                     Editing it again now would race the previous change. Re-read \
                                     {p} and make one combined edit (use edit_file's `edits` array \
                                     for several changes to one file).",
                                ),
                            ));
                            answered.insert(call.id.clone());
                            let _ = tx.send(Event::Notice(format!(
                                "skipped a second edit to {p} in one step — ask for one combined edit"
                            )));
                            continue;
                        }
                    }
                }
                let outcome = self.execute(call, tx).await;
                let mut content = outcome.content;

                // Small-model reliability: if the model re-issues the *same*
                // call that just failed, it is stuck in a retry loop. Escalate
                // the feedback so it changes approach instead of repeating, and
                // after several repeats stop the turn rather than spin.
                if !outcome.ok {
                    let sig = format!("{}::{}", call.function.name, call.function.arguments);
                    let n = match &self.last_failure {
                        Some((prev, n)) if *prev == sig => n + 1,
                        _ => 1,
                    };
                    self.last_failure = Some((sig, n));
                    if n == 2 {
                        content.push_str(
                            "\n\nNOTE: this is the SAME call that just failed. Do not repeat it. \
                             Read the error, then either fix the arguments, try a different \
                             tool, or ask the user.",
                        );
                    } else if n >= 3 {
                        // Three identical failures: break the loop deterministically.
                        self.last_failure = None;
                        self.history.push(Message::tool(
                            &call.id,
                            &call.function.name,
                            format!(
                                "{content}\n\nStopping: this call failed {n} times unchanged. \
                                 Summarise what you were trying to do and what is blocking you."
                            ),
                        ));
                        answered.insert(call.id.clone());
                        let _ = tx.send(Event::Notice(format!(
                            "{} failed {n}× with the same input — stopping the loop",
                            call.function.name
                        )));
                        // Let the model produce a final summary next step, then end.
                        break;
                    }
                } else {
                    self.last_failure = None;
                }

                if self.text_mode {
                    self.history.push(Message::user(format!(
                        "Tool result ({}):\n{}",
                        call.function.name, content
                    )));
                } else {
                    self.history
                        .push(Message::tool(&call.id, &call.function.name, content));
                }
                answered.insert(call.id.clone());
            }

            // Backfill any tool_calls left unanswered by an early break, so the
            // native tool protocol stays well-formed (no orphaned tool_calls).
            if !self.text_mode {
                for call in &result.calls {
                    if !answered.contains(&call.id) {
                        self.history.push(Message::tool(
                            &call.id,
                            &call.function.name,
                            "Not run — the turn was interrupted before this tool executed.",
                        ));
                    }
                }
            }
        }

        if self.depth == 0 {
            self.persist();
            // Mine the turn's observations into candidate rules. Deterministic
            // and cheap; candidates stay dormant until the user runs /learn.
            if self.cfg.learning {
                let mut learned = self.learning.induce();
                // Project-idiom mining (Phase 3): once per session, when the
                // code graph is ready, surface load-bearing internal symbols and
                // common imports as candidate rules.
                if !self.mined_idioms {
                    if let Ok(guard) = self.graph.read() {
                        if let Some(g) = guard.as_ref() {
                            // A symbol used across 3+ files is a real idiom here;
                            // a module imported by 3+ files is a convention.
                            let idioms = g.idioms(3);
                            let imports = g.common_imports(3);
                            drop(guard);
                            learned += self.learning.induce_idioms(&idioms, &imports);
                            self.mined_idioms = true;
                        }
                    }
                }
                let _ = self.learning.save();
                if learned > 0 {
                    let _ = tx.send(Event::Notice(format!(
                        "learned {learned} new project rule candidate(s) — review with /learn"
                    )));
                }
            }
            let _ = tx.send(Event::TurnEnd {
                history_tokens: self.history_tokens(),
            });
            crate::trace::end_turn(self.trace_turn, status, &reply, self.history_tokens());
            self.trace_turn = None;
        }
    }

    /// Which tools this agent may call right now: the subagent subset, or in
    /// plan mode the read-only subset, or everything.
    fn effective_allow(&self) -> Option<&'static [&'static str]> {
        match (self.allow, self.mode.read_only()) {
            (Some(a), _) => Some(a),
            (None, true) => Some(tools::PLAN_TOOLS),
            (None, false) => None,
        }
    }

    /// Advertise only what is actually usable right now, so the model is never
    /// tempted by a tool that will refuse it.
    fn advertised_tools(&self) -> Vec<serde_json::Value> {
        let mut list = tools::openai_schema_for(self.effective_allow());
        if !self.cfg.web_search {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("web_search")
            });
        }
        if !self.cfg.web_fetch {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("web_fetch")
            });
        }
        if !self.cfg.subagents {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("delegate")
            });
        }
        if self.skills.is_empty() {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("skill")
            });
        }
        if !self.cfg.memory {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("remember")
            });
        }
        if !self.cfg.codegraph {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("codegraph")
            });
        } else if let Some(pos) = list.iter().position(|t| {
            t.pointer("/function/name").and_then(|n| n.as_str()) == Some("codegraph")
        }) {
            // Tool order is a meaningful prior for smaller models. Put the
            // structural index before generic search/read tools.
            let graph = list.remove(pos);
            list.insert(0, graph);
        }
        // Writing a skill is how the agent keeps what it worked out, so it is
        // available whenever the top-level agent runs — with or without
        // delegation. A subagent must not author skills: its context is narrow
        // and it would write half-learned procedures.
        if self.depth != 0 {
            list.retain(|t| {
                t.pointer("/function/name").and_then(|n| n.as_str()) != Some("manage_skill")
            });
        }
        // User-defined tools (config `[[tools]]`). Only for the top-level agent
        // and never in plan mode, since they run shell commands.
        if self.depth == 0 && !self.mode.read_only() {
            for ct in &self.cfg.custom_tools {
                let props: serde_json::Map<String, Value> = ct
                    .args
                    .iter()
                    .map(|a| {
                        (
                            a.clone(),
                            serde_json::json!({"type": "string", "description": a}),
                        )
                    })
                    .collect();
                list.push(serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": ct.name,
                        "description": ct.description,
                        "parameters": {
                            "type": "object",
                            "properties": props,
                            "required": ct.args,
                        }
                    }
                }));
            }
        }
        list
    }

    /// A model that answers with nothing is almost always a broken chat
    /// template or a model that only emitted hidden reasoning. Say so, rather
    /// than ending the turn silently.
    fn empty_reply_hint(&self, reasoning_len: usize) -> String {
        if reasoning_len > 0 {
            format!(
                "{} returned only reasoning and no answer. Some models need                  `thinking` disabled, or a higher max_tokens so they can finish.",
                self.model
            )
        } else {
            format!(
                "{} returned an empty response. Check that the model streams correctly:\n                   curl -N {}/chat/completions -d '{{\"model\":\"{}\",\"stream\":true,\
                 \"messages\":[{{\"role\":\"user\",\"content\":\"hi\"}}]}}'\n                   An empty stream usually means the model's chat template is broken in the server.",
                self.model, self.endpoint, self.model
            )
        }
    }

    /// One request/response round trip against the model.
    async fn stream_step(&mut self, tx: &mpsc::UnboundedSender<Event>) -> anyhow::Result<StreamResult> {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        messages.push(Message::system(self.system.clone()));
        messages.extend(self.history.iter().cloned());

        let req = ChatRequest {
            model: self.model.clone(),
            messages,
            temperature: self.cfg.temperature,
            top_p: self.cfg.top_p,
            max_tokens: self.cfg.max_tokens,
            tools: if self.text_mode {
                None
            } else {
                Some(self.advertised_tools())
            },
            reasoning_effort: self.cfg.reasoning_effort.clone(),
        };

        if self.depth == 0 {
            let _ = tx.send(Event::Tokens(self.history_tokens()));
        }

        // Trace this call: the request goes in now (so a stalled call is
        // visible), the raw SSE streams in from the HTTP layer, and the parsed
        // result is attached when the step closes.
        let step = crate::trace::open_step(self.trace_turn, crate::trace::StepKind::Model, &self.model);
        let request_json = step
            .is_some()
            .then(|| serde_json::to_string_pretty(&req.to_json()).unwrap_or_default())
            .unwrap_or_default();
        let prompt_tokens = self.history_tokens();

        let (stx, mut srx) = mpsc::unbounded_channel::<StreamEvent>();
        // Clone so the in-flight future doesn't hold a borrow on `self`.
        let client = self.client.clone();
        let stream = client.stream_traced(&req, &stx, self.cfg.max_retries, step);
        tokio::pin!(stream);

        let mut acc = StepAcc::default();
        let mut cancelled = false;
        let mut stream_err = None;
        let mut stream_error: Option<String> = None;

        loop {
            tokio::select! {
                biased;
                _ = self.notify.notified() => {
                    cancelled = true;
                    break;
                }
                res = &mut stream => {
                    if let Err(e) = res { stream_err = Some(e); }
                    break;
                }
                Some(ev) = srx.recv() => {
                    absorb(ev, &mut acc, tx, self.quiet);
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    // Catches a cancel that arrived while we were not awaiting `notified()`.
                    if self.cancelled() {
                        cancelled = true;
                        break;
                    }
                }
            }
        }

        // Drain anything the stream buffered before finishing.
        while let Ok(ev) = srx.try_recv() {
            absorb(ev, &mut acc, tx, self.quiet);
        }

        let StepAcc {
            mut scan,
            mut text,
            reasoning_len,
            reasoning,
            finish_reason,
            partials,
            text_calls,
        } = acc;

        if let Some(e) = stream_err {
            if text.trim().is_empty() {
                crate::trace::finish_model(
                    step,
                    crate::trace::ModelCall {
                        request: request_json,
                        reasoning,
                        finish_reason,
                        prompt_tokens,
                        error: Some(format!("{e:#}")),
                        ..Default::default()
                    },
                );
                return Err(e);
            }
            // Partial output is still useful; report the error as a notice.
            let _ = tx.send(Event::Error(format!("{e:#}")));
            stream_error = Some(format!("{e:#}"));
        }

        let leftover = scan.finish();
        if !leftover.is_empty() {
            if !self.quiet {
                let _ = tx.send(Event::Text(leftover.clone()));
            }
            text.push_str(&leftover);
        }

        let mut calls: Vec<ToolCall> = partials
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| {
                ToolCall::new(id.unwrap_or_else(|| format!("call_{i}")), name, args)
            })
            .collect();

        for payload in text_calls {
            if let Some(c) = self.parse_text_call(&payload) {
                calls.push(c);
            }
        }

        // Last resort: a bare JSON tool call in a fenced block.
        if calls.is_empty() && !cancelled {
            if let Some(c) = self.parse_fenced_call(&text) {
                calls.push(c);
            }
        }

        crate::trace::finish_model(
            step,
            crate::trace::ModelCall {
                request: request_json,
                reasoning,
                text: text.clone(),
                finish_reason,
                prompt_tokens,
                completion_tokens: (text.len() + reasoning_len) / 4,
                tool_calls: calls.iter().map(|c| c.function.name.clone()).collect(),
                error: stream_error,
                ..Default::default()
            },
        );

        Ok(StreamResult {
            text,
            calls,
            cancelled,
            reasoning_len,
        })
    }


    fn next_call_id(&mut self) -> String {
        self.call_seq += 1;
        format!("call_{}", self.call_seq)
    }

    fn parse_text_call(&mut self, payload: &str) -> Option<ToolCall> {
        let v: Value = serde_json::from_str(payload.trim()).ok()?;
        let name = v
            .get("name")
            .or_else(|| v.get("tool"))
            .and_then(|n| n.as_str())?
            .to_string();
        let args = v
            .get("arguments")
            .or_else(|| v.get("parameters"))
            .or_else(|| v.get("args"))
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let args = match args {
            Value::String(s) => s,
            other => other.to_string(),
        };
        let id = self.next_call_id();
        Some(ToolCall::new(id, name, args))
    }

    fn parse_fenced_call(&mut self, text: &str) -> Option<ToolCall> {
        // First preference: a JSON object inside a fenced block (``` or ```json).
        let mut best: Option<&str> = None;
        for (i, _) in text.match_indices("```") {
            let rest = &text[i + 3..];
            let body_start = rest.find('\n').map(|n| n + 1).unwrap_or(0);
            let body = &rest[body_start..];
            let end = body.find("```").unwrap_or(body.len());
            let body = body[..end].trim();
            if body.starts_with('{') && (body.contains("\"name\"") || body.contains("\"tool\"")) {
                best = Some(body);
            }
        }
        if let Some(payload) = best {
            if let Some(c) = self.parse_text_call(payload) {
                if tools::spec(&c.function.name).is_some() {
                    return Some(c);
                }
            }
        }
        // Fallback for models that forget the fences and drop a bare JSON tool
        // call into prose. Scan for balanced `{...}` objects that mention a
        // tool key and try to parse each; only accept one naming a real tool so
        // we never hijack sample JSON in an explanation.
        for cand in balanced_json_objects(text) {
            if !(cand.contains("\"name\"") || cand.contains("\"tool\"")) {
                continue;
            }
            if let Some(c) = self.parse_text_call(cand) {
                if tools::spec(&c.function.name).is_some() {
                    return Some(c);
                }
            }
        }
        None
    }

    /// Run the read-only tools in `calls` concurrently and stash their outcomes
    /// by call id, so `execute` can hand them back without re-running the work.
    /// Only fires with two or more parallel-safe calls. The per-tool event
    /// stream, approvals and cancellation are still handled in the sequential
    /// loop that follows — only the I/O is parallelised.
    async fn prefetch_parallel(&mut self, calls: &[ToolCall]) {
        self.prefetched.clear();
        let safe: Vec<&ToolCall> = calls
            .iter()
            .filter(|c| tools::is_parallel_safe(&c.function.name) && !c.args().is_null())
            .collect();
        if safe.len() < 2 {
            return;
        }
        let futures = safe.iter().map(|c| {
            let name = c.function.name.clone();
            let args = c.args();
            let ctx = self.ctx.clone();
            let id = c.id.clone();
            async move { (id, tools::run(&name, args, &ctx).await) }
        });
        let results = futures_util::future::join_all(futures).await;
        for (id, outcome) in results {
            self.prefetched.insert(id, outcome);
        }
        crate::tel_debug!("agent", "parallel prefetch", "count" => self.prefetched.len());
    }

    /// Run one tool call, recording it as a step in the turn's trace: the
    /// arguments, the outcome, whether the user was asked, and — for a write —
    /// the diff that was applied.
    async fn execute(&mut self, call: &ToolCall, tx: &mpsc::UnboundedSender<Event>) -> tools::Outcome {
        let step = crate::trace::open_step(
            self.trace_turn,
            crate::trace::StepKind::Tool,
            &call.function.name,
        );
        self.last_approval = None;
        // The preview is the same diff the approval prompt shows. Only computed
        // when tracing, and only for writes, so it costs nothing otherwise.
        let diff = if step.is_some()
            && matches!(call.function.name.as_str(), "write_file" | "edit_file")
        {
            tools::preview(&call.function.name, &call.args(), &self.ctx)
        } else {
            None
        };
        let outcome = self.execute_inner(call, tx).await;
        crate::trace::finish_tool(
            step,
            crate::trace::ToolStep {
                name: call.function.name.clone(),
                args: serde_json::to_string_pretty(&call.args())
                    .unwrap_or_else(|_| call.function.arguments.clone()),
                ok: outcome.ok,
                summary: outcome.summary.clone(),
                detail: outcome.content.clone(),
                approval: self.last_approval.take(),
                diff,
            },
        );
        outcome
    }

    async fn execute_inner(&mut self, call: &ToolCall, tx: &mpsc::UnboundedSender<Event>) -> tools::Outcome {
        let name = call.function.name.clone();
        let args = call.args();
        if args.is_null() {
            // Malformed JSON arguments. Small models often emit almost-JSON
            // (trailing commas, Python literals, prose around it). `args()`
            // already tries to repair it; if it still won't parse, don't hard
            // error — hand back a short, corrective message naming the tool and
            // the parameters it expects so the model can re-issue it cleanly.
            let hint = required_params_hint(&name);
            return tools::Outcome {
                ok: false,
                content: format!(
                    "ERROR: could not parse the JSON arguments for `{name}`. {hint} \
                     Re-send the call with a single valid JSON object as `arguments` \
                     (double-quoted keys and strings, no trailing commas, no extra prose). \
                     Got: {}",
                    call.function.arguments.trim()
                ),
                summary: format!("{name}: bad arguments"),
                view: tools::ToolView::Plain,
            };
        }
        if self.mode.read_only() && tools::is_mutating(&name) {
            let _ = tx.send(Event::NeedsExecuteMode(name.clone()));
            return tools::Outcome {
                ok: false,
                content: format!(
                    "ERROR: `{name}` is not allowed in plan mode — nothing may change on disk \
                     yet. Finish the plan, state it clearly, and end your turn by asking the \
                     user to switch to execute mode (ctrl+p)."
                ),
                summary: format!("{name} blocked — plan mode"),
                view: tools::ToolView::Plain,
            };
        }
        if let Some(allow) = self.effective_allow() {
            if !allow.contains(&name.as_str()) {
                return tools::Outcome {
                    ok: false,
                    content: format!(
                        "ERROR: `{name}` is not available to a subagent. You may only use: {}",
                        allow.join(", ")
                    ),
                    summary: format!("{name}: not allowed here"),
                    view: tools::ToolView::Plain,
                };
            }
        }
        // User-defined tool? Run its shell-command template. Only the top-level
        // agent has them, and never in plan mode (advertised_tools enforces the
        // same, this is defence in depth).
        if let Some(ct) = self
            .cfg
            .custom_tools
            .iter()
            .find(|c| c.name == name)
            .cloned()
        {
            if self.depth > 0 || self.mode.read_only() {
                return tools::Outcome {
                    ok: false,
                    content: format!("ERROR: custom tool `{name}` is not available here."),
                    summary: format!("{name}: unavailable"),
                    view: tools::ToolView::Plain,
                };
            }
            let command = tools::expand_custom_command(&ct.command, &ct.args, &args);
            // Route through the same approval + run path as run_command.
            let run_args = serde_json::json!({ "command": command });
            if ct.mutating && !self.approve("run_command", &run_args, tx).await {
                return tools::Outcome {
                    ok: false,
                    content: "ERROR: the user denied this action.".into(),
                    summary: format!("{name}: denied"),
                    view: tools::ToolView::Plain,
                };
            }
            let _ = tx.send(Event::ToolStart {
                id: call.id.clone(),
                name: name.clone(),
                label: format!("{name}: {}", tools::first_line(&command)),
                depth: self.depth,
            });
            let started = std::time::Instant::now();
            let outcome = tools::run("run_command", run_args, &self.ctx).await;
            let _ = tx.send(Event::ToolEnd {
                id: call.id.clone(),
                ok: outcome.ok,
                summary: format!("{name} → {}", outcome.summary),
                detail: outcome.content.clone(),
                view: outcome.view.clone(),
            });
            crate::tel_info!("tool", "custom tool ran", "name" => name, "ms" => started.elapsed().as_millis());
            return outcome;
        }
        if tools::spec(&name).is_none() {
            let names: Vec<&str> = tools::specs().iter().map(|s| s.name).collect();
            return tools::Outcome {
                ok: false,
                content: format!("ERROR: unknown tool `{name}`. Available: {}", names.join(", ")),
                summary: format!("unknown tool {name}"),
                view: tools::ToolView::Plain,
            };
        }

        if !self.approve(&name, &args, tx).await {
            if self.cfg.learning && self.depth == 0 {
                self.learning
                    .observe(&crate::learning::Observation::Denied { tool: name.clone() });
            }
            return tools::Outcome {
                ok: false,
                content: "ERROR: the user denied this action. Ask what to do instead; do not retry."
                    .into(),
                summary: format!("{name}: denied"),
    view: tools::ToolView::Plain,
            };
        }

        let label = label_for(&name, &args);
        let _ = tx.send(Event::ToolStart {
            id: call.id.clone(),
            name: name.clone(),
            label,
            depth: self.depth,
        });

        let started = std::time::Instant::now();
        let args_for_memory = args.clone();
        if name == "codegraph" {
            self.used_codegraph_this_turn = true;
        }
        // Snapshot before the write, so /undo can put it back without git.
        if matches!(name.as_str(), "write_file" | "edit_file") {
            self.snapshot(&args, &name);
        }
        let mut outcome = match name.as_str() {
            "delegate" => self.delegate(&args, tx).await,
            "ask_user" => self.ask_user(&args, tx).await,
            "remember" => self.remember(&args),
            "codegraph" => self.query_graph(&args).await,
            "skill" => self.read_skill(&args),
            // `manage_agent` was the old name for this, when it could only make
            // role agents; keep accepting it so a model that learned that name
            // still works.
            "manage_skill" | "manage_agent" => self.manage_skill(&args),
            "web_search" => self.web_search(&args).await,
            "web_fetch" => self.web_fetch(&args).await,
            "todo" => {
                let items = tools::parse_todos(&args);
                if items.is_empty() {
                    tools::Outcome {
                        ok: false,
                        content: "ERROR: `items` must be a non-empty array of \
                                  {text, status} objects."
                            .into(),
                        summary: "todo: empty list".into(),
                        view: tools::ToolView::Plain,
                    }
                } else {
                    let done = items
                        .iter()
                        .filter(|i| i.status == tools::TodoStatus::Done)
                        .count();
                    let total = items.len();
                    let summary = format!("plan updated ({done}/{total} done)");
                    let _ = tx.send(Event::Todos(items));
                    tools::Outcome {
                        ok: true,
                        content: format!("Task list recorded: {done}/{total} done."),
                        summary,
                        view: tools::ToolView::Plain,
                    }
                }
            }
            _ => match self.prefetched.remove(&call.id) {
                // Reuse the result computed concurrently in prefetch_parallel.
                Some(o) => o,
                None => tools::run(&name, args, &self.ctx).await,
            },
        };
        if self.depth == 0
            && self.cfg.codegraph
            && !self.used_codegraph_this_turn
            && !self.codegraph_hint_sent
            && looks_like_symbol_search(&name, &args_for_memory)
        {
            let symbol = args_for_memory
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            outcome.content.push_str(&format!(
                "\n\nCODEGRAPH HINT: `{symbol}` looks like a symbol lookup. Call `codegraph` \
                 with query=`symbol` before more search/read calls; it returns the definition \
                 and cross-file users in one result."
            ));
            self.codegraph_hint_sent = true;
        }
        // Command outcomes are the one thing worth learning without being asked:
        // next session should know which test runner actually works here.
        if name == "run_command" && self.cfg.memory && self.depth == 0 {
            if let Some(cmd) = args_for_memory.get("command").and_then(|c| c.as_str()) {
                self.memory.record_command(cmd, outcome.ok);
                let _ = self.memory.save(&self.ctx.root);
            }
        }
        // Self-improvement (Phase 1): log the raw signal. Deterministic rule
        // induction runs later (at turn end / on /learn), never a model here.
        if self.cfg.learning && self.depth == 0 {
            if name == "run_command" {
                if let Some(cmd) = args_for_memory.get("command").and_then(|c| c.as_str()) {
                    self.learning.observe(&crate::learning::Observation::Command {
                        command: cmd.to_string(),
                        ok: outcome.ok,
                    });
                }
            }
            // Phase 2: reading a file koda previously wrote reveals whether the
            // user changed it in the meantime — the on-disk content is theirs.
            if name == "read_file" && outcome.ok {
                if let Some(p) = args_for_memory.get("path").and_then(|c| c.as_str()) {
                    let abs = if std::path::Path::new(p).is_absolute() {
                        PathBuf::from(p)
                    } else {
                        self.ctx.root.join(p)
                    };
                    if let Ok(disk) = std::fs::read_to_string(&abs) {
                        self.check_correction(&abs, &disk);
                    }
                }
            }
        }
        // Which files the work happens in is also observed fact worth carrying
        // forward, so next session koda orients to the active areas first.
        if matches!(name.as_str(), "write_file" | "edit_file")
            && outcome.ok
            && self.cfg.memory
            && self.depth == 0
        {
            if let Some(p) = args_for_memory.get("path").and_then(|c| c.as_str()) {
                self.memory.record_edit(p);
                let _ = self.memory.save(&self.ctx.root);
            }
        }
        // Self-improvement: record the (before, after) of an edit koda made, so
        // rule induction can mine naming/imports and, later, compare koda's
        // proposal against what the user keeps. `before` comes from the undo
        // snapshot taken just before this write; `after` is read from disk.
        if matches!(name.as_str(), "write_file" | "edit_file")
            && outcome.ok
            && self.cfg.learning
            && self.depth == 0
        {
            if let Some(p) = args_for_memory.get("path").and_then(|c| c.as_str()) {
                let abs = if std::path::Path::new(p).is_absolute() {
                    PathBuf::from(p)
                } else {
                    self.ctx.root.join(p)
                };
                let before = self
                    .undo
                    .last()
                    .filter(|e| e.path == abs)
                    .and_then(|e| e.before.clone())
                    .unwrap_or_default();
                let after = std::fs::read_to_string(&abs).unwrap_or_default();
                if !after.is_empty() {
                    self.learning.observe(&crate::learning::Observation::Edit {
                        path: p.to_string(),
                        before,
                        after: after.clone(),
                    });
                }
                // Remember exactly what koda left on disk, so a later divergence
                // is attributable to the user (Phase 2 correction signal).
                // Persistent so it survives across sessions.
                crate::learning::record_write(&self.ctx.root, p, &after);
            }
        }
        // Keep the code graph current without a full rescan: re-index just the
        // file koda changed. Cheap (one file), so codegraph answers stay fresh
        // as the agent works.
        if matches!(name.as_str(), "write_file" | "edit_file") && outcome.ok && self.cfg.codegraph {
            if let Some(p) = args_for_memory.get("path").and_then(|c| c.as_str()) {
                let abs = if std::path::Path::new(p).is_absolute() {
                    std::path::PathBuf::from(p)
                } else {
                    self.ctx.root.join(p)
                };
                if let Ok(mut guard) = self.graph.write() {
                    if let Some(g) = guard.as_mut() {
                        g.update_file(&self.ctx.root, &abs);
                    }
                }
            }
        }
        crate::log::push(
            if outcome.ok {
                crate::log::Level::Info
            } else {
                crate::log::Level::Warn
            },
            "tool",
            outcome.summary.clone(),
            vec![
                ("tool".into(), name.clone()),
                ("ms".into(), started.elapsed().as_millis().to_string()),
                ("depth".into(), self.depth.to_string()),
            ],
        );
        let _ = tx.send(Event::ToolEnd {
            id: call.id.clone(),
            ok: outcome.ok,
            summary: outcome.summary.clone(),
            detail: outcome.content.clone(),
            view: outcome.view.clone(),
        });
        outcome
    }

    /// If `disk_now` differs from what koda last wrote to `abs`, the user has
    /// changed koda's output — record the correction (Phase 2 vibe signal).
    /// Consumes the tracked write so one edit is counted once. Persistent, so
    /// it works across sessions (koda writes today, you edit, koda notices later).
    fn check_correction(&mut self, abs: &std::path::Path, disk_now: &str) {
        if !self.cfg.learning || self.depth != 0 {
            return;
        }
        let rel = crate::tools::rel(&self.ctx, abs);
        if let Some(koda_wrote) = crate::learning::last_write(&self.ctx.root, &rel) {
            if koda_wrote != disk_now && !disk_now.trim().is_empty() {
                self.learning
                    .observe(&crate::learning::Observation::Correction {
                        path: rel.clone(),
                        koda_wrote,
                        user_has: disk_now.to_string(),
                    });
                // Don't re-report the same divergence repeatedly.
                crate::learning::clear_write(&self.ctx.root, &rel);
            }
        }
    }

    /// Remember a file's contents before we change it. Bounded, because the
    /// point is recovering from the last mistake, not full history.
    fn snapshot(&mut self, args: &Value, tool: &str) {
        const DEPTH: usize = 25;
        let Some(rel) = args.get("path").and_then(|p| p.as_str()) else {
            return;
        };
        let full = if std::path::Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            self.ctx.root.join(rel)
        };
        let before = std::fs::read_to_string(&full).ok();
        // Before koda overwrites this file, its current on-disk content is the
        // user's version. If it diverged from what koda last wrote, that is a
        // correction worth learning from.
        if let Some(b) = &before {
            self.check_correction(&full, b);
        }
        self.undo.push(UndoEntry {
            path: full,
            before,
            label: format!("{tool} {rel}"),
            turn: self.turn_seq,
        });
        if self.undo.len() > DEPTH {
            self.undo.remove(0);
        }
    }

    /// Undo the most recent turn's file changes as a group.
    ///
    /// A single turn can touch several files (or the same file more than once);
    /// reverting one file at a time would leave a turn half-undone and confusing.
    /// So this pops every entry belonging to the latest turn and restores each
    /// distinct file to the state it had *before* the turn began — i.e. the
    /// earliest snapshot recorded for that path in the group, since entries are
    /// stored oldest-first.
    /// Build the user message for a turn, attaching any `@`-mentioned image
    /// files as vision content. A token like `@shot.png` that resolves to an
    /// image under the workspace is read, size-checked, and encoded as a data
    /// URL; non-images and misses are left as plain text for the model to open
    /// with `read_file`. Images ride alongside the original text unchanged, so
    /// a text-only model simply never sees the extra content parts.
    fn user_message(&self, input: &str, tx: &mpsc::UnboundedSender<Event>) -> Message {
        let mut images = Vec::new();
        // OCR'd text blocks, appended to the message when the model can't see
        // images (and OCR is enabled) so the content isn't simply lost.
        let mut ocr_blocks: Vec<String> = Vec::new();
        let vision = crate::llm::model_is_vision(&self.model);
        for tok in input.split_whitespace() {
            let Some(raw) = tok.strip_prefix('@') else {
                continue;
            };
            let raw = raw.trim_end_matches(['.', ',', ')', ':', ';']);
            let path = std::path::Path::new(raw);
            if !tools::is_image_path(path) {
                continue;
            }
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.ctx.root.join(path)
            };
            // Path 1 — a vision model: attach the image as a data URL, capped at
            // the file-read ceiling so a huge asset can't blow up the request.
            if vision {
                match tools::image_data_url(&full, self.cfg.max_file_bytes) {
                    Ok(url) => {
                        images.push(url);
                        let _ = tx.send(Event::Notice(format!("attached image {raw}")));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Notice(format!("could not attach {raw}: {e}")));
                    }
                }
                continue;
            }
            // Path 2 — the model isn't vision-capable. If OCR is on, extract the
            // image's text with tesseract and inject that instead; otherwise say
            // why the image was skipped.
            if self.cfg.ocr {
                match tools::ocr_image(&full) {
                    Ok(text) if !text.is_empty() => {
                        let _ = tx.send(Event::Notice(format!("OCR'd image {raw}")));
                        ocr_blocks.push(format!("[OCR text of {raw}]\n{text}"));
                    }
                    Ok(_) => {
                        let _ = tx.send(Event::Notice(format!("OCR found no text in {raw}")));
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Notice(format!("could not OCR {raw}: {e}")));
                    }
                }
            } else {
                let _ = tx.send(Event::Notice(format!(
                    "skipped image {raw}: model '{}' isn't vision-capable (enable OCR in /settings)",
                    self.model
                )));
            }
        }
        if !ocr_blocks.is_empty() {
            // Fold OCR'd text into the message so the model actually receives it.
            let combined = format!("{input}\n\n{}", ocr_blocks.join("\n\n"));
            return Message::user(combined);
        }
        if images.is_empty() {
            Message::user(input)
        } else {
            Message::user_with_images(input, images)
        }
    }

    fn undo_last(&mut self) -> String {
        let Some(&last_turn) = self.undo.last().map(|e| &e.turn) else {
            return "nothing to undo in this session".into();
        };

        // Split off the group: all trailing entries sharing the latest turn.
        let split = self
            .undo
            .iter()
            .rposition(|e| e.turn != last_turn)
            .map(|i| i + 1)
            .unwrap_or(0);
        let group = self.undo.split_off(split);

        // For each path, keep the earliest pre-turn content (first occurrence),
        // preserving encounter order for a stable, readable report.
        let mut order: Vec<PathBuf> = Vec::new();
        let mut first: BTreeMap<PathBuf, (Option<String>, String)> = BTreeMap::new();
        for e in group {
            if !first.contains_key(&e.path) {
                order.push(e.path.clone());
            }
            first.entry(e.path.clone()).or_insert((e.before, e.label));
        }

        let mut reverted = 0usize;
        let mut removed = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for path in &order {
            let (before, label) = first.remove(path).unwrap();
            let shown = crate::tools::rel(&self.ctx, path);
            let result = match &before {
                Some(text) => std::fs::write(path, text).map(|_| false),
                // It did not exist before the turn, so undoing removes it.
                None => std::fs::remove_file(path).map(|_| true),
            };
            match result {
                Ok(was_removed) => {
                    if was_removed {
                        removed += 1;
                    } else {
                        reverted += 1;
                    }
                    crate::tel_info!("undo", "reverted", "tool" => label, "path" => shown);
                }
                Err(e) => {
                    crate::tel_warn!("undo", "restore failed", "detail" => e.to_string());
                    failed.push(format!("{shown}: {e}"));
                }
            }
        }

        let touched = reverted + removed;
        let mut msg = match (reverted, removed) {
            (0, 0) => "nothing to undo in this session".to_string(),
            _ => format!(
                "undid last turn — {} file{} reverted{}",
                touched,
                if touched == 1 { "" } else { "s" },
                if removed > 0 {
                    format!(" ({removed} created file{} removed)", if removed == 1 { "" } else { "s" })
                } else {
                    String::new()
                }
            ),
        };
        if !failed.is_empty() {
            msg.push_str(&format!("; could not undo {}", failed.join(", ")));
        }
        msg
    }

    /// Put a question to the user and block this turn until they answer. In a
    /// subagent (no direct user) or headless run there is nobody to ask, so it
    /// fails cleanly and tells the model to decide for itself.
    async fn ask_user(&mut self, args: &Value, tx: &mpsc::UnboundedSender<Event>) -> tools::Outcome {
        let question = args
            .get("question")
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if question.is_empty() {
            return tools::Outcome {
                ok: false,
                content: "ERROR: `question` is required.".into(),
                summary: "ask_user: empty".into(),
                view: tools::ToolView::Plain,
            };
        }
        if self.quiet || self.depth > 0 {
            return tools::Outcome {
                ok: false,
                content: "ERROR: no user is available to answer here. Decide yourself and \
                          proceed."
                    .into(),
                summary: "ask_user: no user".into(),
                view: tools::ToolView::Plain,
            };
        }
        let (reply, rx) = oneshot::channel();
        let options: Vec<String> = args
            .get("options")
            .and_then(|o| o.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let _ = tx.send(Event::AskUser {
            question: question.clone(),
            options,
            reply,
        });
        match rx.await {
            Ok(answer) if !answer.trim().is_empty() => {
                crate::tel_info!("agent", "user answered", "q" => question);
                tools::Outcome {
                    ok: true,
                    content: format!("The user answered: {answer}"),
                    summary: "answered".into(),
                    view: tools::ToolView::Plain,
                }
            }
            _ => tools::Outcome {
                ok: false,
                content: "ERROR: the user did not answer. Proceed with your best judgement."
                    .into(),
                summary: "ask_user: no answer".into(),
                view: tools::ToolView::Plain,
            },
        }
    }

    fn remember(&mut self, args: &Value) -> tools::Outcome {
        if !self.cfg.memory {
            return tools::Outcome {
                ok: false,
                content: "ERROR: project memory is off (memory = false).".into(),
                summary: "remember: disabled".into(),
    view: tools::ToolView::Plain,
            };
        }
        if let Some(drop) = args.get("forget").and_then(|f| f.as_str()) {
            let n = self.memory.forget(drop);
            let _ = self.memory.save(&self.ctx.root);
            self.rebuild_system();
            return tools::Outcome {
                ok: n > 0,
                content: if n > 0 {
                    format!("Forgot {n} note(s) matching `{drop}`.")
                } else {
                    format!("ERROR: no note matched `{drop}`.")
                },
                summary: format!("forget {drop}"),
                view: tools::ToolView::Plain,
            };
        }
        let note = args.get("note").and_then(|n| n.as_str()).unwrap_or("").trim();
        if note.is_empty() {
            return tools::Outcome {
                ok: false,
                content: "ERROR: `note` must be a sentence stating a durable fact.".into(),
                summary: "remember: empty".into(),
    view: tools::ToolView::Plain,
            };
        }
        let added = self.memory.remember(note);
        if added {
            if let Err(e) = self.memory.save(&self.ctx.root) {
                crate::tel_warn!("memory", "save failed", "detail" => e);
            }
            self.rebuild_system();
        }
        tools::Outcome {
            ok: true,
            content: if added {
                "Noted. It will be in your instructions next session.".into()
            } else {
                "Already recorded.".into()
            },
            summary: format!("remember: {}", note.chars().take(60).collect::<String>()),
            view: tools::ToolView::Plain,
        }
    }

    async fn query_graph(&self, args: &Value) -> tools::Outcome {
        if !self.cfg.codegraph {
            return tools::Outcome {
                ok: false,
                content: "ERROR: the code graph is off (codegraph = false). Use search and \
                          find_files instead."
                    .into(),
                summary: "codegraph: disabled".into(),
    view: tools::ToolView::Plain,
            };
        }
        // If the startup scan has not landed yet, build it now rather than
        // telling the model to come back later — it cannot wait.
        if self.graph.read().map(|g| g.is_none()).unwrap_or(true) {
            let slot = self.graph.clone();
            let root = self.ctx.root.clone();
            let built = tokio::task::spawn_blocking(move || {
                if slot.read().map(|g| g.is_some()).unwrap_or(false) {
                    return true; // the background thread won the race
                }
                let g = crate::graph::scan(&root);
                slot.write().map(|mut w| *w = Some(g)).is_ok()
            })
            .await
            .unwrap_or(false);
            if !built {
                return tools::Outcome {
                    ok: false,
                    content: "ERROR: could not build the code graph. Use search and \
                              find_files instead."
                        .into(),
                    summary: "codegraph: unavailable".into(),
    view: tools::ToolView::Plain,
                };
            }
        }
        let guard = self.graph.read().ok();
        let Some(Some(g)) = guard.as_deref() else {
            return tools::Outcome {
                ok: false,
                content: "ERROR: the code graph is unavailable. Use search instead.".into(),
                summary: "codegraph: unavailable".into(),
    view: tools::ToolView::Plain,
            };
        };
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .unwrap_or("overview")
            .trim()
            .to_ascii_lowercase();
        let (content, summary) = match query.as_str() {
            "symbol" => {
                let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
                if name.is_empty() {
                    (
                        "ERROR: query=symbol needs `name`.".to_string(),
                        "codegraph: missing name".to_string(),
                    )
                } else {
                    (g.symbol(name), format!("codegraph symbol {name}"))
                }
            }
            "file" => {
                let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").trim();
                if path.is_empty() {
                    (
                        "ERROR: query=file needs `path`.".to_string(),
                        "codegraph: missing path".to_string(),
                    )
                } else {
                    (g.file(path), format!("codegraph file {path}"))
                }
            }
            _ => (
                g.overview(),
                format!("codegraph overview ({} files)", g.files),
            ),
        };
        tools::Outcome {
            ok: !content.starts_with("ERROR:"),
            content,
            summary,
            view: tools::ToolView::Plain,
        }
    }

    /// Create, update, or remove a *role agent* on the fly. A role agent is a
    /// skill file with a `role:` line; once written it can be delegated to with
    /// `delegate(role=...)` or orchestrated via `/orc`. This is how the main
    /// agent grows a specialised helper for the task at hand instead of the user
    /// having to hand-author a skill file.
    /// Author a project skill: a reusable procedure the agent worked out, written
    /// to `.koda/skills/<name>.md` so the next session starts with it. A skill
    /// with a `role` is additionally delegatable, which is how role agents are
    /// created — a role agent is just a skill with a role.
    ///
    /// Deliberately conservative: it refuses vague or near-empty skills, refuses
    /// to silently shadow an existing one, and every write goes through the same
    /// approval path as any other file write, so the user always sees it.
    fn manage_skill(&mut self, args: &Value) -> tools::Outcome {
        let err = |msg: String, sum: String| tools::Outcome {
            ok: false,
            content: format!("ERROR: {msg}"),
            summary: sum,
            view: tools::ToolView::Plain,
        };

        let action = args
            .get("action")
            .and_then(|a| a.as_str())
            .unwrap_or("create")
            .trim()
            .to_ascii_lowercase();
        let role = args
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        // `name` is the skill's identity. A role-only call (the older
        // `manage_agent` shape) still works: the role names the agent.
        let raw_name = args
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let name = if !raw_name.is_empty() {
            raw_name
        } else if !role.is_empty() {
            format!("{role}-agent")
        } else {
            String::new()
        };
        let slug_ok = |s: &str| {
            !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        if !slug_ok(&name) {
            return err(
                "`name` is required and must be a short slug (letters, digits, - or _), \
                 e.g. \"run-integration-tests\"."
                    .into(),
                "manage_skill: bad name".into(),
            );
        }
        if !role.is_empty() && !slug_ok(&role) {
            return err(
                "`role` must be a short slug (letters, digits, - or _), e.g. \"qa\".".into(),
                "manage_skill: bad role".into(),
            );
        }

        let dir = self.ctx.root.join(".koda").join("skills");
        let path = dir.join(format!("{name}.md"));
        // Role agents created before this tool was generalised used a
        // `<role>-agent.md` filename; keep updating that file if it is the one
        // that exists, so an update doesn't fork into a second copy.
        let legacy = (!role.is_empty()).then(|| dir.join(format!("{role}-agent.md")));
        let target = match &legacy {
            Some(l) if l.exists() && !path.exists() => l.clone(),
            _ => path.clone(),
        };

        if action == "delete" || action == "remove" {
            if !target.exists() {
                return err(
                    format!("no skill `{name}` to delete."),
                    "manage_skill: not found".into(),
                );
            }
            match std::fs::remove_file(&target) {
                Ok(()) => {
                    let n = self.reload_skills();
                    return tools::Outcome {
                        ok: true,
                        content: format!("Removed skill `{name}`. {n} skills now loaded."),
                        summary: format!("skill {name}: removed"),
                        view: tools::ToolView::Plain,
                    };
                }
                Err(e) => {
                    return err(
                        format!("could not remove `{name}`: {e}"),
                        "manage_skill: remove failed".into(),
                    )
                }
            }
        }

        let when = args
            .get("when")
            .and_then(|w| w.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        // `instructions` is the name the role-agent form used.
        let body = args
            .get("body")
            .or_else(|| args.get("instructions"))
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if when.is_empty() || body.is_empty() {
            return err(
                "a skill needs both `when` (one line: the situation it applies to) and \
                 `body` (the procedure itself)."
                    .into(),
                "manage_skill: missing fields".into(),
            );
        }
        // A skill exists to save the next session real work. Judge substance by
        // the artifact: a procedure needs steps, so it must have more than one
        // line and some detail; a role agent's brief is prose and can be short
        // while still being complete.
        let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
        let (min_len, min_lines) = if role.is_empty() { (120, 2) } else { (80, 1) };
        if body.len() < min_len || lines < min_lines {
            return err(
                format!(
                    "this is too thin to be a {what} ({} chars, {lines} line(s)). {need} For a \
                     single durable fact use `remember` instead.",
                    body.len(),
                    what = if role.is_empty() { "skill" } else { "role brief" },
                    need = if role.is_empty() {
                        "A skill is a procedure: the steps, the exact commands, and what to check."
                    } else {
                        "A role brief says how that agent works and what it must check."
                    }
                ),
                "manage_skill: too thin".into(),
            );
        }
        if when.split_whitespace().count() < 3 {
            return err(
                "`when` must describe the situation in a few words so the skill is found \
                 later, e.g. \"before a release, to verify the TUI end to end\"."
                    .into(),
                "manage_skill: vague when".into(),
            );
        }

        let existing = target.exists();
        let updating = matches!(action.as_str(), "update" | "revise");
        if existing && !updating {
            return err(
                format!(
                    "skill `{name}` already exists. Read it with the `skill` tool first, then \
                     call this again with action=\"update\" if it really needs revising."
                ),
                "manage_skill: already exists".into(),
            );
        }
        // Near-duplicate guard: a different name with the same trigger line just
        // splits the knowledge in two.
        if !existing {
            let same_when = self.skills.iter().find(|s| {
                s.name != name && s.when.trim().eq_ignore_ascii_case(when.trim())
            });
            if let Some(dup) = same_when {
                return err(
                    format!(
                        "skill `{}` already covers that exact situation. Update it instead of \
                         adding a near-duplicate.",
                        dup.name
                    ),
                    "manage_skill: duplicate".into(),
                );
            }
        }

        // Compose a valid skill file: frontmatter (name/role?/when) + body.
        let front_role = if role.is_empty() {
            String::new()
        } else {
            format!("role: {role}\n")
        };
        let doc = format!("---\nname: {name}\n{front_role}when: {when}\n---\n\n{body}\n");
        // Validate before writing so we never persist a skill that won't parse.
        if crate::skills::parse(&doc).is_none() {
            return err(
                "the composed skill did not parse; check the fields.".into(),
                "manage_skill: parse failed".into(),
            );
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return err(
                format!("could not create {}: {e}", dir.display()),
                "manage_skill: mkdir failed".into(),
            );
        }
        if let Err(e) = std::fs::write(&target, doc) {
            return err(
                format!("could not write {}: {e}", target.display()),
                "manage_skill: write failed".into(),
            );
        }
        let n = self.reload_skills();
        let verb = if existing { "Updated" } else { "Created" };
        let mut content = format!(
            "{verb} skill `{name}` at {}. It is loaded now and will be offered in future \
             sessions when the situation matches. {n} skills loaded.",
            target.display()
        );
        if !role.is_empty() {
            if self.cfg.subagents {
                let _ = write!(
                    content,
                    " Delegate to it with `delegate` (role=\"{role}\") or via /orc."
                );
            } else {
                let _ = write!(
                    content,
                    " It carries role `{role}`, but delegation is off (subagents = false), so \
                     nothing can delegate to it yet."
                );
            }
        }
        tools::Outcome {
            ok: true,
            content,
            summary: format!("skill {name}: {}", verb.to_lowercase()),
            view: tools::ToolView::Plain,
        }
    }

    fn read_skill(&self, args: &Value) -> tools::Outcome {
        let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
        if self.skills.is_empty() {
            return tools::Outcome {
                ok: false,
                content: "ERROR: no skills are installed. Proceed without one.".into(),
                summary: "skill: none installed".into(),
    view: tools::ToolView::Plain,
            };
        }
        match crate::skills::find(&self.skills, name) {
            Some(s) => tools::Outcome {
                ok: true,
                content: format!("Skill `{}` — follow this:\n\n{}", s.name, s.body.trim()),
                summary: format!("skill {}", s.name),
                view: tools::ToolView::Plain,
            },
            None => {
                let names: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                tools::Outcome {
                    ok: false,
                    content: format!(
                        "ERROR: no skill named `{name}`. Available: {}",
                        names.join(", ")
                    ),
                    summary: format!("skill {name}: not found"),
                    view: tools::ToolView::Plain,
                }
            }
        }
    }

    async fn web_fetch(&self, args: &Value) -> tools::Outcome {
        if !self.cfg.web_fetch {
            return tools::Outcome {
                ok: false,
                content: "ERROR: web fetch is off. The user can enable it in /settings."
                    .into(),
                summary: "web_fetch: disabled".into(),
                view: tools::ToolView::Plain,
            };
        }
        let url = args.get("url").and_then(|u| u.as_str()).unwrap_or("").trim();
        if url.is_empty() {
            return tools::Outcome {
                ok: false,
                content: "ERROR: `url` is required.".into(),
                summary: "web_fetch: no url".into(),
                view: tools::ToolView::Plain,
            };
        }
        let cap = args
            .get("max_bytes")
            .and_then(|b| b.as_u64())
            .map(|b| b as usize)
            .unwrap_or(self.cfg.max_tool_output_bytes)
            .min(self.cfg.max_tool_output_bytes);
        match crate::web::fetch_url(url, 20).await {
            Ok(text) => {
                let bytes = text.len();
                tools::Outcome {
                    ok: true,
                    content: tools::truncate(&text, cap),
                    summary: format!("fetched {url} ({bytes} bytes)"),
                    view: tools::ToolView::Plain,
                }
            }
            Err(e) => {
                crate::tel_warn!("web", "fetch failed", "detail" => format!("{e:#}"));
                tools::Outcome {
                    ok: false,
                    content: format!("ERROR: web fetch failed: {e:#}"),
                    summary: "web_fetch: failed".into(),
                    view: tools::ToolView::Plain,
                }
            }
        }
    }

    async fn web_search(&self, args: &Value) -> tools::Outcome {
        if !self.cfg.web_search {
            return tools::Outcome {
                ok: false,
                content: "ERROR: web search is off. The user can enable it with /websearch. \
                          Answer from the repository instead."
                    .into(),
                summary: "web_search: disabled".into(),
    view: tools::ToolView::Plain,
            };
        }
        let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("").trim();
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize)
            .unwrap_or(self.cfg.search_results)
            .clamp(1, 20);

        // Honour the explicit backend choice: SearXNG only when selected AND a
        // URL is set; otherwise DuckDuckGo (empty URL routes to DDG).
        let searx = if self.cfg.search_backend.eq_ignore_ascii_case("searxng") {
            self.cfg.searx_url.as_str()
        } else {
            ""
        };
        match crate::web::search_web(searx, query, limit).await {
            Ok(hits) => tools::Outcome {
                ok: true,
                content: crate::web::format_hits(query, &hits),
                summary: format!("search \"{query}\" ({} results)", hits.len()),
    view: tools::ToolView::Plain,
            },
            Err(e) => {
                crate::tel_warn!("web", "search failed", "detail" => format!("{e:#}"));
                tools::Outcome {
                    ok: false,
                    content: format!("ERROR: web search failed: {e:#}"),
                    summary: "web_search: failed".into(),
                    view: tools::ToolView::Plain,
                }
            }
        }
    }

    /// Run a delegated investigation in a fresh child agent and return its
    /// report. The child's tokens never touch this agent's context — that
    /// isolation is the whole point of delegating.
    async fn delegate(&mut self, args: &Value, tx: &mpsc::UnboundedSender<Event>) -> tools::Outcome {
        if !self.cfg.subagents {
            return tools::Outcome {
                ok: false,
                content: "ERROR: delegation is disabled (subagents = false). Do the work yourself."
                    .into(),
                summary: "delegate: disabled".into(),
    view: tools::ToolView::Plain,
            };
        }
        if self.depth >= self.cfg.max_subagent_depth {
            return tools::Outcome {
                ok: false,
                content: format!(
                    "ERROR: delegation depth limit ({}) reached. Investigate directly with \
                     search, find_files and read_file.",
                    self.cfg.max_subagent_depth
                ),
                summary: "delegate: too deep".into(),
                view: tools::ToolView::Plain,
            };
        }
        let task = args.get("task").and_then(|t| t.as_str()).unwrap_or("").trim();
        if task.is_empty() {
            return tools::Outcome {
                ok: false,
                content: "ERROR: `task` is required and must describe the investigation.".into(),
                summary: "delegate: no task".into(),
    view: tools::ToolView::Plain,
            };
        }
        let extra = args
            .get("context")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();
        let role = args.get("role").and_then(|r| r.as_str()).unwrap_or("").trim();

        crate::tel_info!(
            "subagent",
            "delegating",
            "task" => task.chars().take(80).collect::<String>(),
            "role" => role,
        );
        let mut child = self.child();
        let mut brief = String::new();
        // A role-agent runs with its skill body as operating instructions, so a
        // "qa" or "tester" subagent behaves differently from a plain one.
        if !role.is_empty() {
            match crate::skills::find_role(&self.skills, role) {
                Some(skill) => {
                    let _ = writeln!(
                        brief,
                        "You are acting as the **{}** agent. Operate by these role \
                         instructions:\n\n{}\n",
                        role, skill.body
                    );
                }
                None => {
                    let _ = writeln!(
                        brief,
                        "(No `{role}` role skill found; proceeding as a general subagent.)\n"
                    );
                }
            }
        }
        if !extra.is_empty() {
            let _ = writeln!(brief, "Starting context:\n{extra}\n");
        }
        let _ = write!(brief, "Task: {task}");

        // Subagents get a tighter step budget than the parent.
        let steps = self.cfg.subagent_max_steps.max(1);
        // Box breaks the async recursion cycle (execute → delegate → execute).
        let report = Box::pin(child.subagent_turn(brief, steps, tx)).await;

        match report {
            Some(text) if !text.trim().is_empty() => {
                let verified = self.review_report(&mut child, task, text, tx).await;
                let words = verified.split_whitespace().count();
                tools::Outcome {
                    ok: true,
                    content: format!("Subagent report:\n{}", verified.trim()),
                    summary: format!("delegate: report ({words} words)"),
                    view: tools::ToolView::Plain,
                }
            }
            _ => tools::Outcome {
                ok: false,
                content: "ERROR: the subagent produced no report. Investigate directly instead."
                    .into(),
                summary: "delegate: no report".into(),
    view: tools::ToolView::Plain,
            },
        }
    }

    /// In vibe mode, check a subagent's report against the files it cites before
    /// accepting it, and send it back if the citations do not hold.
    ///
    /// A subagent's claim is not evidence. The cheap, decisive check is whether
    /// the paths and lines it cites actually exist and contain what it says —
    /// which is verifiable without another model call.
    async fn review_report(
        &mut self,
        child: &mut Agent,
        task: &str,
        report: String,
        tx: &mpsc::UnboundedSender<Event>,
    ) -> String {
        if self.mode != Mode::Vibe || self.cfg.subagent_review_rounds == 0 {
            return report;
        }
        let mut report = report;
        for round in 0..self.cfg.subagent_review_rounds {
            let problems = self.check_citations(&report);
            if problems.is_empty() {
                crate::tel_debug!("subagent", "report verified", "round" => round + 1);
                return report;
            }
            crate::tel_warn!(
                "subagent",
                "report failed review",
                "round" => round + 1,
                "problems" => problems.len(),
            );
            let _ = tx.send(Event::Notice(format!(
                "subagent report cited {} path(s) that don't check out — asking again",
                problems.len()
            )));

            let complaint = format!(
                "Your report does not hold up. Problems:\n{}\n\nRe-check against the actual \
                 files, then rewrite the report. Cite only paths and lines you have read in \
                 this session. Original task: {task}",
                problems.join("\n")
            );
            let steps = self.cfg.subagent_max_steps.max(1);
            match Box::pin(child.subagent_turn(complaint, steps, tx)).await {
                Some(next) if !next.trim().is_empty() => report = next,
                _ => break,
            }
        }
        // Still shaky after the allowed rounds: pass it up with the caveat
        // rather than pretending it is clean.
        let problems = self.check_citations(&report);
        if problems.is_empty() {
            report
        } else {
            format!(
                "{report}\n\n[unverified: {}]",
                problems.join("; ")
            )
        }
    }

    /// Every `path:line` or bare path the report cites, checked against disk.
    fn check_citations(&self, report: &str) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen = HashSet::new();

        for token in report.split(|c: char| c.is_whitespace() || "()[],;\"'`".contains(c)) {
            let token = token.trim_end_matches(['.', ':']);
            if token.len() < 3 || !token.contains('.') || token.starts_with("http") {
                continue;
            }
            // path or path:line
            let (path, line) = match token.rsplit_once(':') {
                Some((p, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                    (p, n.parse::<usize>().ok())
                }
                _ => (token, None),
            };
            // Only treat it as a path if it really looks like one. Without the
            // stem check, prose abbreviations ("i.e.", "e.g.") get read as
            // filenames and every report fails review.
            let p = std::path::Path::new(path);
            let ext_ok = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| (1..=5).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
                .unwrap_or(false);
            let stem_ok = p
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.len() >= 2)
                .unwrap_or(false);
            let looks_like_file = ext_ok && (stem_ok || path.contains('/'));
            if !looks_like_file || !seen.insert(path.to_string()) {
                continue;
            }
            let full = self.ctx.root.join(path);
            if !full.is_file() {
                problems.push(format!("- `{path}` does not exist"));
                continue;
            }
            if let Some(n) = line {
                let count = std::fs::read_to_string(&full)
                    .map(|t| t.lines().count())
                    .unwrap_or(0);
                if n == 0 || n > count {
                    problems.push(format!(
                        "- `{path}` has {count} lines, so line {n} cannot be right"
                    ));
                }
            }
        }
        problems
    }

    /// Like `turn`, but bounded, silent, and returns the final text.
    async fn subagent_turn(
        &mut self,
        input: String,
        max_steps: usize,
        tx: &mpsc::UnboundedSender<Event>,
    ) -> Option<String> {
        self.history.push(Message::user(input));
        let mut last = String::new();

        for _ in 0..max_steps {
            if self.cancelled() {
                break;
            }
            self.trim();
            let result = match self.stream_step(tx).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("subagent: {e:#}")));
                    break;
                }
            };
            if result.cancelled {
                break;
            }
            if !result.text.trim().is_empty() {
                last = result.text.clone();
            }
            if result.calls.is_empty() {
                self.history.push(Message::assistant(result.text));
                break;
            }
            if self.text_mode {
                let mut raw = result.text.clone();
                for c in &result.calls {
                    raw.push_str(&format!(
                        "\n{TOOL_OPEN}\n{{\"name\": \"{}\", \"arguments\": {}}}\n{TOOL_CLOSE}",
                        c.function.name, c.function.arguments
                    ));
                }
                self.history.push(Message::assistant(raw));
            } else {
                let text = (!result.text.trim().is_empty()).then(|| result.text.clone());
                self.history
                    .push(Message::assistant_calls(text, result.calls.clone()));
            }
            for call in &result.calls {
                if self.cancelled() {
                    break;
                }
                let outcome = self.execute(call, tx).await;
                if self.text_mode {
                    self.history.push(Message::user(format!(
                        "Tool result ({}):\n{}",
                        call.function.name, outcome.content
                    )));
                } else {
                    self.history
                        .push(Message::tool(&call.id, &call.function.name, outcome.content));
                }
            }
        }
        (!last.trim().is_empty()).then_some(last)
    }

    async fn approve(
        &mut self,
        name: &str,
        args: &Value,
        tx: &mpsc::UnboundedSender<Event>,
    ) -> bool {
        if !tools::is_mutating(name)
            || self.auto_approve
            || self.auto_tier.auto_allows(name)
            || self.always.contains(name)
        {
            // Not gated: nothing was asked of the user.
            self.last_approval = Some(crate::trace::Approval::Auto);
            return true;
        }
        let (reply, rx) = oneshot::channel();
        let _ = tx.send(Event::ToolPending {
            name: name.to_string(),
            args_pretty: serde_json::to_string_pretty(args).unwrap_or_default(),
            preview: tools::preview(name, args, &self.ctx),
            reply,
        });
        match rx.await {
            Ok(Approval::Once) => {
                self.last_approval = Some(crate::trace::Approval::Approved);
                true
            }
            Ok(Approval::AlwaysThisTool) => {
                self.always.insert(name.to_string());
                self.last_approval = Some(crate::trace::Approval::Approved);
                true
            }
            _ => {
                self.last_approval = Some(crate::trace::Approval::Denied);
                false
            }
        }
    }

    /// Summarize automatically once the context is nearly full, so a long
    /// session degrades into a summary instead of a hard failure.
    async fn auto_compact(&mut self, tx: &mpsc::UnboundedSender<Event>) {
        let frac = self.cfg.auto_compact_at;
        if frac <= 0.0 || self.depth > 0 || self.history.len() < 4 {
            return;
        }
        let limit = (self.cfg.context_tokens as f64 * frac) as usize;
        if self.history_tokens() < limit {
            return;
        }
        crate::tel_info!("agent", "auto-compacting", "tokens" => self.history_tokens());
        let _ = tx.send(Event::Notice("context nearly full — compacting".into()));
        self.compact(tx).await;
    }

    /// Replace the history with a model-written summary.
    async fn compact(&mut self, tx: &mpsc::UnboundedSender<Event>) {
        if self.history.is_empty() {
            let _ = tx.send(Event::Notice("nothing to compact".into()));
            return;
        }
        let before = self.history_tokens();
        // Compaction is where context is lost, so it belongs in the trace. A
        // manual /compact has no turn open, so it gets one of its own.
        let own_turn = self.trace_turn.is_none();
        if own_turn {
            self.trace_turn = crate::trace::begin_turn(
                &self.mode.to_string(),
                &self.model,
                &self.endpoint,
                "/compact",
            );
        }
        let trace_step = crate::trace::open_step(
            self.trace_turn,
            crate::trace::StepKind::Compaction,
            "compaction",
        );
        // Whatever happens below, the step and (if we made one) the turn close.
        macro_rules! close_trace {
            ($after:expr) => {{
                crate::trace::finish_compaction(trace_step, before, $after);
                if own_turn {
                    crate::trace::end_turn(
                        self.trace_turn,
                        crate::trace::Status::Ok,
                        "",
                        $after,
                    );
                    self.trace_turn = None;
                }
            }};
        }
        // Tell the UI we've started so it can animate and hold input; the
        // matching Compacted event below is emitted on *every* exit path so the
        // prompt can never get stuck "compacting".
        let _ = tx.send(Event::Compacting);
        // A manual /compact should start from a clean cancel slate, and be
        // interruptible with esc like any other long call.
        self.cancel.store(false, Ordering::Relaxed);

        let mut messages = vec![Message::system(
            "You are compacting a coding session's history because the context is nearly full. \
             Write a dense hand-off note to YOUR FUTURE SELF so you can continue without \
             re-reading everything. This is the ONLY memory you will keep, so losing a detail \
             means forgetting it. Cover, with headings:\n\
             - TASK: the user's original goal, in their words if possible.\n\
             - DONE: what has already been accomplished (files created/edited, commands run, \
             decisions made) — be specific with paths and names.\n\
             - CURRENT STEP: exactly what you were doing right now and what you were about to \
             do next. This is the most important part — do not lose it.\n\
             - PLAN: any remaining steps / open problems, in order.\n\
             - FACTS: build/test commands, conventions, and anything you had to discover.\n\
             Be factual and specific. No fluff. Under 500 words.",
        )];
        messages.extend(self.history.iter().cloned());
        messages.push(Message::user(
            "Write the hand-off note now, so you can seamlessly continue the task.",
        ));

        let req = ChatRequest {
            model: self.model.clone(),
            messages,
            temperature: 0.0,
            top_p: self.cfg.top_p,
            max_tokens: 0,
            tools: None,
            reasoning_effort: "off".into(),
        };
        let (stx, mut srx) = mpsc::unbounded_channel();
        let res = self
            .client
            .stream_with_retry(&req, &stx, self.cfg.max_retries)
            .await;
        drop(stx);
        let mut summary = String::new();
        while let Some(ev) = srx.recv().await {
            if let StreamEvent::Text(t) = ev {
                summary.push_str(&t);
            }
        }
        if self.cancelled() {
            self.cancel.store(false, Ordering::Relaxed);
            let _ = tx.send(Event::Notice("compaction cancelled".into()));
            let _ = tx.send(Event::Compacted { before, after: before });
            close_trace!(before);
            return;
        }
        match res {
            Ok(_) if !summary.trim().is_empty() => {
                // Keep the recent tail of the conversation after the summary, so
                // the agent retains its immediate working context (the last user
                // request and the latest tool results) rather than resetting to a
                // bare summary. Bounded by ~1/5 of the budget so we still free
                // most of the space. Start the tail at a user turn to keep the
                // request/response structure coherent.
                let tail_budget = (self.cfg.context_tokens / 5).max(1024);
                let mut tail: Vec<Message> = Vec::new();
                let mut used = 0usize;
                for m in self.history.iter().rev() {
                    let cost = m.approx_tokens();
                    if used + cost > tail_budget && !tail.is_empty() {
                        break;
                    }
                    used += cost;
                    tail.push(m.clone());
                }
                tail.reverse();
                // Trim leading tool/assistant messages so the tail opens on a
                // user turn (a dangling tool result with no matching call
                // confuses some servers).
                while matches!(tail.first().map(|m| m.role), Some(Role::Tool) | Some(Role::Assistant)) {
                    tail.remove(0);
                }

                let mut new_history = vec![
                    Message::user(format!(
                        "[Context was compacted here to free space. Hand-off note from \
                         earlier in this session:]\n\n{}",
                        summary.trim()
                    )),
                    Message::assistant(
                        "Understood — I have the hand-off note and will continue the task \
                         from where I left off.",
                    ),
                ];
                new_history.extend(tail);
                self.history = new_history;
                // History was replaced, not extended, so the file must be too.
                if let Some(s) = self.session.as_mut() {
                    s.rewrite(&self.history);
                }
                let after = self.history_tokens();
                let _ = tx.send(Event::Notice(format!("compacted {before} → {after} tokens")));
                let _ = tx.send(Event::Compacted { before, after });
                close_trace!(after);
            }
            Ok(_) => {
                let _ = tx.send(Event::Error("compaction produced no summary".into()));
                let _ = tx.send(Event::Compacted { before, after: before });
                close_trace!(before);
            }
            Err(e) => {
                let _ = tx.send(Event::Error(format!("compaction failed: {e:#}")));
                let _ = tx.send(Event::Compacted { before, after: before });
                close_trace!(before);
            }
        }
    }

    /// Drop the oldest messages until the history fits the configured budget,
    /// keeping tool results attached to their calls.
    fn trim(&mut self) {
        let reserve = self.system.len() / 4 + 1024;
        let budget = self.cfg.context_tokens.saturating_sub(reserve).max(1024);
        loop {
            let total: usize = self.history.iter().map(|m| m.approx_tokens()).sum();
            if total <= budget || self.history.len() <= 3 {
                break;
            }
            let dropped = self.history.remove(0);
            if dropped.tool_calls.is_some() {
                while matches!(self.history.first().map(|m| m.role), Some(Role::Tool)) {
                    self.history.remove(0);
                }
            }
            while matches!(self.history.first().map(|m| m.role), Some(Role::Tool)) {
                self.history.remove(0);
            }
        }
    }
}

/// Extract top-level balanced `{...}` substrings from arbitrary text, ignoring
/// braces that appear inside JSON strings. Used as a lenient last resort to
/// find a tool call a model buried in prose without code fences.
fn balanced_json_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut depth = 0i32;
            let mut in_str = false;
            let mut escaped = false;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if in_str {
                    if escaped {
                        escaped = false;
                    } else if c == b'\\' {
                        escaped = true;
                    } else if c == b'"' {
                        in_str = false;
                    }
                } else if c == b'"' {
                    in_str = true;
                } else if c == b'{' {
                    depth += 1;
                } else if c == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        if text.is_char_boundary(i) && text.is_char_boundary(j + 1) {
                            out.push(&text[i..=j]);
                        }
                        break;
                    }
                }
                j += 1;
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// A short reminder of what a tool expects, for when a model sends unparseable/// arguments. Names the required parameters (falling back to all declared ones)
/// so a small model can re-issue the call without guessing the schema.
/// A bare identifier (including a qualified `Type::method`) is almost always a
/// symbol lookup rather than a free-text search. Regexes, phrases and literals
/// stay on the normal search path.
fn looks_like_symbol_search(tool: &str, args: &Value) -> bool {
    if tool != "search" {
        return false;
    }
    let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()).map(str::trim) else {
        return false;
    };
    !pattern.is_empty()
        && pattern.len() <= 120
        && pattern.chars().any(|c| c.is_ascii_alphabetic())
        && pattern
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':'))
}

fn required_params_hint(name: &str) -> String {
    let Some(spec) = tools::spec(name) else {
        return format!("`{name}` is not a known tool.");
    };
    let required: Vec<String> = spec
        .params
        .pointer("/required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let params = if required.is_empty() {
        // No explicit `required`: list the declared property names instead.
        spec.params
            .pointer("/properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        required
    };
    if params.is_empty() {
        format!("`{name}` takes a JSON object of arguments.")
    } else {
        format!("`{name}` needs these parameters: {}.", params.join(", "))
    }
}

/// One plain sentence for the screen. `ApiError` already carries a written
/// message; anything else gets its chain flattened rather than debug-printed.
pub fn user_message(e: &anyhow::Error) -> String {
    if let Some(api) = e.downcast_ref::<crate::llm::ApiError>() {
        return api.user.clone();
    }
    let text = e
        .chain()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(": ");
    let clipped: String = text.replace('\n', " ").chars().take(200).collect();
    if clipped.is_empty() {
        "something went wrong — see /logs".into()
    } else {
        clipped
    }
}

fn looks_like_tool_rejection(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    (m.contains("tool") || m.contains("function"))
        && (m.contains("not support")
            || m.contains("unsupported")
            || m.contains("unknown field")
            || m.contains("invalid")
            || m.contains("400"))
}

/// Short one-line description of a tool call for the transcript.
pub fn label_for(name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "read_file" => format!("read {}", s("path")),
        "write_file" => format!("write {}", s("path")),
        "edit_file" => format!("edit {}", s("path")),
        "list_dir" => format!("list {}", if s("path").is_empty() { "." } else { s("path") }),
        "find_files" => format!("find {}", s("glob")),
        "search" => format!("search /{}/", s("pattern")),
        "run_command" => format!("$ {}", s("command")),
        "codegraph" => {
            let q = if s("query").is_empty() { "overview" } else { s("query") };
            match q {
                "symbol" => format!("codegraph symbol {}", s("name")),
                "file" => format!("codegraph file {}", s("path")),
                _ => "codegraph overview".to_string(),
            }
        }
        "skill" => format!("skill {}", s("name")),
        "web_search" => format!("search \"{}\"", s("query")),
        "delegate" => {
            let task: String = s("task").chars().take(60).collect();
            format!("delegate: {task}")
        }
        "todo" => "plan".to_string(),
        "remember" => match args.get("forget").and_then(|f| f.as_str()) {
            Some(f) => format!("forget {f}"),
            None => format!("remember: {}", s("note").chars().take(50).collect::<String>()),
        },
        other => other.to_string(),
    }
}

/// Fold one stream event into the accumulator. A subagent runs `quiet`: its
/// tokens build its own context but never reach the user's transcript.
fn absorb(ev: StreamEvent, acc: &mut StepAcc, tx: &mpsc::UnboundedSender<Event>, quiet: bool) {
    match ev {
        StreamEvent::Text(chunk) => {
            let (display, blocks) = acc.scan.push(&chunk);
            if !display.is_empty() {
                acc.text.push_str(&display);
                if !quiet {
                    let _ = tx.send(Event::Text(display));
                } else {
                    // A subagent's prose is kept out of the transcript, but a
                    // status beat tells the user it is actively working.
                    let _ = tx.send(Event::SubActivity("working".into()));
                }
            }
            acc.text_calls.extend(blocks);
        }
        StreamEvent::Reasoning(chunk) => {
            acc.reasoning_len += chunk.len();
            // Kept for the trace. Bounded here as well as at the trace cap so a
            // very long thinking stream can't balloon this accumulator.
            if acc.reasoning.len() < 64 * 1024 {
                acc.reasoning.push_str(&chunk);
            }
            if !quiet {
                let _ = tx.send(Event::Reasoning(chunk));
            } else {
                let _ = tx.send(Event::SubActivity("thinking".into()));
            }
        }
        StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            args,
        } => {
            let slot = acc.partials.entry(index).or_default();
            if let Some(id) = id {
                slot.0 = Some(id);
            }
            if let Some(n) = name {
                slot.1 = n;
            }
            slot.2.push_str(&args);
        }
        StreamEvent::Finish(reason) => {
            acc.finish_reason = reason.clone();
            if reason.as_deref() == Some("length") && !quiet {
                let _ = tx.send(Event::Notice(
                    "the model hit its output limit; raise max_tokens if replies look cut off"
                        .into(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_hides_tool_blocks() {
        let mut s = TextScan::default();
        let (d1, b1) = s.push("Reading the file.\n<tool_");
        assert_eq!(d1, "Reading the file.\n");
        assert!(b1.is_empty());
        let (d2, b2) = s.push("call>\n{\"name\":\"read_file\"}\n</tool_call>");
        assert_eq!(d2, "");
        assert_eq!(b2.len(), 1);
        assert!(b2[0].contains("read_file"));
    }

    #[test]
    fn scanner_passes_plain_text() {
        let mut s = TextScan::default();
        let (d, b) = s.push("hello world");
        assert_eq!(d, "hello world");
        assert!(b.is_empty());
        assert_eq!(s.finish(), "");
    }

    /// The citation check is what makes vibe-mode review possible without a
    /// second model call, so it needs to be exact about what counts as a claim.
    #[test]
    fn citation_check_catches_invented_paths_and_lines() {
        let dir = std::env::temp_dir().join("koda-cite-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("real.rs"), "one\ntwo\nthree\n").unwrap();

        let cfg = Arc::new(crate::config::Config::default());
        let agent = Agent::new(
            cfg,
            dir.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
        )
        .unwrap();

        // A real file at a real line passes.
        assert!(agent.check_citations("see real.rs:2 for the fix").is_empty());
        // A real file at an impossible line is caught.
        let bad_line = agent.check_citations("see real.rs:99");
        assert_eq!(bad_line.len(), 1, "{bad_line:?}");
        assert!(bad_line[0].contains("cannot be right"));
        // An invented file is caught.
        let missing = agent.check_citations("defined in imaginary.rs:1");
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("does not exist"));
        // Prose and URLs are not treated as citations.
        assert!(agent
            .check_citations("see https://example.com/a.rs and note that i.e. it works")
            .is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_suffix_detects_tag_start() {
        assert_eq!(partial_suffix("abc<tool", TOOL_OPEN), 5);
        assert_eq!(partial_suffix("abc", TOOL_OPEN), 0);
    }

    /// Streamed tags arrive split across arbitrary chunk boundaries.
    #[test]
    fn scanner_survives_tiny_chunks() {
        let full = "Reading it.\n<tool_call>\n{\"name\": \"read_file\", \
                    \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>";
        for size in [1usize, 2, 3, 5, 7, 11] {
            let mut s = TextScan::default();
            let mut display = String::new();
            let mut blocks = Vec::new();
            let chars: Vec<char> = full.chars().collect();
            for chunk in chars.chunks(size) {
                let piece: String = chunk.iter().collect();
                let (d, b) = s.push(&piece);
                display.push_str(&d);
                blocks.extend(b);
            }
            display.push_str(&s.finish());
            assert_eq!(display.trim(), "Reading it.", "chunk size {size}");
            assert_eq!(blocks.len(), 1, "chunk size {size}");
            assert!(blocks[0].contains("read_file"), "chunk size {size}");
        }
    }

    /// Undo reverts a whole turn's edits at once, and only the latest turn.
    #[test]
    fn undo_reverts_a_whole_turn_not_one_file() {        let dir = std::env::temp_dir().join("koda-undo-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        // Pre-existing files with original content.
        std::fs::write(dir.join("a.txt"), "A0").unwrap();
        std::fs::write(dir.join("b.txt"), "B0").unwrap();

        let cfg = Arc::new(crate::config::Config::default());
        let mut agent = Agent::new(
            cfg,
            dir.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
        )
        .unwrap();

        // --- Turn 1: edit a.txt (twice) and b.txt ---
        agent.turn_seq = 1;
        agent.snapshot(&serde_json::json!({"path": "a.txt"}), "edit_file"); // captures "A0"
        std::fs::write(dir.join("a.txt"), "A1").unwrap();
        agent.snapshot(&serde_json::json!({"path": "a.txt"}), "edit_file"); // captures "A1"
        std::fs::write(dir.join("a.txt"), "A2").unwrap();
        agent.snapshot(&serde_json::json!({"path": "b.txt"}), "edit_file"); // captures "B0"
        std::fs::write(dir.join("b.txt"), "B1").unwrap();

        // --- Turn 2: create c.txt ---
        agent.turn_seq = 2;
        agent.snapshot(&serde_json::json!({"path": "c.txt"}), "write_file"); // before = None
        std::fs::write(dir.join("c.txt"), "C1").unwrap();

        // Undo turn 2: c.txt (created this turn) is removed; a/b untouched.
        let msg = agent.undo_last();
        assert!(msg.contains("undid last turn"), "{msg}");
        assert!(!dir.join("c.txt").exists(), "created file should be removed");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "A2");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "B1");

        // Undo turn 1: both files revert to their PRE-TURN state (A0, B0),
        // even though a.txt was edited twice.
        let msg = agent.undo_last();
        assert!(msg.contains("undid last turn"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "A0",
            "a.txt must revert to its earliest pre-turn state, not the last edit"
        );
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "B0");

        // Nothing left to undo.
        assert!(agent.undo_last().contains("nothing to undo"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fresh agent on a workspace of its own. The counter matters: every test
    /// in a binary shares one process id, so a pid-tagged path is a constant,
    /// and the tests that write into `.koda/` were clearing each other's files
    /// mid-run when cargo scheduled them in parallel.
    fn test_agent() -> Agent {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("koda-agent-test-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).ok();
        let cfg = Arc::new(crate::config::Config::default());
        Agent::new(
            cfg,
            dir,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
        )
        .unwrap()
    }

    /// Balanced-object extraction ignores braces inside strings and returns
    /// each top-level object, so a tool call buried in prose can be recovered.
    #[test]
    fn balanced_json_objects_ignores_string_braces() {
        let objs = balanced_json_objects(r#"before {"a": "has } brace", "b": {"c": 1}} after"#);
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0], r#"{"a": "has } brace", "b": {"c": 1}}"#);

        let two = balanced_json_objects("x {\"one\":1} y {\"two\":2} z");
        assert_eq!(two, vec!["{\"one\":1}", "{\"two\":2}"]);

        assert!(balanced_json_objects("no objects here").is_empty());
    }

    /// A model that drops a bare tool-call object into prose (no code fences)
    /// should still be understood, but sample JSON that names no real tool must
    /// not be hijacked.
    #[test]
    fn parse_fenced_call_recovers_prose_wrapped_call() {
        let mut agent = test_agent();
        let text = "Sure, I'll read it. {\"name\": \"read_file\", \
                    \"arguments\": {\"path\": \"a.rs\"}} and that's the plan.";
        let call = agent.parse_fenced_call(text).expect("should recover call");
        assert_eq!(call.function.name, "read_file");
        assert_eq!(call.args().get("path").and_then(|p| p.as_str()), Some("a.rs"));

        // JSON that isn't a known tool must be ignored.
        assert!(agent
            .parse_fenced_call("example: {\"name\": \"not_a_tool\", \"arguments\": {}}")
            .is_none());
    }

    /// A ```json fenced tool call is still recognised.
    #[test]
    fn parse_fenced_call_handles_json_fence() {
        let mut agent = test_agent();
        let text = "Here you go:\n```json\n{\"name\": \"list_dir\", \"arguments\": {\"path\": \".\"}}\n```";
        let call = agent.parse_fenced_call(text).expect("should recover fenced call");
        assert_eq!(call.function.name, "list_dir");
    }

    /// The malformed-argument hint names the tool and its required parameters
    /// so a small model can re-issue the call instead of hitting a hard error.
    #[test]
    fn bare_identifier_search_is_recognised_as_symbol_lookup() {
        assert!(looks_like_symbol_search("search", &serde_json::json!({"pattern": "Agent::execute"})));
        assert!(looks_like_symbol_search("search", &serde_json::json!({"pattern": "compact"})));
        assert!(!looks_like_symbol_search("search", &serde_json::json!({"pattern": "error: connection reset"})));
        assert!(!looks_like_symbol_search("search", &serde_json::json!({"pattern": "foo.*bar"})));
        assert!(!looks_like_symbol_search("read_file", &serde_json::json!({"pattern": "compact"})));
    }

    /// A skill is how the agent keeps a procedure it worked out. The tool has to
    /// accept a real one, refuse the things that would turn `.koda/skills` into
    /// noise, and make a role agent just a skill that carries a role.
    #[test]
    fn the_agent_can_author_a_skill_and_refuses_junk() {
        use serde_json::json;
        let mut a = test_agent();
        let dir = a.ctx.root.join(".koda").join("skills");
        std::fs::remove_dir_all(&dir).ok();

        let body = "1. Start the mock server: python3 tests/mock_server.py 8123\n\
                    2. Run: BIN=./target/release/koda ./tests/e2e.sh\n\
                    3. Every check must print ok; a FAIL blocks the release.\n";
        let when = "before a release, to run the end-to-end suite";

        // A real procedure is written, parses, and is loaded immediately.
        let out = a.manage_skill(&json!({
            "name": "run-e2e-suite", "when": when, "body": body
        }));
        assert!(out.ok, "{}", out.content);
        let path = dir.join("run-e2e-suite.md");
        assert!(path.exists(), "skill file missing: {}", path.display());
        assert!(
            a.skills.iter().any(|s| s.name == "run-e2e-suite"),
            "skill should be loaded without a restart"
        );
        // No role means it is knowledge, not a delegatable agent.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("role:"), "{saved}");

        // Creating the same name again is refused: update it deliberately.
        let dup = a.manage_skill(&json!({ "name": "run-e2e-suite", "when": when, "body": body }));
        assert!(!dup.ok && dup.content.contains("already exists"), "{}", dup.content);
        let upd = a.manage_skill(&json!({
            "name": "run-e2e-suite", "action": "update", "when": when,
            "body": format!("{body}4. Also run tests/tui_test.py for the TUI.\n")
        }));
        assert!(upd.ok, "{}", upd.content);
        assert!(std::fs::read_to_string(&path).unwrap().contains("tui_test.py"));

        // A different name for the same situation splits the knowledge in two.
        let near = a.manage_skill(&json!({ "name": "e2e-again", "when": when, "body": body }));
        assert!(!near.ok && near.content.contains("already covers"), "{}", near.content);

        // A one-liner is a fact, not a procedure.
        let thin = a.manage_skill(&json!({
            "name": "too-thin", "when": "when running tests", "body": "run cargo test"
        }));
        assert!(!thin.ok && thin.content.contains("too thin"), "{}", thin.content);
        // A trigger nobody can match later is refused too.
        let vague = a.manage_skill(&json!({
            "name": "vague-one", "when": "tests", "body": body
        }));
        assert!(!vague.ok && vague.content.contains("`when`"), "{}", vague.content);
        // And a name that isn't a slug can't become a filename.
        let bad = a.manage_skill(&json!({
            "name": "../escape", "when": when, "body": body
        }));
        assert!(!bad.ok && bad.content.contains("slug"), "{}", bad.content);
        assert!(!dir.join("../escape.md").exists());

        // A role turns the same mechanism into a delegatable agent.
        let agent = a.manage_skill(&json!({
            "name": "qa-agent", "role": "qa",
            "when": "when the task needs tests written and run",
            "body": "Write the failing test first, then make it pass, then run the suite \
                     and report exactly which checks changed state.\n"
        }));
        assert!(agent.ok, "{}", agent.content);
        assert!(std::fs::read_to_string(dir.join("qa-agent.md")).unwrap().contains("role: qa"));
        assert!(
            crate::skills::find_role(&a.skills, "qa").is_some(),
            "a role skill must be delegatable"
        );

        // Deleting removes the file and unloads it.
        let del = a.manage_skill(&json!({ "name": "run-e2e-suite", "action": "delete" }));
        assert!(del.ok, "{}", del.content);
        assert!(!path.exists());
        assert!(!a.skills.iter().any(|s| s.name == "run-e2e-suite"));
        let missing = a.manage_skill(&json!({ "name": "run-e2e-suite", "action": "delete" }));
        assert!(!missing.ok && missing.content.contains("no skill"), "{}", missing.content);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The old role-agent call shape (`role` + `instructions`, no `name`) has to
    /// keep working: a model may have learned it.
    #[test]
    fn the_legacy_role_agent_shape_still_works() {
        use serde_json::json;
        let mut a = test_agent();
        let dir = a.ctx.root.join(".koda").join("skills");
        std::fs::remove_dir_all(&dir).ok();
        let out = a.manage_skill(&json!({
            "role": "reviewer",
            "when": "when a change needs reviewing before it ships",
            "instructions": "Read the diff, check edge cases and tests, and list concrete \
                             problems with file:line references rather than general advice.\n"
        }));
        assert!(out.ok, "{}", out.content);
        assert!(dir.join("reviewer-agent.md").exists());
        assert!(crate::skills::find_role(&a.skills, "reviewer").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Authoring a skill is a file write, so it must be gated like one, and a
    /// subagent must never do it.
    #[test]
    fn skill_authoring_is_approval_gated_and_top_level_only() {
        assert!(
            tools::is_mutating("manage_skill"),
            "writing a skill must go through approval"
        );
        let a = test_agent();
        let names = |list: Vec<serde_json::Value>| -> Vec<String> {
            list.iter()
                .filter_map(|t| t.pointer("/function/name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        };
        // Advertised to the top-level agent...
        assert!(names(a.advertised_tools()).contains(&"manage_skill".to_string()));
        // ...and never to a subagent.
        let child = a.child();
        assert!(!names(child.advertised_tools()).contains(&"manage_skill".to_string()));
        // Plan mode is read-only, so the mutating tool is not offered there.
        let mut planning = test_agent();
        planning.set_mode(crate::config::Mode::Plan);
        assert!(!names(planning.advertised_tools()).contains(&"manage_skill".to_string()));
    }

    #[test]
    fn codegraph_is_the_first_advertised_tool_when_enabled() {
        let agent = test_agent();
        assert!(agent.cfg.codegraph);
        let advertised = agent.advertised_tools();
        let names: Vec<&str> = advertised
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(names.first().copied(), Some("codegraph"), "{names:?}");
    }

    #[test]
    fn required_params_hint_names_required_params() {
        let hint = required_params_hint("read_file");
        assert!(hint.contains("read_file"), "{hint}");
        assert!(hint.contains("path"), "{hint}");

        // Unknown tools are reported plainly rather than panicking.
        assert!(required_params_hint("bogus_tool").contains("not a known tool"));
    }
}
