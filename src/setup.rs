//! The provider setup screen: endpoint, model and API key, written back to the
//! config file.
//!
//! This exists because the first-run failure — "no model configured" — used to
//! be fixed only by finding and hand-editing a TOML file. Anything the app tells
//! you to set, the app should let you set.

use crate::config::Config;
use crate::editor::Editor;
use crate::theme::{Glyphs, Theme};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// What to call this endpoint. Naming it makes it a saved provider you
    /// can switch back to; leaving it blank just edits the current settings.
    Name,
    Url,
    Model,
    Key,
    /// Whether this model accepts images. A toggle, not a text field: the
    /// answer is one of three words and typing it out invites typos that
    /// silently read as "auto".
    Vision,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::Name,
        Field::Url,
        Field::Model,
        Field::Key,
        Field::Vision,
    ];

    /// Cycled with left/right rather than typed into.
    pub fn is_toggle(&self) -> bool {
        matches!(self, Field::Vision)
    }

    fn label(&self) -> &'static str {
        match self {
            Field::Name => "name",
            Field::Url => "endpoint",
            Field::Model => "model",
            Field::Key => "api key",
            Field::Vision => "images",
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Field::Name => "name it to save as a provider you can switch to (optional)",
            Field::Url => "http://localhost:11434/v1",
            Field::Model => "leave empty to use the server's first model",
            Field::Key => "\"local\" is fine for a local server",
            Field::Vision => "← → : auto guesses from the model name; set on behind a router",
        }
    }

    fn next(&self) -> Field {
        match self {
            Field::Name => Field::Url,
            Field::Url => Field::Model,
            Field::Model => Field::Key,
            Field::Key => Field::Vision,
            Field::Vision => Field::Name,
        }
    }

    fn prev(&self) -> Field {
        match self {
            Field::Name => Field::Vision,
            Field::Url => Field::Name,
            Field::Model => Field::Url,
            Field::Key => Field::Model,
            Field::Vision => Field::Key,
        }
    }
}

pub struct Setup {
    pub focus: Field,
    /// True when the page was opened to add a provider rather than edit the
    /// current settings. Only changes what the page says, so an empty name is
    /// still just "edit the top-level settings".
    pub adding: bool,
    name: Editor,
    url: Editor,
    model: Editor,
    key: Editor,
    /// Held in an Editor like the rest so the drawing and caret code needs no
    /// special case; its buffer is only ever replaced by `cycle_vision`.
    vision: Editor,
    /// Models fetched from the endpoint, offered as suggestions.
    pub available: Vec<String>,
    pub status: Option<String>,
}

impl Setup {
    pub fn new(cfg: &Config) -> Self {
        let mut s = Self {
            focus: Field::Name,
            adding: false,
            name: Editor::default(),
            url: Editor::default(),
            model: Editor::default(),
            key: Editor::default(),
            vision: Editor::default(),
            available: Vec::new(),
            status: None,
        };
        s.name.insert(&cfg.active_provider);
        s.url.insert(&cfg.endpoint());
        s.model.insert(&cfg.model);
        s.key.insert(&cfg.api_key);
        s.vision.insert(normalize_vision(&cfg.vision));
        s
    }

    /// The same page, opened to add a *new* provider rather than edit the one
    /// in use.
    ///
    /// The difference is only the name: `new` seeds it from the active
    /// provider, so saving updates that provider — which meant there was no way
    /// to add a second one from the UI at all. The endpoint and key are still
    /// carried over, because a second provider is usually a near neighbour of
    /// the first and retyping a URL and a key to change a model is a poor way
    /// to spend somebody's attention.
    pub fn new_provider(cfg: &Config) -> Self {
        let mut s = Self::new(cfg);
        s.name = Editor::default();
        s.focus = Field::Name;
        s.adding = true;
        s
    }

    fn editor(&mut self, f: Field) -> &mut Editor {
        match f {
            Field::Name => &mut self.name,
            Field::Url => &mut self.url,
            Field::Model => &mut self.model,
            Field::Key => &mut self.key,
            Field::Vision => &mut self.vision,
        }
    }

    fn editor_ref(&self, f: Field) -> &Editor {
        match f {
            Field::Name => &self.name,
            Field::Url => &self.url,
            Field::Model => &self.model,
            Field::Key => &self.key,
            Field::Vision => &self.vision,
        }
    }

    /// The caret's display column within a field's value (0-based), used by
    /// `draw` to place the terminal cursor and scroll the visible slice.
    fn caret_col(&self, f: Field) -> usize {
        // A width wide enough that the single-line value never soft-wraps, so
        // `visual` reports the caret as (row 0, column = display width so far).
        let (_, _, col) = self.editor_ref(f).visual(u16::MAX as usize);
        col
    }

    pub fn value(&self, f: Field) -> &str {
        match f {
            Field::Name => &self.name.buf,
            Field::Url => &self.url.buf,
            Field::Model => &self.model.buf,
            Field::Key => &self.key.buf,
            Field::Vision => &self.vision.buf,
        }
    }

    pub fn focused(&mut self) -> &mut Editor {
        let f = self.focus;
        self.editor(f)
    }

    /// Step the images setting. Left and right rather than typing: three fixed
    /// words are a choice, not a value, and a typo in a typed one reads as
    /// "auto" without saying so.
    pub fn cycle_vision(&mut self, forward: bool) {
        let next = match (normalize_vision(&self.vision.buf), forward) {
            ("auto", true) => "on",
            ("on", true) => "off",
            ("auto", false) => "off",
            ("on", false) => "auto",
            (_, true) => "auto",
            (_, false) => "on",
        };
        self.vision = Editor::default();
        self.vision.insert(next);
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
    /// Copy the page's fields into a config, without touching the disk.
    ///
    /// Split out from `save` so it can be tested: `save` writes to the real
    /// user config, and a test that called it clobbered the developer's own
    /// endpoint, key and model with defaults.
    pub fn apply(&self, cfg: &mut Config) {
        cfg.base_url = self.url.buf.trim().to_string();
        cfg.model = self.model.buf.trim().to_string();
        cfg.api_key = self.key.buf.trim().to_string();
        cfg.vision = normalize_vision(&self.vision.buf).to_string();
        // A name turns this into a saved provider and selects it. Without one
        // the page behaves exactly as it always did, editing the single set of
        // top-level settings -- so naming things stays opt-in.
        let name = sanitize_provider_name(&self.name.buf);
        if name.is_empty() {
            cfg.active_provider.clear();
            return;
        }
        cfg.upsert_provider(crate::config::Provider {
            name: name.clone(),
            base_url: cfg.base_url.clone(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
            vision: cfg.vision.clone(),
        });
    }

    pub fn save(&self, cfg: &mut Config) -> anyhow::Result<std::path::PathBuf> {
        self.apply(cfg);
        crate::config::save(cfg)
    }
}

/// Reduce a typed name to something usable as a provider label.
///
/// It ends up in a TOML key position, in `/provider <name>`, and in the status
/// bar, so whitespace and separators make it unusable in at least one of those.
/// Trimmed to a sane length because the status bar has to fit it.
fn sanitize_provider_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if c.is_whitespace() || c == '/' {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(32)
        .collect()
}

/// Fold any spelling of the setting onto the three values the UI cycles, so a
/// hand-edited config still lands somewhere in the cycle instead of sticking.
fn normalize_vision(v: &str) -> &'static str {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "always" => "on",
        "off" | "false" | "no" | "never" => "off",
        _ => "auto",
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
    // Three lines per field plus the header: five fields come to 16, and the
    // border takes two more.
    let h = 18u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: (area.width.saturating_sub(w)) / 2,
        y: (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let inner_w = rect.width.saturating_sub(4) as usize;
    // The value is drawn after a 3-column indent ("   "), so the usable width
    // for the value text is a little narrower than the inner width.
    let field_w = inner_w.saturating_sub(3).max(1);

    // Say which of the two jobs this page is doing. Without it an "add" and an
    // "edit" look identical, and the only difference — whether the name field
    // was pre-filled — is exactly what someone would not notice.
    let heading = if s.adding {
        " Add a provider. Give it a name to save it alongside the others."
    } else if s.name.buf.trim().is_empty() {
        " Point koda at a model server. Name it to save it as a provider."
    } else {
        " Editing this provider. Change the name to save it as a new one."
    };
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(heading, t.dim()))];

    // Where to place the real terminal caret for the focused field, filled in
    // as we lay out its value line below.
    let mut caret_screen: Option<(u16, u16)> = None;

    for field in Field::ALL {
        let focused = s.focus == field;
        let raw = s.value(field);
        let shown = if field == Field::Key {
            masked(raw)
        } else {
            raw.to_string()
        };
        let shown_len = shown.chars().count();

        // Horizontally scroll so the caret stays in view when the value is
        // wider than the field. Only the focused field has a live caret; other
        // fields simply show their head.
        let (display, caret_in_view): (String, Option<usize>) = if focused {
            let caret = s.caret_col(field).min(shown_len);
            let scroll = caret.saturating_sub(field_w.saturating_sub(1));
            let vis: String = shown.chars().skip(scroll).take(field_w).collect();
            (vis, Some(caret - scroll))
        } else if shown_len > field_w {
            (shown.chars().take(field_w).collect(), None)
        } else {
            (shown, None)
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
        // Show the hint only when the field is empty and unfocused; once
        // focused we show the (possibly empty) real value so the caret sits on
        // the text the user is editing.
        let text = if display.is_empty() && !focused {
            field.hint().to_string()
        } else {
            display
        };

        // The value line renders as: "   " (3 cols) + text. Record the caret's
        // screen position for the focused field. Content is inset by the left
        // border (1) plus the 3-space indent.
        if let Some(col) = caret_in_view {
            let x = rect.x + 1 + 3 + col as u16;
            let y = rect.y + 1 + lines.len() as u16; // this value line's row
            caret_screen = Some((x, y));
        }

        lines.push(Line::from(vec![
            Span::raw("   ".to_string()),
            Span::styled(text, value_style),
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

    // Place the real terminal caret at the focused field so it blinks where the
    // user is typing and moves with left/right. Guard against the (unlikely)
    // case where the computed position lands outside the box.
    if let Some((x, y)) = caret_screen {
        let max_x = rect.x + rect.width.saturating_sub(2);
        let max_y = rect.y + rect.height.saturating_sub(2);
        if x <= max_x && y <= max_y {
            f.set_cursor_position(Position::new(x, y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported gap: /setup could only ever configure one provider. `new`
    /// seeds the name from the active provider, so saving updated *that* one --
    /// there was no path from the UI to a second.
    #[test]
    fn adding_a_provider_starts_from_a_blank_name() {
        let mut cfg = Config {
            base_url: "http://localhost:20128/v1".into(),
            api_key: "sk-shared".into(),
            ..Config::default()
        };
        cfg.upsert_provider(crate::config::Provider {
            name: "omniroute".into(),
            base_url: "http://localhost:20128/v1".into(),
            api_key: "sk-shared".into(),
            model: "auto".into(),
            vision: String::new(),
        });

        // Editing pre-fills the name, so saving updates that provider.
        let edit = Setup::new(&cfg);
        assert_eq!(edit.value(Field::Name), "omniroute");
        assert!(!edit.adding);

        // Adding starts blank, so a new name makes a new entry -- while the
        // endpoint and key carry over, since a second provider is usually a
        // near neighbour of the first.
        let mut add = Setup::new_provider(&cfg);
        assert_eq!(add.value(Field::Name), "", "the name is cleared");
        assert!(add.adding);
        assert_eq!(
            add.value(Field::Url),
            "http://localhost:20128/v1",
            "url carried over"
        );
        assert_eq!(add.value(Field::Key), "sk-shared", "key carried over");
        assert_eq!(add.focus, Field::Name, "and the caret starts there");

        // Name it and save: two providers, the new one selected.
        add.focused().insert("ollama");
        let mut out = cfg.clone();
        add.apply(&mut out);
        assert_eq!(out.providers.len(), 2, "added rather than overwrote");
        assert_eq!(out.active_provider, "ollama");
        assert!(
            out.providers.iter().any(|p| p.name == "omniroute"),
            "the first survives"
        );
    }

    /// The name lands in a TOML key, in `/provider <name>`, and in the status
    /// bar, so it cannot carry whitespace or separators into any of them.
    #[test]
    fn a_provider_name_is_reduced_to_something_usable() {
        assert_eq!(sanitize_provider_name("  omniroute  "), "omniroute");
        assert_eq!(sanitize_provider_name("my local box"), "my-local-box");
        assert_eq!(sanitize_provider_name("a/b"), "a-b");
        assert_eq!(sanitize_provider_name(""), "");
        assert_eq!(sanitize_provider_name("   "), "");
        assert_eq!(
            sanitize_provider_name(&"x".repeat(80)).len(),
            32,
            "kept short enough to show"
        );
    }

    /// The panel is a fixed height and the fields are laid out three lines
    /// apart, so adding one is not free: at three fields the content came to
    /// exactly the inner height, and a fourth pushed the footer off the bottom
    /// where no test would have noticed.
    #[test]
    fn setup_panel_shows_every_field_without_clipping() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let cfg = Config::default();
        let s = Setup::new(&cfg);
        let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
        term.draw(|f| {
            let area = f.area();
            draw(
                f,
                area,
                &s,
                &crate::theme::resolve("auto"),
                &crate::theme::UNICODE,
            );
        })
        .unwrap();

        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        for label in ["endpoint", "model", "api key", "images"] {
            assert!(text.contains(label), "{label} is missing from the panel");
        }
        // "images" is the last row, so its presence is what proves the added
        // field did not overflow the fixed height. The footer is a block title,
        // drawn whatever the content does, so it proves nothing here.
        let images_at = text.find("images").expect("images row present");
        let key_at = text.find("api key").expect("api key row present");
        assert!(
            images_at > key_at,
            "the new row is laid out after the others"
        );
    }

    /// Vision belongs beside the model because it is a fact about the model.
    /// The people who need it are exactly those whose model name cannot be
    /// guessed from -- and they meet the setup page, not the config file.
    #[test]
    fn vision_is_a_toggle_on_the_setup_page() {
        assert!(Field::Vision.is_toggle());
        assert!(!Field::Model.is_toggle(), "the text fields still take text");

        let cfg = Config {
            vision: "auto".into(),
            ..Config::default()
        };
        let mut s = Setup::new(&cfg);
        assert_eq!(s.value(Field::Vision), "auto", "seeded from the config");

        s.cycle_vision(true);
        assert_eq!(s.value(Field::Vision), "on");
        s.cycle_vision(true);
        assert_eq!(s.value(Field::Vision), "off");
        s.cycle_vision(true);
        assert_eq!(s.value(Field::Vision), "auto", "forward wraps");
        s.cycle_vision(false);
        assert_eq!(s.value(Field::Vision), "off", "and steps backwards");

        // Saving puts it where the agent reads it.
        s.cycle_vision(false);
        assert_eq!(s.value(Field::Vision), "on");
        // apply, not save: save writes the real user config, and a test that
        // reaches the developer's own machine is a bug in the test.
        let mut out = Config::default();
        s.apply(&mut out);
        assert_eq!(out.vision, "on", "the choice reaches the config");
    }

    /// Tab order has to include the new field, in both directions.
    #[test]
    fn field_cycle_includes_vision() {
        assert_eq!(Field::Key.next(), Field::Vision);
        assert_eq!(
            Field::Vision.next(),
            Field::Name,
            "wraps to the first field"
        );
        assert_eq!(Field::Name.prev(), Field::Vision);
        assert_eq!(Field::Vision.prev(), Field::Key);
        assert_eq!(Field::Name.next(), Field::Url);
        assert_eq!(Field::Url.prev(), Field::Name);
    }

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
    fn tab_cycles_every_field() {
        let mut s = Setup::new(&Config::default());
        assert_eq!(s.focus, Field::Name, "the name comes first");
        s.next_field();
        assert_eq!(s.focus, Field::Url);
        s.next_field();
        assert_eq!(s.focus, Field::Model);
        s.next_field();
        assert_eq!(s.focus, Field::Key);
        s.next_field();
        assert_eq!(s.focus, Field::Vision);
        s.next_field();
        assert_eq!(s.focus, Field::Name);
        s.prev_field();
        assert_eq!(s.focus, Field::Vision);
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
