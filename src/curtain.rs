//! The curtain call: a short title sequence when someone asks who made koda.
//!
//! Event-tied, not ambient — it plays because a person asked, once, and clears
//! itself. That is the whole reason it is allowed to exist: a surprise you asked
//! for is delight, and the same animation arriving unbidden while you read a
//! diff is an interruption. Any key dismisses it, and the key still does what
//! it was pressed for.
//!
//! Painted cell by cell into ratatui's buffer — never as raw escape sequences,
//! which would fight ratatui's own record of what is on screen — as an
//! overlay, so the transcript underneath is never touched and nothing moves.
//!
//! The beats, over 4.5 s:
//!
//! | window        | what happens                                              |
//! | ------------- | --------------------------------------------------------- |
//! | 0 – 500 ms    | the frame traces itself in, clockwise from the top left   |
//! | 300 – 1100 ms | the wordmark assembles left to right behind a bright edge |
//! | 1100 – 1800   | one highlight sweeps across it                            |
//! | 1000 – 1750   | the creator's name, then the contact, fade in             |
//! | 500 – 3900    | a light runs round the frame; a few sparks twinkle        |
//! | 3900 – 4500   | everything holds still, so it can be read before it goes  |
//!
//! Without motion the finished card is shown, still, for the same time.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::anim;
use crate::panel;
use crate::theme::{self, Glyphs, Theme};
use crate::tools::{CREATOR_CONTACT, CREATOR_NAME};

/// How long the card stays up.
pub const DURATION: Duration = Duration::from_millis(4500);

const TRACE: (u64, u64) = (0, 500);
const REVEAL: (u64, u64) = (300, 1100);
const SWEEP: (u64, u64) = (1100, 1800);
const NAME_IN: (u64, u64) = (1000, 1500);
const CONTACT_IN: (u64, u64) = (1250, 1750);
/// After this, nothing moves: the last stretch is for reading.
const SETTLE: u64 = 3900;
/// Where a still card is drawn from: everything arrived, nothing mid-flight.
const STILL: u64 = 2000;

const WHITE: Color = Color::Rgb(255, 255, 255);

/// Progress through a window, 0 before it and 1 after.
fn window(ms: u64, (a, b): (u64, u64)) -> f32 {
    if ms <= a {
        0.0
    } else if ms >= b {
        1.0
    } else {
        (ms - a) as f32 / (b - a) as f32
    }
}

/// What the card will look like at this size: the full wordmark, a compact
/// title, or nothing at all when there is no room to be charming without
/// being in the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Full,
    Compact,
    None,
}

fn lines_below() -> (String, String) {
    (
        format!("made by  {CREATOR_NAME}"),
        CREATOR_CONTACT.to_string(),
    )
}

/// The card's rectangle and layout for a screen of `area` with this `art`.
pub fn place(area: Rect, art: &[&str]) -> (Layout, Rect) {
    let (name, contact) = lines_below();
    let text_w = name.width().max(contact.width());
    let art_w = art.iter().map(|r| r.width()).max().unwrap_or(0);

    // Full: margin, wordmark, gap, two lines, margin — framed.
    let w = (art_w.max(text_w) + 8) as u16 + 2;
    let h = art.len() as u16 + 5 + 2;
    if area.width >= w + 2 && area.height >= h + 2 {
        return (Layout::Full, centred(area, w, h));
    }
    // Compact: a one-line title instead of the wordmark.
    let w = (text_w + 6) as u16 + 2;
    let h = 6 + 2;
    if area.width >= w + 2 && area.height >= h + 2 {
        return (Layout::Compact, centred(area, w, h));
    }
    (Layout::None, Rect::default())
}

fn centred(area: Rect, w: u16, h: u16) -> Rect {
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, sym: &str, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(sym);
        cell.set_style(style);
    }
}

fn put_str(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    buf.set_string(x, y, s, style);
}

/// Draw the card at `elapsed` into `buf`. Returns false when the screen is
/// too small for any version of it.
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    g: &Glyphs,
    art: &[&str],
    elapsed: Duration,
    animate: bool,
) -> bool {
    let (layout, rect) = place(area, art);
    if layout == Layout::None {
        return false;
    }
    let ms = if animate {
        elapsed.as_millis() as u64
    } else {
        STILL
    };
    let moving = animate && ms < SETTLE;

    // Blank the card's cells and lay its fill, in one pass.
    let fill = match t.bg_panel {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    };
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(fill);
            }
        }
    }

    draw_frame(buf, rect, t, g, ms, moving);
    let inner_w = rect.width - 2;
    let x0 = rect.x + 1;
    let mut y = rect.y + 2;
    match layout {
        Layout::Full => {
            draw_wordmark(buf, rect, art, y, t, ms);
            y += art.len() as u16 + 1;
            if moving {
                draw_sparks(buf, rect, art.len() as u16, t, g, ms);
            }
        }
        Layout::Compact => {
            draw_title(buf, x0, y, inner_w, t, ms);
            y += 2;
        }
        Layout::None => unreachable!(),
    }

    // The two facts, fading in one after the other, then simply there.
    let (name, contact) = lines_below();
    let fade = |from: Color, to: Color, k: f32| theme::mix(from, to, anim::ease_out_cubic(k));
    let base = t.bg_panel.unwrap_or(t.muted);
    let k = window(ms, NAME_IN);
    if k > 0.0 {
        let x = x0 + (inner_w.saturating_sub(name.width() as u16)) / 2;
        let lead = "made by  ";
        put_str(buf, x, y, lead, fill.fg(fade(base, t.muted, k)));
        put_str(
            buf,
            x + lead.width() as u16,
            y,
            CREATOR_NAME,
            fill.fg(fade(base, t.accent_alt, k))
                .add_modifier(Modifier::BOLD),
        );
    }
    let k = window(ms, CONTACT_IN);
    if k > 0.0 {
        let x = x0 + (inner_w.saturating_sub(contact.width() as u16)) / 2;
        put_str(buf, x, y + 1, &contact, fill.fg(fade(base, t.info, k)));
    }
    true
}

/// The frame: traced in clockwise, then a soft light runs round it.
fn draw_frame(buf: &mut Buffer, r: Rect, t: &Theme, g: &Glyphs, ms: u64, moving: bool) {
    let set = panel::frame_set(ratatui::widgets::BorderType::Rounded, g);
    let (l, top, rt, bot) = (r.x, r.y, r.x + r.width - 1, r.y + r.height - 1);
    // The perimeter, clockwise from the top-left corner.
    let mut cells: Vec<(u16, u16, &str)> = Vec::new();
    cells.push((l, top, set.top_left));
    for x in l + 1..rt {
        cells.push((x, top, set.horizontal_top));
    }
    cells.push((rt, top, set.top_right));
    for y in top + 1..bot {
        cells.push((rt, y, set.vertical_right));
    }
    cells.push((rt, bot, set.bottom_right));
    for x in (l + 1..rt).rev() {
        cells.push((x, bot, set.horizontal_bottom));
    }
    cells.push((l, bot, set.bottom_left));
    for y in (top + 1..bot).rev() {
        cells.push((l, y, set.vertical_left));
    }

    let p = cells.len();
    let shown = if moving {
        (anim::ease_out_cubic(window(ms, TRACE)) * p as f32).ceil() as usize
    } else {
        p
    };
    let base = theme::mix(t.border, t.accent, 0.55);
    let lit = theme::mix(t.accent, WHITE, 0.45);
    // The running light: one lap every 1.6 s, after the trace.
    let head = (ms.saturating_sub(TRACE.1) as f32 / 1600.0).fract() * p as f32;
    let fill = t
        .bg_panel
        .map(|bg| Style::default().bg(bg))
        .unwrap_or_default();
    for (i, (x, y, sym)) in cells.into_iter().enumerate().take(shown) {
        let colour = if moving && ms >= TRACE.1 {
            let d = (i as f32 - head).abs();
            let d = d.min(p as f32 - d);
            theme::mix(base, lit, (1.0 - d / 10.0).max(0.0))
        } else if moving && i + 6 >= shown {
            // The tracing tip is bright.
            lit
        } else {
            base
        };
        put(buf, x, y, sym, fill.fg(colour));
    }
}

/// The wordmark: assembled left to right behind a bright edge, then swept once.
fn draw_wordmark(buf: &mut Buffer, r: Rect, art: &[&str], y0: u16, t: &Theme, ms: u64) {
    let cols = art.iter().map(|row| row.width()).max().unwrap_or(1).max(1);
    let x0 = r.x + (r.width - cols as u16) / 2;
    let reveal = anim::ease_out_cubic(window(ms, REVEAL)) * (cols as f32 + 4.0);
    let sweep = window(ms, SWEEP);
    let band = anim::ease_in_out_sine(sweep) * (cols as f32 + 10.0) - 5.0;
    let fill = t
        .bg_panel
        .map(|bg| Style::default().bg(bg))
        .unwrap_or_default();
    for (row, line) in art.iter().enumerate() {
        for (c, ch) in line.chars().enumerate() {
            if ch == ' ' || c as f32 >= reveal {
                continue;
            }
            let hue = theme::mix(t.accent, t.accent_alt, c as f32 / cols as f32);
            // The leading edge of the reveal, and the sweep's band.
            let edge = (1.0 - (reveal - c as f32) / 4.0).clamp(0.0, 1.0);
            let swept = if sweep > 0.0 && sweep < 1.0 {
                (1.0 - (c as f32 - band).abs() / 4.0).max(0.0)
            } else {
                0.0
            };
            let colour = theme::mix(hue, WHITE, (edge.max(swept) * 0.7).min(0.7));
            let mut s = [0u8; 4];
            put(
                buf,
                x0 + c as u16,
                y0 + row as u16,
                ch.encode_utf8(&mut s),
                fill.fg(colour).add_modifier(Modifier::BOLD),
            );
        }
    }
}

/// The compact card's title: the name, letter-spaced, in the same gradient.
fn draw_title(buf: &mut Buffer, x0: u16, y: u16, inner_w: u16, t: &Theme, ms: u64) {
    let title = "k  o  d  a";
    let n = title.chars().count();
    let x = x0 + (inner_w.saturating_sub(n as u16)) / 2;
    let reveal = anim::ease_out_cubic(window(ms, REVEAL)) * (n as f32 + 2.0);
    let fill = t
        .bg_panel
        .map(|bg| Style::default().bg(bg))
        .unwrap_or_default();
    for (i, ch) in title.chars().enumerate() {
        if ch == ' ' || i as f32 >= reveal {
            continue;
        }
        let hue = theme::mix(t.accent, t.accent_alt, i as f32 / (n - 1) as f32);
        let mut s = [0u8; 4];
        put(
            buf,
            x + i as u16,
            y,
            ch.encode_utf8(&mut s),
            fill.fg(hue).add_modifier(Modifier::BOLD),
        );
    }
}

/// A few sparks in the margins round the wordmark, each twinkling on its own
/// beat. Restrained on purpose: eight fixed cells, never over any text.
fn draw_sparks(buf: &mut Buffer, r: Rect, art_rows: u16, t: &Theme, g: &Glyphs, ms: u64) {
    if ms < TRACE.1 + 200 {
        return;
    }
    let frames: [&str; 6] = if g.fine_blocks {
        [" ", "·", "✧", "✦", "✧", "·"]
    } else {
        [" ", ".", "+", "*", "+", "."]
    };
    let (l, rt) = (r.x + 2, r.x + r.width - 3);
    let (top, art_end) = (r.y + 1, r.y + 2 + art_rows);
    let spots = [
        (l, top),
        (rt, top),
        (l + 5, top),
        (rt - 6, top),
        (l, r.y + 3),
        (rt, r.y + 2 + art_rows / 2),
        (l + 2, art_end),
        (rt - 3, art_end),
    ];
    let fill = t
        .bg_panel
        .map(|bg| Style::default().bg(bg))
        .unwrap_or_default();
    for (i, (x, y)) in spots.into_iter().enumerate() {
        let beat = ((ms + i as u64 * 173) % 1300) as f32 / 1300.0;
        let f = (beat * frames.len() as f32) as usize % frames.len();
        let colour = theme::mix(t.accent_alt, WHITE, 0.3);
        put(buf, x, y, frames[f], fill.fg(colour));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real wordmark's footprint: 36 columns by 6 rows.
    const ART: [&str; 6] = [
        "████████████████████████████████████",
        "██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ",
        "████████████████████████████████████",
        "██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ",
        "████████████████████████████████████",
        "██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ██ ",
    ];

    fn paint(w: u16, h: u16, ms: u64, animate: bool, g: &Glyphs) -> (bool, Buffer) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let t = theme::resolve("dark");
        let drawn = draw(
            &mut buf,
            area,
            &t,
            g,
            &ART,
            Duration::from_millis(ms),
            animate,
        );
        (drawn, buf)
    }

    fn text(buf: &Buffer) -> String {
        let a = buf.area;
        (0..a.height)
            .map(|y| {
                (0..a.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_size_decides_the_layout() {
        assert_eq!(place(Rect::new(0, 0, 120, 40), &ART).0, Layout::Full);
        assert_eq!(place(Rect::new(0, 0, 40, 12), &ART).0, Layout::Compact);
        assert_eq!(place(Rect::new(0, 0, 20, 6), &ART).0, Layout::None);
        let (drawn, _) = paint(20, 6, 3000, true, &crate::theme::UNICODE);
        assert!(!drawn, "too small draws nothing rather than clipping");
    }

    /// The card never reaches outside the screen, at any size it draws at.
    #[test]
    fn the_card_fits_the_screen() {
        for (w, h) in [(120, 40), (80, 24), (60, 20), (44, 12), (40, 12)] {
            let area = Rect::new(0, 0, w, h);
            let (layout, r) = place(area, &ART);
            if layout != Layout::None {
                assert!(
                    r.right() <= area.right() && r.bottom() <= area.bottom(),
                    "{w}x{h}"
                );
            }
        }
    }

    /// Once settled, both facts are on screen in full, from the one shared source.
    #[test]
    fn the_name_and_contact_are_readable_once_it_settles() {
        for animate in [true, false] {
            let (_, buf) = paint(100, 30, 4000, animate, &crate::theme::UNICODE);
            let s = text(&buf);
            assert!(s.contains(CREATOR_NAME), "{s}");
            assert!(s.contains(CREATOR_CONTACT), "{s}");
        }
    }

    /// Nothing is there before its beat: the name has not faded in at 200 ms.
    #[test]
    fn the_beats_arrive_in_order() {
        let (_, early) = paint(100, 30, 200, true, &crate::theme::UNICODE);
        assert!(!text(&early).contains(CREATOR_NAME));
        // Without motion, the finished card from the first frame.
        let (_, still) = paint(100, 30, 0, false, &crate::theme::UNICODE);
        assert!(text(&still).contains(CREATOR_NAME));
    }

    /// The settled card is identical frame to frame: nothing left moving to
    /// repaint while it is being read.
    #[test]
    fn the_last_stretch_is_still() {
        let (_, a) = paint(100, 30, SETTLE + 50, true, &crate::theme::UNICODE);
        let (_, b) = paint(100, 30, SETTLE + 400, true, &crate::theme::UNICODE);
        assert_eq!(a, b);
    }

    #[test]
    fn ascii_glyphs_get_ascii() {
        let (_, buf) = paint(100, 30, 2500, true, &crate::theme::ASCII);
        let s = text(&buf);
        assert!(s.chars().all(|c| c.is_ascii() || c == '█'), "{s}");
    }

    #[test]
    fn it_is_bounded() {
        assert!(DURATION < Duration::from_secs(5), "WCAG 2.2.2");
    }
}
