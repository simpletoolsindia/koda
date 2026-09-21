//! A dropdown you can type into: the one list component behind every
//! "choose one" in koda — model, provider, theme, mode, thinking level.
//!
//! Typing filters (fuzzy, prefix matches first, matched letters lit when
//! drawn); arrows, PgUp/PgDn and Home/End move; Enter chooses; Esc cancels.
//! The current value is marked, and each item can carry a detail line. The
//! state and its keys live here, free of any terminal, so they are tested
//! directly; `tui` draws it and acts on what it returns.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What choosing it means (a model id, a theme name, …).
    pub value: String,
    /// What the list shows.
    pub label: String,
    /// Dim text beside it: a provider's host and model, say.
    pub detail: String,
    /// The value in force now.
    pub current: bool,
}

impl Item {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: String::new(),
            current: false,
        }
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }

    pub fn current(mut self, yes: bool) -> Self {
        self.current = yes;
        self
    }

    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = l.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing that matters to the caller.
    Idle,
    /// The highlighted item changed — what a live preview listens for.
    Moved,
    Chosen(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Picker<K> {
    pub kind: K,
    pub title: String,
    pub items: Vec<Item>,
    pub filter: String,
    /// Index into `visible()`.
    pub sel: usize,
}

impl<K: Copy> Picker<K> {
    /// Opens on the current item when there is one.
    pub fn new(kind: K, title: impl Into<String>, items: Vec<Item>) -> Self {
        let sel = items.iter().position(|i| i.current).unwrap_or(0);
        Self {
            kind,
            title: title.into(),
            items,
            filter: String::new(),
            sel,
        }
    }

    /// Indices into `items` that match the filter, best first: prefix matches
    /// in their own order, then fuzzy ones by score.
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.trim().to_lowercase();
        if f.is_empty() {
            return (0..self.items.len()).collect();
        }
        let mut prefix = Vec::new();
        let mut fuzzy: Vec<(i32, usize)> = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            let label = it.label.to_lowercase();
            if label.starts_with(&f) || it.value.to_lowercase().starts_with(&f) {
                prefix.push(i);
            } else if let Some(s) = crate::fuzzy::score(&it.label, &f)
                .or_else(|| crate::fuzzy::score(&it.detail, &f).map(|s| s - 20))
            {
                fuzzy.push((s, i));
            }
        }
        fuzzy.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
        prefix.extend(fuzzy.into_iter().map(|(_, i)| i));
        prefix
    }

    pub fn selected(&self) -> Option<&Item> {
        self.visible().get(self.sel).map(|&i| &self.items[i])
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        let n = self.visible().len();
        let before = self.selected().map(|i| i.value.clone());
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Outcome::Cancelled,
            KeyCode::Enter => {
                return match self.selected() {
                    Some(it) => Outcome::Chosen(it.value.clone()),
                    None => Outcome::Idle,
                }
            }
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.sel = 0;
            }
            // Ctrl+N / Ctrl+P, as in every other list in a terminal.
            KeyCode::Char('n') if ctrl => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('p') if ctrl => self.sel = self.sel.saturating_sub(1),
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.sel = 0;
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.sel = 0;
            }
            _ => return Outcome::Idle,
        }
        if self.selected().map(|i| i.value.clone()) != before {
            Outcome::Moved
        } else {
            Outcome::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn picker() -> Picker<u8> {
        Picker::new(
            0,
            "t",
            vec![
                Item::new("qwen2.5-coder:14b"),
                Item::new("llama3.1:8b").current(true),
                Item::new("gemma3:4b").detail("google"),
                Item::new("qwen3:32b"),
            ],
        )
    }

    #[test]
    fn it_opens_on_the_current_value() {
        assert_eq!(picker().selected().unwrap().value, "llama3.1:8b");
    }

    #[test]
    fn typing_filters_prefix_first_then_fuzzy() {
        let mut p = picker();
        for c in "qw".chars() {
            p.key(k(KeyCode::Char(c)));
        }
        let shown: Vec<&str> = p
            .visible()
            .iter()
            .map(|&i| p.items[i].value.as_str())
            .collect();
        assert_eq!(shown, vec!["qwen2.5-coder:14b", "qwen3:32b"]);
        assert_eq!(p.sel, 0, "the filter resets the highlight");
        // The detail is searchable, a little below the label.
        let mut p = picker();
        for c in "google".chars() {
            p.key(k(KeyCode::Char(c)));
        }
        assert_eq!(p.selected().unwrap().value, "gemma3:4b");
        p.key(k(KeyCode::Backspace));
        assert_eq!(p.filter, "googl");
    }

    #[test]
    fn keys_move_choose_and_cancel() {
        let mut p = picker();
        assert_eq!(p.key(k(KeyCode::Down)), Outcome::Moved);
        assert_eq!(p.selected().unwrap().value, "gemma3:4b");
        assert_eq!(p.key(k(KeyCode::End)), Outcome::Moved);
        assert_eq!(p.key(k(KeyCode::Down)), Outcome::Idle, "stops at the end");
        assert_eq!(
            p.key(k(KeyCode::Enter)),
            Outcome::Chosen("qwen3:32b".into())
        );
        assert_eq!(p.key(k(KeyCode::Esc)), Outcome::Cancelled);
        // `j`, `k` and `q` are letters now: they filter.
        let mut p = picker();
        p.key(k(KeyCode::Char('q')));
        assert_eq!(p.filter, "q");
    }

    #[test]
    fn nothing_matching_chooses_nothing() {
        let mut p = picker();
        for c in "zzz".chars() {
            p.key(k(KeyCode::Char(c)));
        }
        assert!(p.selected().is_none());
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Idle);
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn page_home_and_emacs_keys_move_within_bounds() {
        let items: Vec<Item> = (0..25).map(|i| Item::new(format!("m{i:02}"))).collect();
        let mut p = Picker::new(0u8, "t", items);
        assert_eq!(p.sel, 0, "no current value opens at the top");
        assert_eq!(p.key(k(KeyCode::Up)), Outcome::Idle, "already at the top");
        p.key(k(KeyCode::PageDown));
        assert_eq!(p.sel, 10);
        p.key(k(KeyCode::PageDown));
        p.key(k(KeyCode::PageDown));
        assert_eq!(p.sel, 24, "page down stops at the end");
        p.key(k(KeyCode::PageUp));
        assert_eq!(p.sel, 14);
        p.key(k(KeyCode::Home));
        assert_eq!(p.sel, 0);
        p.key(ctrl('n'));
        p.key(ctrl('n'));
        assert_eq!(p.sel, 2);
        p.key(ctrl('p'));
        assert_eq!(p.sel, 1);
    }

    #[test]
    fn ctrl_u_clears_the_filter_and_ctrl_letters_do_not_type() {
        let mut p = picker();
        for c in "qwen".chars() {
            p.key(k(KeyCode::Char(c)));
        }
        p.key(ctrl('x'));
        assert_eq!(p.filter, "qwen", "a control chord is not text");
        p.key(ctrl('u'));
        assert_eq!(p.filter, "");
        assert_eq!(p.visible().len(), 4);
        assert_eq!(p.sel, 0);
    }

    #[test]
    fn the_value_matches_as_well_as_the_label() {
        let mut p = Picker::new(
            0u8,
            "t",
            vec![
                Item::new("ollama").label("Local Ollama"),
                Item::new("openai").label("OpenAI API"),
            ],
        );
        for c in "oll".chars() {
            p.key(k(KeyCode::Char(c)));
        }
        assert_eq!(p.selected().unwrap().value, "ollama");
        assert_eq!(p.key(k(KeyCode::Enter)), Outcome::Chosen("ollama".into()));
    }

    #[test]
    fn narrowing_the_filter_reports_a_move() {
        let mut p = picker();
        // Opens on llama; typing `g` jumps to gemma, which a preview must see.
        assert_eq!(p.key(k(KeyCode::Char('g'))), Outcome::Moved);
        assert_eq!(p.selected().unwrap().value, "gemma3:4b");
        assert_eq!(
            p.key(k(KeyCode::F(5))),
            Outcome::Idle,
            "unbound keys do nothing"
        );
    }

    #[test]
    fn an_empty_list_is_safe() {
        let mut p: Picker<u8> = Picker::new(0, "t", vec![]);
        for key in [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::End,
            KeyCode::PageDown,
            KeyCode::Enter,
        ] {
            assert_eq!(p.key(k(key)), Outcome::Idle);
        }
        assert!(p.selected().is_none());
        assert_eq!(p.key(k(KeyCode::Esc)), Outcome::Cancelled);
    }

    #[test]
    fn builders_set_what_they_say() {
        let it = Item::new("v").label("L").detail("d").current(true);
        assert_eq!(
            (
                it.value.as_str(),
                it.label.as_str(),
                it.detail.as_str(),
                it.current
            ),
            ("v", "L", "d", true)
        );
        assert_eq!(Item::new("x").label, "x", "the label defaults to the value");
    }
}
