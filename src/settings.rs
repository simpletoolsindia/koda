//! The interactive settings page.
//!
//! Anything the app's behaviour depends on that a user might reasonably want to
//! change mid-session lives here, so they never have to quit and hand-edit a
//! TOML file. Each row is either a toggle or a small cycle of choices; changes
//! apply live and are written back to the config file on close.

use crate::config::{AutoTier, Config, Mode};
use crate::theme::{Glyphs, Theme};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Which setting a row controls. Kept as an enum so the row order, labels, and
/// value formatting are all defined in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Mode,
    Autonomy,
    Reasoning,
    Theme,
    Motion,
    Reveal,
    Mouse,
    Vision,
    Sandbox,
    Sessions,
    Memory,
    WebSearch,
    SearchBackend,
    SearxUrl,
    WebFetch,
    Ocr,
    Codegraph,
    Debug,
    WebUi,
    UiDetail,
    Watch,
    SystemPrompt,
    LogDetail,
}

impl Row {
    /// Display order, top to bottom.
    pub const ALL: [Row; 23] = [
        Row::Mode,
        Row::Autonomy,
        Row::Reasoning,
        Row::Theme,
        Row::Motion,
        Row::Reveal,
        Row::Mouse,
        Row::Vision,
        Row::Sandbox,
        Row::Sessions,
        Row::Memory,
        Row::WebSearch,
        Row::SearchBackend,
        Row::SearxUrl,
        Row::WebFetch,
        Row::Ocr,
        Row::Codegraph,
        Row::Debug,
        Row::WebUi,
        Row::UiDetail,
        Row::Watch,
        Row::SystemPrompt,
        Row::LogDetail,
    ];

    fn label(&self) -> &'static str {
        match self {
            Row::Mode => "mode",
            Row::Autonomy => "autonomy",
            Row::Reasoning => "reasoning",
            Row::Theme => "theme",
            Row::Motion => "animation",
            Row::Reveal => "text reveal",
            Row::Mouse => "mouse capture",
            Row::Vision => "image input",
            Row::Sandbox => "sandbox",
            Row::Sessions => "sessions",
            Row::Memory => "memory",
            Row::WebSearch => "web search",
            Row::SearchBackend => "search backend",
            Row::SearxUrl => "searxng url",
            Row::WebFetch => "web fetch",
            Row::Ocr => "image ocr",
            Row::Codegraph => "code graph",
            Row::Debug => "debug capture",
            Row::WebUi => "web ui",
            Row::UiDetail => "ui detail",
            Row::Watch => "watch mode",
            Row::SystemPrompt => "system prompt",
            Row::LogDetail => "detailed logs",
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Row::Mode => "plan reads only · execute edits · vibe spec-checks",
            Row::Autonomy => "ask · auto-write · full-auto (no prompts)",
            Row::Reasoning => "thinking effort: off · low · medium · high",
            Row::Theme => "colour palette",
            Row::Motion => "spinners, gauges, and text reveal",
            Row::Reveal => "stream replies in progressively (needs animation)",
            Row::Mouse => "on: wheel scrolls and drag selects · off: terminal does both",
            Row::Vision => "auto (guess from name) · on · off — set on behind a router",
            Row::Sandbox => "confine file tools to the workspace",
            Row::Sessions => "record conversations to .koda/sessions",
            Row::Memory => "carry facts between sessions in .koda/memory.md",
            Row::WebSearch => "1) enable, then pick a backend below",
            Row::SearchBackend => "2) duckduckgo (no setup) or searxng",
            Row::SearxUrl => "3) enter to edit your SearXNG address",
            Row::WebFetch => "let the agent GET a URL and read it as text",
            Row::Ocr => "OCR images (tesseract) when the model can't see them",
            Row::Codegraph => "scan the project into a symbol graph on open",
            Row::Debug => "dump raw requests/responses to the debug dir",
            Row::WebUi => "serve the React log/debug UI on 127.0.0.1 (restart to apply)",
            Row::UiDetail => "web ui log detail: simple · medium · high",
            Row::Watch => "act on AI! / AI? comment triggers when idle",
            Row::SystemPrompt => "enter to edit the main system prompt (empty = built-in)",
            Row::LogDetail => "show debug-level detail in /logs",
        }
    }

    /// Whether this row's value is free text edited inline (enter opens editor).
    fn editable(&self) -> bool {
        matches!(self, Row::SearxUrl | Row::SystemPrompt)
    }
}

pub struct Settings {
    pub sel: usize,
    /// A working copy; committed to `cfg` and persisted on close.
    pub cfg: Config,
    themes: Vec<&'static str>,
    /// Set true when something changed, so close knows to persist.
    pub dirty: bool,
    /// When `Some`, an inline text editor is open for the selected row and this
    /// holds the in-progress value. Keystrokes go here until enter/esc.
    pub editing: Option<String>,
}

impl Settings {
    pub fn new(cfg: &Config) -> Self {
        Self {
            sel: 0,
            cfg: cfg.clone(),
            themes: crate::theme::names(),
            dirty: false,
            editing: None,
        }
    }

    pub fn up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.sel = (self.sel + 1).min(Row::ALL.len() - 1);
    }

    fn current(&self) -> Row {
        Row::ALL[self.sel.min(Row::ALL.len() - 1)]
    }

    /// Change the selected row's value. `forward` is enter/right; `!forward` is
    /// left. Toggles ignore direction; cycles respect it.
    pub fn change(&mut self, forward: bool) {
        let row = self.current();
        // Editable rows open an inline text editor on enter/right instead of
        // cycling. Left is ignored for them.
        if row.editable() {
            if forward {
                self.editing = Some(match row {
                    Row::SearxUrl => self.cfg.searx_url.clone(),
                    // Editing the system prompt: seed the textarea with the
                    // current custom prompt, or the built-in one when none is
                    // set, so the user edits from real text instead of a blank.
                    Row::SystemPrompt => {
                        if self.cfg.system_prompt.trim().is_empty() {
                            crate::prompt::base_prompt().to_string()
                        } else {
                            self.cfg.system_prompt.clone()
                        }
                    }
                    _ => String::new(),
                });
            }
            return;
        }
        match row {
            Row::Mode => {
                self.cfg.mode = if forward {
                    self.cfg.mode.next()
                } else {
                    prev_mode(self.cfg.mode)
                }
            }
            Row::Autonomy => {
                self.cfg.auto_tier = if forward {
                    self.cfg.auto_tier.next()
                } else {
                    prev_tier(self.cfg.auto_tier)
                };
                self.cfg.auto_approve = self.cfg.auto_tier == AutoTier::Full;
            }
            Row::Reasoning => {
                self.cfg.reasoning_effort = if forward {
                    next_reasoning(&self.cfg.reasoning_effort)
                } else {
                    prev_reasoning(&self.cfg.reasoning_effort)
                };
            }
            Row::Theme => {
                let i = self
                    .themes
                    .iter()
                    .position(|t| *t == self.cfg.theme)
                    .unwrap_or(0);
                let n = self.themes.len().max(1);
                let next = if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                self.cfg.theme = self.themes[next].to_string();
            }
            Row::Motion => self.cfg.motion = !self.cfg.motion,
            Row::Reveal => self.cfg.reveal = !self.cfg.reveal,
            Row::Mouse => self.cfg.mouse_capture = !self.cfg.mouse_capture,
            Row::Vision => {
                // Normalized first: the setting is also hand-edited in TOML, and
                // a value like "On" or "true" must still land somewhere in the
                // cycle rather than falling through to a fixed branch.
                let cur = self.cfg.vision.trim().to_ascii_lowercase();
                let cur = match cur.as_str() {
                    "on" | "true" | "yes" | "always" => "on",
                    "off" | "false" | "no" | "never" => "off",
                    _ => "auto",
                };
                self.cfg.vision = match (cur, forward) {
                    ("auto", true) => "on",
                    ("on", true) => "off",
                    ("auto", false) => "off",
                    ("on", false) => "auto",
                    // "off" in both directions; the normalization above admits
                    // no fourth value, but the match still has to be total.
                    (_, true) => "auto",
                    (_, false) => "on",
                }
                .into();
            }
            Row::Sandbox => self.cfg.sandbox = !self.cfg.sandbox,
            Row::Sessions => self.cfg.sessions = !self.cfg.sessions,
            Row::Memory => self.cfg.memory = !self.cfg.memory,
            Row::WebSearch => self.cfg.web_search = !self.cfg.web_search,
            Row::SearchBackend => {
                self.cfg.search_backend = if self.cfg.search_backend.eq_ignore_ascii_case("searxng")
                {
                    "duckduckgo".into()
                } else {
                    "searxng".into()
                };
            }
            Row::SearxUrl | Row::SystemPrompt => {} // handled above
            Row::WebFetch => self.cfg.web_fetch = !self.cfg.web_fetch,
            Row::Ocr => self.cfg.ocr = !self.cfg.ocr,
            Row::Codegraph => self.cfg.codegraph = !self.cfg.codegraph,
            Row::Debug => {
                self.cfg.debug = !self.cfg.debug;
                crate::debug::set_enabled(self.cfg.debug);
            }
            Row::WebUi => self.cfg.web_ui = !self.cfg.web_ui,
            Row::UiDetail => {
                self.cfg.ui_detail = if forward {
                    next_detail(&self.cfg.ui_detail)
                } else {
                    prev_detail(&self.cfg.ui_detail)
                };
            }
            Row::Watch => self.cfg.watch = !self.cfg.watch,
            Row::LogDetail => self.cfg.log_detail = !self.cfg.log_detail,
        }
        self.dirty = true;
    }

    /// Feed a character to the open inline editor.
    pub fn edit_char(&mut self, c: char) {
        if let Some(buf) = self.editing.as_mut() {
            buf.push(c);
        }
    }

    /// True when the open editor is a multi-line textarea (the system prompt),
    /// so the key handler treats enter as a newline and ctrl+s as save.
    pub fn editing_multiline(&self) -> bool {
        self.editing.is_some() && self.current() == Row::SystemPrompt
    }

    /// Backspace in the open inline editor.
    pub fn edit_backspace(&mut self) {
        if let Some(buf) = self.editing.as_mut() {
            buf.pop();
        }
    }

    /// Clear the open inline editor (ctrl+u).
    pub fn edit_clear(&mut self) {
        if let Some(buf) = self.editing.as_mut() {
            buf.clear();
        }
    }

    /// Commit the inline editor to the selected row and close it.
    pub fn edit_commit(&mut self) {
        let Some(val) = self.editing.take() else {
            return;
        };
        match self.current() {
            Row::SearxUrl => {
                self.cfg.searx_url = val.trim().to_string();
                // Entering a URL implies you want that backend.
                if !self.cfg.searx_url.is_empty() {
                    self.cfg.search_backend = "searxng".into();
                }
            }
            Row::SystemPrompt => {
                // If the user left it identical to the built-in, store empty so
                // it still reads as "(built-in)" and tracks future prompt updates.
                self.cfg.system_prompt = if val.trim() == crate::prompt::base_prompt().trim() {
                    String::new()
                } else {
                    val
                };
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Abandon the inline editor without saving.
    pub fn edit_cancel(&mut self) {
        self.editing = None;
    }

    fn value(&self, row: Row) -> String {
        let on = |b: bool| {
            if b {
                "on".to_string()
            } else {
                "off".to_string()
            }
        };
        match row {
            Row::Mode => self.cfg.mode.to_string(),
            Row::Autonomy => self.cfg.auto_tier.label().to_lowercase(),
            Row::Reasoning => self.cfg.reasoning_effort.to_lowercase(),
            Row::Theme => self.cfg.theme.clone(),
            Row::Motion => on(self.cfg.motion),
            Row::Reveal => on(self.cfg.reveal),
            Row::Mouse => on(self.cfg.mouse_capture),
            Row::Vision => self.cfg.vision.to_lowercase(),
            Row::Sandbox => on(self.cfg.sandbox),
            Row::Sessions => on(self.cfg.sessions),
            Row::Memory => on(self.cfg.memory),
            Row::WebSearch => on(self.cfg.web_search),
            Row::SearchBackend => self.cfg.search_backend.to_lowercase(),
            Row::SearxUrl => {
                if self.cfg.searx_url.is_empty() {
                    "(not set)".to_string()
                } else {
                    self.cfg.searx_url.clone()
                }
            }
            Row::WebFetch => on(self.cfg.web_fetch),
            Row::Ocr => on(self.cfg.ocr),
            Row::Codegraph => on(self.cfg.codegraph),
            Row::Debug => on(self.cfg.debug),
            Row::WebUi => {
                if self.cfg.web_ui {
                    format!("on :{}", self.cfg.web_ui_port)
                } else {
                    "off".to_string()
                }
            }
            Row::UiDetail => self.cfg.ui_detail.to_lowercase(),
            Row::Watch => on(self.cfg.watch),
            Row::SystemPrompt => {
                if self.cfg.system_prompt.trim().is_empty() {
                    "(built-in)".to_string()
                } else {
                    format!("custom · {} chars", self.cfg.system_prompt.len())
                }
            }
            Row::LogDetail => on(self.cfg.log_detail),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect, t: &Theme, g: &Glyphs) {
        // A dedicated, roomy textarea when editing the system prompt: the whole
        // overlay becomes a wrapped multi-line editor showing the real prompt
        // text, so the user can see and edit it rather than typing blind.
        if self.editing_multiline() {
            self.draw_prompt_editor(f, area, t);
            return;
        }
        let w = area.width.saturating_sub(8).clamp(30, 74);
        let h = (Row::ALL.len() as u16 + 4).min(area.height.saturating_sub(2).max(6));
        let rect = Rect {
            x: area.width.saturating_sub(w) / 2,
            y: area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };

        let label_w = Row::ALL.iter().map(|r| r.label().len()).max().unwrap_or(0);
        let mut lines: Vec<Line> = Vec::new();
        for (i, row) in Row::ALL.iter().enumerate() {
            let selected = i == self.sel;
            let marker = if selected { g.pick } else { " " };
            let editing_here = selected && self.editing.is_some();
            let value = if editing_here {
                // Show the in-progress buffer with a caret.
                let buf = self.editing.as_deref().unwrap_or("");
                let shown: String = buf
                    .chars()
                    .rev()
                    .take(40)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("{shown}▏")
            } else {
                self.value(*row)
            };
            let label_style = if selected { t.strong() } else { t.body() };
            let value_style = if editing_here {
                Style::default()
                    .fg(t.accent_alt)
                    .add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                t.fg(t.accent)
            };
            let mut spans = vec![
                Span::styled(format!(" {marker} "), t.fg(t.accent)),
                Span::styled(
                    format!("{:<label_w$}", row.label(), label_w = label_w),
                    label_style,
                ),
                Span::styled("   ".to_string(), t.dim()),
                Span::styled(value, value_style),
            ];
            if selected && !editing_here {
                spans.push(Span::styled(format!("   {}", row.hint()), t.dim()));
            }
            lines.push(Line::from(spans));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(t.fg(t.border_focus))
            .title(Span::styled(
                " Settings ",
                Style::default()
                    .fg(t.border_focus)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                if self.editing.is_some() {
                    " type to edit · enter save · esc cancel "
                } else {
                    " ↑↓ move · ←/→/enter change · esc save & close "
                },
                t.dim(),
            ));

        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(lines).block(block), rect);
    }

    /// A full-size, wrapped multi-line editor for the system prompt. Shows the
    /// current text (built-in when none was set) with a caret, scrolled so the
    /// end stays in view. Enter inserts a newline; ctrl+s saves; esc cancels.
    fn draw_prompt_editor(&self, f: &mut Frame, area: Rect, t: &Theme) {
        let buf = self.editing.as_deref().unwrap_or("");
        let w = area.width.saturating_sub(6).clamp(40, 100);
        let h = area.height.saturating_sub(4).clamp(8, 40);
        let rect = Rect {
            x: area.width.saturating_sub(w) / 2,
            y: area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        let inner_w = rect.width.saturating_sub(2) as usize;
        let body_h = rect.height.saturating_sub(2) as usize;

        // Wrap the text (respecting explicit newlines), append a caret at the end.
        let mut wrapped: Vec<String> = Vec::new();
        for para in buf.split('\n') {
            if para.is_empty() {
                wrapped.push(String::new());
                continue;
            }
            let mut line = String::new();
            for word in para.split_inclusive(' ') {
                if line.chars().count() + word.chars().count() > inner_w && !line.is_empty() {
                    wrapped.push(std::mem::take(&mut line));
                }
                line.push_str(word);
            }
            wrapped.push(line);
        }
        if wrapped.is_empty() {
            wrapped.push(String::new());
        }
        // Caret on the last line.
        if let Some(last) = wrapped.last_mut() {
            last.push('▏');
        }
        // Scroll so the caret (last line) is visible.
        let skip = wrapped.len().saturating_sub(body_h);
        let lines: Vec<Line> = wrapped
            .into_iter()
            .skip(skip)
            .map(|l| Line::from(Span::styled(l, t.body())))
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(t.fg(t.border_focus))
            .title(Span::styled(
                " Edit system prompt ",
                Style::default()
                    .fg(t.border_focus)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                " enter = newline · ctrl+s save · ctrl+u clear · esc cancel ",
                t.dim(),
            ));

        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

const DETAIL: [&str; 3] = ["simple", "medium", "high"];

fn next_detail(cur: &str) -> String {
    let i = DETAIL
        .iter()
        .position(|r| r.eq_ignore_ascii_case(cur))
        .unwrap_or(1);
    DETAIL[(i + 1) % DETAIL.len()].to_string()
}

fn prev_detail(cur: &str) -> String {
    let i = DETAIL
        .iter()
        .position(|r| r.eq_ignore_ascii_case(cur))
        .unwrap_or(1);
    DETAIL[(i + DETAIL.len() - 1) % DETAIL.len()].to_string()
}

const REASONING: [&str; 4] = ["off", "low", "medium", "high"];

fn next_reasoning(cur: &str) -> String {
    let i = REASONING
        .iter()
        .position(|r| r.eq_ignore_ascii_case(cur))
        .unwrap_or(0);
    REASONING[(i + 1) % REASONING.len()].to_string()
}

fn prev_reasoning(cur: &str) -> String {
    let i = REASONING
        .iter()
        .position(|r| r.eq_ignore_ascii_case(cur))
        .unwrap_or(0);
    REASONING[(i + REASONING.len() - 1) % REASONING.len()].to_string()
}

fn prev_mode(m: Mode) -> Mode {
    // Mode::next cycles plan→execute→vibe→plan; prev is two nexts.
    m.next().next()
}

fn prev_tier(a: AutoTier) -> AutoTier {
    a.next().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The setting exists so a router alias can be told what its name cannot
    /// say, so it has to be reachable from the settings page and cycle cleanly
    /// in both directions -- including from a value typed by hand into the TOML.
    #[test]
    fn vision_row_cycles_through_all_three_states() {
        let cfg = Config {
            vision: "auto".into(),
            ..Config::default()
        };
        let mut s = Settings::new(&cfg);
        s.sel = Row::ALL
            .iter()
            .position(|r| *r == Row::Vision)
            .expect("row exists");

        s.change(true);
        assert_eq!(s.cfg.vision, "on");
        s.change(true);
        assert_eq!(s.cfg.vision, "off");
        s.change(true);
        assert_eq!(s.cfg.vision, "auto", "forward wraps");

        s.change(false);
        assert_eq!(s.cfg.vision, "off", "and it goes backwards too");

        // A hand-edited value still lands in the cycle instead of sticking.
        s.cfg.vision = "TRUE".into();
        s.change(true);
        assert_eq!(
            s.cfg.vision, "off",
            "\"TRUE\" is normalized to on, then advances"
        );
    }

    fn settings() -> Settings {
        Settings::new(&Config::default())
    }

    fn select(s: &mut Settings, row: Row) {
        s.sel = Row::ALL.iter().position(|r| *r == row).unwrap();
    }

    #[test]
    fn editing_system_prompt_seeds_the_builtin_when_empty() {
        let mut s = settings();
        assert!(
            s.cfg.system_prompt.is_empty(),
            "default has no custom prompt"
        );
        select(&mut s, Row::SystemPrompt);
        s.change(true); // enter -> open editor
        let buf = s.editing.as_deref().unwrap_or("");
        assert!(!buf.is_empty(), "editor should be seeded, not blank");
        assert_eq!(
            buf,
            crate::prompt::base_prompt(),
            "seeded with the built-in prompt"
        );
        assert!(s.editing_multiline(), "system prompt is a multi-line edit");
    }

    #[test]
    fn committing_the_unchanged_builtin_stores_empty() {
        let mut s = settings();
        select(&mut s, Row::SystemPrompt);
        s.change(true);
        // Commit without changing anything -> stays "(built-in)".
        s.edit_commit();
        assert!(
            s.cfg.system_prompt.is_empty(),
            "unchanged built-in stays empty"
        );
    }

    #[test]
    fn committing_a_real_edit_stores_the_custom_prompt() {
        let mut s = settings();
        select(&mut s, Row::SystemPrompt);
        s.change(true);
        s.edit_char('\n');
        s.edit_char('X');
        s.edit_commit();
        assert!(
            s.cfg.system_prompt.ends_with('X'),
            "custom edit is stored: {:?}",
            s.cfg.system_prompt
        );
        assert!(s.dirty);
    }

    #[test]
    fn searx_url_edit_is_single_line() {
        let mut s = settings();
        select(&mut s, Row::SearxUrl);
        s.change(true);
        assert!(!s.editing_multiline(), "url field is single-line");
    }
}
