//! Terminal UI.
//!
//! Layout, outside in: a one-line header carrying persistent context, the
//! transcript (with a scrollbar only when it can actually scroll), a rule, the
//! input, and a single bottom line that merges state and contextual key hints.
//! There is exactly one border depth anywhere on screen — the terminal edge
//! already frames the app, so nothing else is boxed except a modal.

use crate::anim;
use crate::agent::{Agent, Approval, Command, Event};
use crate::config::{AutoTier, Config, Mode};
use crate::fuzzy::FileIndex;
use crate::log;
use crate::session::{self, Summary};
use crate::settings;
use crate::setup::{self, Setup};
use crate::editor::Editor;
use crate::md;
use crate::panel::{self, Panel};
use crate::theme::{self, Glyphs, Theme};
use crate::view::Transcript;

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture, MouseEventKind};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;
use tokio::sync::{mpsc, oneshot, Notify};

/// A spinner that flashes for instant work is noise, so hold it back briefly.
const SPINNER_DELAY: Duration = Duration::from_millis(200);
/// How long the welcome banner's entrance shimmer plays before it settles into
/// the static gradient. Kept short so it never delays getting to work.
const WELCOME_ANIM: Duration = Duration::from_millis(1400);

/// The KODA banner art, shared by the static welcome and its entrance shimmer.
const BANNER_ART: [&str; 6] = [
    "█  ██   ██████   ██████    █████  ",
    "█ ██    ██   ██  ██   ██  ██   ██ ",
    "███     ██   ██  ██   ██  ███████ ",
    "█ ██    ██   ██  ██   ██  ██   ██ ",
    "█  ██   ██████   ██████   ██   ██ ",
    "                                  ",
];

const COMMANDS: &[(&str, &str)] = &[
    ("/help", "keys and commands"),
    ("/keys", "keyboard shortcuts"),
    ("/model", "show or switch model"),
    ("/models", "list models on the server"),
    ("/mode", "plan, execute or vibe"),
    ("/logs", "what the agent has been doing"),
    ("/websearch", "turn web search on or off"),
    ("/skills", "list skills, or reload them from disk"),
    ("/orc", "orchestrate: split a task across role agents"),
    ("/setup", "set the endpoint, model and API key"),
    ("/settings", "interactive settings page"),
    ("/resume", "reopen an earlier conversation"),
    ("/search", "search saved conversations by text"),
    ("/fork", "branch this conversation into a copy"),
    ("/undo", "put back the last file the agent changed"),
    ("/theme", "switch palette"),
    ("/url", "change the API base URL"),
    ("/clear", "drop the conversation context"),
    ("/compact", "summarize context to free tokens"),
    ("/auto", "toggle auto-approve for writes"),
    ("/tools", "list available tools"),
    ("/think", "show or hide model reasoning"),
    ("/motion", "turn animation on or off"),
    ("/mouse", "toggle mouse capture (off = select & copy text)"),
    ("/reveal", "toggle progressive text reveal"),
    ("/copy", "copy last reply to the clipboard"),
    ("/cwd", "show the workspace root"),
    ("/quit", "exit koda"),
];

struct Pending {
    name: String,
    args_pretty: String,
    preview: Option<String>,
    reply: Option<oneshot::Sender<Approval>>,
    scroll: u16,
}

/// One-line feature hints, surfaced on the welcome and rotated in the bottom
/// bar so a user discovers what koda can do without reading the whole /help.
const TIPS: &[&str] = &[
    "type / to see every command · ↑↓ to pick",
    "@file mentions a file · @image.png attaches an image to a vision model",
    "/auto cycles ask → auto-write → full-auto autonomous mode",
    "/orc <task> splits work across role agents (dev, qa, tester…)",
    "/mouse off lets you select and copy text with the mouse",
    "/settings opens an interactive control page for everything",
    "/search <text> finds past conversations · /fork branches one",
    "ctrl+p cycles plan → execute → vibe mode",
    "paste a big block and it becomes @paste1, expanded when you send",
    "/theme neon · tokyo-night · dracula … switch palette live",
    "the agent can ask you a question mid-task — just answer it",
    "/undo reverts the whole last turn's file changes",
];

/// A tip chosen from wall-clock time, so it rotates without any state.
fn random_tip() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as usize;
    TIPS[n % TIPS.len()]
}

/// RGB components of a truecolor, for gradient maths. `None` for ANSI/named
/// colours, where a gradient is not meaningful and callers fall back to flat.
fn as_rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Width bands. Every layout decision reads these instead of raw numbers.
#[derive(Clone, Copy)]
struct Metrics {
    compact: bool,
    tiny: bool,
}

impl Metrics {
    fn of(width: u16) -> Self {
        Self {
            compact: width < 92,
            tiny: width < 64,
        }
    }
}

pub struct App {
    cmd_tx: mpsc::UnboundedSender<Command>,
    transcript: Transcript,
    editor: Editor,
    theme: Theme,
    glyphs: Glyphs,
    scroll: usize,
    follow: bool,
    busy: bool,
    /// True from the moment the user interrupts until the turn actually ends,
    /// so the status row can say "cancelling…" instead of pretending the work
    /// stopped instantly (a tool call in flight still has to unwind).
    cancelling: bool,
    /// How much motion the environment and config allow.
    motion: anim::Motion,
    /// User preference for the streaming text reveal specifically. Gated by
    /// `motion` — reveal only animates when both this and motion are on.
    reveal_pref: bool,
    turn_started: Option<Instant>,
    pending: Option<Pending>,
    /// A question the agent asked via `ask_user`, awaiting the user's next
    /// message. `(question, reply-channel)`.
    asking: Option<(String, oneshot::Sender<String>)>,
    model: String,
    endpoint: String,
    tokens: usize,
    context_budget: usize,
    auto_tier: AutoTier,
    web: bool,
    searx_configured: bool,
    mode: Mode,
    /// Set when plan mode blocked a change, so the hint bar can offer the switch.
    plan_blocked: bool,
    /// Log overlay: None when closed, else the scroll offset.
    logs: Option<u16>,
    /// Log version at the last draw, so a new entry triggers a repaint while
    /// the overlay is open.
    log_version: u64,
    /// When the welcome banner was shown, for its brief entrance shimmer. None
    /// once the animation has finished (or when motion is off).
    welcome_at: Option<Instant>,
    /// Emit DEC 2026 markers around each frame.
    sync_output: bool,
    /// Whether the mouse is currently captured (wheel scroll vs native select).
    mouse_capture: bool,
    /// Last size we drew at, to drop the duplicate resize events emulators send.
    last_size: (u16, u16),
    /// Set after a destructive key so a second press confirms.
    confirm: Option<(&'static str, Instant)>,
    /// Project files for `@` completion, scanned once off-thread.
    files: FileIndex,
    /// Which row of the `@` completion list is selected.
    mention_sel: usize,
    /// Which row of the slash-command completion list is selected.
    cmd_sel: usize,
    /// Whether the file index had finished at the last frame.
    files_ready: bool,
    /// Session picker: the list, and which row is selected.
    picker: Option<(Vec<Summary>, usize)>,
    /// Provider setup overlay.
    setup: Option<Setup>,
    /// Interactive settings overlay.
    settings: Option<settings::Settings>,
    /// Working copy of the config, edited by the setup screen.
    cfg: Config,
    root: PathBuf,
    branch: Option<String>,
    queued: VecDeque<String>,
    /// Large pasted blocks, shown in the composer as @paste1, @paste2… and
    /// expanded back to full text on submit. Keeps the input readable.
    pastes: Vec<String>,
    quit: bool,
    last_ctrl_c: Option<Instant>,
    cancel: Arc<AtomicBool>,
    notify: Arc<Notify>,
    body_h: usize,
}

impl App {
    fn send(&mut self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
    }

    fn note(&mut self, msg: impl Into<String>) {
        self.transcript.notice(msg.into());
        self.follow = true;
    }

    fn set_theme(&mut self, t: Theme) {
        self.theme = t;
        self.transcript.theme = t;
        self.transcript.invalidate();
    }

    // ------------------------------------------------------------ agent events

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::TurnStart => {
                self.busy = true;
                self.cancelling = false;
                self.follow = true;
                self.turn_started = Some(Instant::now());
            }
            Event::Text(chunk) => self.transcript.assistant_delta(&chunk),
            Event::Reasoning(chunk) => self.transcript.reasoning_delta(&chunk),
            Event::ToolStart {
                id,
                name,
                label,
                depth,
            } => {
                // Prose before a tool call is settled: showing it half-revealed
                // under a running tool would read as though it were still being
                // written.
                self.transcript.finish_reveal();
                // `todo` has a dedicated plan card, so a tool row beside it
                // would say the same thing twice.
                if name != "todo" {
                    self.transcript.tool_start(id, name, label, depth);
                }
                self.follow = true;
            }
            Event::ToolEnd {
                id,
                ok,
                summary,
                detail,
                view,
            } => {
                // A write may have created a file, so `@` completion is stale.
                if summary.starts_with("created") || summary.starts_with("wrote") {
                    self.files.invalidate();
                    self.files_ready = false;
                }
                self.transcript.tool_end(&id, ok, summary, detail, view);
            }
            Event::ToolPending {
                name,
                args_pretty,
                preview,
                reply,
            } => {
                self.pending = Some(Pending {
                    name,
                    args_pretty,
                    preview,
                    reply: Some(reply),
                    scroll: 0,
                });
            }
            Event::AskUser { question, reply } => {
                // Show the question as a distinct prose block, and route the
                // user's next message into the reply channel.
                self.transcript.finish_reveal();
                self.transcript
                    .assistant_delta(&format!("\n**{question}**\n"));
                self.asking = Some((question, reply));
                self.follow = true;
            }
            Event::Tokens(n) => self.tokens = n,
            Event::NeedsExecuteMode(_) => self.plan_blocked = true,
            Event::Todos(items) => {
                self.transcript.todos(items);
                self.follow = true;
            }
            Event::Notice(msg) => self.note(msg),
            Event::Error(msg) => {
                self.transcript.error(msg);
                self.follow = true;
            }
            Event::Models(list) => {
                if let Some(s) = &mut self.setup {
                    s.status = Some(format!("{} model(s) — ctrl+n to cycle", list.len()));
                    s.available = list;
                } else {
                    self.show_models(list);
                }
            }
            Event::Skills(list) => self.show_skills(list),
            Event::TurnEnd { history_tokens } => {
                // A turn that has ended must not leave half a sentence hidden.
                self.transcript.finish_reveal();
                self.busy = false;
                self.cancelling = false;
                self.turn_started = None;
                self.tokens = history_tokens;
                if let Some(next) = self.queued.pop_front() {
                    self.transcript.user(next.clone());
                    self.send(Command::User(next));
                }
            }
        }
    }

    fn show_skills(&mut self, list: Vec<(String, String)>) {
        if list.is_empty() {
            self.transcript.assistant_delta(
                "No skills installed.\n\nA skill is instructions the agent loads only when \
                 they apply. Drop a markdown file in:\n\n```\n~/.config/koda/skills/      \
                 your own, every project\n<project>/.koda/skills/     this repo's\n```\n\n                 Run `koda skills --init` to write a commented example, then `/skills reload`.\n",
            );
            self.follow = true;
            return;
        }
        let width = self.panel_width();
        let mut p = Panel::new("Skills", width).footer("/skills reload after editing one");
        let name_w = list.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (name, when) in &list {
            let room = p.inner().saturating_sub(name_w + 4);
            let when: String = when.chars().take(room.max(10)).collect();
            p.row(vec![
                Span::styled(
                    format!("{name:<name_w$}  "),
                    self.theme.fg(self.theme.accent_alt),
                ),
                Span::styled(when, self.theme.dim()),
            ]);
        }
        let lines = p.render(&self.theme, &self.glyphs);
        self.transcript.raw(lines);
        self.follow = true;
    }

    /// First screen: a framed card with what koda knows about this project and
    /// what to type. Everything here is a fact the user would otherwise have to
    /// ask for.
    /// Whether anything on screen needs periodic repainting.
    ///
    /// Everything else in koda is event-driven, so this is the complete list of
    /// reasons to wake up: a turn in flight (spinner and elapsed clock), an open
    /// log view tailing new entries, and a file index still scanning.
    fn wants_frames(&self) -> bool {
        self.busy
            || self.logs.is_some()
            || self.files.scanning()
            // Text may still be catching up after the turn itself finished.
            || self.transcript.revealing()
            // The welcome banner's brief entrance shimmer.
            || self
                .welcome_at
                .is_some_and(|t| t.elapsed() < WELCOME_ANIM)
    }

    fn show_welcome(&mut self, cfg: &Config) {
        let t = self.theme;
        let g = self.glyphs;
        let width = self.panel_width();

        if cfg.model.trim().is_empty() {
            let mut p = Panel::new("No model configured", width).footer("/setup to fix this");
            p.row(vec![Span::styled(
                "koda needs an OpenAI-compatible endpoint.".to_string(),
                t.body(),
            )]);
            p.row(vec![Span::styled(
                format!("Press /setup, or run `koda models` to see what {} has.", host_of(&self.endpoint)),
                t.dim(),
            )]);
            let lines = p.render(&t, &g);
            self.transcript.raw(lines);
            self.follow = true;
            return;
        }

        // KODA as block-letter art with a diagonal colour gradient across the
        // letters — the look modern CLIs (Claude Code, Gemini CLI, oh-my-logo)
        // converged on. The gradient runs accent → accent-alt over row+col, so
        // it reads as a single lit object rather than flat text. Falls back to a
        // flat accent when the palette is not truecolor (ANSI/mono).
        let art = BANNER_ART;
        let rows = art.len();
        let cols = art.iter().map(|r| r.chars().count()).max().unwrap_or(1);
        let grad = |row: usize, col: usize| -> ratatui::style::Color {
            match (as_rgb(t.accent), as_rgb(t.accent_alt)) {
                (Some(a), Some(b)) => {
                    // Diagonal position 0..1 across the whole banner.
                    let d = (row as f32 / rows as f32 + col as f32 / cols as f32) / 2.0;
                    let (r, g, bl) = anim::lerp_rgb(a, b, d);
                    ratatui::style::Color::Rgb(r, g, bl)
                }
                _ => t.accent,
            }
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::default());
        for (i, row) in art.iter().enumerate() {
            // Colour each character by its gradient position; group nothing, the
            // banner is small enough that per-cell spans are cheap and paint once.
            let mut spans = vec![Span::raw("  ".to_string())];
            for (j, ch) in row.chars().enumerate() {
                if ch == ' ' {
                    spans.push(Span::raw(" ".to_string()));
                } else {
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(grad(i, j)).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(vec![
            Span::raw("  ".to_string()),
            Span::styled(
                format!("terminal coding agent for local models  {}  v{}", g.sep, env!("CARGO_PKG_VERSION")),
                t.dim(),
            ),
        ]));
        lines.push(Line::default());
        // One rotating tip so the first thing on screen also teaches a feature.
        lines.push(Line::from(vec![
            Span::styled("  tip  ".to_string(), Style::default().fg(t.accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)),
            Span::styled(format!(" {}", random_tip()), t.body()),
        ]));
        lines.push(Line::default());

        self.transcript.raw(lines);
        self.follow = true;
        // Arm a brief entrance shimmer over the banner, but only when motion is
        // on and the terminal is a real TTY — never delay or animate for a pipe
        // or a reduced-motion preference (the research is unanimous on this).
        if self.motion.animates() {
            self.welcome_at = Some(Instant::now());
        }
    }

    fn show_models(&mut self, list: Vec<String>) {
        if list.is_empty() {
            self.note("server reported no models");
            return;
        }
        let current = self.model.clone();
        let width = self.panel_width();
        let mut p = Panel::new(
            format!("Models on {}", host_of(&self.endpoint)),
            width,
        )
        .footer("/model <name> to switch");
        for m in &list {
            let selected = *m == current;
            p.row(vec![
                Span::styled(
                    if selected {
                        format!("{} ", self.glyphs.pick)
                    } else {
                        "  ".to_string()
                    },
                    self.theme.fg(self.theme.accent),
                ),
                Span::styled(
                    m.clone(),
                    if selected {
                        self.theme.strong()
                    } else {
                        self.theme.body()
                    },
                ),
            ]);
        }
        let lines = p.render(&self.theme, &self.glyphs);
        self.transcript.raw(lines);
        self.follow = true;
    }

    // -------------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.pending.is_some() {
            self.approval_key(key);
            return;
        }
        if self.logs.is_some() && self.log_key(key) {
            return;
        }
        if self.setup.is_some() {
            self.setup_key(key);
            return;
        }
        if self.settings.is_some() {
            self.settings_key(key);
            return;
        }
        if self.picker.is_some() {
            self.picker_key(key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // The file list steals navigation keys while it is open.
        if !self.mention_hits().is_empty() && !ctrl && !alt {
            match key.code {
                KeyCode::Up => {
                    self.mention_sel = self.mention_sel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.mention_sel += 1;
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    let hits = self.mention_hits();
                    if let Some(pick) = hits.get(self.mention_sel.min(hits.len() - 1)) {
                        self.editor.replace_mention(pick);
                        self.mention_sel = 0;
                        return;
                    }
                }
                KeyCode::Esc => {
                    // Leave the text, just dismiss the list.
                    self.editor.insert(" ");
                    self.mention_sel = 0;
                    return;
                }
                _ => {}
            }
        }

        // Interactive slash autocomplete: navigate and accept the command list.
        let cmds = self.command_matches();
        if cmds.len() > 1 && !ctrl && !alt {
            match key.code {
                KeyCode::Up => {
                    self.cmd_sel = self.cmd_sel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.cmd_sel = (self.cmd_sel + 1).min(cmds.len() - 1);
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    let pick = cmds[self.cmd_sel.min(cmds.len() - 1)];
                    self.editor.clear();
                    self.editor.insert(pick);
                    self.editor.insert(" ");
                    self.cmd_sel = 0;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('c') if ctrl => self.interrupt(),
            KeyCode::Char('d') if ctrl => {
                if self.editor.is_empty() {
                    self.quit = true;
                } else {
                    self.editor.delete();
                }
            }
            KeyCode::Char('l') if ctrl => {
                if self.take_confirm("clear") {
                    self.transcript.clear();
                    self.scroll = 0;
                    self.follow = true;
                } else {
                    self.note("ctrl+l again to clear the screen");
                }
            }
            KeyCode::Char('a') if ctrl => self.editor.home(),
            KeyCode::Char('e') if ctrl => self.editor.end(),
            KeyCode::Char('k') if ctrl => self.editor.kill_to_end(),
            KeyCode::Char('u') if ctrl => self.editor.kill_to_start(),
            KeyCode::Char('w') if ctrl => self.editor.kill_word(),
            KeyCode::Char('r') if ctrl => {
                self.follow |= self.transcript.toggle_last_tool();
            }
            KeyCode::Char('p') if ctrl => self.cycle_mode(),
            KeyCode::Char('t') if ctrl => {
                self.follow |= self.transcript.toggle_last_reasoning();
            }
            KeyCode::Char('j') if ctrl => self.editor.insert("\n"),
            KeyCode::Char('b') if alt => self.editor.word_left(),
            KeyCode::Char('f') if alt => self.editor.word_right(),
            KeyCode::Enter if alt || ctrl => self.editor.insert("\n"),
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if alt => self.editor.word_left(),
            KeyCode::Right if alt => self.editor.word_right(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.start(),
            KeyCode::End => self.editor.finish(),
            KeyCode::Up => self.key_up(),
            KeyCode::Down => self.key_down(),
            KeyCode::PageUp => self.scroll_by(-(self.body_h as isize / 2).max(1)),
            KeyCode::PageDown => self.scroll_by((self.body_h as isize / 2).max(1)),
            KeyCode::Esc => {
                if self.busy {
                    self.interrupt();
                } else {
                    self.editor.clear();
                }
            }
            KeyCode::Tab => self.complete_command(),
            KeyCode::Char(c) => self.editor.insert(&c.to_string()),
            _ => {}
        }
    }

    /// Candidate files for the `@` token under the caret, if any.
    fn mention_hits(&self) -> Vec<String> {
        let Some((_, query)) = self.editor.mention() else {
            return Vec::new();
        };
        self.files.ensure(&self.root);
        self.files.matches(&query, 8)
    }

    /// Slash-command names matching the current buffer, for interactive
    /// autocomplete. Empty unless the buffer is a bare `/word` with no space.
    fn command_matches(&self) -> Vec<&'static str> {
        let buf = &self.editor.buf;
        if !buf.starts_with('/') || buf.contains(' ') {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| c.starts_with(buf.as_str()))
            .collect()
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some((list, sel)) = &mut self.picker else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.picker = None,
            KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                *sel = (*sel + 1).min(list.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let chosen = list.get(*sel).cloned();
                self.picker = None;
                if let Some(s) = chosen {
                    self.open_session(&s);
                }
            }
            _ => {}
        }
    }

    fn open_session(&mut self, s: &Summary) {
        match session::read(&s.path) {
            Ok((_, messages)) => {
                self.transcript.restore(&messages);
                self.scroll = 0;
                self.follow = true;
                self.send(Command::Resume(s.path.clone()));
                self.note(format!("resumed — {} message(s)", messages.len()));
            }
            Err(e) => self
                .transcript
                .error(format!("could not open that session: {e}")),
        }
    }

    fn setup_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.setup = None;
                self.note("setup cancelled");
            }
            KeyCode::Tab | KeyCode::Down => {
                if let Some(s) = &mut self.setup {
                    s.next_field();
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(s) = &mut self.setup {
                    s.prev_field();
                }
            }
            KeyCode::Char('r') if ctrl => {
                // Fetch the model list from whatever URL is typed in right now.
                if let Some(s) = &mut self.setup {
                    let url = s.value(setup::Field::Url).to_string();
                    s.status = Some("fetching models…".into());
                    self.send(Command::ProbeModels(url));
                }
            }
            KeyCode::Char('n') if ctrl => {
                if let Some(s) = &mut self.setup {
                    s.cycle_model();
                }
            }
            KeyCode::Enter => self.save_setup(),
            KeyCode::Backspace => {
                if let Some(s) = &mut self.setup {
                    s.focused().backspace();
                }
            }
            KeyCode::Left => {
                if let Some(s) = &mut self.setup {
                    s.focused().left();
                }
            }
            KeyCode::Right => {
                if let Some(s) = &mut self.setup {
                    s.focused().right();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(s) = &mut self.setup {
                    s.focused().kill_to_start();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = &mut self.setup {
                    s.focused().insert(&c.to_string());
                }
            }
            _ => {}
        }
    }

    fn save_setup(&mut self) {
        let Some(s) = self.setup.take() else { return };
        let mut cfg = self.cfg.clone();
        match s.save(&mut cfg) {
            Ok(path) => {
                self.cfg = cfg.clone();
                self.endpoint = cfg.endpoint();
                self.model = cfg.model.clone();
                self.send(Command::SetEndpoint(cfg.endpoint()));
                self.send(Command::SetModel(cfg.model.clone()));
                self.note(format!("saved to {}", path.display()));
                crate::tel_info!("ui", "provider saved", "endpoint" => cfg.endpoint());
            }
            Err(e) => {
                self.transcript
                    .error(format!("could not save settings: {e}"));
                crate::tel_error!("ui", "provider save failed", "detail" => format!("{e:#}"));
            }
        }
        self.follow = true;
    }

    fn settings_key(&mut self, key: KeyEvent) {
        let Some(s) = self.settings.as_mut() else { return };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => s.up(),
            KeyCode::Down | KeyCode::Char('j') => s.down(),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                s.change(true);
                self.apply_settings();
            }
            KeyCode::Left => {
                s.change(false);
                self.apply_settings();
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                // Save & close.
                let dirty = self.settings.as_ref().map(|s| s.dirty).unwrap_or(false);
                let new_cfg = self.settings.take().map(|s| s.cfg);
                if let Some(cfg) = new_cfg {
                    if dirty {
                        match crate::config::save(&cfg) {
                            Ok(path) => self.note(format!("settings saved to {}", path.display())),
                            Err(e) => self.transcript.error(format!("could not save settings: {e}")),
                        }
                    }
                }
                self.follow = true;
            }
            _ => {}
        }
    }

    /// Push the settings overlay's working config into the live app and agent,
    /// so a change is visible immediately rather than only after close.
    fn apply_settings(&mut self) {
        let Some(cfg) = self.settings.as_ref().map(|s| s.cfg.clone()) else { return };
        // Theme.
        if cfg.theme != self.cfg.theme {
            let th = theme::resolve(&cfg.theme);
            self.set_theme(th);
        }
        // Motion / reveal.
        self.motion = if cfg.motion { anim::Motion::Full } else { anim::Motion::Reduced };
        self.reveal_pref = cfg.reveal;
        self.transcript.animate_reveal = self.motion.animates() && self.reveal_pref;
        if !self.transcript.animate_reveal {
            self.transcript.finish_reveal();
        }
        // Mode + autonomy: mirror into app state and tell the agent.
        if cfg.mode != self.mode {
            self.set_mode(cfg.mode);
        }
        if cfg.auto_tier != self.auto_tier {
            self.auto_tier = cfg.auto_tier;
            self.send(Command::SetAutoTier(cfg.auto_tier));
        }
        self.cfg = cfg;
    }

    /// Returns true when the key belonged to the log overlay.
    fn log_key(&mut self, key: KeyEvent) -> bool {
        let Some(scroll) = self.logs else { return false };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.logs = None;
                true
            }
            KeyCode::Up => {
                self.logs = Some(scroll.saturating_sub(1));
                true
            }
            KeyCode::Down => {
                self.logs = Some(scroll.saturating_add(1));
                true
            }
            KeyCode::PageUp => {
                self.logs = Some(scroll.saturating_sub(10));
                true
            }
            KeyCode::PageDown => {
                self.logs = Some(scroll.saturating_add(10));
                true
            }
            _ => false,
        }
    }

    /// Two-step confirmation for anything that throws work away. Returns true
    /// when this is the second press of the same action within a few seconds.
    fn take_confirm(&mut self, action: &'static str) -> bool {
        let now = Instant::now();
        match self.confirm {
            Some((prev, at))
                if prev == action && now.duration_since(at) < Duration::from_secs(4) =>
            {
                self.confirm = None;
                true
            }
            _ => {
                self.confirm = Some((action, now));
                false
            }
        }
    }

    fn cycle_mode(&mut self) {
        self.set_mode(self.mode.next());
    }

    fn set_mode(&mut self, m: Mode) {
        if self.mode == m {
            return;
        }
        self.mode = m;
        self.plan_blocked = false;
        self.send(Command::SetMode(m));
        let explain = match m {
            Mode::Plan => "plan — reads and thinks, changes nothing",
            Mode::Execute => "execute — edits and commands, with approval",
            Mode::Vibe => "vibe — writes a spec, then verifies its own work",
        };
        self.note(explain);
    }

    fn key_up(&mut self) {
        if !self.editor.on_first_line() {
            self.editor.up();
        } else if !self.editor.history_prev() {
            self.scroll_by(-1);
        }
    }

    fn key_down(&mut self) {
        if !self.editor.on_last_line() {
            self.editor.down();
        } else if !self.editor.history_next() {
            self.scroll_by(1);
        }
    }

    fn interrupt(&mut self) {
        if self.busy {
            self.cancel.store(true, Ordering::Relaxed);
            self.notify.notify_waiters();
            self.cancelling = true;
            self.note("interrupting…");
            return;
        }
        if !self.editor.is_empty() {
            self.editor.clear();
            return;
        }
        let now = Instant::now();
        match self.last_ctrl_c {
            Some(t) if now.duration_since(t) < Duration::from_secs(2) => self.quit = true,
            _ => {
                self.last_ctrl_c = Some(now);
                self.note("press ctrl+c again to quit");
            }
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.transcript.total_lines().saturating_sub(self.body_h);
        let next = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
        self.scroll = next;
        self.follow = next >= max;
    }

    fn complete_command(&mut self) {
        let buf = self.editor.buf.clone();
        if !buf.starts_with('/') || buf.contains(' ') {
            return;
        }
        let hits: Vec<&str> = COMMANDS
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| c.starts_with(&buf))
            .collect();
        if let [only] = hits[..] {
            self.editor.clear();
            self.editor.insert(only);
            self.editor.insert(" ");
        }
    }

    fn submit(&mut self) {
        let line = self.editor.take();
        let mut trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        // Expand any @pasteN placeholders back to the full pasted text before
        // this goes anywhere. Slash commands are expanded too (harmless — they
        // rarely contain one), then the buffer is cleared for the next turn.
        if !self.pastes.is_empty() {
            for (i, body) in self.pastes.iter().enumerate() {
                let token = format!("@paste{}", i + 1);
                if trimmed.contains(&token) {
                    trimmed = trimmed.replace(&token, body);
                }
            }
            self.pastes.clear();
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            self.slash(rest);
            return;
        }
        // If the agent asked a question, this message is the answer, not a new
        // turn. Echo it and hand it to the waiting tool.
        if let Some((_, reply)) = self.asking.take() {
            self.transcript.user(trimmed.clone());
            self.follow = true;
            let _ = reply.send(trimmed);
            return;
        }
        self.transcript.user(trimmed.clone());
        self.follow = true;
        self.plan_blocked = false;
        if self.busy {
            self.queued.push_back(trimmed);
            self.note("queued until the current turn finishes");
        } else {
            self.send(Command::User(trimmed));
        }
    }

    fn slash(&mut self, rest: &str) {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim().to_string();

        match cmd.as_str() {
            "help" | "?" => self.show_help(),
            "keys" => self.show_help(),
            "model" => {
                if arg.is_empty() {
                    let m = self.model.clone();
                    self.note(format!("model: {m}"));
                } else {
                    self.model = arg.clone();
                    self.send(Command::SetModel(arg));
                }
            }
            "models" => self.send(Command::ListModels),
            "mode" => match arg.as_str() {
                "" => {
                    let m = self.mode;
                    self.note(format!("mode: {m} — ctrl+p to cycle"));
                }
                other => match other.parse::<Mode>() {
                    Ok(m) => self.set_mode(m),
                    Err(e) => self.note(e),
                },
            },
            "undo" => self.send(Command::Undo),
            "session" => self.send(Command::WhichSession),
            "resume" | "sessions" => {
                let list = session::list(&self.root);
                if list.is_empty() {
                    self.note("no saved sessions in this project yet");
                } else {
                    self.picker = Some((list, 0));
                }
            }
            "search" | "find" => {
                if arg.is_empty() {
                    self.note("usage: /search <text> — searches saved conversations");
                } else {
                    let hits = session::search(&self.root, &arg);
                    if hits.is_empty() {
                        self.note(format!("no saved session mentions \"{arg}\""));
                    } else {
                        let n = hits.len();
                        self.picker = Some((hits.into_iter().map(|(s, _)| s).collect(), 0));
                        self.note(format!(
                            "{n} session{} match \"{arg}\" — enter to open",
                            if n == 1 { "" } else { "es" }
                        ));
                    }
                }
            }
            "fork" | "branch" => {
                // Fork the most recent session (the one in play) into a new
                // branch, then continue on the fork so the original is left
                // untouched.
                match session::latest(&self.root) {
                    Some(s) => match session::fork(&s.path, &self.root) {
                        Ok(dest) => {
                            self.send(Command::Resume(dest));
                            self.note("forked this conversation — now on the branch");
                        }
                        Err(e) => self.note(format!("could not fork: {e}")),
                    },
                    None => self.note("no session to fork yet — say something first"),
                }
            }
            "setup" | "provider" | "config" => {
                self.setup = Some(Setup::new(&self.cfg));
                self.send(Command::ProbeModels(self.endpoint.clone()));
            }
            "settings" | "preferences" | "prefs" => {
                self.settings = Some(settings::Settings::new(&self.cfg));
            }
            "orc" | "orchestrate" => {
                if arg.is_empty() {
                    self.note("usage: /orc <task> — decompose and delegate to role agents");
                    return;
                }
                // Frame the task as an orchestration brief. The main agent stays
                // the orchestrator: it plans with `todo`, then delegates each
                // subtask to the right role-agent via `delegate` with a role.
                let brief = format!(
                    "You are the ORCHESTRATOR. Break this task down and coordinate role \
                     agents to do it — do not do the hands-on work yourself.\n\n\
                     Task: {arg}\n\n\
                     Do this:\n\
                     1. Use the `todo` tool to lay out the subtasks.\n\
                     2. For each subtask, write a crisp brief — goal, what to change, and how \
                     to validate the result — and hand it to the right role with `delegate` \
                     (pass a `role` such as dev, qa, tester, or manager, matching a role \
                     skill file). If no role skills exist, delegate without a role.\n\
                     3. Integrate the reports, verify each against its validation criteria, \
                     and summarise what was done and what remains.",
                );
                self.transcript.user(format!("/orc {arg}"));
                self.follow = true;
                if self.busy {
                    self.queued.push_back(brief);
                    self.note("queued until the current turn finishes");
                } else {
                    self.send(Command::User(brief));
                }
            }
            "skills" | "skill" => {
                if arg.starts_with("re") {
                    self.send(Command::ReloadSkills);
                } else {
                    self.send(Command::ListSkills);
                }
            }
            "websearch" | "web" => {
                if !self.searx_configured {
                    self.note(
                        "set searx_url in ~/.config/koda/config.toml first (a SearXNG \
                         instance with the json format enabled)",
                    );
                } else {
                    self.web = !self.web;
                    let v = self.web;
                    self.send(Command::SetWebSearch(v));
                }
            }
            "logs" | "log" => {
                self.logs = Some(u16::MAX); // clamped to the tail when drawn
                if let Some(p) = log::file_path() {
                    crate::tel_debug!("ui", "opened log view", "file" => p.display());
                }
            }
            "theme" => self.theme_cmd(&arg),
            "url" | "endpoint" => {
                if arg.is_empty() {
                    let e = self.endpoint.clone();
                    self.note(format!("endpoint: {e}"));
                } else {
                    self.endpoint = arg.trim_end_matches('/').to_string();
                    self.send(Command::SetEndpoint(arg));
                }
            }
            "clear" | "new" | "reset" => {
                if !self.take_confirm("wipe") {
                    self.note("/clear again to drop the conversation");
                    return;
                }
                self.transcript.clear();
                self.scroll = 0;
                self.tokens = 0;
                self.follow = true;
                self.send(Command::Clear);
            }
            "compact" => self.send(Command::Compact),
            "auto" | "autonomy" => {
                // `/auto` cycles ask → auto-write → full-auto; `/auto <tier>`
                // sets it directly.
                let tier = if arg.is_empty() {
                    self.auto_tier.next()
                } else {
                    match arg.parse::<AutoTier>() {
                        Ok(t) => t,
                        Err(e) => {
                            self.note(e);
                            return;
                        }
                    }
                };
                self.auto_tier = tier;
                self.send(Command::SetAutoTier(tier));
                let hint = match tier {
                    AutoTier::Ask => "asks before writes and commands",
                    AutoTier::Write => "auto-approves writes, asks before commands",
                    AutoTier::Full => "runs everything without asking — autonomous",
                };
                self.note(format!("autonomy: {} — {hint}", tier.label()));
            }
            "tools" => {
                let width = self.panel_width();
                let mut p = Panel::new("Tools", width).footer("the agent picks these itself");
                for spec in crate::tools::specs() {
                    let desc: String = spec
                        .desc
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(p.inner().saturating_sub(14))
                        .collect();
                    p.row(vec![
                        Span::styled(
                            format!("{:<12}", spec.name),
                            if spec.mutating {
                                self.theme.emphasis(self.theme.warning)
                            } else {
                                self.theme.fg(self.theme.accent)
                            },
                        ),
                        Span::styled(desc, self.theme.dim()),
                    ]);
                }
                let lines = p.render(&self.theme, &self.glyphs);
                self.transcript.raw(lines);
                self.follow = true;
            }
            "mouse" | "select" => {
                // Toggling capture off hands click-drag back to the terminal so
                // the user can select and copy text; on restores wheel-scroll.
                self.mouse_capture = !self.mouse_capture;
                let mut out = std::io::stdout();
                if self.mouse_capture {
                    let _ = execute!(out, EnableMouseCapture);
                    self.note("mouse capture on — wheel scrolls; text selection is the terminal's");
                } else {
                    let _ = execute!(out, DisableMouseCapture);
                    self.note("mouse capture off — select & copy text with the mouse; scroll with pgup/pgdn");
                }
            }
            "motion" => {
                // Motion is a preference, so it is toggleable at runtime rather
                // than only through config.
                self.motion = match self.motion {
                    anim::Motion::Full => anim::Motion::Reduced,
                    _ => anim::Motion::Full,
                };
                self.transcript.animate_reveal = self.motion.animates() && self.reveal_pref;
                if !self.motion.animates() {
                    self.transcript.finish_reveal();
                }
                let on = self.motion.animates();
                self.note(format!("animation {}", if on { "on" } else { "off" }));
            }
            "reveal" => {
                // The streaming text reveal is a distinct preference from
                // overall motion: keep spinners and gauges, drop the typing-in.
                self.reveal_pref = !self.reveal_pref;
                self.transcript.animate_reveal = self.motion.animates() && self.reveal_pref;
                if !self.transcript.animate_reveal {
                    self.transcript.finish_reveal();
                }
                let on = self.reveal_pref;
                if self.motion.animates() {
                    self.note(format!("text reveal {}", if on { "on" } else { "off" }));
                } else {
                    self.note(format!(
                        "text reveal {} (takes effect when motion is on)",
                        if on { "on" } else { "off" }
                    ));
                }
            }
            "think" => {
                self.transcript.show_reasoning = !self.transcript.show_reasoning;
                let on = self.transcript.show_reasoning;
                self.transcript.invalidate();
                self.note(format!("reasoning {}", if on { "shown" } else { "hidden" }));
            }
            "copy" => match self.transcript.last_assistant() {
                Some(text) => {
                    let text = text.to_string();
                    match copy_to_clipboard(&text) {
                        Ok(()) => self.note(format!("copied {} bytes", text.len())),
                        Err(e) => self.note(format!("copy failed: {e}")),
                    }
                }
                None => self.note("nothing to copy"),
            },
            "cwd" | "pwd" => {
                let r = self.root.display().to_string();
                self.note(r);
            }
            "quit" | "exit" | "q" => self.quit = true,
            other => self.note(format!("unknown command /{other} — try /help")),
        }
    }

    fn theme_cmd(&mut self, arg: &str) {
        if arg.is_empty() {
            let current = self.theme.name;
            let width = self.panel_width();
            let mut p = Panel::new("Themes", width).footer("/theme <name> to switch");
            for th in theme::THEMES {
                let selected = th.name == current;
                let mut row = vec![Span::styled(
                    if selected {
                        format!("{} ", self.glyphs.pick)
                    } else {
                        "  ".to_string()
                    },
                    self.theme.fg(self.theme.accent),
                )];
                row.push(Span::styled(
                    format!("{:<18}", th.name),
                    if selected {
                        self.theme.strong()
                    } else {
                        self.theme.body()
                    },
                ));
                // A swatch of the palette, so you can choose by eye.
                for c in [
                    th.accent, th.accent_alt, th.success, th.warning, th.error, th.info,
                ] {
                    row.push(Span::styled("██", Style::default().fg(c)));
                }
                p.row(row);
            }
            let lines = p.render(&self.theme, &self.glyphs);
            self.transcript.raw(lines);
            self.follow = true;
            return;
        }
        match theme::by_name(arg) {
            Some(t) => {
                self.set_theme(t);
                self.note(format!("theme → {}", t.name));
            }
            None => self.note(format!(
                "unknown theme `{arg}` — one of: {}",
                theme::names().join(", ")
            )),
        }
    }

    /// Help, framed. Two panels so commands and keys are visually separate
    /// rather than one long list you have to parse by eye.
    fn show_help(&mut self) {
        const KEYS: &[(&str, &str)] = &[
            ("enter", "send"),
            ("ctrl+j", "newline"),
            ("ctrl+c", "interrupt · twice quits"),
            ("ctrl+d", "quit"),
            ("ctrl+p", "cycle mode"),
            ("ctrl+r", "expand last tool output"),
            ("ctrl+t", "expand last reasoning"),
            ("pgup/pgdn", "scroll · wheel works"),
            ("up/down", "input history"),
            ("tab", "complete · pick a file"),
            ("@", "mention a file"),
            ("ctrl+a/e", "start / end of line"),
            ("ctrl+k/u/w", "kill to end / start / word"),
            ("esc", "close an overlay"),
        ];

        let width = self.panel_width();
        let mut lines = Vec::new();

        // Detailed examples first — the most useful part, and it teaches by
        // showing real usage rather than just listing names.
        const EXAMPLES: &[(&str, &str)] = &[
            ("/model qwen2.5-coder:14b", "switch to a specific model"),
            ("/mode plan", "read-only planning; also execute, vibe"),
            ("/auto", "cycle ask → auto-write → full-auto"),
            ("/search discount bug", "find past chats mentioning that text"),
            ("/fork", "branch this chat, keep the original"),
            ("/orc build a login page", "split the task across role agents"),
            ("/theme tokyo-night", "switch palette by name"),
            ("/mouse", "off = select & copy text with the mouse"),
            ("@src/main.rs", "attach a file (or an image) to your message"),
            ("/undo", "revert the last turn's file changes"),
        ];
        let mut ex = Panel::new("Examples", width).footer("type a command, or just talk");
        let cmd_w = EXAMPLES.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
        for (cmd, what) in EXAMPLES {
            let room = ex.inner().saturating_sub(cmd_w + 3);
            let what: String = what.chars().take(room).collect();
            ex.row(vec![
                Span::styled(
                    format!("{cmd:<cmd_w$}   "),
                    self.theme.fg(self.theme.accent),
                ),
                Span::styled(what, self.theme.dim()),
            ]);
        }
        lines.extend(ex.render(&self.theme, &self.glyphs));
        lines.push(Line::default());

        let mut k = Panel::new("Keys", width).footer("type / for the full command list");
        for row in panel::key_value_rows(KEYS, k.inner(), &self.theme) {
            k.row(row);
        }
        lines.extend(k.render(&self.theme, &self.glyphs));

        self.transcript.raw(lines);
        self.follow = true;
    }

    /// Panels stop short of the full width so they do not touch the scrollbar.
    fn panel_width(&self) -> usize {
        // Match the transcript's usable text width (full width minus gutters and
        // a possible scrollbar). Cap at 100 for readability, but never exceed
        // what the transcript can actually show — otherwise pre-rendered panel
        // rows get clipped on the right (the "half a banner" bug).
        let usable = (self.last_size.0 as usize).saturating_sub(4).max(8);
        usable.min(100)
    }

    fn approval_key(&mut self, key: KeyEvent) {
        let decision = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(Approval::Once),
            KeyCode::Char('a') | KeyCode::Char('A') => Some(Approval::AlwaysThisTool),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Approval::Deny),
            KeyCode::Up => {
                if let Some(p) = &mut self.pending {
                    p.scroll = p.scroll.saturating_sub(1);
                }
                None
            }
            KeyCode::Down => {
                if let Some(p) = &mut self.pending {
                    p.scroll = p.scroll.saturating_add(1);
                }
                None
            }
            _ => None,
        };
        let Some(decision) = decision else { return };
        if let Some(mut p) = self.pending.take() {
            if let Some(reply) = p.reply.take() {
                let _ = reply.send(decision);
            }
            if decision == Approval::AlwaysThisTool {
                self.note(format!("always allowing {}", p.name));
            }
        }
    }
}

/// Copy via a native helper if one exists, else OSC 52, which works over SSH
/// and on terminals with no local clipboard tool.
fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::process::{Command as Proc, Stdio};
    for (bin, args) in [
        ("pbcopy", &[][..]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
    ] {
        let spawned = Proc::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(text.as_bytes())?;
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return Ok(());
            }
        }
    }
    osc52(text)
}

/// `ESC ] 52 ; c ; <base64> BEL` — the terminal itself does the copying.
fn osc52(text: &str) -> Result<()> {
    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
    out.flush()?;
    Ok(())
}

/// Small base64 encoder: pulling a crate for eleven lines is not worth it.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Current git branch, read once at startup for the header.
fn git_branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?.to_string();
    (!name.is_empty()).then_some(name)
}

// ------------------------------------------------------------------- rendering

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // Record it here as well as on resize: the first frame never sees a resize
    // event, and layout decisions elsewhere need the real width.
    app.last_size = (area.width, area.height);
    let m = Metrics::of(area.width);

    let input_w = area.width.saturating_sub(3).max(4) as usize;
    let (rows, crow, ccol) = app.editor.visual(input_w);
    let max_input = if m.tiny { 4 } else { 8 };
    let input_h = rows.len().clamp(1, max_input) as u16;

    let chunks = Layout::vertical([
        Constraint::Min(1),          // transcript
        Constraint::Length(1),       // hint / state row
        Constraint::Length(input_h), // input
        Constraint::Length(1),       // powerline status bar
    ])
    .split(area);
    let (body, rule, input, status) = (chunks[0], chunks[1], chunks[2], chunks[3]);

    // Transcript, with a one-column scrollbar reserved only when it scrolls.
    let total_before = app.transcript.total_lines();
    let scrollable = total_before > body.height as usize;
    let gutter = u16::from(scrollable);
    let text_area = Rect {
        x: body.x + 1,
        y: body.y,
        width: body.width.saturating_sub(2 + gutter),
        height: body.height,
    };
    app.body_h = text_area.height as usize;
    let total = app.transcript.relayout(text_area.width);
    let max_scroll = total.saturating_sub(app.body_h);
    if app.follow {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
    }
    f.render_widget(
        Paragraph::new(app.transcript.window(app.scroll, app.body_h)),
        text_area,
    );
    // Brief entrance shimmer over the banner: while the welcome animation window
    // is open and we are scrolled to the top, sweep a bright band across the six
    // art rows. It repaints only those rows (over identical content), so when it
    // ends there is no visible jump — just the shimmer stopping.
    if let Some(started) = app.welcome_at {
        let elapsed = started.elapsed();
        if elapsed < WELCOME_ANIM && app.scroll == 0 && app.motion.animates() {
            welcome_shimmer(f, app, text_area, elapsed);
        } else if elapsed >= WELCOME_ANIM {
            app.welcome_at = None;
        }
    }
    if total > app.body_h {
        draw_scrollbar(
            f,
            Rect {
                x: body.x + body.width.saturating_sub(1),
                y: body.y,
                width: 1,
                height: body.height,
            },
            app,
            total,
        );
    }

    let t = &app.theme;
    f.render_widget(Paragraph::new(hint_row(app, area.width, m)), rule);

    // Input.
    let prompt_style = if app.busy {
        t.dim()
    } else {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    };
    let g = &app.glyphs;
    // The input sits on its own tinted surface, so it reads as a distinct field
    // without a frame around it.
    let input_lines: Vec<Line> = if app.editor.is_empty() && !app.busy {
        vec![Line::from(vec![
            Span::styled(format!("{} ", g.prompt), prompt_style),
            Span::styled("ask, or /help for commands".to_string(), t.dim()),
        ])]
    } else {
        let shown = input_h as usize;
        rows.iter()
            .skip(rows.len().saturating_sub(shown))
            .enumerate()
            .map(|(i, r)| {
                Line::from(vec![
                    Span::styled(
                        if i == 0 {
                            format!("{} ", g.prompt)
                        } else {
                            "  ".to_string()
                        },
                        prompt_style,
                    ),
                    Span::styled(r.clone(), t.body()),
                ])
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(panel::fill(
            input_lines,
            area.width as usize,
            t.bg_panel,
            1,
        )),
        input,
    );

    f.render_widget(Paragraph::new(powerline(app, area.width, m)), status);

    // Caret.
    let first_shown = rows.len().saturating_sub(input_h as usize);
    if crow >= first_shown {
        let y = input.y + (crow - first_shown) as u16;
        let x = input.x + 3 + ccol as u16;
        f.set_cursor_position(Position::new(x.min(area.width.saturating_sub(1)), y));
    }

    let mention = app.mention_hits();
    if !mention.is_empty() {
        mention_popup(f, app, input, &mention);
    } else if app.editor.buf.starts_with('/') && !app.editor.buf.contains(' ') {
        command_popup(f, app, input);
    }
    if app.picker.is_some() {
        session_picker(f, app, area);
    }
    if let Some(s) = &app.setup {
        setup::draw(f, area, s, &app.theme, &app.glyphs);
    }
    if let Some(s) = &app.settings {
        s.draw(f, area, &app.theme, &app.glyphs);
    }
    if app.logs.is_some() {
        log_overlay(f, app, area);
    }
    if app.pending.is_some() {
        approval_popup(f, app, area);
    }
}

/// The event log. Opened with `/logs` when something looked wrong and the
/// transcript only showed the short version.
fn log_overlay(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let g = &app.glyphs;
    let w = area.width.saturating_sub(4).max(20);
    let h = area.height.saturating_sub(4).max(6);
    let rect = Rect {
        x: area.width.saturating_sub(w) / 2,
        y: area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let inner_h = rect.height.saturating_sub(2) as usize;
    let inner_w = rect.width.saturating_sub(2) as usize;

    let entries = log::recent(
        if app.cfg.log_detail { log::Level::Debug } else { log::Level::Info },
        500,
    );
    let mut lines: Vec<Line> = Vec::new();
    for e in &entries {
        let style = match e.level {
            log::Level::Error => t.emphasis(t.error),
            log::Level::Warn => t.emphasis(t.warning),
            log::Level::Info => t.body(),
            log::Level::Debug => t.dim(),
        };
        let mut head = vec![
            Span::styled(format!("{:>7.2}s ", e.at), t.dim()),
            Span::styled(format!("{:<5} ", e.level.label()), style),
            Span::styled(format!("{:<8} ", e.area), t.fg(t.accent_alt)),
            Span::styled(e.message.clone(), style),
        ];
        for (k, v) in &e.fields {
            let short: String = v.chars().take(120).collect();
            head.push(Span::styled(format!("  {k}={short}"), t.dim()));
        }
        lines.push(truncate_line(head, inner_w as u16));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing logged yet",
            t.dim(),
        )));
    }

    // Default to the tail: the newest entry is what you came to read.
    let max_scroll = lines.len().saturating_sub(inner_h) as u16;
    let scroll = app.logs.unwrap_or(0).min(max_scroll);
    app.logs = Some(scroll);

    let (warns, errors) = log::counts();
    let title = match log::file_path() {
        Some(p) => format!(" logs {} {} warn {} error {} {} ", g.sep, warns, errors, g.sep, p.display()),
        None => format!(" logs {} {} warn {} error ", g.sep, warns, errors),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.fg(t.border))
        .title(Span::styled(title, t.dim()))
        .title_bottom(Line::from(vec![
            Span::styled(" ↑↓ pgup/pgdn scroll ", t.dim()),
            Span::styled(g.sep.to_string(), t.dim()),
            Span::styled(" esc close ", t.dim()),
        ]));
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        rect,
    );
}


fn host_of(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

fn short_model(model: &str, m: Metrics) -> String {
    if model.is_empty() {
        return "no model".into();
    }
    let limit = if m.compact { 18 } else { 34 };
    if model.chars().count() <= limit {
        return model.to_string();
    }
    // Tail-truncate: the interesting part of a model id is at the front.
    let head: String = model.chars().take(limit.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The row above the input: mode, what the agent is doing, and the keys that do
/// something right now. No frame — the tinted input below is boundary enough.
fn hint_row(app: &App, width: u16, m: Metrics) -> Line<'static> {
    let t = &app.theme;
    let g = &app.glyphs;
    let mut left: Vec<Span<'static>> = Vec::new();

    let mode_colour = match app.mode {
        Mode::Plan => t.warning,
        Mode::Execute => t.success,
        Mode::Vibe => t.accent_alt,
    };
    left.push(Span::styled(
        format!(" {} ", app.mode.label()),
        Style::default()
            .fg(mode_colour)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    ));

    match (app.busy, app.turn_started) {
        (true, _) if app.cancelling => {
            // The interrupt landed but the turn is still unwinding (a tool call
            // in flight, a stream draining). Say so, in the warning tint, rather
            // than showing the ordinary "working" state.
            let glyph = if app.motion.animates() {
                g.thinking[anim::sweep(app.turn_started.map(|s| s.elapsed()).unwrap_or_default())
                    % g.thinking.len()]
            } else {
                g.thinking[0]
            };
            left.push(Span::styled(format!(" {glyph} "), t.fg(t.warning)));
            left.push(Span::styled("cancelling…".to_string(), t.emphasis(t.warning)));
        }
        (true, Some(started)) if started.elapsed() >= SPINNER_DELAY => {
            // The sweep derives its frame from elapsed time rather than a
            // counter, so its pace is identical whether the loop is redrawing
            // for animation or because the user typed.
            let glyph = if app.motion.animates() {
                g.thinking[anim::sweep(started.elapsed()) % g.thinking.len()]
            } else {
                g.thinking[0]
            };
            left.push(Span::styled(format!(" {glyph} "), t.fg(t.accent)));
            let label = format!("working {}", anim::short_elapsed(started.elapsed()));
            if app.motion.animates() {
                // A highlight sweeping the label reads as ongoing activity
                // without moving any text around.
                let bright = anim::shimmer(
                    label.chars().count(),
                    started.elapsed(),
                    Duration::from_millis(1600),
                );
                let base = t.muted;
                for (ch, b) in label.chars().zip(bright) {
                    let colour = if b <= 0.0 {
                        base
                    } else {
                        theme::mix(base, t.accent, b)
                    };
                    left.push(Span::styled(ch.to_string(), t.fg(colour)));
                }
            } else {
                left.push(Span::styled(label, t.dim()));
            }
        }
        (true, _) => left.push(Span::styled("  working".to_string(), t.dim())),
        (false, _) => {
            left.push(Span::styled(format!(" {} ", g.ready), t.emphasis(t.success)));
            left.push(Span::styled("ready".to_string(), t.dim()));
            // When idle with an empty composer, rotate a feature tip so the bar
            // teaches the app instead of sitting blank. Kept short and dim.
            if app.editor.is_empty() && !m.tiny {
                left.push(Span::styled(format!("  {} ", g.sep), t.dim()));
                left.push(Span::styled(random_tip().to_string(), t.dim()));
            }
        }
    }

    if let Some((done, total)) = app.transcript.todo_progress() {
        left.push(Span::styled(format!("  {} ", g.sep), t.dim()));
        left.push(Span::styled(
            format!("{done}/{total} steps"),
            if done == total {
                t.emphasis(t.success)
            } else {
                t.fg(t.accent)
            },
        ));
    }
    if !app.queued.is_empty() {
        left.push(Span::styled(format!("  {} ", g.sep), t.dim()));
        left.push(Span::styled(
            format!("{} queued", app.queued.len()),
            t.emphasis(t.warning),
        ));
    }

    // Only the keys that apply to the current state.
    let hints: &[(&str, &str)] = if app.pending.is_some() {
        &[("y", "once"), ("a", "always"), ("n", "deny")]
    } else if app.asking.is_some() {
        &[("type", "your answer"), ("enter", "send")]
    } else if app.picker.is_some() || app.setup.is_some() {
        &[("↑↓", "move"), ("enter", "choose"), ("esc", "cancel")]
    } else if app.plan_blocked {
        &[("ctrl+p", "switch to execute")]
    } else if app.busy {
        &[("esc", "interrupt")]
    } else if !app.follow {
        &[("pgdn", "latest")]
    } else if m.tiny {
        &[("/help", "")]
    } else {
        &[("@", "file"), ("ctrl+p", "mode"), ("/keys", "")]
    };
    let mut right: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            right.push(Span::styled(format!(" {} ", g.sep), t.dim()));
        }
        right.push(Span::styled((*key).to_string(), t.fg(t.accent)));
        if !label.is_empty() {
            right.push(Span::styled(format!(" {label}"), t.dim()));
        }
    }

    let lw: usize = left.iter().map(|s| s.content.width()).sum();
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    let mut spans = left;
    if (width as usize) > lw + rw + 2 {
        spans.push(Span::raw(" ".repeat(width as usize - lw - rw - 1)));
        spans.extend(right);
    }
    truncate_line(spans, width)
}

/// The bottom bar: model, project, branch, context. Chevron-separated segments,
/// each in its own colour, so the fields are distinguishable at a glance.
fn powerline(app: &App, width: u16, m: Metrics) -> Line<'static> {
    use panel::Segment;
    let t = &app.theme;
    let g = &app.glyphs;

    let mut segs = vec![Segment::new(short_model(&app.model, m), t.accent).bold()];

    let dir = app
        .root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| app.root.display().to_string());
    segs.push(Segment::new(dir, t.info));

    if let Some(b) = &app.branch {
        if !m.tiny {
            segs.push(Segment::new(b.clone(), t.accent_alt));
        }
    }
    if !m.compact {
        segs.push(Segment::new(host_of(&app.endpoint), t.muted));
    }

    let mut right = Vec::new();
    if app.web {
        right.push(Segment::new("web", t.info));
    }
    if app.auto_tier != AutoTier::Ask {
        // Full-auto is the loud one (red): it means no human in the loop.
        let colour = match app.auto_tier {
            AutoTier::Full => t.error,
            _ => t.warning,
        };
        right.push(Segment::new(app.auto_tier.label(), colour));
    }
    let (warns, errors) = log::counts();
    if errors > 0 {
        right.push(Segment::new(format!("{errors} issue /logs"), t.error));
    } else if warns > 0 {
        right.push(Segment::new(format!("{warns} warn /logs"), t.warning));
    }
    if app.tokens > 0 {
        let frac = app.tokens as f64 / app.context_budget.max(1) as f64;
        let pct = (frac * 100.0).round() as usize;
        if m.compact {
            right.push(Segment::new(format!("{}  {pct}%", Tokens(app.tokens)), t.muted));
        } else {
            right.push(Segment::new(
                format!(
                    "{}  {} {pct}%",
                    Tokens(app.tokens),
                    panel::gauge(frac, 8, g)
                ),
                panel::gauge_style(frac, t).fg.unwrap_or(t.muted),
            ));
        }
    }

    panel::status_bar(segs, right, width as usize, t, g)
}

/// Thousands-separated token count, so 12400 reads as 12.4k.
struct Tokens(usize);

impl std::fmt::Display for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 < 1000 {
            write!(f, "{} tok", self.0)
        } else {
            write!(f, "{:.1}k tok", self.0 as f64 / 1000.0)
        }
    }
}



/// Clip a line to the available width without wrapping.
fn truncate_line(spans: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let mut out = Vec::new();
    let mut used = 0usize;
    let limit = width as usize;
    for s in spans {
        let w = s.content.chars().count();
        if used + w <= limit {
            used += w;
            out.push(s);
        } else {
            let room = limit.saturating_sub(used);
            if room > 0 {
                let clipped: String = s.content.chars().take(room).collect();
                out.push(Span::styled(clipped, s.style));
            }
            break;
        }
    }
    Line::from(out)
}

fn draw_scrollbar(f: &mut Frame, rect: Rect, app: &App, total: usize) {
    let t = &app.theme;
    let g = &app.glyphs;
    let h = rect.height as usize;
    if h == 0 || total == 0 {
        return;
    }
    let thumb = ((h * h) / total).max(1).min(h);
    let max_scroll = total.saturating_sub(h);
    let pos = (app.scroll * (h - thumb)).checked_div(max_scroll).unwrap_or(0);
    let lines: Vec<Line> = (0..h)
        .map(|i| {
            if i >= pos && i < pos + thumb {
                Line::from(Span::styled(g.scroll_thumb, t.fg(t.border_focus)))
            } else {
                Line::from(Span::styled(g.scroll_track, t.fg(t.border)))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rect);
}

/// Completions as a single line above the input. A tall overlay would cover the
/// transcript — including whatever the user just asked to see.
fn command_popup(f: &mut Frame, app: &App, input: Rect) {
    let t = &app.theme;
    let prefix = app.editor.buf.clone();
    let hits: Vec<&(&str, &str)> = COMMANDS
        .iter()
        .filter(|(c, _)| c.starts_with(&prefix))
        .collect();
    if hits.is_empty() || input.y == 0 {
        return;
    }

    // One exact match: show what it does on a single row, which is the useful
    // information.
    if let [(name, desc)] = hits[..] {
        let rect = Rect {
            x: input.x,
            y: input.y - 1,
            width: input.width,
            height: 1,
        };
        let spans = vec![
            Span::styled(format!(" {name}"), t.fg(t.accent)),
            Span::styled(format!("  {desc}"), t.dim()),
        ];
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
        return;
    }

    // Vertical list: one command per row with its description, selected row
    // reversed. Grows upward from the input like the file-mention popup, capped
    // to a sensible height with the selection kept in view.
    let sel = app.cmd_sel.min(hits.len().saturating_sub(1));
    let name_w = hits.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
    let max_rows = 10usize;
    // Window the list around the selection so a long list stays navigable.
    let start = if sel >= max_rows { sel + 1 - max_rows } else { 0 };
    let shown: Vec<&&(&str, &str)> = hits.iter().skip(start).take(max_rows).collect();
    let inner_w = input.width.max(10) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, (name, desc)) in shown.iter().map(|h| **h).enumerate() {
        let idx = start + i;
        let selected = idx == sel;
        let marker = if selected { "›" } else { " " };
        let desc: String = desc.chars().take(inner_w.saturating_sub(name_w + 6)).collect();
        let name_style = if selected {
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            t.fg(t.accent)
        };
        let line = Line::from(vec![
            Span::styled(format!(" {marker} "), t.fg(t.accent)),
            Span::styled(format!("{name:<name_w$}  "), name_style),
            Span::styled(desc, t.dim()),
        ]);
        lines.push(line);
    }

    let h = (lines.len() as u16).min(max_rows as u16);
    if input.y < h {
        return;
    }
    let rect = Rect {
        x: input.x,
        y: input.y - h,
        width: input.width,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines), rect);
}

/// The banner's entrance shimmer: repaint the six art rows with the same
/// gradient plus a soft bright band travelling left→right, so the logo "lights
/// up" once on open. Draws over identical content, so when it stops there is no
/// visible jump. Reduced-motion and non-TTY paths never reach here.
fn welcome_shimmer(f: &mut Frame, app: &App, text_area: Rect, elapsed: Duration) {
    let t = &app.theme;
    let (Some(a), Some(b)) = (as_rgb(t.accent), as_rgb(t.accent_alt)) else {
        return; // No gradient on non-truecolor palettes; nothing to shimmer.
    };
    let rows = BANNER_ART.len();
    let cols = BANNER_ART.iter().map(|r| r.chars().count()).max().unwrap_or(1);
    // A band that sweeps across the banner width over the animation window.
    let bright = anim::shimmer(cols, elapsed, WELCOME_ANIM);
    for (i, row) in BANNER_ART.iter().enumerate() {
        // Row 0 of the banner sits one line below the top (a leading blank).
        let y = text_area.y + 1 + i as u16;
        if y >= text_area.y + text_area.height {
            break;
        }
        let mut spans = vec![Span::raw("  ".to_string())];
        for (j, ch) in row.chars().enumerate() {
            if ch == ' ' {
                spans.push(Span::raw(" ".to_string()));
                continue;
            }
            let d = (i as f32 / rows as f32 + j as f32 / cols as f32) / 2.0;
            let (mut r, mut gg, mut bl) = anim::lerp_rgb(a, b, d);
            // Lift toward white where the band is brightest.
            let lift = bright.get(j).copied().unwrap_or(0.0);
            if lift > 0.0 {
                let (wr, wg, wb) = anim::lerp_rgb((r, gg, bl), (255, 255, 255), lift * 0.85);
                r = wr;
                gg = wg;
                bl = wb;
            }
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(Color::Rgb(r, gg, bl)).add_modifier(Modifier::BOLD),
            ));
        }
        let rect = Rect { x: text_area.x, y, width: text_area.width, height: 1 };
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
    }
}

/// The `@` file list, above the input. Selected row is reversed so it reads as
/// a selection rather than just a colour change.
fn mention_popup(f: &mut Frame, app: &App, input: Rect, hits: &[String]) {
    let t = &app.theme;
    let g = &app.glyphs;
    let h = (hits.len() as u16).min(8);
    if input.y < h {
        return;
    }
    let rect = Rect {
        x: input.x,
        y: input.y - h,
        width: input.width,
        height: h,
    };
    let sel = app.mention_sel.min(hits.len().saturating_sub(1));
    let lines: Vec<Line> = hits
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let selected = i == sel;
            Line::from(vec![
                Span::styled(
                    if selected {
                        format!(" {} ", g.prompt)
                    } else {
                        "   ".to_string()
                    },
                    t.fg(t.accent),
                ),
                Span::styled(
                    path.clone(),
                    if selected {
                        Style::default().fg(t.text).add_modifier(Modifier::BOLD)
                    } else {
                        t.dim()
                    },
                ),
            ])
        })
        .collect();
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines), rect);
}

fn session_picker(f: &mut Frame, app: &App, area: Rect) {
    let Some((list, sel)) = &app.picker else { return };
    let t = &app.theme;
    let g = &app.glyphs;

    let w = area.width.saturating_sub(6).clamp(40, 96);
    let rows = (list.len() as u16).min(12);
    let h = (rows + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let inner_w = rect.width.saturating_sub(2) as usize;
    let visible = h.saturating_sub(2) as usize;
    // Keep the selection on screen.
    let first = sel.saturating_sub(visible.saturating_sub(1));

    let lines: Vec<Line> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(i, s)| {
            let selected = i == *sel;
            let model = s
                .header
                .model
                .rsplit('/')
                .next()
                .unwrap_or("")
                .chars()
                .take(20)
                .collect::<String>();
            let meta = format!(
                "  {} {} {} msg {} {model}",
                session::ago(s.modified),
                g.sep,
                s.messages,
                g.sep
            );
            let room = inner_w.saturating_sub(meta.chars().count() + 3);
            let title: String = s.title.chars().take(room.max(8)).collect();
            // A tinted selection reads better than reverse video where the
            // theme knows its own colours; reverse video is the fallback.
            let style = match (selected, t.bg_selected) {
                (true, Some(bg)) => Style::default()
                    .fg(t.heading)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
                (true, None) => Style::default()
                    .fg(t.text)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
                (false, _) => t.body(),
            };
            Line::from(vec![
                Span::styled(
                    if selected {
                        format!(" {} ", g.prompt)
                    } else {
                        "   ".to_string()
                    },
                    t.fg(t.accent),
                ),
                Span::styled(title, style),
                Span::styled(meta, t.dim()),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.fg(t.border_focus))
        .title(Span::styled(
            format!(" {} saved session(s) ", list.len()),
            Style::default()
                .fg(t.border_focus)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(" ↑↓", t.fg(t.accent)),
            Span::styled(" choose  ", t.dim()),
            Span::styled("enter", t.fg(t.accent)),
            Span::styled(" open  ", t.dim()),
            Span::styled("esc", t.fg(t.accent)),
            Span::styled(" cancel ", t.dim()),
        ]));

    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn approval_popup(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.pending else { return };
    let t = &app.theme;
    let g = &app.glyphs;

    // A distinct, loud colour so an approval prompt is impossible to miss and
    // never reads as just another tool block: amber for a write, red for a
    // command (the higher-risk, exec tier).
    let (accent, verb, kind) = match p.name.as_str() {
        "run_command" => (t.error, "RUN COMMAND", "about to run a shell command"),
        "write_file" => (t.warning, "WRITE FILE", "about to write a file"),
        "edit_file" => (t.warning, "EDIT FILE", "about to edit a file"),
        _ => (t.warning, "APPROVE", "needs your approval"),
    };

    let max_w = area.width.saturating_sub(6).clamp(20, 110);
    let body_w = max_w.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(kind.to_string(), t.dim())));
    match &p.preview {
        Some(text) if p.name == "run_command" => {
            for l in md::hard_wrap(text, body_w) {
                lines.push(Line::from(vec![
                    Span::styled("$ ".to_string(), t.dim()),
                    Span::styled(l, t.emphasis(t.text)),
                ]));
            }
        }
        Some(text) => lines.extend(md::render_diff(text, body_w, t)),
        None => {
            for l in md::hard_wrap(&p.args_pretty, body_w) {
                lines.push(Line::from(Span::styled(l, t.body())));
            }
        }
    }

    // The action row: colour-coded keys, spelled out, always the last thing the
    // eye lands on. This is the part users said they could not find before.
    lines.push(Line::default());
    let key = |k: &str, label: &str, c: ratatui::style::Color| {
        vec![
            Span::styled(
                format!(" {k} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(c)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {label}   "), t.body()),
        ]
    };
    let mut action = Vec::new();
    action.extend(key("y", "yes, once", t.success));
    action.extend(key("a", "always", t.info));
    action.extend(key("n", "no", t.error));
    lines.push(Line::from(action));

    let content_w = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0) as u16;
    let w = (content_w + 4).clamp(48, max_w);
    let h = (lines.len() as u16 + 2).clamp(6, area.height.saturating_sub(2).max(6));
    // Dock it low-centre, just above the input, so it appears where the user's
    // attention already is rather than floating in the middle of the screen.
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: area.height.saturating_sub(h + 2).max(1),
        width: w,
        height: h,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Thick)
        .border_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" {} {verb} ", g.pending),
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ))
        .title_bottom(Span::styled(" ↑↓ scroll · esc = no ", t.dim()));

    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((p.scroll, 0)),
        rect,
    );
}

// ------------------------------------------------------------------- lifecycle

type Term = Terminal<ratatui::backend::CrosstermBackend<Stdout>>;

fn setup(mouse: bool) -> Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    let backend = ratatui::backend::CrosstermBackend::new(out);
    let mut term = Terminal::new(backend)?;
    term.clear()?;
    Ok(term)
}

pub fn restore() {
    let mut out = io::stdout();
    let _ = execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

pub async fn run(
    cfg: Arc<Config>,
    root: PathBuf,
    seed: Option<String>,
    resume: Option<Summary>,
) -> Result<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let mut agent = Agent::new(cfg.clone(), root.clone(), cancel.clone(), notify.clone())?;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();

    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if matches!(cmd, Command::Quit) {
                break;
            }
            agent.handle(cmd, &ev_tx).await;
        }
    });

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<event::Event>();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let th = theme::resolve(&cfg.theme);
    let gl = theme::glyphs(&cfg.icons);

    let mut app = App {
        cmd_tx,
        transcript: Transcript::new(th, gl),
        editor: Editor::default(),
        theme: th,
        glyphs: gl,
        scroll: 0,
        follow: true,
        busy: false,
        cancelling: false,
        motion: anim::Motion::Full,
        reveal_pref: cfg.reveal,
        turn_started: None,
        pending: None,
        asking: None,
        model: cfg.model.clone(),
        endpoint: cfg.endpoint(),
        tokens: 0,
        context_budget: cfg.context_tokens,
        auto_tier: if cfg.auto_approve { AutoTier::Full } else { cfg.auto_tier },
        web: cfg.web_search && !cfg.searx_url.trim().is_empty(),
        searx_configured: !cfg.searx_url.trim().is_empty(),
        mode: cfg.mode,
        plan_blocked: false,
        logs: None,
        log_version: 0,
        welcome_at: None,
        // Emission is decoupled from config alone: a terminal that mishandles
        // DEC 2026 (Apple Terminal, screen) must get neither the faster cadence
        // nor the markers, or it tears *worse*. Both the draw wrapper below and
        // the frame budget derive from this one capability-aware flag.
        sync_output: cfg.sync_output && anim::sync_trustworthy(),
        mouse_capture: cfg.mouse_capture,
        last_size: (0, 0),
        confirm: None,
        files: FileIndex::new(),
        mention_sel: 0,
        cmd_sel: 0,
        files_ready: false,
        picker: None,
        setup: None,
        settings: None,
        cfg: (*cfg).clone(),
        branch: git_branch(&root),
        root: root.clone(),
        queued: VecDeque::new(),
        pastes: Vec::new(),
        quit: false,
        last_ctrl_c: None,
        cancel: cancel.clone(),
        notify: notify.clone(),
        body_h: 20,
    };

    // Warm the file index off-thread at startup rather than lazily on the first
    // `@`, so the mention popup is ready the instant the user reaches for it
    // instead of after a scan-thread round trip.
    app.files.ensure(&root);

    let mut term = setup(cfg.mouse_capture)?;
    // The welcome card is laid out to a fixed width, so it needs the real
    // terminal size — which the first draw has not happened to report yet.
    if let Ok(size) = term.size() {
        app.last_size = (size.width, size.height);
    }
    app.show_welcome(&cfg);

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev_hook(info);
    }));

    // Resume before the seed prompt, so a seeded message continues the old
    // conversation rather than starting beside it.
    if let Some(s) = resume {
        app.open_session(&s);
    }

    if let Some(text) = seed {
        if !text.trim().is_empty() {
            app.transcript.user(text.clone());
            app.send(Command::User(text));
        }
    }

    // A clock rather than a ticker: it sleeps indefinitely until an animation
    // arms it, so an idle koda does no work at all.
    let motion = anim::Motion::resolve(cfg.motion, io::stdout().is_terminal());
    let mut clock = anim::Clock::new(anim::frame_budget(app.sync_output), motion);
    app.motion = motion;
    app.transcript.animate_reveal = motion.animates() && app.reveal_pref;
    let mut dirty = true;

    let result = loop {
        if dirty {
            // One atomic presentation per frame: without this the terminal can
            // show a half-applied diff, which is what mid-render tearing is.
            let sync = app.sync_output;
            if sync {
                let _ = execute!(io::stdout(), BeginSynchronizedUpdate);
            }
            let result = term.draw(|f| draw(f, &mut app));
            if sync {
                let _ = execute!(io::stdout(), EndSynchronizedUpdate);
            }
            if let Err(e) = result {
                break Err(anyhow::Error::from(e));
            }
            dirty = false;
        }
        if app.quit {
            break Ok(());
        }
        // The clock follows the app: running a turn, a live log view, or a file
        // index still scanning are the only things that need repainting.
        clock.sync(app.wants_frames());

        tokio::select! {
            biased;
            maybe = key_rx.recv() => match maybe {
                Some(ev) => dirty |= handle_term_event(&mut app, ev),
                None => break Ok(()),
            },
            maybe = ev_rx.recv() => match maybe {
                Some(ev) => { app.on_event(ev); dirty = true; }
                None => break Ok(()),
            },
            _ = clock.tick(), if clock.animating() => {
                clock.schedule();
                // Every animated surface derives its own phase from wall time,
                // so advancing is just telling the transcript what time it is.
                app.transcript.now = Instant::now();
                app.transcript.advance_reveal();
                dirty = true;
                let v = log::version();
                if v != app.log_version {
                    app.log_version = v;
                    if app.logs.is_some() {
                        dirty = true;
                    }
                }
                // The file index finishes scanning off-thread; if the user is
                // mid-`@` when it lands, the list has to appear without a
                // further keystroke.
                let indexed = app.files.ready();
                if indexed != app.files_ready {
                    crate::tel_debug!("ui", "file index ready", "files" => app.files.len());
                    app.files_ready = indexed;
                    dirty = true;
                }
            }
        }

        // Coalesce bursts (streaming tokens, key repeats) into one redraw.
        loop {
            let mut progressed = false;
            while let Ok(ev) = key_rx.try_recv() {
                progressed |= handle_term_event(&mut app, ev);
            }
            while let Ok(ev) = ev_rx.try_recv() {
                app.on_event(ev);
                progressed = true;
            }
            if !progressed {
                break;
            }
            dirty = true;
        }
    };

    let _ = app.cmd_tx.send(Command::Quit);
    restore();
    result
}

/// Returns false when the event changed nothing, so the caller can skip a redraw.
fn handle_term_event(app: &mut App, ev: event::Event) -> bool {
    match ev {
        event::Event::Key(k) => {
            app.on_key(k);
            true
        }
        event::Event::Paste(text) => {
            // A large paste (long or multi-line) is stashed and shown as a short
            // @pasteN token so the composer stays readable; it is expanded back
            // to the full text on submit. Small pastes insert inline as before.
            let trimmed = text.trim_end_matches('\n');
            let big = trimmed.len() > 200 || trimmed.contains('\n');
            if big {
                app.pastes.push(trimmed.to_string());
                let token = format!("@paste{}", app.pastes.len());
                let lines = trimmed.lines().count().max(1);
                app.editor.insert(&token);
                app.note(format!(
                    "pasted {} lines as {token} — it expands when you send",
                    lines
                ));
            } else {
                app.editor.insert(trimmed);
            }
            true
        }
        // Emulators send two or three resize events for one drag; only the ones
        // that actually change the size are worth a relayout.
        event::Event::Resize(w, h) => {
            if app.last_size == (w, h) {
                false
            } else {
                app.last_size = (w, h);
                true
            }
        }
        event::Event::Mouse(m) => match m.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_by(-3);
                true
            }
            MouseEventKind::ScrollDown => {
                app.scroll_by(3);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"hello world"), "aGVsbG8gd29ybGQ=");
    }


    #[test]
    fn token_display_is_compact() {
        assert_eq!(Tokens(940).to_string(), "940 tok");
        assert_eq!(Tokens(12400).to_string(), "12.4k tok");
    }

    #[test]
    fn model_name_is_tail_truncated_when_narrow() {
        let wide = Metrics::of(200);
        let narrow = Metrics::of(70);
        let m = "mtplx-qwen38-27b-bare-speed-fp16";
        assert_eq!(short_model(m, wide), m);
        let short = short_model(m, narrow);
        assert!(short.chars().count() <= 18, "{short}");
        assert!(short.ends_with('…'));
        assert_eq!(short_model("", wide), "no model");
    }

    #[test]
    fn truncate_line_never_exceeds_width() {
        let spans = vec![
            Span::raw("aaaaaa".to_string()),
            Span::raw("bbbbbb".to_string()),
        ];
        let line = truncate_line(spans, 8);
        let w: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(w, 8);
    }

    #[test]
    fn breakpoints_ladder() {
        assert!(!Metrics::of(120).compact);
        assert!(Metrics::of(80).compact);
        assert!(!Metrics::of(80).tiny);
        assert!(Metrics::of(50).tiny);
    }
}
