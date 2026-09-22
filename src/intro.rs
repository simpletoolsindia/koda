//! The opening titles: koda boots.
//!
//! A coding agent's intro, made of code. It plays in front of the welcome
//! card when koda starts:
//!
//! 1. **Code rain.** Columns of falling code glyphs (`{ } => fn :: λ`), each
//!    with a bright head and a fading trail.
//! 2. **Decode.** The `>_` mark is drawn stroke by stroke. Each cell scrambles
//!    through code glyphs, then locks into a block with a flash, as if the
//!    logo were being compiled.
//! 3. **The name.** `k  o  d  a` scrambles and locks, letter by letter.
//! 4. **Boot check.** This launch's real facts (model, workspace and branch,
//!    mode) type in, each with a spinner that resolves to a tick.
//! 5. **Sweep and dissolve.** A light crosses the mark as the rain thins
//!    out, then the whole overlay dissolves, content last, into the real
//!    screen underneath.
//!
//! It keeps to the rules every other piece of motion in koda keeps:
//!
//! - **Bounded**, at 2.5 s, inside WCAG's five seconds.
//! - **Never in the way.** Any key ends it, and the key then does what it was
//!   pressed for.
//! - **Honours the motion setting.** It does not play under reduced or no
//!   motion (the welcome card is the still frame), and `intro = false` turns
//!   it off. `/intro` replays it.
//! - **Fits the screen.** A compact version on a small terminal, the boot
//!   check dropped when there is no room for it, nothing at all when there
//!   is no room for the mark.
//!
//! It is painted cell by cell into ratatui's buffer as an overlay, like the
//! curtain call: nothing underneath is touched, so when it dissolves, what
//! shows through is the screen as it really is. Every frame is a pure
//! function of elapsed time, so any moment of it can be tested.
//!
//! | window         | what happens                                        |
//! | -------------- | --------------------------------------------------- |
//! | 0 – 1400 ms    | code rain, thinning out from 1000 ms                |
//! | 250 – 1000     | the mark decodes, stroke by stroke                  |
//! | 850 – 1300     | the name scrambles and locks                        |
//! | 1000 – 1350    | the version and tagline fade in                     |
//! | 1050 – 1900    | the boot check: each fact types in, then ticks      |
//! | 1600 – 2000    | a light sweeps across the mark                      |
//! | 2050 – 2500    | it dissolves into the screen underneath             |

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::anim;
use crate::theme::{self, Glyphs, Theme};

/// How long the whole sequence runs.
pub const DURATION: Duration = Duration::from_millis(2500);

const RAIN: (u64, u64) = (0, 1400);
/// The rain starts thinning here and is gone at `RAIN.1`.
const RAIN_FADE: u64 = 1000;
const DECODE: (u64, u64) = (250, 1000);
/// How long a cell scrambles before it locks.
const SCRAMBLE: u64 = 220;
/// How long a cell stays lit after it locks.
const LOCK_GLOW: u64 = 240;
const NAME: (u64, u64) = (850, 1300);
const SUB: (u64, u64) = (1000, 1350);
/// When the first fact starts, and the gap to the next.
const FACTS_AT: u64 = 1050;
const FACT_GAP: u64 = 200;
/// How long a fact takes to type, then how long its spinner turns.
const FACT_TYPE: u64 = 180;
const FACT_SPIN: u64 = 240;
const SWEEP: (u64, u64) = (1600, 2000);
const DISSOLVE: (u64, u64) = (2050, 2500);

const WHITE: Color = Color::Rgb(255, 255, 255);
const WORDMARK: &str = "k  o  d  a";
/// What the rain and the decode are made of. ASCII, so every terminal draws
/// it; the few symbols that are not are swapped in only where glyphs allow.
const CODE: &[&str] = &[
    "{", "}", "(", ")", "<", ">", "[", "]", ";", ":", "=", "+", "-", "*", "/", "&", "|", "!", "?",
    "#", "$", "%", "_", "0", "1", "f", "n", "x", "i", "=>", "::", "fn",
];
const CODE_FINE: &[&str] = &["λ", "→", "∷", "≠", "≤", "⇒"];
const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

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

/// A small, fixed hash: the same inputs always give the same "random"
/// number, so every frame is reproducible.
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

/// A one-cell code glyph for this seed. Multi-character tokens (`=>`, `fn`)
/// are for the rain, which has room; a single cell takes their first char.
fn glyph(seed: u32, g: &Glyphs, one_cell: bool) -> &'static str {
    let fine = g.fine_blocks && seed % 7 == 0;
    let pool = if fine { CODE_FINE } else { CODE };
    let s = pool[(seed as usize / 7) % pool.len()];
    if one_cell && s.len() > 1 && !fine {
        &s[..1]
    } else {
        s
    }
}

/// One cell of the mark: where it is, relative to the mark's top left, and
/// what is drawn there.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    col: u16,
    row: u16,
    glyph: String,
}

/// The mark as cells, in the order a pen would draw it: the chevron top to
/// bottom, then the underscore left to right. In block glyphs it is doubled
/// (each half-block pixel becomes a full block, two columns wide), which is
/// what makes it read as a logo rather than a character.
fn mark_cells(art: &[&str], scale: bool) -> Vec<Cell> {
    let mut out = Vec::new();
    if scale {
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
    // The pen: row by row down the chevron; the last row is the underscore,
    // drawn left to right after it. Row-major order already is exactly that.
    out.sort_by_key(|c| (c.row, c.col));
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

/// What the intro says: the subtitle, and the facts the boot check ticks off.
pub struct Titles<'a> {
    pub subtitle: &'a str,
    /// `("model", "qwen2.5-coder:14b")`, `("workspace", "koda ⎇ master")`, …
    pub facts: &'a [(&'a str, String)],
}

struct Plan {
    layout: Layout,
    cells: Vec<Cell>,
    mark_w: u16,
    mark_h: u16,
    mark_x: u16,
    mark_y: u16,
    word_y: u16,
    sub_y: u16,
    /// Where the boot check starts, and how many facts fit.
    facts_y: u16,
    facts: usize,
}

const LABEL_W: usize = 10;

fn fact_width(label: &str, value: &str) -> usize {
    // spinner/tick, a space, the label padded, the value
    2 + LABEL_W.max(label.width() + 1) + value.width()
}

fn plan(area: Rect, art: &[&str], g: &Glyphs, titles: &Titles) -> Plan {
    let rows = art.len() as u16;
    let cols = art.iter().map(|r| r.width()).max().unwrap_or(0) as u16;
    let sub_w = titles.subtitle.width() as u16;
    let facts_w = titles
        .facts
        .iter()
        .map(|(l, v)| fact_width(l, v))
        .max()
        .unwrap_or(0) as u16;
    let attempt = |layout: Layout, scale: bool| -> Option<Plan> {
        let (mark_w, mark_h) = if scale {
            (cols * 2, rows * 2)
        } else {
            (cols, rows)
        };
        // Mark, blank, name, blank, subtitle.
        let core_h = mark_h + 4;
        let core_w = mark_w.max(WORDMARK.len() as u16).max(sub_w);
        if area.width < core_w + 4 || area.height < core_h + 2 {
            return None;
        }
        // Then a blank row and the facts, as many as fit.
        let room = area.height.saturating_sub(core_h + 2 + 1) as usize;
        let facts = if area.width >= facts_w + 4 {
            titles.facts.len().min(room)
        } else {
            0
        };
        let block_h = core_h + if facts > 0 { 1 + facts as u16 } else { 0 };
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
            facts_y: top + mark_h + 5,
            facts,
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
            facts_y: 0,
            facts: 0,
        })
}

/// Which layout this screen gets.
#[cfg(test)]
pub fn layout(area: Rect, art: &[&str], g: &Glyphs, titles: &Titles) -> Layout {
    plan(area, art, g, titles).layout
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
    let threshold = if content {
        0.45 + noise * 0.55
    } else {
        noise * 0.7
    };
    k < threshold
}

/// Draw the intro at `elapsed`. Returns false when the screen has no room for
/// it, or it is over, so the caller can stop asking.
pub fn draw(
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    g: &Glyphs,
    art: &[&str],
    titles: &Titles,
    elapsed: Duration,
) -> bool {
    if elapsed >= DURATION {
        return false;
    }
    let p = plan(area, art, g, titles);
    if p.layout == Layout::None {
        return false;
    }
    let ms = elapsed.as_millis() as u64;
    let base = match t.bg_panel {
        Some(c) => Style::default().bg(c),
        None => Style::default(),
    };
    // What colours fade from and to. Without a known background, the muted
    // colour stands in for "nearly invisible".
    let dark = t.bg_panel.unwrap_or(t.muted);

    // 1. The backdrop.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if covered(x, y, ms, false) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                    cell.set_style(base);
                }
            }
        }
    }

    // 2. The rain, falling round the content but never through it: a clear
    //    margin round the mark and the text keeps them readable.
    let facts_w = titles.facts[..p.facts]
        .iter()
        .map(|(l, v)| fact_width(l, v))
        .max()
        .unwrap_or(0) as u16;
    let content_w = p
        .mark_w
        .max(WORDMARK.len() as u16)
        .max(titles.subtitle.width() as u16)
        .max(facts_w)
        + 6;
    let bottom = if p.facts > 0 {
        p.facts_y + p.facts as u16
    } else {
        p.sub_y + 1
    };
    let clear = Rect {
        x: area.x + area.width.saturating_sub(content_w) / 2,
        y: p.mark_y.saturating_sub(1),
        width: content_w.min(area.width),
        height: (bottom + 1).min(area.bottom()) - p.mark_y.saturating_sub(1),
    };
    rain(buf, area, clear, t, g, ms, base, dark);

    let hue = |col: u16, row: u16| {
        let span = (p.mark_w + p.mark_h).max(1) as f32;
        theme::mix(t.accent, t.accent_alt, (col + row) as f32 / span)
    };

    // 3. The mark, decoding stroke by stroke, then swept.
    let sweep = window(ms, SWEEP);
    let band = anim::ease_in_out_sine(sweep) * (p.mark_w + p.mark_h + 12) as f32 - 6.0;
    let n = p.cells.len().max(1) as u64;
    let span = DECODE.1 - DECODE.0 - SCRAMBLE;
    for (i, cell) in p.cells.iter().enumerate() {
        let (x, y) = (p.mark_x + cell.col, p.mark_y + cell.row);
        if !covered(x, y, ms, true) {
            continue;
        }
        let lock = DECODE.0 + SCRAMBLE + span * i as u64 / n;
        let starts = lock - SCRAMBLE;
        if ms < starts {
            continue;
        }
        let colour = hue(cell.col, cell.row);
        if ms < lock {
            // Scrambling: a new code glyph every 45 ms, brighter as it nears
            // the lock.
            let k = (ms - starts) as f32 / SCRAMBLE as f32;
            let seed = hash(i as u32, (ms / 45) as u32);
            put(
                buf,
                x,
                y,
                glyph(seed, g, true),
                base.fg(theme::mix(colour, WHITE, 0.2 + 0.5 * k)),
            );
            continue;
        }
        let glow = 1.0 - ((ms - lock) as f32 / LOCK_GLOW as f32).min(1.0);
        let swept = if sweep > 0.0 && sweep < 1.0 {
            (1.0 - ((cell.col + cell.row) as f32 - band).abs() / 5.0).max(0.0)
        } else {
            0.0
        };
        put(
            buf,
            x,
            y,
            &cell.glyph,
            base.fg(theme::mix(colour, WHITE, (glow.max(swept) * 0.8).min(0.8)))
                .add_modifier(Modifier::BOLD),
        );
    }

    // 4. The name: each letter scrambles, then locks, left to right.
    let letters: Vec<char> = WORDMARK.chars().collect();
    let word_x = area.x + (area.width - letters.len() as u16) / 2;
    let step = (NAME.1 - NAME.0) / letters.len() as u64;
    for (i, ch) in letters.iter().enumerate() {
        let x = word_x + i as u16;
        let lock = NAME.0 + step * (i as u64 + 1);
        if *ch == ' ' || ms < NAME.0 + step * i as u64 / 2 || !covered(x, p.word_y, ms, true) {
            continue;
        }
        let colour = theme::mix(t.accent, t.accent_alt, i as f32 / letters.len() as f32);
        let (sym, style) = if ms < lock {
            let r = LETTERS[hash(i as u32, (ms / 40) as u32) as usize % LETTERS.len()] as char;
            (r, base.fg(theme::mix(colour, dark, 0.35)))
        } else {
            let glow = 1.0 - ((ms - lock) as f32 / 200.0).min(1.0);
            (
                *ch,
                base.fg(theme::mix(colour, WHITE, glow * 0.8))
                    .add_modifier(Modifier::BOLD),
            )
        };
        let mut s = [0u8; 4];
        put(buf, x, p.word_y, sym.encode_utf8(&mut s), style);
    }

    // 5. The version and tagline, fading in.
    let k = window(ms, SUB);
    if k > 0.0 {
        let colour = theme::mix(dark, t.muted, anim::ease_out_cubic(k));
        let sub = titles.subtitle;
        let mut x = area.x + (area.width.saturating_sub(sub.width() as u16)) / 2;
        for ch in sub.chars() {
            let w = ch.to_string().width() as u16;
            if covered(x, p.sub_y, ms, true) {
                let mut s = [0u8; 4];
                put(buf, x, p.sub_y, ch.encode_utf8(&mut s), base.fg(colour));
            }
            x += w;
        }
    }

    // 6. The boot check: each fact types in, its spinner turns, it ticks.
    if p.facts > 0 {
        let facts = &titles.facts[..p.facts];
        let w = facts
            .iter()
            .map(|(l, v)| fact_width(l, v))
            .max()
            .unwrap_or(0) as u16;
        let x0 = area.x + (area.width.saturating_sub(w)) / 2;
        for (i, (label, value)) in facts.iter().enumerate() {
            let y = p.facts_y + i as u16;
            let at = FACTS_AT + FACT_GAP * i as u64;
            if ms < at {
                continue;
            }
            let typed = ((ms - at) as f32 / FACT_TYPE as f32).min(1.0);
            let done = ms >= at + FACT_TYPE + FACT_SPIN;
            let mark = if done {
                g.ok
            } else {
                g.spinner[((ms / 70) as usize) % g.spinner.len()]
            };
            let mark_colour = if done { t.success } else { t.accent };
            let mut cells: Vec<(String, Style)> = vec![
                (
                    mark.to_string(),
                    base.fg(mark_colour).add_modifier(Modifier::BOLD),
                ),
                (" ".into(), base),
            ];
            let label = format!("{label:<LABEL_W$}");
            let full: Vec<char> = label.chars().chain(value.chars()).collect();
            let shown = (typed * full.len() as f32).ceil() as usize;
            for (j, ch) in full.iter().take(shown).enumerate() {
                let style = if j < label.chars().count() {
                    base.fg(t.muted)
                } else {
                    base.fg(t.text)
                };
                cells.push((ch.to_string(), style));
            }
            let mut x = x0;
            for (sym, style) in cells {
                let cw = sym.width().max(1) as u16;
                if x + cw > area.right() {
                    break;
                }
                if covered(x, y, ms, true) {
                    put(buf, x, y, &sym, style);
                }
                x += cw;
            }
        }
    }
    true
}

/// Columns of falling code, each at its own speed, with a bright head and a
/// fading trail. About one column in three rains; the rest stay dark, so it
/// reads as rain rather than a wall.
#[allow(clippy::too_many_arguments)] // one frame's worth of context, all read-only
fn rain(
    buf: &mut Buffer,
    area: Rect,
    clear: Rect,
    t: &Theme,
    g: &Glyphs,
    ms: u64,
    base: Style,
    dark: Color,
) {
    if ms >= RAIN.1 {
        return;
    }
    // Everything fades out together at the end.
    let fade = 1.0 - window(ms, (RAIN_FADE, RAIN.1));
    const TRAIL: i32 = 7;
    let h = i32::from(area.height);
    let mut x = area.left();
    while x < area.right() {
        let seed = hash(u32::from(x), 0xC0DE);
        if seed % 3 != 0 {
            x += 1;
            continue;
        }
        // Rows a second, and where in its fall this column starts.
        let speed = 14.0 + (seed % 17) as f32;
        let offset = (seed >> 8) % (h as u32 + TRAIL as u32);
        let head = ((ms as f32 / 1000.0 * speed) as i32 + offset as i32) % (h + TRAIL) - TRAIL / 2;
        let tint = theme::mix(t.accent, t.accent_alt, unit(u32::from(x), 9));
        let mut widest = 1;
        for k in 0..TRAIL {
            let row = head - k;
            if row < 0 || row >= h {
                continue;
            }
            let y = area.top() + row as u16;
            if !covered(x, y, ms, false) {
                continue;
            }
            // The glyph changes as it falls; the head flickers fastest.
            let tick = if k == 0 { ms / 60 } else { ms / 160 };
            let sym = glyph(
                hash(u32::from(x) ^ (row as u32) << 8, tick as u32),
                g,
                false,
            );
            let sym_w = sym.width() as u16;
            if x + sym_w > area.right()
                || (clear.contains((x, y).into()) || clear.contains((x + sym_w - 1, y).into()))
            {
                continue;
            }
            widest = widest.max(sym_w);
            let strength = (1.0 - k as f32 / TRAIL as f32) * fade;
            let colour = if k == 0 {
                theme::mix(dark, theme::mix(tint, WHITE, 0.5), strength)
            } else {
                theme::mix(dark, tint, strength * 0.7)
            };
            put(buf, x, y, sym, base.fg(colour));
            if sym_w > 1 {
                // A two-character token covers the next cell too.
                if let Some(next) = buf.cell_mut((x + 1, y)) {
                    next.set_symbol("");
                }
            }
        }
        x += widest.max(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: [&str; 4] = ["██▄     ", " ▀██▄   ", " ▄██▀   ", "██▀ ▄▄▄▄"];
    const ASCII_MARK: [&str; 4] = ["\\\\      ", " \\\\     ", " //     ", "//  ____"];

    fn facts() -> Vec<(&'static str, String)> {
        vec![
            ("model", "qwen2.5-coder:14b".to_string()),
            ("workspace", "koda  on master".to_string()),
            ("mode", "EXEC - ASK".to_string()),
        ]
    }

    fn paint(w: u16, h: u16, ms: u64, g: &Glyphs) -> (bool, Buffer) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let t = theme::resolve("dark");
        let art: &[&str] = if g.fine_blocks { &MARK } else { &ASCII_MARK };
        let sub = subtitle("0.3.0", "an agent, locally grown", g);
        let f = facts();
        let titles = Titles {
            subtitle: &sub,
            facts: &f,
        };
        let drawn = draw(
            &mut buf,
            area,
            &t,
            g,
            art,
            &titles,
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
        let sub = subtitle("0.3.0", "an agent, locally grown", g);
        let f = facts();
        let titles = Titles {
            subtitle: &sub,
            facts: &f,
        };
        let l = |w, h| layout(Rect::new(0, 0, w, h), &MARK, g, &titles);
        assert_eq!(l(120, 40), Layout::Full);
        assert_eq!(l(80, 24), Layout::Full);
        assert_eq!(l(50, 12), Layout::Compact);
        assert_eq!(l(20, 6), Layout::None);
        let (drawn, _) = paint(20, 6, 500, g);
        assert!(!drawn, "no room draws nothing rather than clipping");
    }

    /// At the hold, before the dissolve: the whole mark, the name, the
    /// subtitle and every fact, ticked.
    #[test]
    fn everything_has_arrived_by_the_hold() {
        let (drawn, buf) = paint(100, 30, 2040, &crate::theme::UNICODE);
        assert!(drawn);
        let s = text(&buf);
        assert!(s.contains(WORDMARK), "{s}");
        assert!(s.contains("v0.3.0  ·  an agent, locally grown"), "{s}");
        for (_, v) in facts() {
            assert!(s.contains(&v), "{v} missing:\n{s}");
        }
        assert_eq!(s.matches(crate::theme::UNICODE.ok).count(), 3, "{s}");
        assert_eq!(s.matches('█').count(), mark_cells(&MARK, true).len(), "{s}");
    }

    /// The beats arrive in order: early on there is rain but no mark and no
    /// name; mid-decode the mark is partly locked.
    #[test]
    fn the_beats_arrive_in_order() {
        let (_, early) = paint(100, 30, 150, &crate::theme::UNICODE);
        let s = text(&early);
        assert!(!s.contains('█') && !s.contains(WORDMARK), "{s}");
        assert!(
            s.trim().chars().any(|c| !c.is_whitespace()),
            "the rain is falling"
        );
        let (_, mid) = paint(100, 30, 650, &crate::theme::UNICODE);
        let locked = text(&mid).matches('█').count();
        assert!(
            locked > 0 && locked < mark_cells(&MARK, true).len(),
            "{locked}"
        );
    }

    /// The rain never falls through the text: the name's row holds only the
    /// name, whatever the moment.
    #[test]
    fn the_rain_keeps_clear_of_the_text() {
        for ms in [900, 1100, 1300] {
            let (_, buf) = paint(100, 30, ms, &crate::theme::UNICODE);
            let s = text(&buf);
            let row: Vec<char> = s
                .lines()
                .find(|l| l.contains("o  d"))
                .unwrap_or("")
                .chars()
                .collect();
            // The name and three columns either side of it.
            let start = row
                .iter()
                .position(|c| !c.is_whitespace() && c.is_ascii_lowercase());
            let Some(start) = start.map(|i| i.saturating_sub(3)) else {
                continue;
            };
            let near: String = row[start..(start + WORDMARK.len() + 6).min(row.len())]
                .iter()
                .collect();
            let stray: String = near
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_ascii_lowercase())
                .collect();
            assert!(stray.is_empty(), "{ms}: {near:?}");
        }
    }

    /// The pen draws the chevron first and the underscore last.
    #[test]
    fn the_mark_is_drawn_in_pen_order() {
        let cells = mark_cells(&MARK, true);
        let last_row = cells.iter().map(|c| c.row).max().unwrap();
        let first_underscore = cells.iter().position(|c| c.row == last_row).unwrap();
        assert!(cells[..first_underscore].iter().all(|c| c.row < last_row));
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
        let (a, b, c) = (untouched(2040), untouched(2250), untouched(2450));
        assert!(a < b && b < c, "{a} {b} {c}");
    }

    #[test]
    fn every_frame_is_reproducible() {
        for ms in [0, 250, 700, 1400, 2200] {
            assert_eq!(
                paint(90, 28, ms, &crate::theme::UNICODE).1,
                paint(90, 28, ms, &crate::theme::UNICODE).1,
                "{ms}"
            );
        }
    }

    #[test]
    fn ascii_glyphs_get_ascii() {
        for ms in [100, 500, 900, 1300, 1700, 2100] {
            let (_, buf) = paint(100, 30, ms, &crate::theme::ASCII);
            let s = text(&buf);
            assert!(s.is_ascii(), "{ms}: {s}");
        }
    }

    /// No size, however odd, makes it draw outside the screen or panic.
    #[test]
    fn any_size_is_safe() {
        for w in [1u16, 10, 24, 41, 60, 80, 200] {
            for h in [1u16, 5, 9, 13, 17, 24, 60] {
                for ms in (0..2500).step_by(137) {
                    paint(w, h, ms, &crate::theme::UNICODE);
                }
            }
        }
    }

    /// Short on height, the boot check goes first; the mark and name stay.
    #[test]
    fn the_boot_check_gives_way_on_a_short_screen() {
        let (_, buf) = paint(80, 15, 2040, &crate::theme::UNICODE);
        let s = text(&buf);
        assert!(s.contains(WORDMARK), "{s}");
        assert!(!s.contains("qwen2.5-coder"), "{s}");
    }

    #[test]
    fn the_doubled_mark_keeps_its_shape() {
        let cells = mark_cells(&MARK, true);
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
    }
}
