//! Terminal UI.
//!
//! Layout, outside in: a one-line header carrying persistent context, the
//! transcript (with a scrollbar only when it can actually scroll), a rule, the
//! input, and a single bottom line that merges state and contextual key hints.
//! There is exactly one border depth anywhere on screen — the terminal edge
//! already frames the app, so nothing else is boxed except a modal.

use crate::agent::{Agent, Approval, Command, Event};
use crate::anim;
use crate::config::{AutoTier, Config, Mode};
use crate::editor::Editor;
use crate::fuzzy::FileIndex;
use crate::log;
use crate::md;
use crate::panel::{self, Panel};
use crate::session::{self, Summary};
use crate::settings;
use crate::setup::{self, Setup};
use crate::theme::{self, Glyphs, Theme};
use crate::view::Transcript;

use anyhow::Result;
#[cfg(windows)]
use ratatui::crossterm::event::EnableMouseCapture;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::event::{DisableMouseCapture, MouseButton, MouseEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::VecDeque;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Notify};
use unicode_width::UnicodeWidthStr;

/// A spinner that flashes for instant work is noise, so hold it back briefly.
const SPINNER_DELAY: Duration = Duration::from_millis(200);
/// How long the welcome banner's entrance shimmer plays before it settles into
/// the static gradient. Kept short so it never delays getting to work.
const WELCOME_ANIM: Duration = Duration::from_millis(1400);

/// Light-hearted status messages shown while koda is working and no specific
/// tool activity is in flight. They rotate every ~10s so a long turn feels
/// alive and a little fun, rather than a frozen "working". Kept short so they
/// fit the one-line status even on a narrow terminal.
const WORKING_MSGS: &[&str] = &[
    "cooking",
    "summoning tokens",
    "reticulating splines",
    "thinking hard",
    "consulting the rubber duck",
    "untangling the logic",
    "warming up the neurons",
    "doing the needful",
    "chasing semicolons",
    "herding functions",
    "compiling thoughts",
    "reading between the lines",
    "aligning the bits",
    "brewing a fix",
    "poking the codebase",
    "connecting the dots",
    "wrangling edge cases",
    "still on it",
];

/// Pick a working message from the elapsed time so it advances every ~10s and
/// stays stable within each 10s window (no jitter between redraws).
fn working_message(elapsed: Duration) -> &'static str {
    let slot = (elapsed.as_secs() / 10) as usize;
    WORKING_MSGS[slot % WORKING_MSGS.len()]
}

/// The KODA banner art, shared by the static welcome and its entrance shimmer.
/// The KODA banner art, shared by the static welcome and its entrance shimmer.
/// A bold "ANSI Shadow" face with drop shadows — fancier and more striking than
/// a flat block face, while still compact.
const BANNER_ART: [&str; 6] = [
    "██╗  ██╗  ██████╗  ██████╗   █████╗ ",
    "██║ ██╔╝ ██╔═══██╗ ██╔══██╗ ██╔══██╗",
    "█████╔╝  ██║   ██║ ██║  ██║ ███████║",
    "██╔═██╗  ██║   ██║ ██║  ██║ ██╔══██║",
    "██║  ██╗ ╚██████╔╝ ██████╔╝ ██║  ██║",
    "╚═╝  ╚═╝  ╚═════╝  ╚═════╝  ╚═╝  ╚═╝",
];

const COMMANDS: &[(&str, &str)] = &[
    ("/help", "keys and commands"),
    ("/detailhelp", "open the full feature guide in your browser"),
    ("/keys", "keyboard shortcuts"),
    ("/model", "show or switch model"),
    ("/models", "list models on the server"),
    ("/mode", "plan, execute or vibe"),
    ("/logs", "what the agent has been doing"),
    ("/debug", "toggle raw request/response capture"),
    ("/watch", "watch files for AI! / AI? triggers"),
    ("/reason", "reasoning effort: off/low/medium/high"),
    ("/websearch", "turn web search on or off"),
    ("/skills", "list skills, or reload them from disk"),
    (
        "/learn",
        "review & accept what koda learned about this project",
    ),
    (
        "/orc",
        "run a task in vibe mode (spec, orchestrate, verify)",
    ),
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
    ("/provider", "list saved providers, or switch to one"),
    (
        "/mouse",
        "toggle mouse capture (on = wheel scrolls + drag selects)",
    ),
    ("/reveal", "toggle progressive text reveal"),
    ("/copy", "copy last reply to the clipboard"),
    ("/cwd", "show the workspace root"),
    ("/quit", "exit koda"),
];

/// Actions on the text you have already typed, opened with `#`.
///
/// These are editor operations, not agent requests: nothing here reaches the
/// model or costs a turn. The list is deliberately short — a palette people
/// scroll is one they stop opening.
const ACTIONS: &[(&str, &str)] = &[
    ("#copy", "copy the whole prompt to the clipboard"),
    ("#copyline", "copy the line the caret is on"),
    ("#cutline", "delete the line the caret is on"),
    ("#start", "move the caret to the beginning"),
    ("#end", "move the caret to the end"),
    ("#clear", "empty the input"),
    ("#undo", "put back what the last action removed"),
    ("#paste", "insert the clipboard's text"),
];

struct Pending {
    name: String,
    args_pretty: String,
    preview: Option<String>,
    reply: Option<oneshot::Sender<Approval>>,
    scroll: u16,
}

/// RGB components of a truecolor, for gradient maths. `None` for ANSI/named
/// colours, where a gradient is not meaningful and callers fall back to flat.
fn as_rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Alert a user who may have tabbed away: ring the terminal bell (an audible
/// beep in most terminals) and emit an OSC 9 desktop notification, which iTerm2,
/// Kitty, WezTerm and Ghostty surface as a system notification and others
/// silently ignore. Written straight to stdout so it works mid-frame.
fn notify_user(title: &str, body: &str) {
    use std::io::Write as _;
    // Keep the notification text to one line and a sane length.
    let msg: String = format!("{title}: {body}")
        .replace(['\n', '\r'], " ")
        .chars()
        .take(120)
        .collect();
    let mut out = std::io::stdout();
    // \x07 = BEL (sound); ESC ] 9 ; <text> BEL = OSC 9 notification.
    let _ = write!(out, "\x07\x1b]9;{msg}\x07");
    let _ = out.flush();
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

/// Which action a `choices` overlay selection performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChoiceKind {
    Mode,
    Model,
}

/// State for an in-flight `ask_user` question. When `options` is non-empty the
/// UI shows a dropdown (the last entry is always "type a custom answer…");
/// picking it sets `custom` so the next typed message is the answer.
struct Asking {
    question: String,
    options: Vec<String>,
    sel: usize,
    /// True once the user chose the custom-answer entry: the input becomes the
    /// answer field and the dropdown is dismissed.
    custom: bool,
    reply: oneshot::Sender<String>,
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
    /// Set while a compaction (`/compact` or auto-compact) is running. The
    /// prompt shows an animated "compacting…" status and holds input until the
    /// matching Compacted event arrives, so a slow summary call never looks like
    /// a frozen prompt. Carries the moment it started, for the elapsed clock.
    compacting: Option<Instant>,
    /// What the agent is doing right now (e.g. "reading cart.py", "running the
    /// tests"), from the latest tool start — shown in the working status so the
    /// user sees live activity, not a generic spinner.
    activity: Option<String>,
    /// How much motion the environment and config allow.
    motion: anim::Motion,
    /// User preference for the streaming text reveal specifically. Gated by
    /// `motion` — reveal only animates when both this and motion are on.
    reveal_pref: bool,
    turn_started: Option<Instant>,
    pending: Option<Pending>,
    /// A question the agent asked via `ask_user`, awaiting the user's next
    /// message. `(question, reply-channel)`.
    asking: Option<Asking>,
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
    /// Where the transcript text is drawn, so a mouse position can be turned
    /// back into a line and column. Filled in by `draw` each frame.
    text_area: Rect,
    /// Images pasted this turn, shown in the composer as a short `@imageN`
    /// token and expanded to their real paths on submit. The path is an
    /// implementation detail of getting the bytes to the model; it is not
    /// something the user typed, so it should not fill their input line.
    images: Vec<PathBuf>,
    /// An in-progress or finished drag selection, in absolute transcript
    /// coordinates: ((anchor_line, anchor_col), (cursor_line, cursor_col)).
    /// Absolute rather than screen-relative so scrolling mid-drag does not
    /// smear the selection across whatever happens to be under the pointer.
    selection: Option<((usize, usize), (usize, usize))>,
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
    /// Highlighted row in the `#` action palette.
    action_sel: usize,
    /// Whether the file index had finished at the last frame.
    files_ready: bool,
    /// Session picker: the list, and which row is selected.
    picker: Option<(Vec<Summary>, usize)>,
    /// A generic selectable list overlay (mode / model pickers): the choices,
    /// the selected index, and which kind so Enter applies the right action.
    choices: Option<(Vec<String>, usize, ChoiceKind)>,
    /// Set when the user ran `/model` with no argument: the next model list that
    /// arrives opens a picker rather than being printed.
    model_picker_pending: bool,
    /// Provider setup overlay.
    setup: Option<Setup>,
    /// Interactive settings overlay.
    settings: Option<settings::Settings>,
    /// Working copy of the config, edited by the setup screen.
    cfg: Config,
    root: PathBuf,
    branch: Option<String>,
    queued: VecDeque<String>,
    /// Files the user asked to watch via `/watch @file`, drained by the run
    /// loop into the Watcher. `watch_clear` signals `/unwatch` (drop all).
    watch_add: Vec<PathBuf>,
    watch_clear: bool,
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

    /// Turn mouse tracking on or off, keeping the live terminal state and the
    /// config in step. Capture is an escape sequence, not just a flag, so every
    /// route that changes it — /mouse, the settings page, the web rail — has to
    /// come through here or the terminal and the config drift apart.
    fn set_mouse_capture(&mut self, on: bool) {
        self.mouse_capture = on;
        self.cfg.mouse_capture = on;
        let mut out = std::io::stdout();
        if on {
            let _ = execute!(out, EnableMouseTracking);
        } else {
            // The disable is deliberately crossterm's, not the inverse of
            // EnableMouseTracking: it clears all five modes, so it also cleans
            // up after a terminal left in a wider mode by something else.
            let _ = execute!(out, DisableMouseCapture);
        }
    }

    /// The `#` action query at the caret: the `#` and anything typed after it.
    ///
    /// Anchored at the end of the buffer so it acts on a prompt already written
    /// — you type the request, then reach for `#` — and so a `#` in the middle
    /// of a sentence is left alone as ordinary text.
    fn action_query(&self) -> Option<String> {
        let buf = &self.editor.buf;
        let hash = buf.rfind('#')?;
        let tail = &buf[hash..];
        // Only a bare word may follow, so "fix #3 in the parser" is not a query.
        if tail[1..].chars().any(|c| c.is_whitespace()) {
            return None;
        }
        Some(tail.to_string())
    }

    /// The actions matching what has been typed after `#`.
    fn action_hits(&self) -> Vec<&'static (&'static str, &'static str)> {
        let Some(q) = self.action_query() else {
            return Vec::new();
        };
        ACTIONS.iter().filter(|(n, _)| n.starts_with(&q)).collect()
    }

    /// Run the chosen action, having first taken the `#…` query back out of the
    /// buffer so the prompt is exactly what the user wrote.
    fn apply_action(&mut self, name: &str) {
        if let Some(q) = self.action_query() {
            self.editor.backspace_n(q.len());
        }
        self.action_sel = 0;
        match name {
            "#copy" => {
                let text = self.editor.buf.trim().to_string();
                if text.is_empty() {
                    self.note("nothing to copy");
                } else {
                    match copy_to_clipboard(&text) {
                        Ok(()) => self.note(format!("copied {} characters", text.chars().count())),
                        Err(e) => self.note(format!("copy failed: {e}")),
                    }
                }
            }
            "#copyline" => {
                let line = self.editor.current_line().trim().to_string();
                if line.is_empty() {
                    self.note("this line is empty");
                } else {
                    match copy_to_clipboard(&line) {
                        Ok(()) => self.note("copied the line"),
                        Err(e) => self.note(format!("copy failed: {e}")),
                    }
                }
            }
            "#cutline" => {
                self.editor.checkpoint();
                let gone = self.editor.cut_line();
                if gone.trim().is_empty() {
                    self.note("cut an empty line");
                } else {
                    self.note("cut the line — #undo puts it back");
                }
            }
            "#start" => self.editor.start(),
            "#end" => self.editor.finish(),
            "#clear" => {
                self.editor.checkpoint();
                self.editor.clear();
                self.note("input cleared — #undo puts it back");
            }
            "#undo" => {
                if !self.editor.undo() {
                    self.note("nothing to undo");
                }
            }
            "#paste" => match clipboard_text() {
                Some(t) => self.paste_text(&t),
                None => self.note("nothing on the clipboard"),
            },
            other => self.note(format!("unknown action {other}")),
        }
    }

    /// Insert pasted text into the composer. A large paste (long or multi-line)
    /// is stashed and shown as a short `@pasteN` token so the composer stays
    /// readable; it expands back to the full text on submit.
    ///
    /// Shared by bracketed paste and ctrl+v so the two cannot drift apart.
    fn paste_text(&mut self, text: &str) {
        let trimmed = text.trim_end_matches('\n');
        if trimmed.is_empty() {
            return;
        }
        // A path, not prose: attach it instead of typing it out. This is how an
        // image paste actually arrives in most terminals.
        if let Some(path) = paste_as_path(trimmed) {
            if crate::tools::is_image_path(&path) {
                self.attach_image(path);
            } else {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                self.editor.insert(&format!("@{} ", path.display()));
                self.note(format!("pasted path {name} — it attaches when you send"));
            }
            return;
        }
        if trimmed.len() > 200 || trimmed.contains('\n') {
            self.pastes.push(trimmed.to_string());
            let token = format!("@paste{}", self.pastes.len());
            let lines = trimmed.lines().count().max(1);
            self.editor.insert(&token);
            self.note(format!(
                "pasted {lines} lines as {token} — it expands when you send"
            ));
        } else {
            self.editor.insert(trimmed);
        }
    }

    /// Note a pasted image and put a short token in the composer.
    ///
    /// The composer shows `@image1`, not a 90-character temp path: the user
    /// pasted a picture, and a wall of path is noise in the one line they are
    /// trying to write a sentence in. `submit` swaps it back for the real path,
    /// which is what the attach step matches on.
    fn attach_image(&mut self, path: PathBuf) {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.images.push(path);
        let n = self.images.len();
        self.editor.insert(&format!("@image{n} "));
        // "0 KB" for a small screenshot reads like something went wrong.
        let size = if bytes < 1024 {
            format!("{bytes} B")
        } else {
            format!("{} KB", bytes / 1024)
        };
        self.note(format!(
            "pasted image ({size}) as @image{n} — it attaches when you send"
        ));
    }

    /// Put a clipboard image in front of the model.
    ///
    /// koda already attaches an image written as `@path`, so the whole job is
    /// getting the bytes onto disk and naming them in the composer — the send
    /// path needs no special case for this at all. Returns false when the
    /// clipboard holds no image, so the caller can fall back to text.
    fn paste_clipboard_image(&mut self) -> bool {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // A temp file rather than the workspace: a pasted screenshot is not
        // something the user asked to have dropped in their repo.
        let dest = std::env::temp_dir().join(format!("koda-paste-{stamp}.png"));
        if clipboard_image(&dest).is_err() {
            return false;
        }
        self.attach_image(dest);
        true
    }

    /// Turn a mouse position into an absolute transcript coordinate.
    ///
    /// Returns None outside the transcript body, so a drag over the input line
    /// or the status bar does not start a selection in the text above it.
    fn point_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        let a = self.text_area;
        if a.width == 0 || a.height == 0 {
            return None;
        }
        if row < a.y || row >= a.y.saturating_add(a.height) || col < a.x {
            return None;
        }
        let line = self.scroll + (row - a.y) as usize;
        // Past the right edge counts as end-of-line rather than nothing, so
        // dragging off the side selects to the end the way it does everywhere.
        let col = (col - a.x) as usize;
        Some((line, col.min(a.width as usize)))
    }

    /// The selection ordered start-before-end, whichever way it was dragged.
    fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.selection?;
        if a == b {
            return None; // a click, not a drag
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, taken from the same lines the renderer drew.
    fn selected_text(&self) -> String {
        let Some(range) = self.selection_range() else {
            return String::new();
        };
        let (sl, el) = (range.0 .0, range.1 .0);
        let lines = self.transcript.window(sl, el - sl + 1);
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let chars: Vec<char> = text.chars().collect();
            if let Some((from, to)) = line_span(sl + i, range, chars.len()) {
                out.extend(&chars[from..to]);
            }
            if i + 1 < lines.len() {
                out.push('\n');
            }
        }
        // Trailing spaces come from the padded render, not from the text.
        out.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Finish a drag: copy what was selected and say so.
    fn finish_selection(&mut self) {
        let text = self.selected_text();
        self.selection = None;
        if text.trim().is_empty() {
            return;
        }
        let n = text.chars().count();
        match copy_to_clipboard(&text) {
            Ok(()) => self.note(format!("copied {n} characters")),
            Err(e) => self.note(format!("copy failed: {e}")),
        }
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
                self.activity = None;
                self.follow = true;
                self.turn_started = Some(Instant::now());
            }
            Event::Text(chunk) => {
                // The model is producing the reply — say so, so the status row
                // isn't stuck on a stale tool label or a generic quip.
                self.activity = Some("writing the reply".into());
                self.transcript.assistant_delta(&chunk);
            }
            Event::Reasoning(chunk) => {
                // Reasoning can run for many seconds before any visible output;
                // surface it so a thinking model never reads as a frozen app.
                self.activity = Some("thinking".into());
                self.transcript.reasoning_delta(&chunk);
            }
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
                // Surface what the agent is doing right now in the status row.
                // Inside a delegated subagent (depth>0) say so, so the user can
                // see the child is working — e.g. "↳ subagent: reading cart.py".
                let phrase = activity_label(&name, &label);
                self.activity = Some(if depth > 0 {
                    format!("↳ subagent: {phrase}")
                } else {
                    phrase
                });
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
                // Done with this tool — the model now decides the next step,
                // which can take a few seconds. Say so rather than dropping to a
                // generic quip that reads as idle.
                self.activity = Some("thinking about the next step".into());
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
            Event::AskUser {
                question,
                options,
                reply,
            } => {
                // Show the question as a distinct prose block. With options, the
                // user picks from a dropdown (a custom-answer entry is added);
                // otherwise their next typed message is the answer.
                self.transcript.finish_reveal();
                self.transcript
                    .assistant_delta(&format!("\n**{question}**\n"));
                let custom = options.is_empty();
                self.asking = Some(Asking {
                    question: question.clone(),
                    options,
                    sel: 0,
                    custom,
                    reply,
                });
                self.follow = true;
                // Alert a user who has tabbed away: ring the terminal bell and
                // post an OSC 9 desktop notification (honoured by iTerm2, Kitty,
                // WezTerm, Ghostty…; ignored elsewhere).
                notify_user("koda needs you", &question);
            }
            Event::Tokens(n) => self.tokens = n,
            Event::NeedsExecuteMode(_) => self.plan_blocked = true,
            Event::Todos(items) => {
                self.transcript.todos(items);
                self.follow = true;
            }
            Event::Notice(msg) => {
                // While the provider setup is open, a probe result (e.g. an
                // unreachable server) belongs in the overlay, not the transcript
                // behind it — so the user sees why the model list is empty and
                // can fix the URL or just type the model name.
                if let Some(s) = &mut self.setup {
                    s.status = Some(msg);
                } else {
                    self.note(msg);
                }
            }
            Event::SubActivity(what) => {
                // A subagent is alive and doing something — surface it in the
                // status row so the user isn't staring at a frozen "working".
                self.activity = Some(format!("↳ subagent: {what}"));
                self.follow = true;
            }
            Event::Compacting => {
                self.compacting = Some(Instant::now());
                self.cancelling = false;
                self.note("compacting context…");
                self.follow = true;
            }
            Event::Compacted { after, .. } => {
                self.compacting = None;
                self.cancelling = false;
                self.tokens = after;
                // Anything the user typed while compaction ran was queued rather
                // than lost or dropped into a frozen prompt. Send the first now
                // that history is ready; the rest flush on the next TurnEnd.
                if !self.busy {
                    if let Some(next) = self.queued.pop_front() {
                        self.send(Command::User(next));
                    }
                }
            }
            Event::Error(msg) => {
                self.transcript.error(msg);
                self.follow = true;
            }
            Event::Models(list) => {
                if let Some(s) = &mut self.setup {
                    s.status = Some(if list.is_empty() {
                        "couldn't list models — check the URL/key, or just type the model name and press enter".into()
                    } else {
                        format!("{} model(s) — ctrl+n to cycle", list.len())
                    });
                    s.available = list;
                } else if self.model_picker_pending {
                    self.model_picker_pending = false;
                    if list.is_empty() {
                        self.note(
                            "no models reported — check the endpoint (/url) and key (/setup)",
                        );
                    } else {
                        let cur = list.iter().position(|m| m == &self.model).unwrap_or(0);
                        self.choices = Some((list, cur, ChoiceKind::Model));
                    }
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
                self.activity = None;
                self.turn_started = None;
                self.tokens = history_tokens;
                // Prompts the user queued while koda was working are picked up
                // here, once the current task's tool calls have all finished.
                // Frame them so the agent folds them into its plan rather than
                // treating each as an unrelated cold start — matching "add to the
                // end of the todo list and work on it after the current task."
                if let Some(next) = self.queued.pop_front() {
                    self.transcript.user(next.clone());
                    let framed = if self.queued.is_empty() {
                        format!(
                            "While you were working, I added this task. Add it to your todo \
                             list and do it now:\n\n{next}"
                        )
                    } else {
                        // More still queued — tell it to enqueue them all in order.
                        format!(
                            "While you were working, I added this task (more follow). Append \
                             it to your todo list and work through them in order:\n\n{next}"
                        )
                    };
                    self.send(Command::User(framed));
                } else {
                    // Truly done — no more queued work. Retire any lingering plan
                    // so the sticky panel and step counter clear, even if the
                    // model forgot to flip the last step to done.
                    self.transcript.complete_current_plan();
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
            || self.compacting.is_some()
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
                format!(
                    "Type /setup, or run `koda models` to see what {} has.",
                    host_of(&self.endpoint)
                ),
                t.dim(),
            )]);
            let lines = p.render(&t, &g);
            self.transcript.raw(lines);
            self.follow = true;
            return;
        }

        // KODA in a condensed half-block face with a horizontal 3-stop colour
        // gradient (accent → accent-alt → info) so it reads as one lit object.
        // Falls back to a flat accent on non-truecolor palettes (ANSI/mono).
        let art = BANNER_ART;
        let cols = art.iter().map(|r| r.chars().count()).max().unwrap_or(1);
        let grad = |col: usize| -> ratatui::style::Color {
            match (as_rgb(t.accent), as_rgb(t.accent_alt), as_rgb(t.info)) {
                (Some(a), Some(b), Some(c)) => {
                    // 0..1 across the width; first half a→b, second half b→c.
                    let x = col as f32 / cols.max(1) as f32;
                    let (r, g, bl) = if x < 0.5 {
                        anim::lerp_rgb(a, b, x * 2.0)
                    } else {
                        anim::lerp_rgb(b, c, (x - 0.5) * 2.0)
                    };
                    ratatui::style::Color::Rgb(r, g, bl)
                }
                _ => t.accent,
            }
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::default());
        for row in art.iter() {
            let mut spans = vec![Span::raw("  ".to_string())];
            for (j, ch) in row.chars().enumerate() {
                if ch == ' ' {
                    spans.push(Span::raw(" ".to_string()));
                } else {
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(grad(j)).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());

        // A tagline + a compact quick-start, so the empty state guides a new user
        // instead of leaving a banner floating over dead space. Centered under the
        // banner, styled entirely from the theme (no hard-coded colours) so it
        // adapts to every palette and the ASCII glyph set.
        let indent = "  ".to_string();
        let model_short: String = cfg.model.chars().take(28).collect();
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(
                "a fast terminal coding agent".to_string(),
                t.emphasis(t.accent_alt),
            ),
            Span::styled(format!("  {}  {}", g.sep, model_short), t.dim()),
        ]));
        lines.push(Line::default());
        // Quick-start tips: the few things a new user most needs, one per line,
        // key highlighted in the accent, description dimmed.
        let tips: [(&str, &str); 4] = [
            (
                "type a task",
                "and press enter — e.g. \"fix the failing test\"",
            ),
            ("@", "attach a file to your message"),
            ("/help", "see all commands"),
            ("ctrl+p", "switch mode (plan · execute · vibe)"),
        ];
        for (key, desc) in tips {
            lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(format!("{} ", g.bullet), t.dim()),
                Span::styled(
                    format!("{key:<12}"),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {desc}"), t.dim()),
            ]));
        }
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
        let mut p = Panel::new(format!("Models on {}", host_of(&self.endpoint)), width)
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
        if self.choices.is_some() {
            self.choices_key(key);
            return;
        }
        // Ask-user dropdown: navigate/select options until the user picks the
        // custom-answer entry, after which typing flows to the input as normal.
        if self
            .asking
            .as_ref()
            .map(|a| !a.custom && !a.options.is_empty())
            .unwrap_or(false)
        {
            self.asking_key(key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // The file list steals navigation keys while it is open.
        if !self.mention_hits().is_empty() && !ctrl && !alt {
            match key.code {
                KeyCode::Up => {
                    self.mention_sel = self.mention_sel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    let n = self.mention_hits().len();
                    self.mention_sel = (self.mention_sel + 1).min(n.saturating_sub(1));
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
        // The `#` palette takes the arrows and enter while it is open, the way
        // the `/` list below does. Esc closes it and leaves the text alone.
        let actions = self.action_hits();
        if !actions.is_empty() && !ctrl && !alt {
            match key.code {
                KeyCode::Up => {
                    self.action_sel = self.action_sel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.action_sel = (self.action_sel + 1).min(actions.len() - 1);
                    return;
                }
                KeyCode::Esc => {
                    // Take the query out but keep the prompt: the point of esc
                    // is to change your mind, not to lose what you wrote.
                    if let Some(q) = self.action_query() {
                        self.editor.backspace_n(q.len());
                    }
                    self.action_sel = 0;
                    return;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    let pick = actions[self.action_sel.min(actions.len() - 1)].0;
                    self.apply_action(pick);
                    return;
                }
                _ => {}
            }
        }

        let cmds = self.command_matches();
        if cmds.len() > 1 && !ctrl && !alt {
            // If what's typed is already an exact command (e.g. "/mode" while
            // "/model" and "/models" also match), Enter should RUN it, not
            // complete to a longer neighbour. Tab still completes.
            let typed = self.editor.buf.trim();
            let exact = cmds.contains(&typed);
            match key.code {
                KeyCode::Up => {
                    self.cmd_sel = self.cmd_sel.saturating_sub(1);
                    return;
                }
                KeyCode::Down => {
                    self.cmd_sel = (self.cmd_sel + 1).min(cmds.len() - 1);
                    return;
                }
                KeyCode::Enter if exact => {
                    // Fall through to normal submit below.
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
                self.follow |= self.transcript.toggle_tools_pref();
                let on = self.transcript.expand_tools;
                self.note(if on {
                    "tool output expanded (stays until ctrl+r)"
                } else {
                    "tool output collapsed"
                });
            }
            KeyCode::Char('p') if ctrl => self.cycle_mode(),
            KeyCode::Char('t') if ctrl => {
                self.follow |= self.transcript.toggle_reasoning_pref();
                let on = self.transcript.show_reasoning;
                self.note(if on {
                    "reasoning shown (stays until ctrl+t)"
                } else {
                    "reasoning hidden"
                });
            }
            // clippy suggests collapsing this into the match guard. Do not: see
            // below.
            #[allow(clippy::collapsible_match)]
            // Not a match guard: pasting the image is the work, not a test for
            // whether this arm applies. As a guard, a successful image paste
            // makes the arm *not* match, and the literal "v" falls through to
            // the catch-all and lands in the composer.
            KeyCode::Char('v') if ctrl => {
                // Images first: that is the case the terminal cannot deliver on
                // its own. Text then behaves exactly as a bracketed paste would.
                if !self.paste_clipboard_image() {
                    match clipboard_text() {
                        Some(t) => self.paste_text(&t),
                        None => self.note("nothing on the clipboard"),
                    }
                }
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
            // Scroll the transcript a line at a time. Ctrl+Up/Down works in many
            // terminals, but macOS grabs Ctrl+Up/Down for Mission Control, so
            // Shift+Up/Down is offered as a reliable alternative. The mouse wheel
            // and PageUp/PageDown are the primary, always-working scroll paths.
            KeyCode::Up if ctrl || shift => self.scroll_by(-1),
            KeyCode::Down if ctrl || shift => self.scroll_by(1),
            // Plain Up / Down walk the user's typed-message history.
            KeyCode::Up => self.key_up(),
            KeyCode::Down => self.key_down(),
            KeyCode::PageUp => self.scroll_by(-(self.body_h as isize / 2).max(1)),
            KeyCode::PageDown => self.scroll_by((self.body_h as isize / 2).max(1)),
            KeyCode::Esc => {
                if self.busy || self.compacting.is_some() {
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

    /// Key handling for the ask_user dropdown (options + custom-answer entry).
    fn asking_key(&mut self, key: KeyEvent) {
        let Some(a) = self.asking.as_mut() else {
            return;
        };
        // The last row is always the custom-answer entry.
        let total = a.options.len() + 1;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => a.sel = a.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => a.sel = (a.sel + 1).min(total - 1),
            // Number keys 1-9 jump to (and select) an option directly.
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                if idx < a.options.len() {
                    let answer = a.options[idx].clone();
                    let asking = self.asking.take().unwrap();
                    self.transcript.user(answer.clone());
                    let _ = asking.reply.send(answer);
                    self.follow = true;
                } else if idx == a.options.len() {
                    // The custom-answer row number.
                    a.custom = true;
                    a.sel = a.options.len();
                    self.note("type your answer and press enter");
                }
            }
            KeyCode::Esc => {
                // Cancel the picker: fall back to a custom free-text answer.
                a.custom = true;
                a.sel = a.options.len();
                self.note("type your answer and press enter");
            }
            KeyCode::Enter => {
                if a.sel < a.options.len() {
                    // Chose a concrete option — answer immediately.
                    let answer = a.options[a.sel].clone();
                    let asking = self.asking.take().unwrap();
                    self.transcript.user(answer.clone());
                    let _ = asking.reply.send(answer);
                    self.follow = true;
                } else {
                    // Chose "custom answer" — switch to free-text input.
                    a.custom = true;
                    self.note("type your answer and press enter");
                }
            }
            _ => {}
        }
    }

    /// Key handling for the generic mode/model picker overlay.
    fn choices_key(&mut self, key: KeyEvent) {
        let Some((list, sel, kind)) = &mut self.choices else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.choices = None,
            KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                *sel = (*sel + 1).min(list.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let kind = *kind;
                let chosen = list.get(*sel).cloned();
                self.choices = None;
                if let Some(val) = chosen {
                    match kind {
                        ChoiceKind::Mode => {
                            if let Ok(m) = val.parse::<Mode>() {
                                self.set_mode(m);
                            }
                        }
                        ChoiceKind::Model => {
                            self.model = val.clone();
                            self.send(Command::SetModel(val));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn setup_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // A toggle field is a choice, not a value: left/right and space step it,
        // and typing must not reach the editor behind it -- a half-typed "of"
        // would otherwise be saved and read back as "auto".
        if let Some(s) = self.setup.as_mut() {
            if s.focus.is_toggle() {
                match key.code {
                    KeyCode::Left => {
                        s.cycle_vision(false);
                        return;
                    }
                    KeyCode::Right | KeyCode::Char(' ') => {
                        s.cycle_vision(true);
                        return;
                    }
                    KeyCode::Char(_) if !ctrl => return,
                    KeyCode::Backspace => return,
                    _ => {}
                }
            }
        }
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
                // The agent keeps its own copy of the config, and endpoint and
                // model are the only two it is told about by name. Anything else
                // set here -- the images toggle -- would otherwise be saved to
                // disk and ignored until the next start.
                self.send(Command::UpdateConfig(Box::new(cfg.clone())));
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
        let Some(s) = self.settings.as_mut() else {
            return;
        };
        // Inline text editor open (SearXNG URL, system prompt): capture typing.
        if s.editing.is_some() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            let multiline = s.editing_multiline();
            match key.code {
                // In the multi-line system-prompt editor, enter inserts a
                // newline and ctrl+s saves; a single-line field saves on enter.
                KeyCode::Enter if multiline && !ctrl => s.edit_char('\n'),
                KeyCode::Char('s') if ctrl => {
                    s.edit_commit();
                    self.apply_settings();
                }
                KeyCode::Enter => {
                    s.edit_commit();
                    self.apply_settings();
                }
                KeyCode::Char('j') if ctrl => s.edit_char('\n'),
                KeyCode::Char('u') if ctrl => s.edit_clear(),
                KeyCode::Esc => s.edit_cancel(),
                KeyCode::Backspace => s.edit_backspace(),
                KeyCode::Char(c) => s.edit_char(c),
                _ => {}
            }
            return;
        }
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
                            Err(e) => self
                                .transcript
                                .error(format!("could not save settings: {e}")),
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
        let Some(cfg) = self.settings.as_ref().map(|s| s.cfg.clone()) else {
            return;
        };
        self.adopt_config(cfg);
    }

    /// Adopt an edited config into the live app and agent. Shared by the
    /// settings overlay and the web control rail, so both routes behave
    /// identically.
    fn adopt_config(&mut self, cfg: crate::config::Config) {
        // Theme.
        if cfg.theme != self.cfg.theme {
            let th = theme::resolve(&cfg.theme);
            self.set_theme(th);
        }
        // Motion / reveal.
        self.motion = if cfg.motion {
            anim::Motion::Full
        } else {
            anim::Motion::Reduced
        };
        self.reveal_pref = cfg.reveal;
        self.transcript.animate_reveal = self.motion.animates() && self.reveal_pref;
        if !self.transcript.animate_reveal {
            self.transcript.finish_reveal();
        }
        // Mode + autonomy: mirror into app state and tell the agent.
        if cfg.mode != self.mode {
            self.set_mode(cfg.mode);
        }
        // Model / endpoint: the agent holds its own copies, so they need telling.
        if cfg.model != self.cfg.model {
            self.model = cfg.model.clone();
            self.send(Command::SetModel(cfg.model.clone()));
        }
        if cfg.base_url != self.cfg.base_url {
            self.send(Command::SetEndpoint(cfg.endpoint()));
        }
        if cfg.auto_tier != self.auto_tier {
            self.auto_tier = cfg.auto_tier;
            self.send(Command::SetAutoTier(cfg.auto_tier));
        }
        // Provider: switching endpoint and model is the whole point, and the
        // agent holds its own copies of both, so they have to be sent.
        if cfg.active_provider != self.cfg.active_provider {
            let mut next = cfg.resolved();
            next.providers = cfg.providers.clone();
            next.active_provider = cfg.active_provider.clone();
            self.endpoint = next.endpoint();
            self.model = next.model.clone();
            self.send(Command::SetEndpoint(next.endpoint()));
            self.send(Command::SetModel(next.model.clone()));
            self.note(format!("provider → {}", cfg.provider_label()));
            // Carry the resolved view forward so the rest of this function and
            // everything after it sees the provider's settings, not the
            // top-level ones it just replaced.
            self.cfg = next.clone();
        }
        // Mouse capture: a terminal mode rather than a stored flag, so a change
        // has to be written out, not just recorded.
        if cfg.mouse_capture != self.mouse_capture {
            self.set_mouse_capture(cfg.mouse_capture);
        }
        // Debug capture: flip the global switch as soon as it's toggled.
        crate::debug::set_enabled(cfg.debug);
        // Web search + backend: mirror to the agent so the tool availability and
        // backend choice take effect this turn.
        if cfg.web_search != self.cfg.web_search
            || cfg.search_backend != self.cfg.search_backend
            || cfg.searx_url != self.cfg.searx_url
            || cfg.web_fetch != self.cfg.web_fetch
            || cfg.ocr != self.cfg.ocr
            || cfg.vision != self.cfg.vision
            || cfg.reasoning_effort != self.cfg.reasoning_effort
            || cfg.system_prompt != self.cfg.system_prompt
        {
            self.send(Command::UpdateConfig(Box::new(cfg.clone())));
        }
        self.cfg = cfg;
    }

    /// Apply everything the web control center has asked for since the last
    /// poll. The browser can only queue requests; this is where they become real
    /// changes, on the same thread as every other state change, so there is one
    /// path into the live session rather than two.
    fn drain_web_control(&mut self) -> bool {
        if !crate::webui::has_control() {
            return false;
        }
        let mut changed = false;
        for c in crate::webui::take_control() {
            changed = true;
            match c {
                crate::webui::Control::Config(cfg) => {
                    self.adopt_config(*cfg);
                    self.note("web: settings applied");
                }
                crate::webui::Control::Learn(action) => {
                    self.send(Command::Learn(action));
                }
                crate::webui::Control::Remember(note) => {
                    self.send(Command::RememberNote(note));
                }
                crate::webui::Control::Forget(needle) => {
                    self.send(Command::ForgetNote(needle));
                }
                crate::webui::Control::Resume(path) => {
                    // Rebuild the visible transcript from the file, exactly as
                    // the session picker does, so the UI matches the agent.
                    match crate::session::read(&path) {
                        Ok((header, messages)) => {
                            self.transcript.restore(&messages);
                            self.scroll = 0;
                            self.follow = true;
                            self.send(Command::Resume(path));
                            self.note(format!("web: resumed session {}", header.id));
                        }
                        Err(e) => self
                            .transcript
                            .error(format!("could not read that session: {e}")),
                    }
                }
            }
        }
        changed
    }

    /// Returns true when the key belonged to the log overlay.
    fn log_key(&mut self, key: KeyEvent) -> bool {
        let Some(scroll) = self.logs else {
            return false;
        };
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
            Mode::Vibe => "vibe — spec-driven: plans, delegates, and verifies its own work",
        };
        self.note(explain);
    }

    fn key_up(&mut self) {
        // Multi-line composing: move the caret up within the input first, so
        // editing a pasted block still works naturally.
        if !self.editor.on_first_line() {
            self.editor.up();
            return;
        }
        // Otherwise, plain Up recalls the previous message the user typed.
        // (Transcript scrolling is Ctrl+Up / PageUp / the mouse wheel.)
        self.editor.history_prev();
    }

    fn key_down(&mut self) {
        if !self.editor.on_last_line() {
            self.editor.down();
            return;
        }
        // Plain Down walks forward through typed-message history.
        self.editor.history_next();
    }

    fn interrupt(&mut self) {
        if self.busy {
            self.cancel.store(true, Ordering::Relaxed);
            self.notify.notify_waiters();
            self.cancelling = true;
            self.note("interrupting…");
            return;
        }
        if self.compacting.is_some() {
            self.cancel.store(true, Ordering::Relaxed);
            self.notify.notify_waiters();
            self.cancelling = true;
            self.note("cancelling compaction…");
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
        // `!cmd` runs a shell command directly — no agent, no tokens. So you can
        // `!git status`, `!ls`, `!git commit -m ...` without leaving koda.
        if let Some(cmd) = trimmed.strip_prefix('!') {
            let cmd = cmd.trim().to_string();
            if !cmd.is_empty() {
                self.run_bang(&cmd);
            }
            return;
        }
        // `$ code` runs Python the same way: straight to the interpreter, no
        // agent and no tokens. Reaching for a calculator or a one-line parse
        // should not cost a turn, and the answer is the interpreter's rather
        // than a model's guess at what the interpreter would say.
        if let Some(code) = trimmed.strip_prefix('$') {
            let code = code.trim().to_string();
            if !code.is_empty() {
                self.run_python(&code);
            }
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
        if !self.images.is_empty() {
            for (i, path) in self.images.iter().enumerate() {
                let token = format!("@image{}", i + 1);
                if trimmed.contains(&token) {
                    trimmed = trimmed.replace(&token, &format!("@{}", path.display()));
                }
            }
            self.images.clear();
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            // ...unless it is a path. No command name contains a slash, while a
            // pasted absolute path is nothing but slashes, and dispatching it
            // threw the message away with "unknown command". Arguments may still
            // contain slashes (`/url http://host/v1`), so only the command word
            // is examined.
            let word = rest.split_whitespace().next().unwrap_or("");
            if !word.contains('/') {
                self.slash(rest);
                return;
            }
        }
        // If the agent asked a question, this message is the answer, not a new
        // turn. Echo it and hand it to the waiting tool.
        if let Some(asking) = self.asking.take() {
            self.transcript.user(trimmed.clone());
            self.follow = true;
            let _ = asking.reply.send(trimmed);
            return;
        }
        self.transcript.user(trimmed.clone());
        self.follow = true;
        self.plan_blocked = false;
        if self.busy || self.compacting.is_some() {
            self.queued.push_back(trimmed);
            self.note(format!("queued ({})", self.queued.len()));
        } else {
            self.send(Command::User(trimmed));
        }
    }

    /// Run a `!cmd` shell command directly: echo it and hand it to the agent's
    /// command channel, which runs it as a tool block without any model call.
    fn run_bang(&mut self, cmd: &str) {
        self.transcript.user(format!("!{cmd}"));
        self.follow = true;
        if self.busy {
            self.note("busy — wait for the current turn, then try !cmd again");
            return;
        }
        self.send(Command::Bang(cmd.to_string()));
    }

    /// Run a snippet of Python and show its output, without involving the agent.
    ///
    /// Routed through the same command path as `!`, so it inherits the approval
    /// rules, the timeout and the transcript block -- and, like `!`, it never
    /// enters the conversation the model sees.
    fn run_python(&mut self, code: &str) {
        self.transcript.user(format!("$ {code}"));
        self.follow = true;
        if self.busy {
            self.note("busy — wait for the current turn, then try $ again");
            return;
        }
        // -c takes the program as one argument, so the snippet is passed
        // through a heredoc instead: it keeps quotes and newlines intact
        // without a layer of shell escaping to get wrong.
        let script = format!("{} <<'KODA_PY_EOF'\n{}\nKODA_PY_EOF", python_bin(), code);
        self.send(Command::Bang(script));
    }

    fn slash(&mut self, rest: &str) {
        let mut parts = rest.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_ascii_lowercase();
        let arg = parts.next().unwrap_or("").trim().to_string();

        match cmd.as_str() {
            "help" | "?" => self.show_help(),
            "detailhelp" | "guide" => match crate::detailhelp::open(COMMANDS) {
                Ok(path) => self.note(format!(
                    "opened the full guide in your browser ({})",
                    path.display()
                )),
                Err(e) => self
                    .transcript
                    .error(format!("could not open the guide: {e}")),
            },
            "keys" => self.show_help(),
            "model" => {
                if arg.is_empty() {
                    // No argument: fetch the model list and open a picker.
                    self.model_picker_pending = true;
                    self.note("fetching models…");
                    self.send(Command::ListModels);
                } else {
                    self.model = arg.clone();
                    self.send(Command::SetModel(arg));
                }
            }
            "models" => self.send(Command::ListModels),
            "mode" => match arg.as_str() {
                "" => {
                    // No argument: open a selectable list of the three modes,
                    // pre-selecting the current one.
                    let modes = vec![
                        "plan".to_string(),
                        "execute".to_string(),
                        "vibe".to_string(),
                    ];
                    let cur = modes
                        .iter()
                        .position(|m| m == &self.mode.to_string())
                        .unwrap_or(0);
                    self.choices = Some((modes, cur, ChoiceKind::Mode));
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
            "setup" | "config" => {
                self.setup = Some(Setup::new(&self.cfg));
                self.send(Command::ProbeModels(self.endpoint.clone()));
            }
            "settings" | "preferences" | "prefs" => {
                self.settings = Some(settings::Settings::new(&self.cfg));
            }
            "orc" | "orchestrate" => {
                if arg.is_empty() {
                    self.note(
                        "usage: /orc <task> — runs it in vibe mode (spec → orchestrate → verify)",
                    );
                    return;
                }
                // Orc is now folded into vibe mode: vibe already writes a spec,
                // plans with `todo`, delegates substantial subtasks to role
                // agents, and verifies its own work. So switch to vibe and run
                // the task there, rather than maintaining a separate orc concept.
                if self.mode != Mode::Vibe {
                    self.set_mode(Mode::Vibe);
                    self.send(Command::SetMode(Mode::Vibe));
                    self.note("switched to vibe mode");
                }
                self.transcript.user(arg.to_string());
                self.follow = true;
                if self.busy || self.compacting.is_some() {
                    self.queued.push_back(arg.to_string());
                    self.note(format!("queued ({})", self.queued.len()));
                } else {
                    self.send(Command::User(arg.to_string()));
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
                self.web = !self.web;
                let v = self.web;
                self.send(Command::SetWebSearch(v));
                if v {
                    let backend = if self.searx_configured {
                        "your SearXNG instance"
                    } else {
                        "DuckDuckGo"
                    };
                    self.note(format!("web search on — using {backend}"));
                } else {
                    self.note("web search off");
                }
            }
            "logs" | "log" => {
                self.logs = Some(u16::MAX); // clamped to the tail when drawn
                if let Some(p) = log::file_path() {
                    crate::tel_debug!("ui", "opened log view", "file" => p.display());
                }
            }
            "debug" => {
                // Toggle developer request/response capture, then show where the
                // artifacts land so the user can go read them.
                let on = !crate::debug::enabled();
                crate::debug::set_enabled(on);
                self.cfg.debug = on;
                let mut lines = vec![Line::from(Span::styled(
                    format!("debug capture {}", if on { "on" } else { "off" }),
                    self.theme.emphasis(if on {
                        self.theme.success
                    } else {
                        self.theme.warning
                    }),
                ))];
                for l in crate::debug::report().lines() {
                    lines.push(Line::from(Span::styled(l.to_string(), self.theme.dim())));
                }
                self.transcript.raw(lines);
                self.follow = true;
            }
            "watch" => {
                // `/watch` toggles whole-workspace watch; `/watch @file …` (or
                // `/watch path …`) scopes watching to specific files.
                let paths: Vec<String> = arg
                    .split_whitespace()
                    .map(|p| p.trim_start_matches('@').to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
                if !paths.is_empty() {
                    for p in &paths {
                        let abs = if std::path::Path::new(p).is_absolute() {
                            PathBuf::from(p)
                        } else {
                            self.root.join(p)
                        };
                        self.watch_add.push(abs);
                    }
                    self.cfg.watch = true;
                    self.note(format!(
                        "watching {} file(s) — add an AI! / AI? comment and koda acts when idle (/unwatch to stop)",
                        paths.len()
                    ));
                } else {
                    self.cfg.watch = !self.cfg.watch;
                    let on = self.cfg.watch;
                    if on {
                        self.note(
                            "watch on (whole workspace) — end a comment with AI! to implement it, \
                             or AI? to ask. Or /watch @file to scope it. koda acts when idle.",
                        );
                    } else {
                        self.watch_clear = true;
                        self.note("watch off");
                    }
                }
            }
            "unwatch" => {
                self.cfg.watch = false;
                self.watch_clear = true;
                self.note("watch off — cleared all watched files");
            }
            "reason" | "reasoning" => {
                // /reason [off|low|medium|high] — cycle if no arg given.
                let next = match arg.trim().to_ascii_lowercase().as_str() {
                    "" => match self.cfg.reasoning_effort.as_str() {
                        "off" => "low",
                        "low" => "medium",
                        "medium" => "high",
                        _ => "off",
                    }
                    .to_string(),
                    other @ ("off" | "low" | "medium" | "high") => other.to_string(),
                    _ => {
                        self.note("usage: /reason [off|low|medium|high]");
                        return;
                    }
                };
                self.cfg.reasoning_effort = next.clone();
                self.send(Command::UpdateConfig(Box::new(self.cfg.clone())));
                self.note(format!("reasoning effort → {next}"));
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
            "fam" | "fullauto" | "yolo" => {
                // One-shot switch straight to full-auto: approve everything, no
                // prompts. Shown in red in the status bar so it's never a surprise.
                self.auto_tier = AutoTier::Full;
                self.send(Command::SetAutoTier(AutoTier::Full));
                self.note("full-auto — approving everything, no prompts (/auto to dial back)");
            }
            "learn" => {
                use crate::agent::LearnAction;
                let mut it = arg.split_whitespace();
                let action = match it.next().unwrap_or("") {
                    "" | "review" | "show" => LearnAction::Review,
                    "all" => LearnAction::Accept(None),
                    "accept" | "ok" | "yes" => {
                        match it.next().and_then(|n| n.parse::<usize>().ok()) {
                            Some(n) => LearnAction::Accept(Some(n)),
                            None => LearnAction::Accept(None),
                        }
                    }
                    "reject" | "no" | "drop" => {
                        match it.next().and_then(|n| n.parse::<usize>().ok()) {
                            Some(n) => LearnAction::Reject(n),
                            None => {
                                self.note("usage: /learn reject <n>");
                                return;
                            }
                        }
                    }
                    _ => {
                        self.note("usage: /learn [accept <n> | all | reject <n>]");
                        return;
                    }
                };
                self.send(Command::Learn(action));
            }
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
                let mut p =
                    Panel::new("Tools", width).footer("the agent picks these · ● asks approval");
                for spec in crate::tools::specs() {
                    let desc: String = spec
                        .desc
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(p.inner().saturating_sub(18))
                        .collect();
                    // A dot marks tools that pause for your approval.
                    let mark = if spec.mutating {
                        Span::styled("● ".to_string(), self.theme.fg(self.theme.warning))
                    } else {
                        Span::styled("  ".to_string(), self.theme.dim())
                    };
                    p.row(vec![
                        mark,
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
            "provider" | "providers" => {
                if arg == "add" || arg == "new" {
                    // new_provider, not new: `new` pre-fills the active
                    // provider's name, so saving updated that provider instead
                    // of adding one — there was no way to get a second.
                    self.setup = Some(setup::Setup::new_provider(&self.cfg));
                    self.note("give it a name — that is what saves it as a new provider");
                } else if self.cfg.providers.is_empty() {
                    self.note("no saved providers — /provider add, then give it a name");
                } else if arg.is_empty() {
                    let width = self.panel_width();
                    let mut p = Panel::new("Providers", width)
                        .footer("/provider <name> to switch · /provider add to add one");
                    for prov in self.cfg.providers.clone() {
                        let here = prov.name == self.cfg.active_provider;
                        p.row(vec![
                            Span::styled(
                                if here { "● " } else { "  " }.to_string(),
                                self.theme.fg(self.theme.success),
                            ),
                            Span::styled(
                                format!("{:<14}", prov.name),
                                self.theme.fg(self.theme.accent),
                            ),
                            Span::styled(
                                format!("{}  {}", host_of(&prov.base_url), prov.model),
                                self.theme.dim(),
                            ),
                        ]);
                    }
                    let lines = p.render(&self.theme, &self.glyphs);
                    self.transcript.raw(lines);
                    self.follow = true;
                } else if self.cfg.providers.iter().any(|p| p.name == arg) {
                    // Resolve through the same path the loader uses, then carry
                    // the list and the choice across, so switching cannot
                    // flatten the providers away.
                    let mut cfg = self.cfg.clone();
                    cfg.active_provider = arg.clone();
                    let mut next = cfg.resolved();
                    next.providers = cfg.providers.clone();
                    next.active_provider = cfg.active_provider.clone();
                    self.adopt_config(next);
                    match crate::config::save(&self.cfg) {
                        Ok(_) => self.note(format!("provider → {arg}")),
                        Err(e) => self
                            .transcript
                            .error(format!("switched, but could not save: {e}")),
                    }
                } else {
                    self.note(format!("no provider called {arg} — /provider to list them"));
                }
            }
            "mouse" | "select" => {
                // Toggling capture off hands click-drag back to the terminal so
                // the user can select and copy text; on restores wheel-scroll.
                let on = !self.mouse_capture;
                self.set_mouse_capture(on);
                if on {
                    self.note("mouse capture on — the wheel scrolls, drag selects and copies");
                } else {
                    self.note(format!(
                        "mouse capture off — the terminal handles selection; \
                         pgup/pgdn scrolls ({} no longer needed)",
                        select_override()
                    ));
                }
                // Unlike the other toggles this one is a persisted preference:
                // someone who wants to select text wants it in every session,
                // not just this one, and rediscovering /mouse each launch is
                // the whole reason selection felt broken in the first place.
                if let Err(e) = crate::config::save(&self.cfg) {
                    self.transcript
                        .error(format!("could not save settings: {e}"));
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
                    th.accent,
                    th.accent_alt,
                    th.success,
                    th.warning,
                    th.error,
                    th.info,
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
            ("wheel / pgup/pgdn", "scroll the reply"),
            (
                "shift+↑/↓",
                "scroll a line (ctrl+↑/↓ too, where the OS allows)",
            ),
            ("up/down", "previous / next message you typed"),
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
            ("/reason high", "reasoning effort: off/low/medium/high"),
            ("/watch", "act on AI! / AI? comment triggers"),
            (
                "/orc build a login page",
                "split the task across role agents",
            ),
            ("/settings", "edit everything (system prompt, web UI, …)"),
            ("/theme tokyo-night", "switch palette by name"),
            (
                "@src/main.rs",
                "attach a file, image, PDF, spreadsheet or doc",
            ),
            ("!git status", "run a shell command directly (no agent)"),
            ("$ print(2**10)", "run Python directly (no agent)"),
            ("/detailhelp", "open the full feature guide in your browser"),
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

        let mut k =
            Panel::new("Keys", width).footer("/detailhelp opens the full guide in your browser");
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
        ("pbcopy", &[][..]),                     // macOS
        ("clip", &[]),                           // Windows
        ("wl-copy", &[]),                        // Linux/Wayland
        ("xclip", &["-selection", "clipboard"]), // Linux/X11
        ("xsel", &["--clipboard", "--input"]),   // Linux/X11 alt
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

/// The half-open character range of line `abs` that a selection covers, or None
/// when the line is outside it.
///
/// Shared by the renderer and the extractor: if they disagreed by one character
/// the user would copy something other than what they saw highlighted.
fn line_span(
    abs: usize,
    ((sl, sc), (el, ec)): ((usize, usize), (usize, usize)),
    len: usize,
) -> Option<(usize, usize)> {
    if abs < sl || abs > el {
        return None;
    }
    let (from, to) = match (abs == sl, abs == el) {
        (true, true) => (sc.min(len), ec.min(len)),
        (true, false) => (sc.min(len), len),
        (false, true) => (0, ec.min(len)),
        (false, false) => (0, len),
    };
    (from < to).then_some((from, to))
}

/// Paint the selected range over the already-rendered window.
///
/// The lines are restyled rather than re-rendered: they are what the transcript
/// actually drew, so a selection can never disagree with what is on screen.
fn highlight_selection(
    lines: &mut [Line<'static>],
    scroll: usize,
    range: ((usize, usize), (usize, usize)),
    theme: &Theme,
) {
    for (i, line) in lines.iter_mut().enumerate() {
        let abs = scroll + i;
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let chars: Vec<char> = text.chars().collect();
        let Some((from, to)) = line_span(abs, range, chars.len()) else {
            continue;
        };
        // Rebuilt as three spans so the highlight lands on exactly the selected
        // characters; the original styling of the rest is not preserved, which
        // is the trade for keeping this a plain slice of the rendered text.
        let head: String = chars[..from].iter().collect();
        let mid: String = chars[from..to].iter().collect();
        let tail: String = chars[to..].iter().collect();
        *line = Line::from(vec![
            Span::styled(head, theme.body()),
            Span::styled(mid, theme.body().add_modifier(Modifier::REVERSED)),
            Span::styled(tail, theme.body()),
        ]);
    }
}

/// Warn about a custom system prompt big enough to dominate every request.
///
/// The built-in prompt is around 2.5 KB. Anything past this is either a
/// deliberate and very large prompt -- worth knowing the price of -- or
/// something that got in by accident.
fn oversized_prompt_warning(prompt: &str) -> Option<String> {
    const LIMIT: usize = 20_000;
    let n = prompt.len();
    (n > LIMIT).then(|| {
        format!(
            "your custom system prompt is {} KB (~{} tokens) and is sent with every \
             message — clear it with /settings → system prompt, or edit system_prompt \
             in {}",
            n / 1024,
            n / 4,
            crate::config::config_path().display()
        )
    })
}

/// The Python to run for `$`. Prefers python3, since `python` is Python 2 on
/// some older systems and absent entirely on others.
fn python_bin() -> &'static str {
    fn has(bin: &str) -> bool {
        std::process::Command::new(bin)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    if has("python3") {
        "python3"
    } else {
        "python"
    }
}

/// A paste that is really a file path.
///
/// Several terminals answer an image paste by writing the image to a temp file
/// and pasting *its path* as text — that is what koda actually receives, not
/// image bytes. Left as literal text the path sits in the composer doing
/// nothing, and an absolute one is then read as a slash command
/// ("unknown command /var/folders/.../clipboard-....png"). Recognising it lets
/// the existing `@path` attachment do the rest.
fn paste_as_path(text: &str) -> Option<PathBuf> {
    let t = text.trim();
    if t.is_empty() || t.contains('\n') {
        return None;
    }
    // Some terminals paste a percent-encoded file:// URL rather than a path.
    let path = match t.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => t.to_string(),
    };
    let p = Path::new(&path);
    // Absolute and real: a bare word that happens to name something in the
    // workspace is far more likely to be text the user meant to type.
    (p.is_absolute() && p.is_file()).then(|| p.to_path_buf())
}

/// Decode `%XX` escapes in a file:// URL. Bytes are collected first so a
/// multi-byte UTF-8 character split across escapes survives.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pull an image off the system clipboard into `dest`.
///
/// Bracketed paste is a *text* channel. When the clipboard holds a screenshot
/// there is nothing for the terminal to send, so koda never receives a paste
/// event at all and the keystroke just looks broken. The bytes are sitting on
/// the clipboard the whole time — they have to be fetched rather than waited
/// for, which is what this does.
fn clipboard_image(dest: &Path) -> Result<()> {
    use std::process::{Command as Proc, Stdio};
    let _ = std::fs::remove_file(dest);

    #[cfg(target_os = "macos")]
    {
        // pbpaste is text-only and yields nothing for an image. osascript is the
        // dependency-free way in, but `-e` mangles the «class PNGf» chevrons on
        // their way through the shell, so the script goes over stdin instead.
        let script = format!(
            "set d to (the clipboard as «class PNGf»)\n\
             set fh to open for access POSIX file \"{}\" with write permission\n\
             set eof fh to 0\n\
             write d to fh\n\
             close access fh\n",
            dest.display()
        );
        if let Ok(mut child) = Proc::new("osascript")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(si) = child.stdin.as_mut() {
                let _ = si.write_all(script.as_bytes());
            }
            let _ = child.wait();
        }
    }

    #[cfg(windows)]
    {
        // Windows has no clipboard CLI of its own, so PowerShell and WinForms
        // do the work: GetImage() returns the bitmap, Save() writes the PNG.
        // Quoting the path in single quotes keeps a space in the temp path from
        // splitting the argument.
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
             $i=[Windows.Forms.Clipboard]::GetImage(); \
             if($i){{$i.Save('{}',[System.Drawing.Imaging.ImageFormat]::Png)}}",
            dest.display()
        );
        let _ = Proc::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        // Wayland and X11 both hand the bytes back on stdout.
        for (bin, args) in [
            ("wl-paste", &["--type", "image/png"][..]),
            (
                "xclip",
                &["-selection", "clipboard", "-t", "image/png", "-o"],
            ),
        ] {
            if let Ok(out) = Proc::new(bin).args(args).stderr(Stdio::null()).output() {
                if out.status.success() && !out.stdout.is_empty() {
                    let _ = std::fs::write(dest, &out.stdout);
                    break;
                }
            }
        }
    }

    // Every branch above is best-effort and stays quiet when it fails, so the
    // file is the real test: no image on the clipboard leaves nothing behind,
    // or an empty stub.
    let ok = std::fs::read(dest)
        .map(|b| b.len() > 8 && b.starts_with(&[0x89, b'P', b'N', b'G']))
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(dest);
        anyhow::bail!("no image on the clipboard");
    }
    Ok(())
}

/// The clipboard's text, for the ctrl+v path. The terminal delivers this by
/// itself on a normal paste; this is only needed when koda asks.
fn clipboard_text() -> Option<String> {
    use std::process::{Command as Proc, Stdio};
    for (bin, args) in [
        ("pbpaste", &[][..]), // macOS
        // Windows: no clipboard CLI, so PowerShell reads it. -Raw keeps a
        // multi-line paste in one piece instead of an array of lines.
        (
            "powershell",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        ),
        ("wl-paste", &["--no-newline"]),               // Wayland
        ("xclip", &["-selection", "clipboard", "-o"]), // X11
        ("xsel", &["--clipboard", "--output"]),        // X11 alt
    ] {
        if let Ok(out) = Proc::new(bin).args(args).stderr(Stdio::null()).output() {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    None
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
    // A roomier composer: it grows well before it starts scrolling. When empty
    // it stays a two-line field so it reads as a real input box, and it can
    // expand to a tall field as you type.
    let max_input = if m.tiny { 6 } else { 14 };
    let min_input = if app.editor.is_empty() {
        if m.tiny {
            1
        } else {
            2
        }
    } else {
        3
    };
    let input_h = rows.len().saturating_add(1).clamp(min_input, max_input) as u16;

    // A one-row gap between the transcript/hint area and the input keeps the
    // last line of tool output (a long command, a diff) from colliding with the
    // input and bottom bar — the user can always read the full final line.
    let spacer = if m.tiny { 0 } else { 1 };

    // Sticky plan: while a multi-step task is in progress, pin the todo list just
    // above the input so it stays visible and updates in place as steps finish —
    // even after it has scrolled out of the transcript. Hidden when there is no
    // plan or every step is done. Capped so a long plan can't eat the screen.
    let sticky = app.transcript.current_todos().filter(|items| {
        !items.is_empty()
            && items
                .iter()
                .any(|i| i.status != crate::tools::TodoStatus::Done)
    });
    let plan_h: u16 = if m.tiny {
        0
    } else {
        sticky
            .as_ref()
            .map(|items| (items.len() as u16).min(6) + 1) // +1 header row
            .unwrap_or(0)
    };

    let chunks = Layout::vertical([
        Constraint::Min(1),          // transcript
        Constraint::Length(plan_h),  // sticky plan (0 when none)
        Constraint::Length(1),       // hint / state row (status + keys)
        Constraint::Length(spacer),  // breathing room above the input
        Constraint::Length(input_h), // input
        Constraint::Length(1),       // powerline status bar (mode + model)
    ])
    .split(area);
    let (body, plan_area, rule, input, status) =
        (chunks[0], chunks[1], chunks[2], chunks[4], chunks[5]);

    // Transcript, with a one-column scrollbar reserved only when it scrolls.
    // Decide scrollability from the total at the ACTUAL text width, not a stale
    // count: reserve no gutter, lay out, and only if that overflows do we
    // reserve the gutter and re-lay-out at the narrower width. Doing it in this
    // order keeps the reserved gutter and the real line count consistent, so the
    // scrollbar can never paint over the last column of text (and never flickers
    // on/off between frames from a stale total).
    let full_w = body.width.saturating_sub(2); // left pad (1) + right pad (1)
    let mut total = app.transcript.relayout(full_w);
    let mut gutter = 0u16;
    if total > body.height as usize {
        gutter = 1;
        total = app.transcript.relayout(full_w.saturating_sub(1));
    }
    let text_area = Rect {
        x: body.x + 1,
        y: body.y,
        width: full_w.saturating_sub(gutter),
        height: body.height,
    };
    app.body_h = text_area.height as usize;
    app.text_area = text_area;
    let max_scroll = total.saturating_sub(app.body_h);
    if app.follow {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
    }
    let mut window = app.transcript.window(app.scroll, app.body_h);
    if let Some(range) = app.selection_range() {
        highlight_selection(&mut window, app.scroll, range, &app.theme);
    }
    f.render_widget(Paragraph::new(window), text_area);
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
    if plan_h > 0 {
        if let Some(items) = sticky {
            draw_sticky_plan(f, plan_area, app, &items);
        }
    }
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
        let (label, style) = if app.asking.is_some() {
            // koda asked a question — make the input read as an answer field.
            (
                "type your answer and press enter".to_string(),
                t.emphasis(t.accent),
            )
        } else {
            ("ask, or /help for commands".to_string(), t.dim())
        };
        vec![Line::from(vec![
            Span::styled(format!("{} ", g.prompt), prompt_style),
            Span::styled(label, style),
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
    // Pad up to the full box height so the whole taller field is one solid
    // tinted surface, not a single lit line with dead space beneath it.
    let mut input_lines = input_lines;
    while input_lines.len() < input_h as usize {
        input_lines.push(Line::from(vec![Span::raw("  ".to_string())]));
    }
    f.render_widget(
        Paragraph::new(panel::fill(input_lines, area.width as usize, t.bg_panel, 1)),
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

    let actions = app.action_hits();
    let mention = app.mention_hits();
    if !actions.is_empty() {
        action_popup(f, app, input, &actions);
    } else if !mention.is_empty() {
        mention_popup(f, app, input, &mention);
    } else if app.editor.buf.starts_with('/') && !app.editor.buf.contains(' ') {
        command_popup(f, app, input);
    }
    if app.picker.is_some() {
        session_picker(f, app, area);
    }
    if app.choices.is_some() {
        choices_popup(f, app, area);
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
    // A focused question card when koda asked something and nothing else is up.
    if app.asking.is_some()
        && app.pending.is_none()
        && app.setup.is_none()
        && app.settings.is_none()
        && app.logs.is_none()
        && app.picker.is_none()
    {
        asking_popup(f, app, area);
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
        if app.cfg.log_detail {
            log::Level::Debug
        } else {
            log::Level::Info
        },
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
        lines.push(Line::from(Span::styled("nothing logged yet", t.dim())));
    }

    // Default to the tail: the newest entry is what you came to read.
    let max_scroll = lines.len().saturating_sub(inner_h) as u16;
    let scroll = app.logs.unwrap_or(0).min(max_scroll);
    app.logs = Some(scroll);

    let (warns, errors) = log::counts();
    let title = match log::file_path() {
        Some(p) => format!(
            " logs {} {} warn {} error {} {} ",
            g.sep,
            warns,
            errors,
            g.sep,
            p.display()
        ),
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
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), rect);
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

    // Compaction runs outside the normal turn (no `busy`), so give it its own
    // animated status ahead of the ready/working match — otherwise the prompt
    // would read "ready" while the summary call is still in flight.
    if let Some(started) = app.compacting {
        let glyph = if app.motion.animates() {
            g.thinking[anim::sweep(started.elapsed()) % g.thinking.len()]
        } else {
            g.thinking[0]
        };
        let tint = if app.cancelling { t.warning } else { t.accent };
        left.push(Span::styled(format!(" {glyph} "), t.fg(tint)));
        let verb = if app.cancelling {
            "cancelling compaction"
        } else {
            "compacting context"
        };
        let label = format!("{verb}…  {}", anim::short_elapsed(started.elapsed()));
        if app.motion.animates() && !app.cancelling {
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
            left.push(Span::styled(
                label,
                if app.cancelling {
                    t.emphasis(t.warning)
                } else {
                    t.dim()
                },
            ));
        }
    } else {
        match (app.busy, app.turn_started) {
            (true, _) if app.cancelling => {
                // The interrupt landed but the turn is still unwinding (a tool call
                // in flight, a stream draining). Say so, in the warning tint, rather
                // than showing the ordinary "working" state.
                let glyph = if app.motion.animates() {
                    g.thinking[anim::sweep(
                        app.turn_started.map(|s| s.elapsed()).unwrap_or_default(),
                    ) % g.thinking.len()]
                } else {
                    g.thinking[0]
                };
                left.push(Span::styled(format!(" {glyph} "), t.fg(t.warning)));
                left.push(Span::styled(
                    "cancelling…".to_string(),
                    t.emphasis(t.warning),
                ));
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
                // Show what the agent is doing right now (reading X, editing Y). When
                // no specific tool activity is in flight, rotate a light-hearted
                // message every ~10s so a long turn feels alive rather than frozen.
                let verb = app
                    .activity
                    .as_deref()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| working_message(started.elapsed()).to_string());
                let label = format!("{verb}  {}", anim::short_elapsed(started.elapsed()));
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
                left.push(Span::styled(
                    format!(" {} ", g.ready),
                    t.emphasis(t.success),
                ));
                left.push(Span::styled("ready".to_string(), t.dim()));
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
        &[("y", "allow"), ("a", "always"), ("n", "decline")]
    } else if app.asking.is_some() {
        &[("type", "your answer"), ("enter", "send")]
    } else if app.picker.is_some() || app.setup.is_some() {
        &[("↑↓", "move"), ("enter", "choose"), ("esc", "cancel")]
    } else if app.plan_blocked {
        &[("ctrl+p", "switch to execute")]
    } else if app.busy || app.compacting.is_some() {
        // Two distinct states, one thing the user can do about either.
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

/// A short present-tense phrase for what a tool is doing, shown live in the
/// working status. `label` is the tool's own summary (e.g. a path or command);
/// we pair it with a verb so the user sees "reading cart.py", "running tests".
fn activity_label(name: &str, label: &str) -> String {
    let target: String = label.trim().chars().take(48).collect();
    let verb = match name {
        "read_file" => "reading",
        "write_file" => "writing",
        "edit_file" => "editing",
        "list_dir" => "listing",
        "find_files" => "finding files",
        "search" => "searching",
        "run_command" => "running",
        "codegraph" => "mapping the code",
        "delegate" => "delegating",
        "web_search" => "searching the web",
        "skill" => "reading a skill",
        "remember" => "noting",
        "ask_user" => "waiting for you",
        "todo" => "planning",
        _ => "working on",
    };
    if target.is_empty() {
        verb.to_string()
    } else {
        format!("{verb} {target}")
    }
}

/// The bottom bar: model, project, branch, context. Chevron-separated segments,
/// each in its own colour, so the fields are distinguishable at a glance.
fn powerline(app: &App, width: u16, m: Metrics) -> Line<'static> {
    use panel::Segment;
    let t = &app.theme;
    let g = &app.glyphs;

    let mut segs = vec![];

    // While a turn is running, surface what the agent is doing right now (the
    // running tool / command / subagent) in the persistent bottom bar, so the
    // user can always see it even when the transcript has scrolled the tool
    // block out of view. It leads the bar so it's the first thing read.
    if app.busy {
        if let Some(act) = &app.activity {
            let glyph = if app.motion.animates() {
                g.thinking[anim::sweep(app.turn_started.map(|s| s.elapsed()).unwrap_or_default())
                    % g.thinking.len()]
            } else {
                g.thinking[0]
            };
            // Keep it short so the model/mode/tokens still fit on the right.
            let cap = if m.tiny {
                16
            } else if m.compact {
                24
            } else {
                40
            };
            let text: String = act.chars().take(cap).collect();
            let text = if act.chars().count() > cap {
                format!("{text}…")
            } else {
                text
            };
            segs.push(Segment::new(format!("{glyph} {text}"), t.accent).bold());
        }
    }

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
        // A named provider replaces the host: "omniroute" says more at a glance
        // than "localhost:20128", and the name is the thing the user chose.
        let label = if app.cfg.active().is_some() {
            app.cfg.provider_label()
        } else {
            host_of(&app.endpoint)
        };
        segs.push(Segment::new(label, t.muted));
    }

    let mut right = Vec::new();
    // Model name and the current mode both live in the bottom-right corner now.
    right.push(Segment::new(short_model(&app.model, m), t.accent).bold());
    let mode_colour = match app.mode {
        Mode::Plan => t.warning,
        Mode::Execute => t.success,
        Mode::Vibe => t.accent_alt,
    };
    right.push(Segment::new(app.mode.label().to_string(), mode_colour).bold());
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
            right.push(Segment::new(
                format!("{}  {pct}%", Tokens(app.tokens)),
                t.muted,
            ));
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
    let pos = (app.scroll * (h - thumb))
        .checked_div(max_scroll)
        .unwrap_or(0);
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

/// The sticky plan panel pinned above the input while a multi-step task runs.
/// Compact: a header with live progress, then one line per task with the same
/// status glyphs as the transcript card. The active step is highlighted; done
/// steps are struck through. Capped to a few rows so it never dominates.
fn draw_sticky_plan(f: &mut Frame, rect: Rect, app: &App, items: &[crate::tools::Todo]) {
    use crate::tools::TodoStatus;
    let t = &app.theme;
    let g = &app.glyphs;
    if rect.height == 0 {
        return;
    }
    let done = items
        .iter()
        .filter(|i| i.status == TodoStatus::Done)
        .count();
    let total = items.len();
    let avail = rect.width.saturating_sub(4) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Header row: a pin-style marker, "Plan", and live progress.
    lines.push(Line::from(vec![
        Span::styled(format!("{} ", g.pending), t.fg(t.accent)),
        Span::styled("Plan".to_string(), t.emphasis(t.heading)),
        Span::styled(format!("  {done}/{total} done"), t.dim()),
    ]));

    // Show the tasks; if there are more than fit, keep the active one in view by
    // windowing around it, and note how many are hidden.
    let cap = (rect.height as usize).saturating_sub(1).max(1);
    let active = items
        .iter()
        .position(|i| i.status == TodoStatus::Active)
        .unwrap_or(0);
    let start = if items.len() <= cap {
        0
    } else {
        active.saturating_sub(cap / 2).min(items.len() - cap)
    };
    let shown = &items[start..(start + cap).min(items.len())];
    for it in shown {
        let (glyph, gstyle, tstyle) = match it.status {
            TodoStatus::Done => (
                g.ok,
                t.emphasis(t.success),
                t.dim().add_modifier(Modifier::CROSSED_OUT),
            ),
            TodoStatus::Active => (
                g.running,
                t.emphasis(t.warning),
                t.body().add_modifier(Modifier::BOLD),
            ),
            TodoStatus::Pending => (g.pending, t.dim(), t.dim()),
        };
        let text: String = it.text.chars().take(avail.saturating_sub(3)).collect();
        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph} "), gstyle),
            Span::styled(text, tstyle),
        ]));
    }

    f.render_widget(Paragraph::new(lines), rect);
}

/// The `#` action palette, drawn above the input like the other popups.
fn action_popup(
    f: &mut Frame,
    app: &App,
    input: Rect,
    hits: &[&'static (&'static str, &'static str)],
) {
    if hits.is_empty() || input.y == 0 {
        return;
    }
    let t = &app.theme;
    let rows = hits.len().min(8);
    let h = rows as u16 + 2;
    let y = input.y.saturating_sub(h);
    let w = input.width.min(56);
    let rect = Rect {
        x: input.x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);

    let sel = app.action_sel.min(hits.len().saturating_sub(1));
    let lines: Vec<Line> = hits
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, (name, desc))| {
            let on = i == sel;
            let mark = if on { "▸ " } else { "  " };
            Line::from(vec![
                Span::styled(mark, t.fg(t.accent)),
                Span::styled(
                    format!("{:<11}", name),
                    if on {
                        t.emphasis(t.accent)
                    } else {
                        t.fg(t.accent)
                    },
                ),
                Span::styled((*desc).to_string(), t.dim()),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(t.dim())
        .title(Span::styled(" edit ", t.dim()))
        .title_bottom(Span::styled(" ↑↓ pick · enter run · esc cancel ", t.dim()));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

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
    let start = if sel >= max_rows {
        sel + 1 - max_rows
    } else {
        0
    };
    let shown: Vec<&&(&str, &str)> = hits.iter().skip(start).take(max_rows).collect();
    let inner_w = input.width.max(10) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, (name, desc)) in shown.iter().map(|h| **h).enumerate() {
        let idx = start + i;
        let selected = idx == sel;
        let marker = if selected { "›" } else { " " };
        let desc: String = desc
            .chars()
            .take(inner_w.saturating_sub(name_w + 6))
            .collect();
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
    let (Some(a), Some(b), Some(c)) = (as_rgb(t.accent), as_rgb(t.accent_alt), as_rgb(t.info))
    else {
        return; // No gradient on non-truecolor palettes; nothing to shimmer.
    };
    let cols = BANNER_ART
        .iter()
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(1);
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
            // Same horizontal 3-stop gradient as the static banner.
            let x = j as f32 / cols.max(1) as f32;
            let (mut r, mut gg, mut bl) = if x < 0.5 {
                anim::lerp_rgb(a, b, x * 2.0)
            } else {
                anim::lerp_rgb(b, c, (x - 0.5) * 2.0)
            };
            // Lift toward white where the sweeping band is brightest.
            let lift = bright.get(j).copied().unwrap_or(0.0);
            if lift > 0.0 {
                let (wr, wg, wb) = anim::lerp_rgb((r, gg, bl), (255, 255, 255), lift * 0.85);
                r = wr;
                gg = wg;
                bl = wb;
            }
            spans.push(Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(Color::Rgb(r, gg, bl))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let rect = Rect {
            x: text_area.x,
            y,
            width: text_area.width,
            height: 1,
        };
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
    let Some((list, sel)) = &app.picker else {
        return;
    };
    let t = &app.theme;
    let g = &app.glyphs;

    let w = area.width.saturating_sub(6).clamp(40, 96).min(area.width);
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

/// A generic centered picker for the /mode and /model overlays.
fn choices_popup(f: &mut Frame, app: &App, area: Rect) {
    let Some((list, sel, kind)) = &app.choices else {
        return;
    };
    let t = &app.theme;
    let g = &app.glyphs;
    let title = match kind {
        ChoiceKind::Mode => "select mode",
        ChoiceKind::Model => "select model",
    };
    let w = area.width.saturating_sub(6).clamp(30, 80).min(area.width);
    let rows = (list.len() as u16).clamp(1, 14);
    let h = (rows + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let inner_w = rect.width.saturating_sub(4) as usize;
    let visible = h.saturating_sub(2) as usize;
    let first = sel.saturating_sub(visible.saturating_sub(1));
    let lines: Vec<Line> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(i, s)| {
            let selected = i == *sel;
            let marker = if selected { g.pick } else { " " };
            let shown: String = s.chars().take(inner_w).collect();
            let style = if selected {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                t.body()
            };
            Line::from(vec![
                Span::styled(format!(" {marker} "), t.fg(t.accent)),
                Span::styled(shown, style),
            ])
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(t.fg(t.border_focus))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(t.border_focus)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(vec![
            Span::styled(" ↑↓", t.fg(t.accent)),
            Span::styled(" choose  ", t.dim()),
            Span::styled("enter", t.fg(t.accent)),
            Span::styled(" select  ", t.dim()),
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

    // Action row FIRST, so it is always visible even when the previewed diff or
    // command output is long — the user should never have to scroll to the
    // bottom to find the allow/deny choices.
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
    action.extend(key("y", "allow once", t.success));
    action.extend(key("a", "always allow", t.info));
    action.extend(key("n", "decline", t.error));
    lines.push(Line::from(action));
    lines.push(Line::from(Span::styled(kind.to_string(), t.dim())));
    lines.push(Line::default());

    // Then the payload the choice is about.
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

    let content_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0) as u16;
    let w = (content_w + 4).clamp(48, max_w);
    let h = (lines.len() as u16 + 2).clamp(6, area.height.saturating_sub(4).max(6));
    // Center it — it's a modal decision, so it belongs in the middle of the
    // screen where the eye lands, dimmed context behind it, not tucked at the
    // bottom edge.
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
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

    // Dim the whole screen behind the modal so the decision stands out.
    if let Some(bg) = t.bg_panel {
        f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    }
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(block).scroll((p.scroll, 0)),
        rect,
    );
}

/// A focused question card shown when koda calls `ask_user`. The answer is the
/// user's next message (routed to the waiting tool), so this just presents the
/// question prominently and points at the input — a one-shot, unmissable prompt
/// rather than a line of text lost in the transcript.
fn asking_popup(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let Some(a) = &app.asking else { return };
    let t = &app.theme;
    let g = &app.glyphs;

    let max_w = area.width.saturating_sub(6).clamp(28, 100);
    let body_w = max_w.saturating_sub(6) as usize;

    let picking = !a.options.is_empty() && !a.custom;
    let mut lines: Vec<Line> = Vec::new();

    // Question header — wrapped, emphasized, like oh-my-pi's dialog header.
    for l in md::hard_wrap(&a.question, body_w) {
        lines.push(Line::from(Span::styled(l, t.emphasis(t.text))));
    }
    lines.push(Line::default());

    if picking {
        // Options as a radio list: a cursor arrow on the focused row, a filled
        // dot for the selection, a hollow dot otherwise — mirrors oh-my-pi's
        // single-select rows. Number keys 1-9 quick-select.
        let custom_idx = a.options.len();
        let radio_on = if g.fine_blocks { "●" } else { "*" };
        let radio_off = if g.fine_blocks { "○" } else { "o" };
        for (i, opt) in a.options.iter().enumerate() {
            let focused = i == a.sel;
            let cursor = if focused {
                format!("{} ", g.pick)
            } else {
                "  ".into()
            };
            let marker = if focused { radio_on } else { radio_off };
            let color = if focused { t.accent } else { t.text };
            let num = t.dim();
            let shown: String = opt.chars().take(body_w.saturating_sub(6)).collect();
            lines.push(Line::from(vec![
                Span::styled(cursor, t.fg(t.accent)),
                Span::styled(
                    format!("{marker} "),
                    t.fg(if focused { t.accent } else { t.muted }),
                ),
                Span::styled(format!("{}. ", i + 1), num),
                Span::styled(
                    shown,
                    if focused {
                        Style::default().fg(color).add_modifier(Modifier::BOLD)
                    } else {
                        t.fg(color)
                    },
                ),
            ]));
        }
        // The "type your own" row, always last (oh-my-pi's "Other").
        let focused = a.sel == custom_idx;
        let cursor = if focused {
            format!("{} ", g.pick)
        } else {
            "  ".into()
        };
        let marker = if focused { radio_on } else { radio_off };
        lines.push(Line::from(vec![
            Span::styled(cursor, t.fg(t.accent)),
            Span::styled(
                format!("{marker} "),
                t.fg(if focused { t.accent } else { t.muted }),
            ),
            Span::styled(format!("{}. ", custom_idx + 1), t.dim()),
            Span::styled(
                format!("{} type your own answer", g.pencil),
                if focused {
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
                } else {
                    t.dim()
                },
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", g.arrow), t.fg(t.accent)),
            Span::styled(
                "type your answer in the input below, then press enter".to_string(),
                t.dim(),
            ),
        ]));
    }

    let content_w = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0) as u16;
    let w = (content_w + 6).clamp(44, max_w);
    let h = (lines.len() as u16 + 2).clamp(6, area.height.saturating_sub(4).max(6));
    // Bottom-anchored, like oh-my-pi: the dialog rises from just above the
    // input/status dock rather than floating in the middle, so the eye stays
    // near where typing happens.
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: area.height.saturating_sub(h),
        width: w,
        height: h,
    };

    let footer = if picking {
        " ↑↓ move · 1-9 pick · enter select · esc type your own "
    } else {
        " enter to send · esc cancel "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(t.info).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" {} Ask ", g.pending),
            Style::default()
                .fg(t.info)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ))
        .title_bottom(Span::styled(footer.to_string(), t.dim()));

    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

// ------------------------------------------------------------------- lifecycle

type Term = Terminal<ratatui::backend::CrosstermBackend<Stdout>>;

/// Mouse tracking narrowed to what koda actually reads.
///
/// crossterm's `EnableMouseCapture` switches on four tracking modes at once,
/// among them `?1003h` — any-event tracking, a report for every cell the
/// pointer crosses whether a button is down or not. koda consumes exactly two
/// mouse events, the wheel, so the rest is noise, and the noise costs the user
/// something real: while any-event tracking is on, emulators stop honouring the
/// shift/option override that hands a drag back to the terminal for native
/// select-and-copy, and tmux switches to mouse handling of its own. Asking for
/// button tracking (`?1000h`, which carries the wheel), drag tracking
/// (`?1002h`, how we notice someone trying to select), and SGR coordinates
/// (`?1006h`) keeps scrolling intact and gives the override back. `?1015h` is
/// the obsolete rxvt encoding `?1006h` supersedes; nothing wants both.
struct EnableMouseTracking;

impl ratatui::crossterm::Command for EnableMouseTracking {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1002h\x1b[?1006h")
    }

    // Legacy Windows consoles have no VT parser, and there crossterm reads the
    // mouse through the console API rather than these sequences — the narrowing
    // does not apply, so defer to crossterm's own path.
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        ratatui::crossterm::Command::execute_winapi(&EnableMouseCapture)
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

/// The modifier that makes an emulator do its own text selection while an
/// application is reading the mouse. Shift is the xterm convention and what
/// most terminals use; the two common macOS ones bind option instead.
fn select_override() -> &'static str {
    select_override_for(&std::env::var("TERM_PROGRAM").unwrap_or_default())
}

fn select_override_for(term_program: &str) -> &'static str {
    match term_program {
        "Apple_Terminal" | "iTerm.app" => "hold \u{2325} option",
        _ => "hold shift",
    }
}

fn setup(mouse: bool) -> Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        execute!(out, EnableMouseTracking)?;
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
        compacting: None,
        activity: None,
        motion: anim::Motion::Full,
        reveal_pref: cfg.reveal,
        turn_started: None,
        pending: None,
        asking: None,
        model: cfg.model.clone(),
        endpoint: cfg.endpoint(),
        tokens: 0,
        context_budget: cfg.context_tokens,
        auto_tier: if cfg.auto_approve {
            AutoTier::Full
        } else {
            cfg.auto_tier
        },
        web: cfg.web_search,
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
        images: Vec::new(),
        text_area: Rect::new(0, 0, 0, 0),
        selection: None,
        last_size: (0, 0),
        confirm: None,
        files: FileIndex::new(),
        mention_sel: 0,
        cmd_sel: 0,
        action_sel: 0,
        files_ready: false,
        picker: None,
        setup: None,
        settings: None,
        cfg: (*cfg).clone(),
        branch: git_branch(&root),
        root: root.clone(),
        queued: VecDeque::new(),
        watch_add: Vec::new(),
        watch_clear: false,
        choices: None,
        model_picker_pending: false,
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
    // A custom system prompt is sent whole on every single request, so an
    // oversized one is a standing bill nothing else reports. One config here
    // held 80 KB of repeated filler -- a stray test write -- which cost about
    // 20k tokens per message, and the only visible symptom was that koda felt
    // expensive. Say it out loud.
    if let Some(msg) = oversized_prompt_warning(&cfg.system_prompt) {
        app.transcript.error(msg);
    }

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

    // Watch mode (aider-style AI! triggers). The interval always ticks; the
    // handler is a no-op unless watch is enabled and the agent is idle.
    let mut watcher = crate::watch::Watcher::new();
    let mut watch_tick = tokio::time::interval(std::time::Duration::from_millis(
        cfg.watch_interval_ms.clamp(300, 60_000),
    ));
    watch_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // The web control center queues requests from the browser; drain them here
    // so they are applied on the UI thread like any other change. Only armed
    // when the web UI is actually serving.
    let mut web_tick = tokio::time::interval(std::time::Duration::from_millis(400));
    web_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let web_ui = cfg.web_ui;

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
            _ = web_tick.tick(), if web_ui => {
                // Tell the browser what this session is actually using, then
                // apply anything it asked for.
                crate::webui::publish_runtime(
                    &app.model,
                    &app.cfg.endpoint(),
                    &app.mode.to_string(),
                    &app.auto_tier.to_string(),
                );
                dirty |= app.drain_web_control();
            }
            _ = watch_tick.tick() => {
                // Apply any /watch @file additions or /unwatch clears queued by
                // the command handler before scanning.
                if app.watch_clear {
                    watcher.clear_paths();
                    app.watch_clear = false;
                }
                if !app.watch_add.is_empty() {
                    for p in app.watch_add.drain(..) {
                        watcher.watch_path(p);
                    }
                }
                // Only act when watch is on, the agent is idle, and nothing is
                // queued or awaiting the user — so triggers don't stack up on a
                // busy turn or interrupt an approval prompt.
                if app.cfg.watch
                    && !app.busy
                    && app.pending.is_none()
                    && app.asking.is_none()
                    && app.queued.is_empty()
                {
                    if let Some(t) = watcher.scan(&app.root) {
                        watcher.mark(&t);
                        let text = crate::watch::turn_text(&t, &app.root);
                        let rel = t.path.strip_prefix(&app.root).unwrap_or(&t.path).display();
                        let tok = match t.kind { crate::watch::Kind::Do => "AI!", _ => "AI?" };
                        app.note(format!("watch: {tok} in {rel}:{} → {}", t.line, t.instruction));
                        app.transcript.user(text.clone());
                        app.send(Command::User(text));
                        dirty = true;
                    }
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
            // Route the paste to whatever has focus. Pasting into the LLM setup
            // fields (URL / model / API key) or an inline settings editor must
            // land there, not in the chat composer behind the overlay.
            if let Some(s) = app.setup.as_mut() {
                // Setup fields are single-line; strip newlines so a copied line
                // with a trailing return doesn't corrupt the field.
                let clean: String = text.replace(['\n', '\r'], "");
                s.focused().insert(&clean);
                return true;
            }
            if let Some(s) = app.settings.as_mut() {
                if s.editing.is_some() {
                    let multiline = s.editing_multiline();
                    for ch in text.chars() {
                        if ch == '\r' {
                            continue;
                        }
                        if ch == '\n' && !multiline {
                            continue; // single-line field: drop newlines
                        }
                        s.edit_char(ch);
                    }
                    return true;
                }
            }
            // Otherwise it's the chat composer, and this is the same routine
            // ctrl+v uses. It has to be: this is the route a real paste takes,
            // and for a while it was a copy of the logic rather than a call to
            // it, so pasted-path detection worked only on the key binding that
            // almost nobody presses.
            app.paste_text(&text);
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
            // Capture takes click-drag away from the terminal, so koda has to do
            // the selecting itself or nobody does. It has the drag events
            // already; this turns them into the selection the user expected.
            MouseEventKind::Down(MouseButton::Left) => {
                app.selection = app.point_at(m.column, m.row).map(|p| (p, p));
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let (Some((anchor, _)), Some(p)) = (app.selection, app.point_at(m.column, m.row))
                {
                    app.selection = Some((anchor, p));
                    return true;
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if app.selection.is_some() {
                    app.finish_selection();
                    return true;
                }
                false
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

    /// The whole point of the narrowed command: any-event tracking (?1003) is
    /// what stops emulators honouring the shift/option selection override, and
    /// koda never reads a motion event, so it must not be asked for. ?1015 is
    /// the obsolete encoding ?1006 replaces.
    #[test]
    fn mouse_tracking_asks_only_for_the_modes_koda_reads() {
        let mut seq = String::new();
        ratatui::crossterm::Command::write_ansi(&EnableMouseTracking, &mut seq).unwrap();
        assert!(seq.contains("?1000h"), "wheel and button tracking: {seq:?}");
        assert!(
            seq.contains("?1002h"),
            "drag, to notice a failed selection: {seq:?}"
        );
        assert!(seq.contains("?1006h"), "SGR coordinates: {seq:?}");
        assert!(
            !seq.contains("?1003"),
            "any-event tracking breaks selection: {seq:?}"
        );
        assert!(
            !seq.contains("?1015"),
            "rxvt encoding is superseded: {seq:?}"
        );
    }

    #[test]
    fn select_override_names_the_terminals_own_modifier() {
        assert_eq!(
            select_override_for("Apple_Terminal"),
            "hold \u{2325} option"
        );
        assert_eq!(select_override_for("iTerm.app"), "hold \u{2325} option");
        assert_eq!(select_override_for("WezTerm"), "hold shift");
        assert_eq!(select_override_for(""), "hold shift");
    }

    /// The reported bug: terminals answer an image paste by writing a temp file
    /// and pasting its path, so koda saw text beginning with "/" and reported
    /// `unknown command /var/folders/.../clipboard-....png`.
    #[test]
    fn a_pasted_image_path_is_recognised() {
        let dir = std::env::temp_dir().join(format!("koda-paste-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("clipboard-2026-09-02-232843-302d27eb.png");
        std::fs::write(&img, [0x89, b'P', b'N', b'G', 0, 0, 0, 0, 1]).unwrap();

        let got = paste_as_path(img.to_str().unwrap()).expect("an absolute path to a real file");
        assert_eq!(got, img);
        assert!(crate::tools::is_image_path(&got), "and it is an image");

        // file:// URLs, percent-encoded, are the other form terminals send.
        let spaced = dir.join("a shot.png");
        std::fs::write(&spaced, b"x").unwrap();
        let url = format!(
            "file://{}",
            spaced.display().to_string().replace(' ', "%20")
        );
        assert_eq!(paste_as_path(&url), Some(spaced));

        // Ordinary text must not be mistaken for a path.
        assert_eq!(paste_as_path("fix the login bug"), None);
        assert_eq!(paste_as_path("/help"), None);
        assert_eq!(paste_as_path(""), None);
        assert_eq!(
            paste_as_path(&format!("{}\nsecond line", img.display())),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A slash command never has a slash in its name, but its arguments may.
    /// That distinction is what keeps a pasted path from being swallowed by the
    /// command dispatcher while `/url http://host/v1` still works.
    #[test]
    fn a_command_word_never_contains_a_slash() {
        let word = |s: &str| s.split_whitespace().next().unwrap_or("").contains('/');
        assert!(word("var/folders/xk/T/clipboard-1.png"), "a pasted path");
        assert!(word("Users/sridhar/shot.png"), "any absolute path");
        assert!(!word("help"), "a bare command");
        assert!(
            !word("url http://localhost:11434/v1"),
            "slashes in an argument"
        );
        assert!(!word("learn accept 3"), "command with plain arguments");
    }

    #[test]
    fn percent_decode_handles_escapes_and_utf8() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%E2%9C%93"), "\u{2713}");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    /// The renderer and the extractor both ask this what is selected on a line.
    /// If they could disagree, the user would copy something other than what
    /// they saw highlighted -- so there is one answer, and this is it.
    #[test]
    fn line_span_bounds_each_line_of_a_selection() {
        let sel = ((2, 3), (4, 5));
        assert_eq!(line_span(1, sel, 10), None, "before the selection");
        assert_eq!(line_span(5, sel, 10), None, "after it");
        assert_eq!(
            line_span(2, sel, 10),
            Some((3, 10)),
            "first line runs to its end"
        );
        assert_eq!(
            line_span(3, sel, 10),
            Some((0, 10)),
            "middle lines are whole"
        );
        assert_eq!(
            line_span(4, sel, 10),
            Some((0, 5)),
            "last line stops at the cursor"
        );

        // One line, bounded at both ends.
        assert_eq!(line_span(2, ((2, 1), (2, 4)), 10), Some((1, 4)));
        // Dragging past the right edge clamps to the text rather than panicking.
        assert_eq!(line_span(2, ((2, 1), (2, 99)), 10), Some((1, 10)));
        // An empty range selects nothing.
        assert_eq!(line_span(2, ((2, 4), (2, 4)), 10), None);
        // A short line inside a multi-line selection.
        assert_eq!(
            line_span(3, sel, 0),
            None,
            "nothing to take from a blank line"
        );
    }

    /// Dragging up the screen has to select the same text as dragging down it.
    #[test]
    fn a_backwards_drag_selects_the_same_range() {
        let order = |a: (usize, usize), b: (usize, usize)| {
            if a == b {
                None
            } else if a <= b {
                Some((a, b))
            } else {
                Some((b, a))
            }
        };
        assert_eq!(order((2, 3), (4, 5)), Some(((2, 3), (4, 5))));
        assert_eq!(
            order((4, 5), (2, 3)),
            Some(((2, 3), (4, 5))),
            "same range, dragged up"
        );
        assert_eq!(order((2, 3), (2, 3)), None, "a click is not a selection");
    }

    /// The highlight must land on exactly the selected characters -- the point
    /// of restyling the rendered line rather than re-rendering it.
    #[test]
    fn highlight_marks_only_the_selected_characters() {
        let theme = crate::theme::resolve("auto");
        let mut lines = vec![
            Line::from("hello world".to_string()),
            Line::from("second line".to_string()),
        ];
        highlight_selection(&mut lines, 0, ((0, 6), (1, 6)), &theme);

        // Line 0: "hello " plain, "world" reversed, nothing after.
        let rev: String = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(rev, "world");

        // Line 1 is the last: selected up to column 6.
        let rev1: String = lines[1]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(rev1, "second");

        // A line outside the range is untouched.
        let mut other = vec![Line::from("untouched".to_string())];
        highlight_selection(&mut other, 9, ((0, 0), (1, 1)), &theme);
        assert_eq!(other[0].spans.len(), 1, "left exactly as it was drawn");
    }

    /// A stray 80 KB system prompt cost ~20k tokens on every message and the
    /// only symptom was that koda felt expensive. The price of a custom prompt
    /// should never be invisible.
    #[test]
    fn an_oversized_system_prompt_is_reported() {
        assert_eq!(oversized_prompt_warning(""), None, "the built-in is silent");
        assert_eq!(
            oversized_prompt_warning(&"x".repeat(2_500)),
            None,
            "a normal custom prompt is not nagged about"
        );
        let junk = "You are a reviewer. ".repeat(4000);
        let msg = oversized_prompt_warning(&junk).expect("80 KB is reported");
        assert!(msg.contains("78 KB"), "{msg}");
        assert!(msg.contains("20000 tokens"), "{msg}");
        assert!(msg.contains("/settings"), "and says how to clear it: {msg}");
    }

    /// `#` acts on a prompt already written, so the query is anchored at the
    /// end. A `#` mid-sentence is ordinary text -- "fix #3 in the parser" must
    /// not pop a palette over what someone is typing.
    #[test]
    fn the_action_query_is_only_a_trailing_hash_word() {
        let q = |buf: &str| {
            let hash = buf.rfind('#')?;
            let tail = &buf[hash..];
            if tail[1..].chars().any(|c| c.is_whitespace()) {
                return None;
            }
            Some(tail.to_string())
        };
        assert_eq!(q("fix the bug#"), Some("#".to_string()));
        assert_eq!(q("fix the bug#cl"), Some("#cl".to_string()));
        assert_eq!(q("#copy"), Some("#copy".to_string()));
        assert_eq!(q("fix #3 in the parser"), None, "mid-sentence hash is text");
        assert_eq!(q("no hash at all"), None);
    }

    /// Every action in the palette must be one apply_action handles; a row that
    /// does nothing when chosen is worse than a row that is not there.
    #[test]
    fn every_listed_action_is_implemented() {
        const HANDLED: &[&str] = &[
            "#copy",
            "#copyline",
            "#cutline",
            "#start",
            "#end",
            "#clear",
            "#undo",
            "#paste",
        ];
        for (name, desc) in ACTIONS {
            assert!(HANDLED.contains(name), "{name} is listed but not handled");
            assert!(!desc.is_empty(), "{name} needs a description");
            assert!(name.starts_with('#'), "{name} should carry its prefix");
        }
        assert_eq!(ACTIONS.len(), HANDLED.len(), "handled but unlisted actions");
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
