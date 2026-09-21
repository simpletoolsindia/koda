//! The opening titles: a short sequence when koda starts, before the welcome
//! card.
//!
//! Particles stream in from around the screen and assemble koda's mark (the
//! `>_` prompt), a ring spreads out from it as it lands, the name types in
//! beneath, a light sweeps across, and the whole thing dissolves into the real
//! screen underneath.
//!
//! It keeps to the rules every other piece of motion in koda keeps:
//!
//! - **Bounded**, at 2.2 s, well inside WCAG's five seconds.
//! - **Never in the way.** Any key ends it, and the key then does what it was
//!   pressed for; nobody waits on an intro to start typing.
//! - **Honours the motion setting.** It does not play under reduced or no
//!   motion (the settled welcome card is the still frame), and `intro = false`
//!   turns it off.
//! - **Fits the screen.** A compact version on a small terminal, and nothing
//!   at all when there is no room.
//!
//! It is painted cell by cell into ratatui's buffer, as an overlay, like the
//! curtain call: nothing underneath is touched, so when it dissolves, what
//! shows through is the screen as it really is.
//!
//! | window        | what happens                                          |
//! | ------------- | ----------------------------------------------------- |
//! | 0 – 700 ms    | particles fly in and assemble the mark                |
//! | 600 – 1050    | a ring spreads out from the mark                      |
//! | 800 – 1300    | `k o d a` types in beneath it                         |
//! | 1100 – 1500   | the version and a tagline fade in                     |
//! | 1300 – 1750   | a light sweeps across the mark                        |
//! | 1800 – 2200   | it dissolves into the screen underneath               |

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::anim;
use crate::theme::{self, Glyphs, Theme};

/// How long the whole sequence runs.
pub const DURATION: Duration = Duration::from_millis(2200);

/// Each particle waits up to this long before setting off, so the mark
/// assembles as a stream rather than all at once.
const STAGGER: u64 = 280;
/// How long one particle takes to reach its cell.
const FLIGHT: u64 = 420;
/// How long a cell stays lit after its particle lands.
const LANDING_GLOW: u64 = 260;
const RING: (u64, u64) = (600, 1050);
const WORD: (u64, u64) = (800, 1300);
const SUB: (u64, u64) = (1100, 1500);
const SWEEP: (u64, u64) = (1300, 1750);
const DISSOLVE: (u64, u64) = (1800, 2200);

const WHITE: Color = Color::Rgb(255, 255, 255);
const WORDMARK: &str = "k  o  d  a";

/// Progress through a window: 0 before it, 1 after.
fn window(ms: u64, (a, b): (u64, u64)) -> f32 {
    if ms <= a {
        0.0
    } else if ms >= b {
        1.0
    } else {
        (ms - a) as f32 / (b - a) as f32
    }
}

/// A small, fixed hash: the same cell always gets the same "random" number, so
/// every frame of the sequence is reproducible.
fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^ (h >> 13)
}

/// 0..1 from a hash.
fn unit(a: u32, b: u32) -> f32 {
    (hash(a, b) % 10_000) as f32 / 10_000.0
}

/// One cell of the mark: where it is, relative to the mark's top left, and
/// what is drawn there.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    col: u16,
    row: u16,
    glyph: String,
}

/// The mark as cells. In block glyphs it is doubled (each half-block pixel
/// becomes a full two-column block), which is what makes it read as a logo
/// rather than a character; `scale` 1 draws it as it is.
fn mark_cells(art: &[&str], scale: bool) -> Vec<Cell> {
    let mut out = Vec::new();
    if scale {
        // Each character holds two vertical pixels; doubled, each pixel is a
        // full block two columns wide and one row tall.
        for (r, line) in art.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                let (top, bottom) = match ch {
                    '█' => (true, true),
                    '▀' => (true, false),
                    '▄' => (false, true),
                    _ => (false, false),
                };
                for (i, on) in [top, bottom].into_iter().enumerate() {
                    if on {
                        for dx in 0..2 {
                            out.push(Cell {
                                col: (c * 2 + dx) as u16,
                                row: (r * 2 + i) as u16,
                                glyph: "█".into(),
                            });
                        }
                    }
                }
            }
        }
    } else {
        for (r, line) in art.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                if ch != ' ' {
                    out.push(Cell {
                        col: c as u16,
                        row: r as u16,
                        glyph: ch.to_string(),
                    });
                }
            }
        }
    }
    out
}

/// Which version fits, and where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// The doubled mark.
    Full,
    /// The mark at its own size.
    Compact,
    /// No room: nothing is drawn.
    None,
}

/// The text under the name: `v0.3.0 · an agent, locally grown`.
pub fn subtitle(version: &str, tagline: &str, g: &Glyphs) -> String {
    let dot = if g.fine_blocks { "·" } else { "-" };
    if tagline.is_empty() {
        format!("v{version}")
    } else {
        format!("v{version}  {dot}  {tagline}")
    }
}

struct Plan {
    layout: Layout,
    cells: Vec<Cell>,
    /// The mark's size in cells.
    mark_w: u16,
    mark_h: u16,
    /// Top-left of the mark on screen.
    mark_x: u16,
    mark_y: u16,
    /// Rows for the name and the subtitle.
    word_y: u16,
    sub_y: u16,
}

fn plan(area: Rect, art: &[&str], g: &Glyphs, sub_w: u16) -> Plan {
    let rows = art.len() as u16;
    let cols = art.iter().map(|r| r.width()).max().unwrap_or(0) as u16;
    let attempt = |layout: Layout, scale: bool| -> Option<Plan> {
        let (mark_w, mark_h) = if scale {
            (cols * 2, rows * 2)
        } else {
            (cols, rows)
        };
        // Mark, a blank row, the name, a blank row, the subtitle.
        let block_h = mark_h + 4;
        let block_w = mark_w.max(WORDMARK.len() as u16).max(sub_w);
        if area.width < block_w + 4 || area.height < block_h + 2 {
            return None;
        }
        let top = area.y + (area.height - block_h) / 2;
        Some(Plan {
            layout,
            cells: mark_cells(art, scale),
            mark_w,
            mark_h,
            mark_x: area.x + (area.width - mark_w) / 2,
            mark_y: top,
            word_y: top + mark_h + 1,
            sub_y: top + mark_h + 3,
        })
    };
    attempt(Layout::Full, g.fine_blocks)
        .or_else(|| attempt(Layout::Compact, false))
        .unwrap_or(Plan {
            layout: Layout::None,
            cells: Vec::new(),
            mark_w: 0,
            mark_h: 0,
            mark_x: 0,
            mark_y: 0,
            word_y: 0,
            sub_y: 0,
        })
}

/// Which layout this screen gets.
#[cfg(test)]
pub fn layout(area: Rect, art: &[&str], g: &Glyphs, sub: &str) -> Layout {
    plan(area, art, g, sub.width() as u16).layout
}

fn put(buf: &mut Buffer, x: u16, y: u16, sym: &str, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(sym);
        cell.set_style(style);
    }
}

/// Whether the overlay still covers (x, y) at this point of the dissolve.
/// Background cells go first, in a scatter; the mark and the text last.
fn covered(x: u16, y: u16, ms: u64, content: bool) -> bool {
    let k = window(ms, DISSOLVE);
    if k <= 0.0 {
        return true;
    }
    let noise = unit(u32::from(x), u32::from(y) ^ 0x51ED);
    // Content dissolves in the back half, so the logo is the last thing seen.
    let threshold = if content {
        0.45 + noise * 0.55
    } else {
        noise * 0.7
    };
    k < threshold
}

/// Draw the intro at `elapsed`. Returns false when the screen has no room for
/// it (or it is over), so the caller can stop asking.
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    g: &Glyphs,
    art: &[&str],
    sub: &str,
    elapsed: Duration,
) -> bool {
    if elapsed >= DURATION {
        return false;
    }
    let p = plan(area, art, g, sub.width() as u16);
    if p.layout == Layout::None {
        return false;
    }
    let ms = elapsed.as_millis() as u64;
    let bg = t.bg_panel;
    let base = match bg {
        Some(c) => Style::default().bg(c),
        None => Style::default(),
    };
    // Where colours fade from and to. Without a known background, the muted
    // colour stands in for "nearly invisible".
    let dark = bg.unwrap_or(t.muted);

    // 1. The backdrop, and a faint scatter of stars.
    let star = if g.fine_blocks { "·" } else { "." };
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if !covered(x, y, ms, false) {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(base);
            }
            let h = hash(u32::from(x), u32::from(y));
            if h % 97 == 0 {
                // Each twinkles on its own beat, fading in over the first beats.
                let phase = ((ms + u64::from(h % 900)) % 1400) as f32 / 1400.0;
                let bright = (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                let k = window(ms, (0, 500)) * (0.25 + 0.35 * bright);
                put(buf, x, y, star, base.fg(theme::mix(dark, t.accent_alt, k)));
            }
        }
    }

    let hue = |col: u16, row: u16| {
        let span = (p.mark_w + p.mark_h).max(1) as f32;
        theme::mix(t.accent, t.accent_alt, (col + row) as f32 / span)
    };
    let centre_x = p.mark_x as f32 + p.mark_w as f32 / 2.0;
    let centre_y = p.mark_y as f32 + p.mark_h as f32 / 2.0;

    // 2. The ring, spreading out from the mark as the last particles land.
    let ring = window(ms, RING);
    if ring > 0.0 && ring < 1.0 {
        let reach = (area.width as f32 / 2.0).max(area.height as f32);
        let radius = anim::ease_out_cubic(ring) * reach;
        let colour = theme::mix(t.accent, dark, ring);
        let glyph = if g.fine_blocks { "∙" } else { "." };
        // Terminal cells are about twice as tall as wide: halve the rows.
        let steps = (radius * 7.0).max(16.0) as usize;
        for i in 0..steps {
            let a = i as f32 / steps as f32 * std::f32::consts::TAU;
            let x = (centre_x + radius * a.cos()).round();
            let y = (centre_y + radius * a.sin() / 2.0).round();
            if x < area.left() as f32 || y < area.top() as f32 {
                continue;
            }
            let (x, y) = (x as u16, y as u16);
            if x < area.right() && y < area.bottom() && covered(x, y, ms, false) {
                put(buf, x, y, glyph, base.fg(colour));
            }
        }
    }

    // 3. The mark: each cell flies in as a particle, lands lit, then cools to
    //    its colour; later a light sweeps across it.
    let sweep = window(ms, SWEEP);
    let band = anim::ease_in_out_sine(sweep) * (p.mark_w + p.mark_h + 12) as f32 - 6.0;
    let (particle, trail) = if g.fine_blocks {
        ("•", "·")
    } else {
        ("*", ".")
    };
    for (i, cell) in p.cells.iter().enumerate() {
        let delay = u64::from(hash(i as u32, 7) % STAGGER as u32);
        let target = (p.mark_x + cell.col, p.mark_y + cell.row);
        if ms < delay {
            continue;
        }
        let flight = (ms - delay) as f32 / FLIGHT as f32;
        if flight < 1.0 {
            // In flight, from a point on a wide ellipse round the screen.
            let a = unit(i as u32, 3) * std::f32::consts::TAU;
            let (rx, ry) = (area.width as f32 * 0.6, area.height as f32 * 0.6);
            let start = (centre_x + rx * a.cos(), centre_y + ry * a.sin());
            let k = anim::ease_in_out_sine(flight);
            let at = |k: f32| {
                (
                    start.0 + (target.0 as f32 - start.0) * k,
                    start.1 + (target.1 as f32 - start.1) * k,
                )
            };
            let colour = theme::mix(hue(cell.col, cell.row), WHITE, 0.35);
            for (pos, glyph, fade) in [
                (at(k), particle, 0.0),
                (at((k - 0.08).max(0.0)), trail, 0.5),
            ] {
                let (x, y) = (pos.0.round(), pos.1.round());
                if x >= area.left() as f32
                    && y >= area.top() as f32
                    && x < area.right() as f32
                    && y < area.bottom() as f32
                {
                    put(
                        buf,
                        x as u16,
                        y as u16,
                        glyph,
                        base.fg(theme::mix(colour, dark, fade)),
                    );
                }
            }
            continue;
        }
        if !covered(target.0, target.1, ms, true) {
            continue;
        }
        let landed = ms - delay - FLIGHT;
        let glow = 1.0 - (landed as f32 / LANDING_GLOW as f32).min(1.0);
        let swept = if sweep > 0.0 && sweep < 1.0 {
            (1.0 - ((cell.col + cell.row) as f32 - band).abs() / 5.0).max(0.0)
        } else {
            0.0
        };
        let colour = theme::mix(
            hue(cell.col, cell.row),
            WHITE,
            (glow.max(swept) * 0.75).min(0.75),
        );
        put(
            buf,
            target.0,
            target.1,
            &cell.glyph,
            base.fg(colour).add_modifier(Modifier::BOLD),
        );
    }

    // 4. The name, typed in behind a bright caret.
    let typed = anim::ease_out_cubic(window(ms, WORD)) * (WORDMARK.len() as f32 + 1.0);
    let word_x = area.x + (area.width - WORDMARK.len() as u16) / 2;
    for (i, ch) in WORDMARK.chars().enumerate() {
        let x = word_x + i as u16;
        if ch == ' ' || i as f32 >= typed || !covered(x, p.word_y, ms, true) {
            continue;
        }
        let colour = theme::mix(t.accent, t.accent_alt, i as f32 / WORDMARK.len() as f32);
        let edge = (1.0 - (typed - i as f32) / 3.0).clamp(0.0, 1.0);
        let mut s = [0u8; 4];
        put(
            buf,
            x,
            p.word_y,
            ch.encode_utf8(&mut s),
            base.fg(theme::mix(colour, WHITE, edge * 0.8))
                .add_modifier(Modifier::BOLD),
        );
    }

    // 5. The version and tagline, fading in.
    let k = window(ms, SUB);
    if k > 0.0 {
        let colour = theme::mix(dark, t.muted, anim::ease_out_cubic(k));
        let sub_x = area.x + (area.width - sub.width() as u16) / 2;
        let mut x = sub_x;
        for ch in sub.chars() {
            let w = ch.to_string().width() as u16;
            if covered(x, p.sub_y, ms, true) {
                let mut s = [0u8; 4];
                put(buf, x, p.sub_y, ch.encode_utf8(&mut s), base.fg(colour));
            }
            x += w;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: [&str; 4] = ["██▄     ", " ▀██▄   ", " ▄██▀   ", "██▀ ▄▄▄▄"];
    const ASCII_MARK: [&str; 4] = ["\\\\      ", " \\\\     ", " //     ", "//  ____"];

    fn paint(w: u16, h: u16, ms: u64, g: &Glyphs) -> (bool, Buffer) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let t = theme::resolve("dark");
        let art: &[&str] = if g.fine_blocks { &MARK } else { &ASCII_MARK };
        let drawn = draw(
            &mut buf,
            area,
            &t,
            g,
            art,
            &subtitle("0.3.0", "an agent, locally grown", g),
            Duration::from_millis(ms),
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
    fn it_is_bounded_and_ends() {
        assert!(DURATION < Duration::from_secs(5), "WCAG 2.2.2");
        let (drawn, buf) = paint(100, 30, DURATION.as_millis() as u64, &crate::theme::UNICODE);
        assert!(!drawn);
        assert_eq!(
            buf,
            Buffer::empty(buf.area),
            "an ended intro touches nothing"
        );
    }

    #[test]
    fn the_size_decides_the_layout() {
        let g = &crate::theme::UNICODE;
        let l = |w, h| {
            layout(
                Rect::new(0, 0, w, h),
                &MARK,
                g,
                &subtitle("0.3.0", "an agent, locally grown", g),
            )
        };
        assert_eq!(l(120, 40), Layout::Full);
        assert_eq!(l(80, 24), Layout::Full);
        assert_eq!(l(50, 12), Layout::Compact);
        assert_eq!(l(20, 6), Layout::None);
        let (drawn, _) = paint(20, 6, 500, g);
        assert!(!drawn, "no room draws nothing rather than clipping");
    }

    /// At the hold, before the dissolve: the whole mark, the name and the
    /// subtitle are on screen.
    #[test]
    fn everything_has_arrived_by_the_hold() {
        let (drawn, buf) = paint(100, 30, 1780, &crate::theme::UNICODE);
        assert!(drawn);
        let s = text(&buf);
        assert!(s.contains(WORDMARK), "{s}");
        assert!(s.contains("v0.3.0  ·  an agent, locally grown"), "{s}");
        let blocks = s.matches('█').count();
        assert_eq!(blocks, mark_cells(&MARK, true).len(), "{s}");
    }

    /// The beats arrive in order: at 100 ms nothing has landed and the name
    /// has not started.
    #[test]
    fn nothing_is_there_before_its_beat() {
        let (_, buf) = paint(100, 30, 100, &crate::theme::UNICODE);
        let s = text(&buf);
        assert!(!s.contains('█') && !s.contains('k'), "{s}");
    }

    /// The dissolve hands the screen back: as it runs, fewer cells are covered.
    #[test]
    fn the_dissolve_uncovers_the_screen() {
        let untouched = |ms| {
            let (_, buf) = paint(100, 30, ms, &crate::theme::UNICODE);
            buf.content
                .iter()
                .filter(|c| **c == ratatui::buffer::Cell::default())
                .count()
        };
        let (a, b, c) = (untouched(1790), untouched(1950), untouched(2150));
        assert!(a < b && b < c, "{a} {b} {c}");
    }

    #[test]
    fn every_frame_is_reproducible() {
        for ms in [0, 250, 700, 1400, 2000] {
            assert_eq!(
                paint(90, 28, ms, &crate::theme::UNICODE).1,
                paint(90, 28, ms, &crate::theme::UNICODE).1,
                "{ms}"
            );
        }
    }

    #[test]
    fn ascii_glyphs_get_ascii() {
        for ms in [200, 500, 900, 1600] {
            let (_, buf) = paint(100, 30, ms, &crate::theme::ASCII);
            let s = text(&buf);
            assert!(s.is_ascii(), "{ms}: {s}");
        }
    }

    /// No size, however odd, makes it draw outside the screen or panic.
    #[test]
    fn any_size_is_safe() {
        for w in [1u16, 10, 24, 41, 60, 80, 200] {
            for h in [1u16, 5, 9, 13, 24, 60] {
                for ms in (0..2200).step_by(137) {
                    paint(w, h, ms, &crate::theme::UNICODE);
                }
            }
        }
    }

    #[test]
    fn the_doubled_mark_keeps_its_shape() {
        let cells = mark_cells(&MARK, true);
        // "██▄" top-left: two full pixels, doubled to four blocks on row 0.
        assert!(cells.iter().any(|c| c.row == 0 && c.col == 0));
        assert!(cells.iter().any(|c| c.row == 0 && c.col == 3));
        assert!(
            !cells.iter().any(|c| c.row == 0 && c.col == 4),
            "▄ has no top pixel"
        );
        assert!(
            cells.iter().any(|c| c.row == 1 && c.col == 4),
            "▄ has a bottom one"
        );
        assert_eq!(
            mark_cells(&MARK, false).len(),
            MARK.iter()
                .map(|r| r.chars().filter(|c| *c != ' ').count())
                .sum::<usize>()
        );
    }
}
