//! Transitions: the short, event-tied pieces of motion the UI plays when
//! something *changes* — a mode switch, a finished turn, a step checked off, the
//! context gauge moving.
//!
//! `anim` owns the clock and the curves; this module owns the handful of
//! stateful effects built from them, so `tui` holds one small value per effect
//! instead of an `Instant` and a pile of arithmetic at every call site.
//!
//! Every effect here obeys the same four rules, which are the ones
//! `docs/research-tui-delight.md` argues for and `docs/spec-ui-next.md` restates:
//!
//! 1. **Caused, never scheduled.** Each one starts because of an event. None
//!    loops, so an idle koda still never wakes.
//! 2. **Bounded, and under five seconds.** WCAG 2.2.2 needs a pause control for
//!    motion past 5 s; nothing here gets near it.
//! 3. **Colour, not position.** Brightness and hue change; no text moves, so
//!    nothing a person is reading shifts under them and nothing here is on
//!    WebKit's vestibular-trigger list.
//! 4. **Settles to the static frame.** At `t = 1` every effect draws exactly
//!    what the screen shows without it, so stopping — or motion being off —
//!    never causes a jump.

use std::time::{Duration, Instant};

use ratatui::style::Color;

use crate::anim;
use crate::theme;

/// A one-shot timeline: started once, done once.
#[derive(Debug, Clone, Copy)]
pub struct Pulse {
    started: Instant,
    life: Duration,
}

impl Pulse {
    pub fn new(life: Duration) -> Self {
        Self {
            started: Instant::now(),
            life,
        }
    }

    /// Linear progress in `0..=1`, or `None` once the pulse is over.
    pub fn t(&self) -> Option<f32> {
        self.t_at(self.started.elapsed())
    }

    fn t_at(&self, elapsed: Duration) -> Option<f32> {
        if elapsed >= self.life || self.life.is_zero() {
            return None;
        }
        Some(elapsed.as_secs_f32() / self.life.as_secs_f32())
    }

    pub fn live(&self) -> bool {
        self.t().is_some()
    }
}

/// How long a mode switch takes to settle. Inside NN/g's 100–500 ms band for a
/// state change, and short enough that typing straight after never waits.
pub const MODE_SHIFT: Duration = Duration::from_millis(320);

/// The composer frame's colour partway through a mode switch: the old mode's
/// colour eases into the new one, and the new one arrives a little lit.
///
/// Returns `(edge, title)`. At `t = 1` both are exactly `to`.
pub fn mode_shift(from: Color, to: Color, t: f32) -> (Color, Color) {
    let e = anim::ease_out_cubic(t.clamp(0.0, 1.0));
    let edge = theme::mix(from, to, e);
    // The title ignites: brightest at the start, settling into the mode colour.
    let lift = 0.55 * (1.0 - e);
    let title = theme::mix(to, Color::Rgb(255, 255, 255), lift);
    (edge, title)
}

/// How a transient status message sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something finished well.
    Done,
    /// A setting changed.
    Info,
    /// Worth a second look.
    Warn,
}

/// A transient message in the status row: "✓ done in 12s", "mode → execute".
///
/// The status row is where ephemeral feedback belongs — the transcript is the
/// record, and a setting flipped is not something anyone scrolls back to read.
/// It fades in lit, holds, and hands the row back to "ready".
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pulse: Pulse,
}

/// Long enough to read a short sentence twice; under WCAG's five seconds.
pub const TOAST_LIFE: Duration = Duration::from_millis(3600);
/// The lit arrival, then the settle.
const TOAST_IN: f32 = 0.10;
/// The last stretch dims toward muted, so leaving is not a pop.
const TOAST_OUT: f32 = 0.80;

impl Toast {
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            pulse: Pulse::new(TOAST_LIFE),
        }
    }

    pub fn live(&self) -> bool {
        self.pulse.live()
    }

    /// The toast's colour now, given its settled tone colour. `None` when over.
    pub fn colour(&self, base: Color, muted: Color, animates: bool) -> Option<Color> {
        let t = self.pulse.t()?;
        Some(if animates {
            toast_colour(base, muted, t)
        } else {
            base
        })
    }
}

/// Arrive lit, hold at the tone colour, fade toward muted before leaving.
pub fn toast_colour(base: Color, muted: Color, t: f32) -> Color {
    if t < TOAST_IN {
        let k = anim::ease_out_cubic(t / TOAST_IN);
        theme::mix(Color::Rgb(255, 255, 255), base, 0.45 + 0.55 * k)
    } else if t > TOAST_OUT {
        let k = (t - TOAST_OUT) / (1.0 - TOAST_OUT);
        theme::mix(base, muted, anim::ease_in_out_sine(k))
    } else {
        base
    }
}

/// A number easing from where it was to where it now is — the context gauge,
/// so a jump from 30% to 70% reads as the context filling rather than a glitch.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    from: f32,
    to: f32,
    pulse: Option<Pulse>,
}

pub const GAUGE_EASE: Duration = Duration::from_millis(450);

impl Tween {
    pub const fn at(v: f32) -> Self {
        Self {
            from: v,
            to: v,
            pulse: None,
        }
    }

    /// Head for `to`, starting from wherever the value is *now* — so a second
    /// change mid-ease continues smoothly instead of snapping back.
    pub fn set(&mut self, to: f32, animates: bool) {
        if (to - self.to).abs() < f32::EPSILON {
            return;
        }
        if !animates {
            *self = Self::at(to);
            return;
        }
        self.from = self.value();
        self.to = to;
        self.pulse = Some(Pulse::new(GAUGE_EASE));
    }

    pub fn value(&self) -> f32 {
        match self.pulse.and_then(|p| p.t()) {
            Some(t) => self.from + (self.to - self.from) * anim::ease_out_cubic(t),
            None => self.to,
        }
    }

    pub fn moving(&self) -> bool {
        self.pulse.is_some_and(|p| p.live())
    }
}

/// How long a step that just finished stays lit in the plan panel.
pub const STEP_FLASH: Duration = Duration::from_millis(700);
/// How long a finished plan lingers so its last tick is seen before it folds.
pub const PLAN_LINGER: Duration = Duration::from_millis(1400);

/// A just-completed step's colour: lit, easing into the success colour.
pub fn step_flash(success: Color, t: f32) -> Color {
    let e = anim::ease_out_cubic(t.clamp(0.0, 1.0));
    theme::mix(Color::Rgb(255, 255, 255), success, 0.35 + 0.65 * e)
}

/// The working bar: a bright crest that sweeps along a short track while a
/// turn runs, trailing off in shaded blocks — `░▒▓█▓▒░`. Returns each cell's
/// glyph and brightness (0..1), derived from elapsed time alone like every
/// other animation here, so it keeps pace however often the screen redraws.
///
/// This one runs for the whole turn: it is the "koda is working" signal the
/// status row exists to give, and it stops the instant the turn ends.
pub fn wave(cells: usize, elapsed: Duration, fine: bool) -> Vec<(&'static str, f32)> {
    const PERIOD: f32 = 1.6;
    const HALF: f32 = 3.5;
    let ramp: [&str; 5] = if fine {
        [" ", "░", "▒", "▓", "█"]
    } else {
        [" ", ".", ":", "=", "#"]
    };
    let span = cells as f32 + 2.0 * HALF;
    let phase = (elapsed.as_secs_f32() % PERIOD) / PERIOD;
    let head = anim::ease_in_out_sine(phase) * span - HALF;
    (0..cells)
        .map(|i| {
            let d = ((i as f32) - head).abs();
            let k = (1.0 - d / HALF).clamp(0.0, 1.0);
            // A faint floor so the track reads as a track between sweeps.
            let k = k.max(0.12);
            let idx = ((k * 4.0).round() as usize).clamp(1, 4);
            (ramp[idx], k)
        })
        .collect()
}

/// Which characters of `candidate` a fuzzy `pattern` lands on, as char
/// indices — the alignment `fuzzy::score` actually ranked, so the letters lit
/// in a list are the ones that earned its place. Empty when it does not match.
/// The fzf convention, and the fastest way to trust a filter.
pub fn match_positions(candidate: &str, pattern: &str) -> Vec<usize> {
    crate::fuzzy::positions(candidate, pattern)
        .map(|(_, p)| p)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::Rgb(200, 40, 40);
    const GREEN: Color = Color::Rgb(40, 200, 40);
    const GREY: Color = Color::Rgb(90, 90, 90);

    /// Rule 4: every effect lands on the colour the static frame would draw.
    #[test]
    fn every_effect_settles_on_the_static_colour() {
        assert_eq!(mode_shift(RED, GREEN, 1.0), (GREEN, GREEN));
        assert_eq!(step_flash(GREEN, 1.0), GREEN);
        assert_eq!(toast_colour(GREEN, GREY, 0.5), GREEN, "holds at its tone");
    }

    #[test]
    fn a_mode_shift_starts_from_the_old_colour() {
        let (edge, title) = mode_shift(RED, GREEN, 0.0);
        assert_eq!(edge, RED);
        assert_ne!(title, GREEN, "the new title arrives lit");
    }

    /// Named colours cannot be blended; the shift must still end on the new
    /// mode's colour rather than sticking on the old one.
    #[test]
    fn a_mode_shift_between_named_colours_still_lands() {
        let (edge, _) = mode_shift(Color::Yellow, Color::Green, 1.0);
        assert_eq!(edge, Color::Green);
    }

    /// Rule 2, asserted rather than trusted.
    #[test]
    fn every_effect_is_shorter_than_wcag_five_seconds() {
        for d in [MODE_SHIFT, TOAST_LIFE, GAUGE_EASE, STEP_FLASH, PLAN_LINGER] {
            assert!(d < Duration::from_secs(5), "{d:?}");
        }
    }

    #[test]
    fn the_wave_is_a_fixed_width_track_with_one_crest() {
        for ms in [0u64, 200, 800, 1500] {
            let w = wave(10, Duration::from_millis(ms), true);
            assert_eq!(w.len(), 10, "never changes width");
            assert!(w
                .iter()
                .all(|(g, _)| unicode_width::UnicodeWidthStr::width(*g) == 1));
        }
        let mid = wave(10, Duration::from_millis(800), true);
        assert!(
            mid.iter().any(|(g, _)| *g == "█"),
            "a crest mid-sweep: {mid:?}"
        );
        assert!(wave(10, Duration::ZERO, false)
            .iter()
            .all(|(g, _)| g.is_ascii()));
    }

    #[test]
    fn a_pulse_ends() {
        let p = Pulse::new(Duration::from_millis(100));
        assert_eq!(p.t_at(Duration::ZERO), Some(0.0));
        assert!(p.t_at(Duration::from_millis(50)).is_some());
        assert_eq!(p.t_at(Duration::from_millis(100)), None);
        assert!(
            Pulse::new(Duration::ZERO).t().is_none(),
            "zero life never runs"
        );
    }

    #[test]
    fn a_tween_without_motion_jumps_straight_to_the_value() {
        let mut g = Tween::at(0.2);
        g.set(0.7, false);
        assert_eq!(g.value(), 0.7);
        assert!(!g.moving());
    }

    #[test]
    fn a_tween_eases_from_where_it_was() {
        let mut g = Tween::at(0.2);
        g.set(0.7, true);
        assert!(g.moving());
        let v = g.value();
        assert!((0.2..=0.7).contains(&v), "{v}");
    }

    #[test]
    fn match_positions_follow_the_subsequence() {
        assert_eq!(match_positions("/model", "mdl"), vec![1, 3, 5]);
        assert_eq!(match_positions("/Reason", "/rea"), vec![0, 1, 2, 3]);
        assert!(match_positions("/help", "xyz").is_empty());
        assert!(match_positions("/help", "").is_empty());
    }

    /// Every highlighted position must agree with the scorer: a row the list
    /// shows as matching has to be a row `fuzzy::score` accepts.
    #[test]
    fn match_positions_agree_with_the_scorer() {
        for (cand, pat) in [("/compact", "cpt"), ("src/tui.rs", "tui"), ("/help", "zz")] {
            assert_eq!(
                !match_positions(cand, pat).is_empty(),
                crate::fuzzy::score(cand, pat).is_some(),
                "{cand} / {pat}"
            );
        }
    }
    #[test]
    fn a_pulse_runs_linearly_then_stops() {
        let p = Pulse::new(Duration::from_millis(1000));
        assert_eq!(p.t_at(Duration::ZERO), Some(0.0));
        let half = p.t_at(Duration::from_millis(500)).unwrap();
        assert!((half - 0.5).abs() < 1e-3, "{half}");
        assert_eq!(p.t_at(Duration::from_millis(1000)), None);
        assert_eq!(
            Pulse::new(Duration::ZERO).t(),
            None,
            "a zero life is over at once"
        );
    }

    #[test]
    fn a_toast_arrives_lit_holds_and_fades() {
        let (base, muted) = (Color::Rgb(0, 200, 0), Color::Rgb(90, 90, 90));
        assert_ne!(toast_colour(base, muted, 0.0), base, "arrives lit");
        assert_eq!(toast_colour(base, muted, 0.5), base, "holds its tone");
        let late = toast_colour(base, muted, 0.99);
        assert_ne!(late, base, "fades before it leaves");
        let t = Toast::new("saved", Tone::Done);
        assert!(t.live());
        assert_eq!(
            t.colour(base, muted, false),
            Some(base),
            "no motion: plain tone"
        );
    }

    #[test]
    fn a_tween_without_motion_jumps_and_with_motion_eases() {
        let mut g = Tween::at(0.3);
        assert_eq!(g.value(), 0.3);
        assert!(!g.moving());
        g.set(0.7, false);
        assert_eq!(g.value(), 0.7);
        assert!(!g.moving());
        g.set(0.9, true);
        assert!(g.moving());
        let v = g.value();
        assert!((0.7..=0.9).contains(&v), "{v}");
        // Setting the same target again does not restart the ease.
        let before = g.pulse.map(|p| p.started);
        g.set(0.9, true);
        assert_eq!(g.pulse.map(|p| p.started), before);
    }

    #[test]
    fn a_step_flash_ends_on_the_success_colour() {
        let ok = Color::Rgb(40, 180, 90);
        assert_eq!(step_flash(ok, 1.0), ok);
        assert_ne!(step_flash(ok, 0.0), ok, "starts lit");
        assert_eq!(step_flash(ok, 7.0), ok, "t is clamped");
        let (edge, title) = mode_shift(Color::Rgb(1, 2, 3), ok, 2.0);
        assert_eq!((edge, title), (ok, ok), "t is clamped");
    }

    #[test]
    fn the_wave_is_the_same_width_at_every_moment() {
        for ms in [0u64, 137, 800, 1599, 5000] {
            for fine in [true, false] {
                let w = wave(12, Duration::from_millis(ms), fine);
                assert_eq!(w.len(), 12);
                assert!(w.iter().all(|(_, b)| (0.0..=1.0).contains(b)), "{w:?}");
            }
        }
        assert!(wave(0, Duration::ZERO, true).is_empty());
    }
}
