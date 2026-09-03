//! Input line editor: emacs-style keys, soft wrapping, and command history.

use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Editor {
    pub buf: String,
    /// Byte offset of the caret inside `buf`.
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
}

impl Editor {
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.hist_pos = None;
    }

    pub fn insert(&mut self, s: &str) {
        self.buf.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn take(&mut self) -> String {
        let out = std::mem::take(&mut self.buf);
        self.cursor = 0;
        self.hist_pos = None;
        if !out.trim().is_empty() && self.history.last().map(|h| h != &out).unwrap_or(true) {
            self.history.push(out.clone());
        }
        out
    }

    fn prev_boundary(&self, from: usize) -> usize {
        self.buf[..from]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn next_boundary(&self, from: usize) -> usize {
        self.buf[from..]
            .chars()
            .next()
            .map(|c| from + c.len_utf8())
            .unwrap_or(from)
    }

    pub fn left(&mut self) {
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn right(&mut self) {
        self.cursor = self.next_boundary(self.cursor);
    }

    pub fn home(&mut self) {
        self.cursor = self.buf[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.cursor = self.buf[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.buf.len());
    }

    pub fn start(&mut self) {
        self.cursor = 0;
    }

    pub fn finish(&mut self) {
        self.cursor = self.buf.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_boundary(self.cursor);
        self.buf.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    pub fn delete(&mut self) {
        let next = self.next_boundary(self.cursor);
        if next != self.cursor {
            self.buf.replace_range(self.cursor..next, "");
        }
    }

    fn word_start(&self) -> usize {
        let mut i = self.cursor;
        let bytes = self.buf.as_bytes();
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i = self.prev_boundary(i);
        }
        while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
            i = self.prev_boundary(i);
        }
        i
    }

    fn word_end(&self) -> usize {
        let bytes = self.buf.as_bytes();
        let mut i = self.cursor;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i = self.next_boundary(i);
        }
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i = self.next_boundary(i);
        }
        i
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
    }

    pub fn kill_word(&mut self) {
        let start = self.word_start();
        self.buf.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn kill_to_end(&mut self) {
        let end = self.buf[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.buf.len());
        self.buf.replace_range(self.cursor..end, "");
    }

    pub fn kill_to_start(&mut self) {
        let start = self.buf[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.buf.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Byte offset of the current line's start, and the caret's offset into it.
    fn line_pos(&self) -> (usize, usize) {
        let start = self.buf[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        (start, self.cursor - start)
    }

    fn snap(&mut self) {
        while self.cursor < self.buf.len() && !self.buf.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    /// Move the caret to the line above, keeping the column where possible.
    pub fn up(&mut self) {
        let (start, col) = self.line_pos();
        if start == 0 {
            return;
        }
        let prev_start = self.buf[..start - 1]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_len = start - 1 - prev_start;
        self.cursor = prev_start + col.min(prev_len);
        self.snap();
    }

    pub fn down(&mut self) {
        let (start, col) = self.line_pos();
        let Some(rel_nl) = self.buf[start..].find('\n') else {
            return;
        };
        let next_start = start + rel_nl + 1;
        let next_len = self.buf[next_start..]
            .find('\n')
            .unwrap_or(self.buf.len() - next_start);
        self.cursor = next_start + col.min(next_len);
        self.snap();
    }

    /// Returns true when the key was consumed by history navigation.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.hist_pos {
            None => {
                self.draft = self.buf.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_pos = Some(next);
        self.buf = self.history[next].clone();
        self.cursor = self.buf.len();
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(i) = self.hist_pos else {
            return false;
        };
        if i + 1 >= self.history.len() {
            self.hist_pos = None;
            self.buf = std::mem::take(&mut self.draft);
        } else {
            self.hist_pos = Some(i + 1);
            self.buf = self.history[i + 1].clone();
        }
        self.cursor = self.buf.len();
        true
    }

    /// True when the caret sits on the first visual row (used to decide whether
    /// Up should navigate history or move within the text).
    pub fn on_first_line(&self) -> bool {
        !self.buf[..self.cursor].contains('\n')
    }

    pub fn on_last_line(&self) -> bool {
        !self.buf[self.cursor..].contains('\n')
    }

    /// The `@path` token the caret sits in, as (start byte, query).
    ///
    /// Returns None unless the caret is inside a token that begins with `@`,
    /// which is what makes the completion popup appear and disappear at the
    /// right moments without any extra state.
    pub fn mention(&self) -> Option<(usize, String)> {
        let before = &self.buf[..self.cursor];
        let start = before
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let token = &before[start..];
        let rest = token.strip_prefix('@')?;
        // A second @ means this is not a path (an email, say).
        if rest.contains('@') {
            return None;
        }
        Some((start, rest.to_string()))
    }

    /// Replace the `@...` token under the caret with `path`.
    pub fn replace_mention(&mut self, path: &str) {
        let Some((start, _)) = self.mention() else {
            return;
        };
        self.buf.replace_range(start..self.cursor, path);
        self.cursor = start + path.len();
        // A trailing space means the popup closes and you can keep typing.
        self.insert(" ");
    }

    /// Soft-wrap the buffer to `width`, returning the visual rows and the caret
    /// position as (row, column).
    pub fn visual(&self, width: usize) -> (Vec<String>, usize, usize) {
        let width = width.max(4);
        let mut rows: Vec<String> = Vec::new();
        let mut caret = (0usize, 0usize);
        let mut idx = 0usize;

        for (li, logical) in self.buf.split('\n').enumerate() {
            if li > 0 {
                idx += 1; // the newline itself
            }
            let mut cur = String::new();
            let mut w = 0usize;
            for ch in logical.chars() {
                let cw = ch.width().unwrap_or(0).max(1);
                if w + cw > width {
                    rows.push(std::mem::take(&mut cur));
                    w = 0;
                }
                if idx == self.cursor {
                    caret = (rows.len(), w);
                }
                cur.push(ch);
                w += cw;
                idx += ch.len_utf8();
            }
            if idx == self.cursor {
                caret = (rows.len(), w);
            }
            rows.push(cur);
        }
        if rows.is_empty() {
            rows.push(String::new());
        }
        (rows, caret.0, caret.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_moves() {
        let mut e = Editor::default();
        e.insert("hello world");
        e.word_left();
        assert_eq!(&e.buf[e.cursor..], "world");
        e.kill_to_end();
        assert_eq!(e.buf, "hello ");
        e.backspace();
        assert_eq!(e.buf, "hello");
    }

    #[test]
    fn wraps_and_tracks_caret() {
        let mut e = Editor::default();
        e.insert("abcdef");
        let (rows, r, c) = e.visual(4);
        assert_eq!(rows, vec!["abcd", "ef"]);
        assert_eq!((r, c), (1, 2));
    }

    #[test]
    fn history_round_trip() {
        let mut e = Editor::default();
        e.insert("first");
        e.take();
        e.insert("draft");
        assert!(e.history_prev());
        assert_eq!(e.buf, "first");
        assert!(e.history_next());
        assert_eq!(e.buf, "draft");
    }

    #[test]
    fn moves_between_lines() {
        let mut e = Editor::default();
        e.insert("first line\nsecond\nthird line");
        e.up();
        assert_eq!(&e.buf[e.cursor..], "\nthird line");
        e.up();
        // Column 6 of "first line" is the start of "line".
        assert_eq!(&e.buf[e.cursor..], "line\nsecond\nthird line");
        e.down();
        e.down();
        assert!(e.on_last_line());
    }

    #[test]
    fn detects_the_mention_under_the_caret() {
        let mut e = Editor::default();
        e.insert("look at @src/tu");
        let (start, query) = e.mention().expect("should see a mention");
        assert_eq!(query, "src/tu");
        assert_eq!(&e.buf[start..start + 1], "@");

        e.insert(" and");
        assert!(e.mention().is_none(), "space ends the mention");

        let mut e2 = Editor::default();
        e2.insert("mail me at a@b.com");
        assert!(e2.mention().is_none(), "an email is not a path mention");
    }

    #[test]
    fn replacing_a_mention_inserts_the_path() {
        let mut e = Editor::default();
        e.insert("check @vw");
        e.replace_mention("src/view.rs");
        assert_eq!(e.buf, "check src/view.rs ");
        assert_eq!(e.cursor, e.buf.len());
        assert!(e.mention().is_none());
    }

    #[test]
    fn handles_multibyte() {
        let mut e = Editor::default();
        e.insert("héllo");
        e.left();
        e.left();
        e.backspace();
        assert_eq!(e.buf, "hélo");
    }
}
