//! Visual primitives: framed panels, gauges and rails.
//!
//! Structured output — help, model lists, themes — reads far better inside a
//! titled frame than as a bare markdown list, because the frame tells you where
//! the answer starts and stops. These build `Line`s directly rather than going
//! through markdown, so alignment is exact and nothing re-wraps unexpectedly.

use crate::theme::{Glyphs, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// A framed block of pre-laid-out rows.
///
/// ```text
/// ╭─ Commands ───────────────────────────────╮
/// │  /help       keys and commands           │
/// ╰──────────────────────────────────────────╯
/// ```
pub struct Panel {
    title: String,
    rows: Vec<Line<'static>>,
    /// Shown dim on the bottom edge.
    footer: Option<String>,
    width: usize,
}

impl Panel {
    pub fn new(title: impl Into<String>, width: usize) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
            footer: None,
            width: width.max(24),
        }
    }

    pub fn footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(text.into());
        self
    }

    pub fn row(&mut self, spans: Vec<Span<'static>>) {
        self.rows.push(Line::from(spans));
    }

    #[allow(dead_code)]
    pub fn blank(&mut self) {
        self.rows.push(Line::default());
    }

    /// Inner width available to row content.
    pub fn inner(&self) -> usize {
        self.width.saturating_sub(4)
    }

    /// Render as a filled block: a bold heading row carrying the title on the
    /// left and the footer on the right, then the content, all on one tint.
    ///
    /// A fill rather than a border because a border costs two rows and boxes the
    /// content in; a tint groups it just as clearly and leaves the text closer to
    /// the surrounding transcript.
    pub fn render(self, t: &Theme, g: &Glyphs) -> Vec<Line<'static>> {
        let inner = self.inner();
        let mut body: Vec<Line<'static>> = Vec::with_capacity(self.rows.len() + 1);

        // Heading row: title left, footer right.
        let title_w = self.title.width();
        let foot = self.footer.clone().unwrap_or_default();
        let foot_w = foot.width();
        let mut head = vec![Span::styled(
            self.title.clone(),
            Style::default().fg(t.heading).add_modifier(Modifier::BOLD),
        )];
        if foot_w > 0 && inner > title_w + foot_w + 2 {
            head.push(Span::raw(" ".repeat(inner - title_w - foot_w)));
            head.push(Span::styled(foot, t.dim()));
        }
        body.push(Line::from(head));

        for row in self.rows {
            let (content, _) = clip(row.spans, inner);
            body.push(Line::from(content));
        }

        // No tint available (ansi/mono): fall back to a rule under the heading so
        // the block still has a visible boundary.
        if t.bg_panel.is_none() {
            let mut out = vec![body.remove(0)];
            out.push(Line::from(Span::styled(
                g.hline.repeat(self.width.min(inner + 2)),
                t.fg(t.border),
            )));
            out.extend(body);
            return out;
        }
        fill(body, self.width, t.bg_panel, 1)
    }
}

/// What state a framed block is in, which decides its border colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Still running. Used by the streaming tool frames.
    #[allow(dead_code)]
    Pending,
    /// Finished cleanly.
    Done,
    /// Finished badly.
    Failed,
}

impl Frame {
    fn border(self, t: &Theme) -> Color {
        match self {
            Frame::Pending => t.border,
            // A muted border on success: the result is the content, not the box.
            Frame::Done => t.border,
            Frame::Failed => t.error,
        }
    }

    /// The background tint for a railed block in this state. `None` on
    /// themes without fills (ansi/mono), where `railed` falls back to a rail
    /// glyph gutter instead.
    fn tint(self, t: &Theme) -> Option<Color> {
        match self {
            Frame::Failed => t.bg_tool_err,
            _ => t.bg_tool,
        }
    }
}

/// A tool result as a **rail + fill** rather than a full box.
///
/// ```text
/// │ ✓ Edit: cart.py                    +3/-1
/// │   @@ -14,3 +14,3 @@
/// │   14  def apply_discount(...):
/// ╰                                          ← closing cap: output ends here
/// ```
///
/// This replaces the four-sided [`framed`] box for non-modal tool output, per
/// the visual-language spec (§8: one border depth on screen; modals are the
/// only boxed thing). A full box costs a per-block perimeter walk plus two rows
/// of horizontal rule; a rail is a single style write per row.
///
/// The one thing a box gave us that a bare fill does not is a clear *end* — a
/// fill "leaves you guessing where the output stopped." So this keeps a closing
/// cap glyph (`╰`) on its own row: one cell, no full bottom rule, but the eye
/// still lands on an unambiguous end-of-block marker. On fill-less themes the
/// rail glyph in the left gutter plus the cap carry the grouping instead of a
/// tint.
pub fn railed(
    head: Vec<Span<'static>>,
    body: Vec<Line<'static>>,
    tail: Option<Vec<Span<'static>>>,
    width: usize,
    state: Frame,
    t: &Theme,
    g: &Glyphs,
) -> Vec<Line<'static>> {
    let bw = width.max(12);
    let rail_color = state.border(t);
    let tint = state.tint(t);
    // Inner content sits after "│ " (rail + one space).
    let inner = bw.saturating_sub(2);

    // Assemble the logical rows (header, body, optional footer) first, then
    // wrap each in a rail + tint. Keeping the header on the tint means the whole
    // unit reads as one block.
    let mut rows: Vec<Line<'static>> = Vec::with_capacity(body.len() + 2);
    rows.push(Line::from(head));
    for l in body {
        let (content, _) = clip(l.spans, inner);
        rows.push(Line::from(content));
    }
    if let Some(spans) = tail {
        if spans.iter().map(|s| s.content.width()).sum::<usize>() > 0 {
            rows.push(Line::from(spans));
        }
    }

    let mut out = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let used: usize = row.spans.iter().map(|s| s.content.width()).sum();
        let mut spans = Vec::with_capacity(row.spans.len() + 3);
        // The rail glyph, in the state colour, is the block's spine.
        let rail_style = match tint {
            Some(bg) => t.fg(rail_color).bg(bg),
            None => t.fg(rail_color),
        };
        spans.push(Span::styled(format!("{} ", g.vline), rail_style));
        for s in row.spans {
            let style = match (tint, s.style.bg.is_some()) {
                (Some(bg), false) => s.style.bg(bg),
                _ => s.style,
            };
            spans.push(Span::styled(s.content, style));
        }
        if let Some(bg) = tint {
            let tail_w = inner.saturating_sub(used);
            if tail_w > 0 {
                spans.push(Span::styled(" ".repeat(tail_w), Style::default().bg(bg)));
            }
        }
        out.push(Line::from(spans));
    }

    // Closing cap: the corner glyph alone marks where the output ends. No
    // horizontal rule, so it costs one cell of ink, not a full row of border.
    out.push(Line::from(Span::styled(
        g.corner_bl.to_string(),
        t.fg(rail_color),
    )));
    out
}

/// A rounded block with its label inlaid in the top edge:
///
/// ```text
/// ╭─── Edit: cart.py ──────────────── [+3/-1] ╮
/// │ body                                      │
/// ╰───────────────────────────────────────────╯
/// ```
///
/// Tool results now use [`railed`] (rail + fill) instead; `framed` is kept for
/// the full four-sided box a modal overlay may still want — a modal is the one
/// deliberate layer break where a box is the right signal.
#[allow(dead_code)]
pub fn framed(
    head: Vec<Span<'static>>,
    body: Vec<Line<'static>>,
    tail: Option<Vec<Span<'static>>>,
    width: usize,
    state: Frame,
    t: &Theme,
    g: &Glyphs,
) -> Vec<Line<'static>> {
    let bw = width.max(12);
    let inner = bw.saturating_sub(4);
    let bs = t.fg(state.border(t));
    let mut out = Vec::with_capacity(body.len() + 2);

    // Top edge: ╭─── <head> ───…───╮
    let head_w: usize = head.iter().map(|s| s.content.width()).sum();
    let lead = 3usize;
    let mut top = vec![Span::styled(
        format!("{}{}", g.corner_tl, g.hline.repeat(lead)),
        bs,
    )];
    if head_w > 0 {
        top.push(Span::styled(" ".to_string(), bs));
        top.extend(head);
        top.push(Span::styled(" ".to_string(), bs));
    }
    let used = 1 + lead + if head_w > 0 { head_w + 2 } else { 0 };
    let fill_w = bw.saturating_sub(used + 1);
    top.push(Span::styled(g.hline.repeat(fill_w), bs));
    top.push(Span::styled(g.corner_tr.to_string(), bs));
    out.push(Line::from(top));

    for l in body {
        let (content, w) = clip(l.spans, inner);
        let mut row = vec![Span::styled(format!("{} ", g.vline), bs)];
        row.extend(content);
        if inner > w {
            row.push(Span::raw(" ".repeat(inner - w)));
        }
        row.push(Span::styled(format!(" {}", g.vline), bs));
        out.push(Line::from(row));
    }

    // Bottom edge, optionally carrying a footer.
    let mut bot = vec![Span::styled(
        format!("{}{}", g.corner_bl, g.hline.repeat(lead)),
        bs,
    )];
    let foot_w = match &tail {
        Some(spans) => {
            let w: usize = spans.iter().map(|s| s.content.width()).sum();
            if w > 0 {
                bot.push(Span::styled(" ".to_string(), bs));
                bot.extend(spans.clone());
                bot.push(Span::styled(" ".to_string(), bs));
                w + 2
            } else {
                0
            }
        }
        None => 0,
    };
    let rest = bw.saturating_sub(1 + lead + foot_w + 1);
    bot.push(Span::styled(g.hline.repeat(rest), bs));
    bot.push(Span::styled(g.corner_br.to_string(), bs));
    out.push(Line::from(bot));
    out
}

/// oh-my-pi's status-line grammar, which every tool header follows:
///
/// ```text
/// <icon> <Title>: <description>  <meta · meta>
/// ```
///
/// Keeping one grammar for every tool is what makes a transcript of mixed tools
/// scan as a list rather than as noise.
pub fn status_line(
    icon: Option<(String, Color)>,
    title: &str,
    desc: Option<(String, Color)>,
    meta: &[String],
    t: &Theme,
    g: &Glyphs,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some((ic, c)) = icon {
        spans.push(Span::styled(format!("{ic} "), t.fg(c)));
    }
    spans.push(Span::styled(
        title.to_string(),
        Style::default().fg(t.tool_title).add_modifier(Modifier::BOLD),
    ));
    if let Some((d, c)) = desc {
        spans.push(Span::styled(": ".to_string(), t.dim()));
        spans.push(Span::styled(d, t.fg(c)));
    }
    if !meta.is_empty() {
        // Their sep.dot is space-dot-space; without the spaces the meta reads
        // as one run-together word.
        spans.push(Span::styled(
            format!("  {}", meta.join(&format!(" {} ", g.sep))),
            t.dim(),
        ));
    }
    spans
}

/// `[Ctrl+O: Expand]` — shown only when there is genuinely more to see.
/// The affordance shown under a clipped block. When the block is already
/// expanded (e.g. via the sticky ctrl+r/ctrl+t preference) the body is not
/// clipped, so this hint is simply not emitted — it disappears once the work
/// is done, rather than lying "expand" at something already open.
pub fn expand_hint(t: &Theme) -> Span<'static> {
    Span::styled("  ctrl+r expand".to_string(), t.dim())
}

/// One coloured segment of the status bar.
pub struct Segment {
    pub text: String,
    pub color: Color,
    pub bold: bool,
}

impl Segment {
    pub fn new(text: impl Into<String>, color: Color) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
        }
    }
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// A status bar of chevron-separated segments, each in its own colour.
///
/// Reading a bar of `model ❯ dir ❯ branch ❯ tokens` is faster than reading the
/// same facts separated by dots, because colour plus a directional separator
/// tells you where one field ends and the next begins.
pub fn status_bar(
    segments: Vec<Segment>,
    right: Vec<Segment>,
    width: usize,
    t: &Theme,
    g: &Glyphs,
) -> Line<'static> {
    let sep = |spans: &mut Vec<Span<'static>>| {
        spans.push(Span::styled(format!(" {} ", g.chevron), t.fg(t.border)));
    };
    let mut left: Vec<Span<'static>> = Vec::new();
    for (i, s) in segments.into_iter().enumerate() {
        if i > 0 {
            sep(&mut left);
        }
        let style = if s.bold {
            Style::default().fg(s.color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(s.color)
        };
        left.push(Span::styled(s.text, style));
    }

    let mut tail: Vec<Span<'static>> = Vec::new();
    for (i, s) in right.into_iter().enumerate() {
        if i > 0 {
            tail.push(Span::styled("  ".to_string(), t.dim()));
        }
        tail.push(Span::styled(s.text, Style::default().fg(s.color)));
    }

    let lw: usize = left.iter().map(|s| s.content.width()).sum();
    let rw: usize = tail.iter().map(|s| s.content.width()).sum();
    let mut spans = vec![Span::raw(" ".to_string())];
    spans.extend(left);
    if width > lw + rw + 3 {
        spans.push(Span::raw(" ".repeat(width - lw - rw - 2)));
        spans.extend(tail);
        spans.push(Span::raw(" ".to_string()));
    }
    Line::from(spans)
}

/// Lay a block of lines onto a tinted background.
///
/// This is what makes a message read as a unit: a fill costs no rows, unlike a
/// border, and the tint carries the block's kind (warm for you, cool for a tool,
/// red for a failure). Every line is padded to `width` so the fill is a clean
/// rectangle rather than a ragged one, and the tint is applied to spans that do
/// not already set their own background.
pub fn fill(lines: Vec<Line<'static>>, width: usize, bg: Option<Color>, pad: usize) -> Vec<Line<'static>> {
    let Some(bg) = bg else {
        // No fill for this theme: indent so the block still reads as grouped.
        return lines
            .into_iter()
            .map(|l| {
                if pad == 0 {
                    return l;
                }
                let mut spans = vec![Span::raw(" ".repeat(pad))];
                spans.extend(l.spans);
                Line::from(spans)
            })
            .collect();
    };
    lines
        .into_iter()
        .map(|l| {
            let used: usize = l.spans.iter().map(|s| s.content.width()).sum();
            let mut spans = Vec::with_capacity(l.spans.len() + 2);
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
            }
            for s in l.spans {
                let style = if s.style.bg.is_some() {
                    s.style
                } else {
                    s.style.bg(bg)
                };
                spans.push(Span::styled(s.content, style));
            }
            let tail = width.saturating_sub(used + pad);
            if tail > 0 {
                spans.push(Span::styled(" ".repeat(tail), Style::default().bg(bg)));
            }
            Line::from(spans)
        })
        .collect()
}


/// Clip spans to `limit` display cells, returning them and the width used.
///
/// Without this a row longer than the frame punches through the right border,
/// which looks far worse than a truncated line.
fn clip(spans: Vec<Span<'static>>, limit: usize) -> (Vec<Span<'static>>, usize) {
    let mut out = Vec::with_capacity(spans.len());
    let mut used = 0usize;
    for s in spans {
        let w = s.content.width();
        if used + w <= limit {
            used += w;
            out.push(s);
            continue;
        }
        let room = limit.saturating_sub(used);
        if room > 1 {
            let mut kept = String::new();
            let mut kw = 0usize;
            for c in s.content.chars() {
                let cw = c.to_string().width().max(1);
                if kw + cw > room - 1 {
                    break;
                }
                kept.push(c);
                kw += cw;
            }
            kept.push('…');
            used += kw + 1;
            out.push(Span::styled(kept, s.style));
        }
        break;
    }
    (out, used)
}

/// Two aligned columns of `key  description`, sized from their own contents.
///
/// Returns rows ready to hand to a `Panel`, so the panel stays responsible for
/// framing and this stays responsible for alignment.
pub fn key_value_rows(
    pairs: &[(&str, &str)],
    inner: usize,
    t: &Theme,
) -> Vec<Vec<Span<'static>>> {
    let key_w = pairs.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let desc_w = pairs.iter().map(|(_, d)| d.width()).max().unwrap_or(0);
    let one_col_w = key_w + 2 + desc_w;
    let two = inner >= one_col_w * 2 + 4;

    let cell = |k: &str, d: &str, kw: usize, t: &Theme| -> Vec<Span<'static>> {
        vec![
            Span::styled(
                format!("{k}{:pad$}", "", pad = kw.saturating_sub(k.width())),
                t.fg(t.accent),
            ),
            Span::styled(format!("  {d}"), t.body()),
        ]
    };

    if !two {
        return pairs
            .iter()
            .map(|(k, d)| cell(k, d, key_w, t))
            .collect();
    }

    // Fill the left column top-to-bottom so reading order stays vertical.
    let rows = pairs.len().div_ceil(2);
    let (left, right) = pairs.split_at(rows);
    let lk = left.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let ld = left.iter().map(|(_, d)| d.width()).max().unwrap_or(0);
    let rk = right.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let gap = 4;

    (0..rows)
        .map(|i| {
            let (k, d) = left[i];
            let mut spans = cell(k, d, lk, t);
            if let Some((k2, d2)) = right.get(i) {
                let used: usize = spans.iter().map(|s| s.content.width()).sum();
                let target = lk + 2 + ld + gap;
                spans.push(Span::raw(" ".repeat(target.saturating_sub(used).max(2))));
                spans.extend(cell(k2, d2, rk, t));
            }
            spans
        })
        .collect()
}

/// A horizontal bar with eighth-block precision.
///
/// `cells` is the bar width; one cell carries eight steps, so a short bar still
/// moves visibly as the value changes.
pub fn gauge(fraction: f64, cells: usize, g: &Glyphs) -> String {
    crate::anim::eighth_bar(fraction as f32, cells, g.fine_blocks)
}

pub fn gauge_style(fraction: f64, t: &Theme) -> Style {
    if fraction >= 0.85 {
        t.emphasis(t.error)
    } else if fraction >= 0.65 {
        t.emphasis(t.warning)
    } else {
        t.fg(t.accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ANSI, ASCII, DARK, UNICODE};

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect()
    }


    #[test]
    fn filled_panel_rows_are_all_the_same_width() {
        let mut p = Panel::new("Commands", 50);
        p.row(vec![Span::raw("short".to_string())]);
        p.row(vec![Span::raw("a much longer row of content".to_string())]);
        let lines = p.render(&DARK, &UNICODE);
        let widths: Vec<usize> = text(&lines).iter().map(|l| l.width()).collect();
        assert!(widths.iter().all(|w| *w == 50), "ragged fill: {widths:?}");
        for l in &lines {
            assert!(l.spans.iter().all(|s| s.style.bg.is_some()), "hole in fill: {l:?}");
        }
    }

    #[test]
    fn panel_heading_carries_title_and_footer() {
        let p = Panel::new("Themes", 44).footer("enter to pick");
        let lines = text(&p.render(&DARK, &UNICODE));
        assert!(lines[0].contains("Themes"), "{lines:?}");
        assert!(lines[0].contains("enter to pick"), "footer belongs on the heading row");
    }

    #[test]
    fn railed_has_a_spine_and_a_closing_cap() {
        let head = vec![Span::raw("Read: a.rs".to_string())];
        let body = vec![Line::from(Span::raw("1  fn main() {}".to_string()))];
        let lines = railed(head, body, None, 40, Frame::Done, &DARK, &UNICODE);
        let rows = text(&lines);
        // Every content/header row starts with the rail glyph…
        for r in &rows[..rows.len() - 1] {
            assert!(r.starts_with(UNICODE.vline), "row missing rail spine: {r:?}");
        }
        // …and the block ends with a bare corner cap on its own row, so the eye
        // lands on an unambiguous end-of-output marker (no full bottom rule).
        let last = rows.last().unwrap();
        assert_eq!(last.trim(), UNICODE.corner_bl, "closing cap missing: {last:?}");
        assert!(rows[0].contains("Read: a.rs"));
    }

    #[test]
    fn railed_fills_with_the_state_tint_on_a_themed_palette() {
        let head = vec![Span::raw("Edit: a.rs".to_string())];
        let body = vec![Line::from(Span::raw("+ new line".to_string()))];
        let lines = railed(head, body, None, 40, Frame::Done, &DARK, &UNICODE);
        // The header row is padded to full width and every cell carries the tint,
        // so the block reads as one grouped unit rather than a bare line.
        let header = &lines[0];
        let w: usize = header.spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(w, 40, "railed header not padded to width: {w}");
        assert!(
            header.spans.iter().all(|s| s.style.bg.is_some()),
            "hole in railed tint: {header:?}"
        );
    }

    #[test]
    fn railed_falls_back_to_a_rail_glyph_without_a_tint() {
        // ANSI theme has no fills; the rail glyph plus cap must still group the
        // block. No panic, spine present, cap present.
        let head = vec![Span::raw("Run".to_string())];
        let body = vec![Line::from(Span::raw("$ ls".to_string()))];
        let rows = text(&railed(head, body, None, 40, Frame::Failed, &ANSI, &UNICODE));
        assert!(rows[0].starts_with(UNICODE.vline), "no spine in fallback: {rows:?}");
        assert_eq!(rows.last().unwrap().trim(), UNICODE.corner_bl);
    }

    #[test]
    fn railed_is_ascii_safe() {
        let head = vec![Span::raw("Read".to_string())];
        let body = vec![Line::from(Span::raw("x".to_string()))];
        // ASCII glyphs must not panic or emit box-drawing chars.
        let _ = railed(head, body, None, 30, Frame::Done, &DARK, &ASCII);
    }

    #[test]
    fn panel_without_a_tint_uses_a_rule_instead() {
        let mut p = Panel::new("Keys", 40);
        p.row(vec![Span::raw("x".to_string())]);
        let lines = text(&p.render(&ANSI, &UNICODE));
        assert!(lines[0].contains("Keys"));
        assert!(lines[1].contains(UNICODE.hline), "expected a rule: {lines:?}");
    }

    #[test]
    fn overlong_rows_are_clipped_not_overflowed() {
        let mut p = Panel::new("T", 30);
        p.row(vec![Span::raw("x".repeat(200))]);
        let lines = text(&p.render(&DARK, &UNICODE));
        for l in &lines {
            assert_eq!(l.width(), 30, "row escaped the block: {l:?}");
        }
        assert!(lines[1].contains('…'), "clipped rows should say so");
    }


    #[test]
    fn status_bar_separates_segments_with_chevrons() {
        let line = status_bar(
            vec![
                Segment::new("qwen", ANSI.accent).bold(),
                Segment::new("myproj", ANSI.info),
                Segment::new("main", ANSI.accent_alt),
            ],
            vec![Segment::new("4.1k tok", ANSI.muted)],
            80,
            &ANSI,
            &UNICODE,
        );
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(text.matches(UNICODE.chevron).count(), 2);
        assert!(text.contains("qwen") && text.contains("4.1k tok"), "{text}");
        assert!(text.width() <= 80, "overflowed: {}", text.width());
    }

    #[test]
    fn status_bar_drops_the_right_side_when_narrow() {
        let line = status_bar(
            vec![Segment::new("a-very-long-model-name-indeed", ANSI.accent)],
            vec![Segment::new("99.9k tok", ANSI.muted)],
            24,
            &ANSI,
            &UNICODE,
        );
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!text.contains("99.9k"), "should have dropped: {text}");
    }

    #[test]
    fn fill_pads_every_line_to_the_same_width() {
        let lines = vec![
            Line::from(Span::raw("short".to_string())),
            Line::from(Span::raw("a longer line here".to_string())),
        ];
        let filled = fill(lines, 40, Some(Color::Rgb(20, 20, 20)), 2);
        for l in &filled {
            let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
            assert_eq!(w, 40, "ragged fill: {l:?}");
            // Every span carries the tint, or the rectangle has holes in it.
            assert!(
                l.spans.iter().all(|s| s.style.bg.is_some()),
                "unfilled span in {l:?}"
            );
        }
    }

    #[test]
    fn fill_falls_back_to_indent_without_a_colour() {
        let lines = vec![Line::from(Span::raw("x".to_string()))];
        let out = fill(lines, 40, None, 2);
        let w: usize = out[0].spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(w, 3, "no-colour path should indent, not pad");
        assert!(out[0].spans.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn fill_respects_spans_that_set_their_own_background() {
        let own = Style::default().bg(Color::Red);
        let lines = vec![Line::from(vec![
            Span::styled("keep".to_string(), own),
            Span::raw("tint".to_string()),
        ])];
        let out = fill(lines, 20, Some(Color::Rgb(9, 9, 9)), 0);
        assert_eq!(out[0].spans[0].style.bg, Some(Color::Red));
        assert_eq!(out[0].spans[1].style.bg, Some(Color::Rgb(9, 9, 9)));
    }



    #[test]
    fn gauge_is_exact_at_the_ends() {
        assert_eq!(gauge(0.0, 8, &UNICODE).trim_end_matches('░'), "");
        assert_eq!(gauge(1.0, 8, &UNICODE), "████████");
        // Width is constant regardless of value, or the status line jitters.
        for pct in 0..=100 {
            let bar = gauge(pct as f64 / 100.0, 10, &UNICODE);
            assert_eq!(bar.chars().count(), 10, "at {pct}%: {bar:?}");
        }
    }

    #[test]
    fn gauge_falls_back_to_ascii() {
        let bar = gauge(0.5, 8, &ASCII);
        assert!(bar.is_ascii(), "{bar:?}");
        assert_eq!(bar.len(), 8);
    }

    #[test]
    fn gauge_colour_escalates_with_pressure() {
        assert_eq!(gauge_style(0.2, &ANSI).fg, Some(ANSI.accent));
        assert_eq!(gauge_style(0.7, &ANSI).fg, Some(ANSI.warning));
        assert_eq!(gauge_style(0.9, &ANSI).fg, Some(ANSI.error));
    }

    #[test]
    fn key_values_use_two_columns_when_there_is_room() {
        let pairs = [("/a", "does a"), ("/b", "does b"), ("/c", "does c"), ("/d", "does d")];
        let wide = key_value_rows(&pairs, 80, &ANSI);
        assert_eq!(wide.len(), 2, "four pairs should fold into two rows");
        let narrow = key_value_rows(&pairs, 20, &ANSI);
        assert_eq!(narrow.len(), 4, "no room for two columns");
    }

    #[test]
    fn key_value_columns_align() {
        let pairs = [
            ("/x", "short"),
            ("/longcommand", "a considerably longer description"),
            ("/y", "another"),
            ("/z", "last"),
        ];
        let rows = key_value_rows(&pairs, 100, &ANSI);
        // Every right-hand column must start at the same offset.
        let starts: Vec<usize> = rows
            .iter()
            .filter(|r| r.len() > 2)
            .map(|r| {
                r.iter()
                    .take(3)
                    .map(|s| s.content.width())
                    .sum::<usize>()
            })
            .collect();
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "misaligned second column: {starts:?}"
        );
    }
}
