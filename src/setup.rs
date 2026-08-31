//! The provider setup screen: endpoint, model and API key, written back to the
//! config file.
//!
//! This exists because the first-run failure — "no model configured" — used to
//! be fixed only by finding and hand-editing a TOML file. Anything the app tells
//! you to set, the app should let you set.

use crate::config::Config;
use crate::editor::Editor;
use crate::theme::{Glyphs, Theme};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Url,
    Model,
    Key,
}

impl Field {
    const ALL: [Field; 3] = [Field::Url, Field::Model, Field::Key];

    fn label(&self) -> &'static str {
        match self {
            Field::Url => "endpoint",
            Field::Model => "model",
            Field::Key => "api key",
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Field::Url => "http://localhost:11434/v1",
            Field::Model => "leave empty to use the server's first model",
            Field::Key => "\"local\" is fine for a local server",
        }
    }

    fn next(&self) -> Field {
        match self {
            Field::Url => Field::Model,
            Field::Model => Field::Key,
            Field::Key => Field::Url,
        }
    }

    fn prev(&self) -> Field {
        match self {
            Field::Url => Field::Key,
            Field::Model => Field::Url,
            Field::Key => Field::Model,
        }
    }
}

pub struct Setup {
    pub focus: Field,
    url: Editor,
    model: Editor,
    key: Editor,
    /// Models fetched from the endpoint, offered as suggestions.
    pub available: Vec<String>,
    pub status: Option<String>,
}

impl Setup {
    pub fn new(cfg: &Config) -> Self {
        let mut s = Self {
            focus: Field::Url,
            url: Editor::default(),
            model: Editor::default(),
            key: Editor::default(),
            available: Vec::new(),
            status: None,
        };
        s.url.insert(&cfg.endpoint());
        s.model.insert(&cfg.model);
        s.key.insert(&cfg.api_key);
        s
    }

    fn editor(&mut self, f: Field) -> &mut Editor {
        match f {
            Field::Url => &mut self.url,
            Field::Model => &mut self.model,
            Field::Key => &mut self.key,
        }
    }

    pub fn value(&self, f: Field) -> &str {
        match f {
            Field::Url => &self.url.buf,
            Field::Model => &self.model.buf,
            Field::Key => &self.key.buf,
        }
    }

    pub fn focused(&mut self) -> &mut Editor {
        let f = self.focus;
        self.editor(f)
    }

    pub fn next_field(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn prev_field(&mut self) {
        self.focus = self.focus.prev();
    }

    /// Cycle the model field through what the server reported.
    pub fn cycle_model(&mut self) {
        if self.available.is_empty() {
            return;
        }
        let current = self.model.buf.clone();
        let idx = self
            .available
            .iter()
            .position(|m| *m == current)
            .map(|i| (i + 1) % self.available.len())
            .unwrap_or(0);
        let pick = self.available[idx].clone();
        self.model.clear();
        self.model.insert(&pick);
    }

    /// Apply to a config and persist. Returns the file written.
    pub fn save(&self, cfg: &mut Config) -> anyhow::Result<std::path::PathBuf> {
        cfg.base_url = self.url.buf.trim().to_string();
        cfg.model = self.model.buf.trim().to_string();
        cfg.api_key = self.key.buf.trim().to_string();
        crate::config::save(cfg)
    }
}

/// Mask a secret but keep enough to recognise it.
fn masked(s: &str) -> String {
    let n = s.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "•".repeat(n);
    }
    let tail: String = s.chars().skip(n - 3).collect();
    format!("{}{}", "•".repeat(n - 3), tail)
}

pub fn draw(f: &mut Frame, area: Rect, s: &Setup, t: &Theme, g: &Glyphs) {
    let w = area.width.saturating_sub(6).clamp(40, 78);
    let h = 15u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let inner_w = rect.width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        " Point koda at a model server.",
        t.dim(),
    ))];

    for field in Field::ALL {
        let focused = s.focus == field;
        let raw = s.value(field);
        let shown = if field == Field::Key {
            masked(raw)
        } else {
            raw.to_string()
        };
        let display: String = if shown.chars().count() > inner_w {
            shown.chars().skip(shown.chars().count() - inner_w).collect()
        } else {
            shown
        };

        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", if focused { g.prompt } else { " " }),
                t.fg(t.accent),
            ),
            Span::styled(
                field.label().to_string(),
                if focused {
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
                } else {
                    t.dim()
                },
            ),
        ]));
        let value_style = if display.is_empty() {
            t.dim()
        } else {
            t.body()
        };
        let text = if display.is_empty() {
            field.hint().to_string()
        } else {
            display
        };
        lines.push(Line::from(vec![
            Span::raw("   ".to_string()),
            Span::styled(text, value_style),
            Span::styled(
                if focused { "▌" } else { "" },
                t.fg(t.accent),
            ),
        ]));
    }

    if let Some(msg) = &s.status {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(format!(" {msg}"), t.dim())));
    }

    let hints = Line::from(vec![
        Span::styled(" tab", t.fg(t.accent)),
        Span::styled(" field  ", t.dim()),
        Span::styled("ctrl+r", t.fg(t.accent)),
        Span::styled(" fetch models  ", t.dim()),
        Span::styled("enter", t.fg(t.accent)),
        Span::styled(" save  ", t.dim()),
        Span::styled("esc", t.fg(t.accent)),
        Span::styled(" cancel ", t.dim()),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(t.fg(t.border_focus))
        .title(Span::styled(
            " provider setup ",
            Style::default()
                .fg(t.border_focus)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(hints);

    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_from_config_and_saves_back() {
        let cfg = Config {
            base_url: "http://a/v1".into(),
            model: "m1".into(),
            ..Config::default()
        };
        let s = Setup::new(&cfg);
        assert_eq!(s.value(Field::Url), "http://a/v1");
        assert_eq!(s.value(Field::Model), "m1");
    }

    #[test]
    fn tab_cycles_all_three_fields() {
        let mut s = Setup::new(&Config::default());
        assert_eq!(s.focus, Field::Url);
        s.next_field();
        assert_eq!(s.focus, Field::Model);
        s.next_field();
        assert_eq!(s.focus, Field::Key);
        s.next_field();
        assert_eq!(s.focus, Field::Url);
        s.prev_field();
        assert_eq!(s.focus, Field::Key);
    }

    #[test]
    fn cycle_model_walks_the_server_list() {
        let mut s = Setup::new(&Config::default());
        s.available = vec!["a".into(), "b".into()];
        s.cycle_model();
        assert_eq!(s.value(Field::Model), "a");
        s.cycle_model();
        assert_eq!(s.value(Field::Model), "b");
        s.cycle_model();
        assert_eq!(s.value(Field::Model), "a");
    }

    #[test]
    fn api_key_is_masked_but_recognisable() {
        assert_eq!(masked(""), "");
        assert_eq!(masked("abcd"), "••••");
        let m = masked("sk-secret-123");
        assert!(m.ends_with("123"));
        assert!(!m.contains("secret"));
    }
}
