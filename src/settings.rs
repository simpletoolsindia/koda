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
    Theme,
    Motion,
    Reveal,
    Sandbox,
    Sessions,
    Memory,
    WebSearch,
    Codegraph,
    LogDetail,
}

impl Row {
    /// Display order, top to bottom.
    pub const ALL: [Row; 11] = [
        Row::Mode,
        Row::Autonomy,
        Row::Theme,
        Row::Motion,
        Row::Reveal,
        Row::Sandbox,
        Row::Sessions,
        Row::Memory,
        Row::WebSearch,
        Row::Codegraph,
        Row::LogDetail,
    ];

    fn label(&self) -> &'static str {
        match self {
            Row::Mode => "mode",
            Row::Autonomy => "autonomy",
            Row::Theme => "theme",
            Row::Motion => "animation",
            Row::Reveal => "text reveal",
            Row::Sandbox => "sandbox",
            Row::Sessions => "sessions",
            Row::Memory => "memory",
            Row::WebSearch => "web search",
            Row::Codegraph => "code graph",
            Row::LogDetail => "detailed logs",
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Row::Mode => "plan reads only · execute edits · vibe spec-checks",
            Row::Autonomy => "ask · auto-write · full-auto (no prompts)",
            Row::Theme => "colour palette",
            Row::Motion => "spinners, gauges, and text reveal",
            Row::Reveal => "stream replies in progressively (needs animation)",
            Row::Sandbox => "confine file tools to the workspace",
            Row::Sessions => "record conversations to .koda/sessions",
            Row::Memory => "carry facts between sessions in .koda/memory.md",
            Row::WebSearch => "needs a SearXNG URL in config",
            Row::Codegraph => "scan the project into a symbol graph on open",
            Row::LogDetail => "show debug-level detail in /logs",
        }
    }
}

pub struct Settings {
    pub sel: usize,
    /// A working copy; committed to `cfg` and persisted on close.
    pub cfg: Config,
    themes: Vec<&'static str>,
    /// Set true when something changed, so close knows to persist.
    pub dirty: bool,
}

impl Settings {
    pub fn new(cfg: &Config) -> Self {
        Self {
            sel: 0,
            cfg: cfg.clone(),
            themes: crate::theme::names(),
            dirty: false,
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
            Row::Theme => {
                let i = self
                    .themes
                    .iter()
                    .position(|t| *t == self.cfg.theme)
                    .unwrap_or(0);
                let n = self.themes.len().max(1);
                let next = if forward { (i + 1) % n } else { (i + n - 1) % n };
                self.cfg.theme = self.themes[next].to_string();
            }
            Row::Motion => self.cfg.motion = !self.cfg.motion,
            Row::Reveal => self.cfg.reveal = !self.cfg.reveal,
            Row::Sandbox => self.cfg.sandbox = !self.cfg.sandbox,
            Row::Sessions => self.cfg.sessions = !self.cfg.sessions,
            Row::Memory => self.cfg.memory = !self.cfg.memory,
            Row::WebSearch => self.cfg.web_search = !self.cfg.web_search,
            Row::Codegraph => self.cfg.codegraph = !self.cfg.codegraph,
            Row::LogDetail => self.cfg.log_detail = !self.cfg.log_detail,
        }
        self.dirty = true;
    }

    fn value(&self, row: Row) -> String {
        let on = |b: bool| if b { "on".to_string() } else { "off".to_string() };
        match row {
            Row::Mode => self.cfg.mode.to_string(),
            Row::Autonomy => self.cfg.auto_tier.label().to_lowercase(),
            Row::Theme => self.cfg.theme.clone(),
            Row::Motion => on(self.cfg.motion),
            Row::Reveal => on(self.cfg.reveal),
            Row::Sandbox => on(self.cfg.sandbox),
            Row::Sessions => on(self.cfg.sessions),
            Row::Memory => on(self.cfg.memory),
            Row::WebSearch => on(self.cfg.web_search),
            Row::Codegraph => on(self.cfg.codegraph),
            Row::LogDetail => on(self.cfg.log_detail),
        }
    }

    pub fn draw(&self, f: &mut Frame, area: Rect, t: &Theme, g: &Glyphs) {
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
            let value = self.value(*row);
            let label_style = if selected { t.strong() } else { t.body() };
            let value_style = if selected {
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
            if selected {
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
                " ↑↓ move · ←/→/enter change · esc save & close ",
                t.dim(),
            ));

        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

fn prev_mode(m: Mode) -> Mode {
    // Mode::next cycles plan→execute→vibe→plan; prev is two nexts.
    m.next().next()
}

fn prev_tier(a: AutoTier) -> AutoTier {
    a.next().next()
}
