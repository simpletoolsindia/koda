//! Small markdown renderer for the transcript, plus a keyword-based syntax
//! highlighter. Deliberately not a full parser: it handles the subset that LLM
//! output actually uses, in a single pass, with no allocations beyond the spans.

use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Where a growing markdown document can be split without changing how it
/// renders.
///
/// `render` is a single pass over lines whose only cross-line state is (a) being
/// inside a fenced code block and (b) the contiguous rows of a table. So for a
/// byte offset `p` that starts a line, if no fence is open there and the line
/// before it is blank, then
///
/// ```text
/// render(text[..p-1]) ++ render(text[p..]) == render(text)
/// ```
///
/// (the dropped byte is the `\n` that separated the two halves, which `split`
/// consumes). A blank line before `p` is what rules out splitting a table.
///
/// Returns the largest such `p`, or 0 when the document has no safe split yet.
/// This is what lets a streaming reply re-render only its tail instead of the
/// whole block on every frame.
pub fn stable_prefix_end(text: &str) -> usize {
    let mut in_fence = false;
    let mut best = 0usize;
    let mut offset = 0usize;
    for line in text.split('\n') {
        // `offset` is this line's start; the next line starts after its '\n'.
        let next = offset + line.len() + 1;
        let fence = line.trim_start();
        if fence.starts_with("```") || fence.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && line.trim_end().is_empty() && next <= text.len() {
            // A blank line outside a fence: everything up to here is settled, so
            // the next line is a safe place to resume.
            best = next;
        }
        offset = next;
    }
    best
}

pub fn render(text: &str, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let width = width.max(8);
    let dim = || t.dim();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut code_lang: Option<String> = None;

    let all: Vec<&str> = text.split('\n').collect();
    let mut i = 0usize;
    while i < all.len() {
        let raw = all[i];
        i += 1;
        let trimmed = raw.trim_end();
        let fence = trimmed.trim_start();

        if fence.starts_with("```") || fence.starts_with("~~~") {
            match code_lang.take() {
                Some(_) => {} // closing fence
                None => {
                    let info = fence[3..].trim().to_string();
                    let label = if info.is_empty() {
                        "code".into()
                    } else {
                        info.clone()
                    };
                    out.push(Line::from(Span::styled(format!("  ─ {label}"), dim())));
                    code_lang = Some(normalize_lang(&info));
                }
            }
            continue;
        }

        if let Some(lang) = &code_lang {
            let spans = highlight(trimmed, lang, t);
            out.extend(wrap_spans(indent(spans, 2), width, 4));
            continue;
        }

        // Tables: a `|`-delimited row followed by a `|---|` separator.
        if is_table_row(trimmed)
            && all
                .get(i)
                .map(|n| is_table_divider(n.trim()))
                .unwrap_or(false)
        {
            let mut rows = vec![split_row(trimmed)];
            i += 1; // skip the divider
            while let Some(next) = all.get(i) {
                if !is_table_row(next.trim()) {
                    break;
                }
                rows.push(split_row(next.trim()));
                i += 1;
            }
            out.extend(render_table(&rows, width, t));
            continue;
        }

        if trimmed.is_empty() {
            out.push(Line::default());
            continue;
        }

        // Headings
        if let Some(rest) = strip_heading(trimmed) {
            out.extend(wrap_spans(
                vec![Span::styled(
                    rest.to_string(),
                    Style::default().fg(t.heading).add_modifier(Modifier::BOLD),
                )],
                width,
                0,
            ));
            continue;
        }

        // Horizontal rule
        if matches!(trimmed, "---" | "***" | "___") {
            out.push(Line::from(Span::styled("─".repeat(width.min(60)), dim())));
            continue;
        }

        // Block quote
        if let Some(rest) = trimmed
            .strip_prefix("> ")
            .or_else(|| trimmed.strip_prefix(">"))
        {
            let mut spans = vec![Span::styled("│ ", dim())];
            spans.extend(inline(rest, Style::default().fg(t.muted), t));
            out.extend(wrap_spans(spans, width, 2));
            continue;
        }

        // Lists (preserving nesting indentation)
        if let Some((lead, marker, rest)) = strip_list(trimmed) {
            // Task lists read better as a checkbox than as "- [x] text".
            let (marker, rest, marker_colour) = match rest.get(..4) {
                Some("[ ] ") => ("☐ ".to_string(), &rest[4..], t.muted),
                Some("[x] ") | Some("[X] ") => ("☑ ".to_string(), &rest[4..], t.success),
                _ => (marker.clone(), rest, t.heading),
            };
            let mut spans = vec![
                Span::raw(" ".repeat(lead)),
                Span::styled(marker.clone(), Style::default().fg(marker_colour)),
            ];
            spans.extend(inline(rest, Style::default(), t));
            let hang = lead + marker.width();
            out.extend(wrap_spans(spans, width, hang));
            continue;
        }

        out.extend(wrap_spans(inline(trimmed, t.body(), t), width, 0));
    }
    out
}

/// Render a unified diff the way `git diff` reads: a line-number gutter, then
/// a `+`/`-` sign, then the content. The sign carries the meaning on its own,
/// so this stays readable with no colour at all.
pub fn render_diff(text: &str, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let dim = || t.dim();
    let mut out = Vec::new();
    let gutter = 4usize;
    let body_w = width.saturating_sub(gutter + 3).max(8);
    let mut old_ln = 0usize;
    let mut new_ln = 0usize;

    for raw in text.split('\n') {
        // Keep the leading marker byte intact: a context line for an empty
        // source line is exactly " ", which trimming would erase.
        let line = raw.trim_end_matches(['\r', '\n']);

        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some((o, n)) = parse_hunk(line) {
            old_ln = o;
            new_ln = n;
            out.push(Line::from(vec![
                Span::raw(" ".repeat(gutter + 1)),
                Span::styled(line.to_string(), t.fg(t.diff_hunk)),
            ]));
            continue;
        }
        // The hunk header names the first line, so report the current counter
        // and advance afterwards.
        let (sign, content, style, num) = match line.chars().next() {
            Some('+') => {
                let n = new_ln;
                new_ln += 1;
                ("+", &line[1..], t.emphasis(t.diff_add), Some(n))
            }
            Some('-') => {
                let n = old_ln;
                old_ln += 1;
                ("-", &line[1..], t.emphasis(t.diff_del), Some(n))
            }
            Some(' ') => {
                let n = new_ln;
                old_ln += 1;
                new_ln += 1;
                (" ", &line[1..], t.body(), Some(n))
            }
            None => {
                out.push(Line::default());
                continue;
            }
            _ => (" ", line, t.dim(), None),
        };

        let content = content.trim_end();
        for (i, chunk) in hard_wrap(content, body_w).into_iter().enumerate() {
            let label = match (num, i) {
                (Some(n), 0) => format!("{n:>gutter$} "),
                _ => " ".repeat(gutter + 1),
            };
            out.push(Line::from(vec![
                Span::styled(label, dim()),
                Span::styled(sign.to_string(), style),
                Span::styled(chunk, style),
            ]));
        }
    }
    out
}

/// `@@ -12,4 +12,5 @@` -> (12, 12)
fn parse_hunk(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ ")?;
    let mut parts = rest.split_whitespace();
    let old = parts.next()?.trim_start_matches('-');
    let new = parts.next()?.trim_start_matches('+');
    let first = |s: &str| -> Option<usize> { s.split(',').next()?.parse().ok() };
    Some((first(old)?, first(new)?))
}

fn is_table_row(s: &str) -> bool {
    let s = s.trim();
    s.starts_with('|') && s.len() > 1
}

fn is_table_divider(s: &str) -> bool {
    is_table_row(s)
        && s.trim().trim_matches('|').split('|').all(|c| {
            let c = c.trim();
            !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
        })
}

fn split_row(s: &str) -> Vec<String> {
    s.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

/// Render a table with light column separators and a rule under the header.
/// Cells are rendered through `inline`, so **bold**, *italic*, `code` and links
/// inside a cell are styled rather than shown as raw markdown.
fn render_table(rows: &[Vec<String>], width: usize, t: &Theme) -> Vec<Line<'static>> {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    // Column widths are measured on the DISPLAY text (markdown markers removed),
    // so a `**bold**` cell doesn't reserve width for the asterisks.
    let mut w = vec![0usize; cols];
    for r in rows {
        for (cell_w, cell) in w.iter_mut().zip(r.iter()) {
            *cell_w = (*cell_w).max(strip_inline(cell).width());
        }
    }
    // Shrink the widest column(s) proportionally if the natural width overflows.
    let gap = 3usize; // " │ " separator
    let sep = " │ ";
    let border = |n: usize| "─".repeat(n);
    let natural: usize = w.iter().sum::<usize>() + gap * cols.saturating_sub(1) + 1;
    if natural > width {
        let mut over = natural - width;
        // Trim from the widest columns first, keeping a floor of 4.
        while over > 0 {
            let (idx, &widest) = w.iter().enumerate().max_by_key(|(_, v)| **v).unwrap();
            if widest <= 4 {
                break;
            }
            w[idx] -= 1;
            over -= 1;
        }
    }

    let total: usize = w.iter().sum::<usize>() + gap * cols.saturating_sub(1);
    let mut out = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut spans = vec![Span::raw(" ".to_string())];
        for (c, &cw) in w.iter().enumerate() {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            // Header stays bold+plain (predictable); body cells get inline styling.
            let base = if ri == 0 {
                Style::default().fg(t.text).add_modifier(Modifier::BOLD)
            } else {
                t.body()
            };
            let mut cell_spans = if ri == 0 {
                vec![Span::styled(strip_inline(cell), base)]
            } else {
                inline(cell, base, t)
            };
            // Truncate to the column width on display width, then pad.
            let shown_w: usize = cell_spans.iter().map(|s| s.content.width()).sum();
            if shown_w > cw {
                cell_spans = clip_spans(cell_spans, cw);
            }
            let pad =
                cw.saturating_sub(cell_spans.iter().map(|s| s.content.width()).sum::<usize>());
            spans.extend(cell_spans);
            if pad > 0 {
                spans.push(Span::raw(" ".repeat(pad)));
            }
            if c + 1 < cols {
                spans.push(Span::styled(sep.to_string(), t.dim()));
            }
        }
        out.push(Line::from(spans));
        if ri == 0 {
            out.push(Line::from(vec![
                Span::raw(" ".to_string()),
                Span::styled(border(total.min(width)), Style::default().fg(t.border)),
            ]));
        }
    }
    out
}

/// Remove inline markdown markers so a cell's on-screen width is measured on
/// what the reader actually sees, not the asterisks/backticks around it.
fn strip_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            i += 1;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            continue;
        }
        if c == '*' || c == '_' {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Clip a run of spans to `max` display columns, appending an ellipsis.
fn clip_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0usize;
    let cap = max.saturating_sub(1);
    for sp in spans {
        if used >= cap {
            break;
        }
        let w = sp.content.width();
        if used + w <= cap {
            used += w;
            out.push(sp);
        } else {
            let take = cap - used;
            let s: String = sp.content.chars().take(take).collect();
            out.push(Span::styled(s, sp.style));
            break;
        }
    }
    out.push(Span::raw("…".to_string()));
    out
}

fn strip_heading(s: &str) -> Option<&str> {
    let hashes = s.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = s[hashes..].trim_start();
        if !rest.is_empty() {
            return Some(rest);
        }
    }
    None
}

fn strip_list(s: &str) -> Option<(usize, String, &str)> {
    let lead = s.len() - s.trim_start().len();
    let body = s.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = body.strip_prefix(m) {
            return Some((lead, "▪ ".to_string(), rest));
        }
    }
    // Ordered list: `12. text`
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let after = &body[digits..];
        if let Some(rest) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some((lead, format!("{}. ", &body[..digits]), rest));
        }
    }
    None
}

fn indent(spans: Vec<Span<'static>>, n: usize) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len() + 1);
    out.push(Span::raw(" ".repeat(n)));
    out.extend(spans);
    out
}

/// Inline markdown: `code`, **bold**, *italic*.
fn inline(s: &str, base: Style, t: &Theme) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;

    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), base));
        }
    };

    while i < bytes.len() {
        let c = bytes[i];
        // [text](url) -> text, underlined; the URL itself is noise on screen.
        if c == '[' {
            if let Some(close) = find_char(&bytes, i + 1, ']') {
                if bytes.get(close + 1) == Some(&'(') {
                    if let Some(end) = find_char(&bytes, close + 2, ')') {
                        flush(&mut buf, &mut spans);
                        let label: String = bytes[i + 1..close].iter().collect();
                        spans.push(Span::styled(
                            label,
                            base.fg(t.info).add_modifier(Modifier::UNDERLINED),
                        ));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if c == '`' {
            if let Some(end) = find_char(&bytes, i + 1, '`') {
                flush(&mut buf, &mut spans);
                let code: String = bytes[i + 1..end].iter().collect();
                spans.push(Span::styled(code, base.fg(t.syn_string)));
                i = end + 1;
                continue;
            }
        }
        if c == '*' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            if let Some(end) = find_pair(&bytes, i + 2) {
                flush(&mut buf, &mut spans);
                let inner: String = bytes[i + 2..end].iter().collect();
                spans.push(Span::styled(inner, base.add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        }
        if (c == '*' || c == '_') && i + 1 < bytes.len() && bytes[i + 1] != c {
            if let Some(end) = find_char(&bytes, i + 1, c) {
                // Avoid mangling snake_case identifiers.
                let looks_like_word =
                    i > 0 && (bytes[i - 1].is_alphanumeric() || bytes[i - 1] == '_');
                if !looks_like_word {
                    flush(&mut buf, &mut spans);
                    let inner: String = bytes[i + 1..end].iter().collect();
                    spans.push(Span::styled(inner, base.add_modifier(Modifier::ITALIC)));
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|i| chars[*i] == target)
}

fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|i| chars[*i] == '*' && chars[*i + 1] == '*')
}

/// Word-wrap styled spans, indenting continuation lines by `hanging`.
pub fn wrap_spans(spans: Vec<Span<'static>>, width: usize, hanging: usize) -> Vec<Line<'static>> {
    let limit = width.max(8);
    let hanging = hanging.min(limit / 2);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut w = 0usize;

    let flush = |cur: &mut Vec<Span<'static>>, w: &mut usize, lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(std::mem::take(cur)));
        if hanging > 0 {
            cur.push(Span::raw(" ".repeat(hanging)));
            *w = hanging;
        } else {
            *w = 0;
        }
    };

    for span in spans {
        let style = span.style;
        let content = span.content.to_string();
        for token in tokens(&content) {
            let tw = token.width();
            if token.chars().all(char::is_whitespace) {
                if w > 0 && w < limit {
                    cur.push(Span::styled(token, style));
                    w += tw;
                }
                continue;
            }
            if tw > limit {
                // Word longer than the line: hard split.
                for piece in hard_wrap(&token, limit.saturating_sub(w).max(4)) {
                    let pw = piece.width();
                    if w + pw > limit && w > 0 {
                        flush(&mut cur, &mut w, &mut lines);
                    }
                    cur.push(Span::styled(piece, style));
                    w += pw;
                }
                continue;
            }
            if w + tw > limit && w > 0 {
                flush(&mut cur, &mut w, &mut lines);
                // Drop the leading space produced by wrapping.
                if let Some(last) = cur.last() {
                    if last.content.trim().is_empty() && cur.len() > 1 {
                        cur.pop();
                    }
                }
            }
            cur.push(Span::styled(token, style));
            w += tw;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(Line::from(cur));
    }
    lines
}

/// Split into alternating word / whitespace tokens.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut ws = false;
    for c in s.chars() {
        let is_ws = c.is_whitespace();
        if !cur.is_empty() && is_ws != ws {
            out.push(std::mem::take(&mut cur));
        }
        ws = is_ws;
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn hard_wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    if s.width() <= width {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.to_string().width().max(1);
        if w + cw > width {
            out.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// ---------------------------------------------------------------- highlighting

fn normalize_lang(info: &str) -> String {
    let l = info
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    match l.as_str() {
        "rs" => "rust".into(),
        "py" | "python3" => "python".into(),
        "js" | "jsx" | "mjs" | "cjs" => "javascript".into(),
        "ts" | "tsx" => "typescript".into(),
        "sh" | "zsh" | "bash" | "shell" | "console" | "terminal" => "shell".into(),
        "yml" => "yaml".into(),
        "c++" | "cc" | "cxx" | "hpp" | "h" => "cpp".into(),
        "" => "text".into(),
        other => other.into(),
    }
}

#[allow(dead_code)]
pub fn lang_for_path(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    normalize_lang(ext)
}

fn keywords(lang: &str) -> &'static [&'static str] {
    const RUST: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ];
    const PY: &[&str] = &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
        "try", "while", "with", "yield",
    ];
    const JS: &[&str] = &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "yield",
    ];
    const GO: &[&str] = &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ];
    const C: &[&str] = &[
        "auto",
        "bool",
        "break",
        "case",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "false",
        "float",
        "for",
        "goto",
        "if",
        "inline",
        "int",
        "long",
        "namespace",
        "new",
        "nullptr",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "true",
        "try",
        "typedef",
        "unsigned",
        "using",
        "virtual",
        "void",
        "while",
    ];
    const SH: &[&str] = &[
        "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for", "function",
        "if", "in", "local", "return", "set", "then", "unset", "until", "while",
    ];
    const SQL: &[&str] = &[
        "and", "as", "by", "create", "delete", "drop", "from", "group", "having", "insert", "into",
        "join", "left", "limit", "not", "null", "on", "or", "order", "select", "set", "table",
        "update", "values", "where",
    ];
    match lang {
        "rust" => RUST,
        "python" => PY,
        "javascript" | "typescript" => JS,
        "go" => GO,
        "c" | "cpp" | "java" | "swift" | "kotlin" => C,
        "shell" => SH,
        "sql" => SQL,
        _ => &[],
    }
}

fn comment_prefixes(lang: &str) -> &'static [&'static str] {
    match lang {
        "python" | "shell" | "yaml" | "toml" | "ruby" | "makefile" | "dockerfile" => &["#"],
        "sql" | "lua" | "haskell" => &["--"],
        "lisp" | "clojure" => &[";"],
        "text" | "json" => &[],
        _ => &["//"],
    }
}

/// Single-line syntax highlighting. Good enough for review at a glance and
/// costs a single pass over the line.
pub fn highlight(line: &str, lang: &str, t: &Theme) -> Vec<Span<'static>> {
    if lang == "text" || line.trim().is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    // Diff-style lines inside code fences read better with diff colouring.
    if lang == "diff" {
        let style = match line.chars().next() {
            Some('+') => Style::default().fg(t.diff_add),
            Some('-') => Style::default().fg(t.diff_del),
            Some('@') => Style::default().fg(t.diff_hunk),
            _ => Style::default(),
        };
        return vec![Span::styled(line.to_string(), style)];
    }

    let kw = keywords(lang);
    let comments = comment_prefixes(lang);
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !plain.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut plain)));
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];

        // Comments run to end of line.
        let rest: String = chars[i..].iter().collect();
        if comments.iter().any(|p| rest.starts_with(p)) {
            flush!();
            spans.push(Span::styled(rest, Style::default().fg(t.syn_comment)));
            break;
        }

        // Strings.
        if c == '"' || c == '\'' || c == '`' {
            flush!();
            let mut s = String::from(c);
            let mut j = i + 1;
            let mut escaped = false;
            while j < chars.len() {
                let d = chars[j];
                s.push(d);
                if escaped {
                    escaped = false;
                } else if d == '\\' {
                    escaped = true;
                } else if d == c {
                    break;
                }
                j += 1;
            }
            spans.push(Span::styled(s, Style::default().fg(t.syn_string)));
            i = j + 1;
            continue;
        }

        // Numbers.
        if c.is_ascii_digit() && !prev_is_ident(&chars, i) {
            flush!();
            let mut s = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                s.push(chars[i]);
                i += 1;
            }
            spans.push(Span::styled(s, Style::default().fg(t.syn_number)));
            continue;
        }

        // Identifiers.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            let mut s = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            let next = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let style = if kw.contains(&s.as_str()) {
                Some(Style::default().fg(t.syn_keyword))
            } else if next == Some('(') {
                Some(Style::default().fg(t.syn_func))
            } else if s.chars().next().is_some_and(|c| c.is_uppercase())
                // A leading `key:` reads as a type in yaml/json.
                || (next == Some(':') && start == first_non_ws(&chars))
            {
                Some(Style::default().fg(t.syn_type))
            } else {
                None
            };
            match style {
                Some(st) => {
                    flush!();
                    spans.push(Span::styled(s, st));
                }
                None => plain.push_str(&s),
            }
            continue;
        }

        plain.push(c);
        i += 1;
    }
    flush!();
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

fn first_non_ws(chars: &[char]) -> usize {
    chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the incremental streaming render depends on: splitting at
    /// `stable_prefix_end` and rendering the halves separately must produce
    /// exactly what rendering the whole document produces. If this ever fails,
    /// a streaming reply would render differently from a settled one.
    #[test]
    fn splitting_at_a_stable_point_renders_identically() {
        let t = crate::theme::resolve("default");
        let corpus = [
            "one paragraph only",
            "para one\n\npara two",
            "# Heading\n\nbody text here\n\n- a list item\n- another\n",
            "text\n\n```rust\nfn main() {\n\n    // a blank line inside a fence\n}\n```\n\nafter",
            "before\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter the table",
            "> quoted\n\n---\n\n1. first\n2. second\n\ntail",
            "a\n\n\n\nb",
            "trailing blank line\n\n",
            "```\nunterminated fence\n\nstill inside\n",
            "· unicode — em dash, ünïcödé, 日本語\n\nsecond block 日本語\n",
        ];
        for doc in corpus {
            let whole = render(doc, 60, &t);
            let p = stable_prefix_end(doc);
            if p == 0 {
                continue; // no safe split yet; the full render is the only option
            }
            let mut split = render(&doc[..p - 1], 60, &t);
            split.extend(render(&doc[p..], 60, &t));
            assert_eq!(
                fmt(&whole),
                fmt(&split),
                "split at {p} changed the render of {doc:?}"
            );
        }
    }

    /// A split must never land inside a code fence, because the fence changes how
    /// every following line is rendered.
    #[test]
    fn a_split_never_lands_inside_a_fence() {
        // The only blank line is inside the fence, so there is no safe split.
        assert_eq!(stable_prefix_end("```\ncode\n\nmore code\n"), 0);
        // Once the fence closes, the blank line after it is safe.
        let doc = "```\ncode\n```\n\ntail";
        let p = stable_prefix_end(doc);
        assert!(p > 0 && &doc[p..] == "tail", "p={p} rest={:?}", &doc[p..]);
        // Splitting mid-table is impossible: a table has no blank line in it.
        let doc = "| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(stable_prefix_end(doc), 0);
    }

    /// Streaming appends, so the split point may only ever move forward — that is
    /// what lets the already-rendered prefix be kept.
    #[test]
    fn the_split_point_only_moves_forward_as_text_arrives() {
        let full = "alpha\n\nbeta\n\ngamma\n\ndelta";
        let mut last = 0;
        for end in 1..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let p = stable_prefix_end(&full[..end]);
            assert!(p >= last, "split moved backwards: {last} -> {p} at {end}");
            last = p;
        }
        assert!(last > 0);
    }

    /// Render lines to a comparable string.
    fn fmt(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| format!("{}|{:?}", s.content, s.style))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn th() -> Theme {
        crate::theme::ANSI
    }

    fn text_of(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }

    #[test]
    fn wraps_long_paragraphs() {
        let lines = render("the quick brown fox jumps over the lazy dog", 15, &th());
        assert!(lines.len() > 2);
        for l in text_of(&lines) {
            assert!(l.width() <= 15, "line too wide: {l:?}");
        }
    }

    #[test]
    fn renders_code_fence_with_header() {
        let lines = render("```rust\nfn main() {}\n```", 40, &th());
        let t = text_of(&lines);
        assert!(t[0].contains("rust"));
        assert!(t[1].contains("fn main"));
    }

    #[test]
    fn highlights_keywords_and_strings() {
        let spans = highlight(r#"let x = "hi"; // note"#, "rust", &th());
        let styled: Vec<_> = spans.iter().filter(|s| s.style.fg.is_some()).collect();
        assert!(styled.len() >= 3);
    }

    #[test]
    fn diff_shows_line_numbers_and_signs() {
        let d = "--- a\n+++ a\n@@ -1,2 +1,2 @@\n def f():\n-    return 1\n+    return 2\n";
        let lines = render_diff(d, 60, &th());
        let text: Vec<String> = text_of(&lines);
        // File headers are dropped; the tool card already names the file.
        assert!(!text.iter().any(|l| l.contains("--- a")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("@@")), "{text:?}");
        let minus = text.iter().find(|l| l.contains("-    return 1")).unwrap();
        let plus = text.iter().find(|l| l.contains("+    return 2")).unwrap();
        assert!(
            minus.trim_start().starts_with('2'),
            "old line number: {minus:?}"
        );
        assert!(
            plus.trim_start().starts_with('2'),
            "new line number: {plus:?}"
        );
    }

    #[test]
    fn parses_hunk_headers() {
        assert_eq!(parse_hunk("@@ -12,4 +30,5 @@"), Some((12, 30)));
        assert_eq!(parse_hunk("not a hunk"), None);
    }

    #[test]
    fn renders_a_table_with_aligned_columns() {
        let md = "| tool | time |\n|---|---|\n| read | 8ms |\n| edit | 120ms |";
        let lines = render(md, 60, &th());
        let text = text_of(&lines);
        assert!(
            text[0].contains("tool") && text[0].contains("time"),
            "{text:?}"
        );
        assert!(text[1].contains('─'), "expected a header rule: {text:?}");
        // Columns line up: `time` and `8ms` start at the same offset.
        let h = text[0].find("time").unwrap();
        let r = text[2].find("8ms").unwrap();
        assert_eq!(h, r, "columns misaligned: {text:?}");
    }

    #[test]
    fn table_cells_render_inline_formatting() {
        // A cell with `code` and **bold** should show the text, not the raw
        // markdown markers, and columns should still align.
        let md = "| name | note |\n|---|---|\n| `learning.rs` | **new** module |\n| tui.rs | old |";
        let lines = render(md, 70, &th());
        let text = text_of(&lines);
        let joined = text.join("\n");
        assert!(
            joined.contains("learning.rs"),
            "code cell text shown: {text:?}"
        );
        assert!(
            !joined.contains("`learning.rs`"),
            "backticks stripped: {text:?}"
        );
        assert!(joined.contains("new"), "bold cell text shown: {text:?}");
        assert!(!joined.contains("**new**"), "asterisks stripped: {text:?}");
        assert!(
            text.iter().any(|l| l.contains('│')),
            "column separators present: {text:?}"
        );
    }

    #[test]
    fn renders_task_lists_as_checkboxes() {
        let text = text_of(&render("- [x] done\n- [ ] todo", 40, &th()));
        assert!(text[0].contains('☑'), "{text:?}");
        assert!(text[1].contains('☐'), "{text:?}");
    }

    #[test]
    fn links_show_the_label_not_the_url() {
        let text = text_of(&render(
            "see [the docs](https://example.com/x) now",
            60,
            &th(),
        ));
        let joined = text.join(" ");
        assert!(joined.contains("the docs"), "{joined}");
        assert!(!joined.contains("example.com"), "{joined}");
    }

    #[test]
    fn diff_colours_added_lines() {
        let lines = render_diff("--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new", 40, &th());
        let sign_colour = |l: &Line| l.spans.get(1).and_then(|s| s.style.fg);
        assert_eq!(sign_colour(&lines[1]), Some(th().diff_del));
        assert_eq!(sign_colour(&lines[2]), Some(th().diff_add));
    }
}
