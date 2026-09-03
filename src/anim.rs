//! Animation: a clock that only ticks when something is moving, plus the easing
//! and colour maths the animated primitives need.
//!
//! Three constraints shaped this module, all of them learned the hard way by
//! people who shipped animated terminal UIs:
//!
//! 1. **A terminal has no compositor.** Every frame is a full repaint driven by
//!    stdout writes. Push frames too fast and terminals flicker, throttle, or
//!    reveal a half-cleared screen. GitHub's Copilot CLI team landed on ~13fps
//!    (75ms) as the point where flicker starts in some terminals; we treat that
//!    as the floor for a full-screen change and allow faster frames only for
//!    single-cell swaps under synchronized output.
//! 2. **Motion must be opt-out.** Rapid character changes are noise to a screen
//!    reader, and a coding tool is not the place to insist on decoration.
//! 3. **Idle must cost nothing.** koda relayouts a 4000-block transcript in
//!    ~8.6µs precisely because nothing wakes it up. A fixed-interval ticker
//!    throws that away, so the clock sleeps forever until something arms it.

use std::time::Duration;

use tokio::time::{sleep_until, Instant as TokioInstant, Sleep};

/// How motion behaves, and why it was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Everything moves: spinner sweep, text reveal, gauges.
    Full,
    /// State still updates, but nothing moves for its own sake: no text reveal,
    /// the spinner becomes a single glyph whose label updates about once a second.
    Reduced,
    /// No periodic repaints at all.
    Off,
}

impl Motion {
    /// Resolve motion from config and environment, honouring the conventions
    /// that already exist rather than inventing a new one.
    ///
    /// Environment wins over config because it is how a user expresses a
    /// standing accessibility preference across every tool they run.
    pub fn resolve(configured: bool, tty: bool) -> Self {
        // A pipe or a file cannot animate, and writing frames into one would
        // corrupt the output.
        if !tty {
            return Motion::Off;
        }
        for key in ["KODA_REDUCED_MOTION", "NO_MOTION", "REDUCED_MOTION"] {
            if std::env::var_os(key).is_some_and(|v| !v.is_empty() && v != "0") {
                return Motion::Reduced;
            }
        }
        if std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
            return Motion::Off;
        }
        if !configured {
            return Motion::Reduced;
        }
        Motion::Full
    }

    pub fn animates(self) -> bool {
        matches!(self, Motion::Full)
    }
}

/// Terminals that mishandle synchronized output, where we stay conservative.
pub fn sync_trustworthy() -> bool {
    if std::env::var("TERM")
        .map(|t| t.starts_with("screen"))
        .unwrap_or(false)
    {
        return false;
    }
    // Apple's Terminal.app ignores DEC 2026 and tears instead.
    !std::env::var("TERM_PROGRAM")
        .map(|p| p == "Apple_Terminal")
        .unwrap_or(false)
}

/// The gap between animation frames.
///
/// With synchronized output the terminal presents each frame atomically, so a
/// faster cadence is safe. Without it we stay at the researched ~13fps ceiling.
pub fn frame_budget(sync: bool) -> Duration {
    if sync && sync_trustworthy() {
        Duration::from_millis(33) // ~30fps, atomic presentation
    } else {
        Duration::from_millis(75) // ~13fps, the flicker floor
    }
}

/// A clock that is asleep unless something has asked for frames.
///
/// Arming is reference-counted so two independent animations (a spinner and a
/// text reveal, say) can overlap without one disarming the other.
pub struct Clock {
    next: Option<TokioInstant>,
    armed: u32,
    budget: Duration,
    motion: Motion,
}

impl Clock {
    pub fn new(budget: Duration, motion: Motion) -> Self {
        Self {
            next: None,
            armed: 0,
            budget,
            motion,
        }
    }

    pub fn animating(&self) -> bool {
        self.armed > 0
    }

    /// Match the clock to whether anything currently wants frames.
    ///
    /// Deriving the armed state from app state each iteration is deliberate:
    /// paired arm/disarm calls leak a claim the moment any code path returns
    /// early, and a leaked claim means koda spins forever.
    pub fn sync(&mut self, want: bool) {
        if self.motion == Motion::Off {
            self.armed = 0;
            self.next = None;
            return;
        }
        match (want, self.armed) {
            (true, 0) => {
                self.armed = 1;
                self.next = Some(TokioInstant::now() + self.budget);
            }
            (false, _) => {
                self.armed = 0;
                self.next = None;
            }
            _ => {}
        }
    }

    /// The future to select on. When nothing is armed this resolves so far in
    /// the future that the process is genuinely idle; pairing it with an
    /// `if animating()` guard in `select!` means it is never even polled.
    pub fn tick(&self) -> Sleep {
        match self.next {
            Some(at) => sleep_until(at),
            None => sleep_until(TokioInstant::now() + Duration::from_secs(86_400)),
        }
    }

    /// Schedule the next frame. Advancing from the previous deadline keeps the
    /// cadence even; if we fell behind (a slow draw, a suspended process) we
    /// resync to now rather than firing a burst of catch-up frames.
    pub fn schedule(&mut self) {
        if self.armed == 0 {
            return;
        }
        let now = TokioInstant::now();
        let mut next = self.next.unwrap_or(now) + self.budget;
        if next <= now {
            next = now + self.budget;
        }
        self.next = Some(next);
    }
}

// ---------------------------------------------------------------------------
// Easing
// ---------------------------------------------------------------------------

/// Decelerating: fast to start, settling at the end. The default for anything
/// arriving on screen, because it reads as responsive.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Symmetric acceleration and deceleration, for things that travel across the
/// screen and back.
pub fn ease_in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    -(std::f32::consts::PI * t).cos() / 2.0 + 0.5
}

// ---------------------------------------------------------------------------
// The thinking sweep
// ---------------------------------------------------------------------------

/// Per-frame dwell times for the thinking sweep, in milliseconds.
///
/// Claude Code's spinner is not a fixed-interval cycle: the first and last
/// glyphs hold noticeably longer, which turns a mechanical loop into something
/// that reads as breathing. These durations reproduce that shape, and none is
/// below 70ms so even the fastest frame stays under the flicker ceiling.
const SWEEP_DWELL: [u64; 10] = [230, 150, 90, 70, 90, 230, 90, 70, 90, 150];
/// Which glyph each step of the sweep shows: out and back, so the motion
/// reverses rather than snapping from the last frame to the first.
const SWEEP_INDEX: [usize; 10] = [0, 1, 2, 3, 4, 5, 4, 3, 2, 1];

/// Which sweep glyph to show for a given elapsed time.
pub fn sweep(elapsed: Duration) -> usize {
    let cycle: u64 = SWEEP_DWELL.iter().sum();
    let mut t = (elapsed.as_millis() as u64) % cycle.max(1);
    for (i, d) in SWEEP_DWELL.iter().enumerate() {
        if t < *d {
            return SWEEP_INDEX[i];
        }
        t -= *d;
    }
    0
}

// ---------------------------------------------------------------------------
// Streaming reveal
// ---------------------------------------------------------------------------

/// Characters per second for the streaming text reveal.
///
/// Tuned so the reveal reads as text *flowing in* rather than a machine-gun
/// dump: at a typical reading-adjacent pace the eye tracks the leading edge
/// instead of the whole paragraph appearing at once. Text that arrives in one
/// large chunk still animates in; the catch-up and panic-snap below stop this
/// slow base rate from ever making a long response feel like it is lagging.
const REVEAL_CPS: f32 = 50.0;
/// If the model gets far ahead, catch up proportionally so the reveal can never
/// fall permanently behind on a long response.
const CATCHUP: usize = 6;
/// Above this backlog the reveal stops animating and snaps to the end. A slow
/// base rate is pleasant for a sentence and maddening for a 20 KB paste or a
/// resumed transcript; past this threshold the "flowing in" effect has no value
/// and the only kind thing to do is show the text immediately.
const REVEAL_PANIC: usize = 2000;

/// How far to advance a reveal cursor over `dt`, given how much is waiting.
pub fn reveal_step(dt: Duration, revealed: usize, total: usize) -> usize {
    if revealed >= total {
        return total;
    }
    let backlog = total - revealed;
    // Panic-snap: an enormous backlog is never worth animating through.
    if backlog > REVEAL_PANIC {
        return total;
    }
    let base = (REVEAL_CPS * dt.as_secs_f32()).ceil() as usize;
    let step = base.max(2).max(backlog / CATCHUP);
    (revealed + step).min(total)
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Blend two RGB colours. Interpolating in squared space approximates working
/// in linear light, which stops mid-blends of saturated colours going muddy —
/// the usual giveaway of a naive gradient.
pub fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| {
        let (x, y) = (x as f32 / 255.0, y as f32 / 255.0);
        let v = ((x * x) * (1.0 - t) + (y * y) * t).sqrt();
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Per-character brightness for a highlight sweeping across `len` cells.
///
/// A shimmer says "still working" without moving any text, which is why it suits
/// a status label: a spinner tells you something is happening, a shimmer tells
/// you it is still happening without competing for attention. The band uses an
/// eased falloff so its edges are soft rather than a hard three-cell block.
pub fn shimmer(len: usize, elapsed: Duration, period: Duration) -> Vec<f32> {
    if len == 0 {
        return Vec::new();
    }
    let t = if period.is_zero() {
        0.0
    } else {
        (elapsed.as_secs_f32() / period.as_secs_f32()).fract()
    };
    // Travel a little beyond both ends so the highlight enters and leaves
    // rather than appearing and vanishing at the edges.
    let span = len as f32 + 8.0;
    let head = ease_in_out_sine(t) * span - 4.0;
    const WIDTH: f32 = 4.0;
    (0..len)
        .map(|i| {
            let d = (i as f32 - head).abs();
            if d >= WIDTH {
                0.0
            } else {
                // Eased falloff: brightest at the centre of the band.
                ease_out_cubic(1.0 - d / WIDTH)
            }
        })
        .collect()
}

/// A horizontal bar with eighth-cell precision.
///
/// Sub-cell resolution is what separates a gauge that glides from one that
/// jumps a whole character at a time. Returns a constant display width so a
/// status bar containing one does not jitter as the value changes.
pub fn eighth_bar(fraction: f32, cells: usize, fine: bool) -> String {
    const EIGHTHS: [&str; 8] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
    let f = fraction.clamp(0.0, 1.0);
    if cells == 0 {
        return String::new();
    }
    if !fine {
        // Without eighth blocks there is no sub-cell precision to have, and the
        // shading glyph is not ascii either — so this path stays plain.
        let full = ((f * cells as f32).round() as usize).min(cells);
        return "#".repeat(full) + &"-".repeat(cells - full);
    }
    let total_eighths = (f * (cells * 8) as f32).round() as usize;
    let full = total_eighths / 8;
    let rest = total_eighths % 8;
    let mut out = "█".repeat(full.min(cells));
    let mut used = full.min(cells);
    if used < cells && rest > 0 {
        out.push_str(EIGHTHS[rest - 1]);
        used += 1;
    }
    out.push_str(&"░".repeat(cells - used));
    out
}

/// Elapsed time as a short human string, for a live clock.
pub fn short_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_asleep_until_something_wants_frames() {
        let mut c = Clock::new(Duration::from_millis(75), Motion::Full);
        assert!(!c.animating(), "a fresh clock must not wake the loop");
        c.sync(true);
        assert!(c.animating());
        c.sync(false);
        assert!(!c.animating(), "going idle must return it to sleep");
    }

    #[test]
    fn sync_follows_state_and_cannot_leak() {
        let mut c = Clock::new(Duration::from_millis(75), Motion::Full);
        c.sync(true);
        assert!(c.animating());
        // Repeated syncs must not stack claims.
        c.sync(true);
        c.sync(true);
        c.sync(false);
        assert!(!c.animating(), "sync(false) must fully stop the clock");
    }

    #[test]
    fn motion_off_never_arms() {
        let mut c = Clock::new(Duration::from_millis(75), Motion::Off);
        c.sync(true);
        assert!(!c.animating(), "motion off must stay completely idle");
    }

    #[test]
    fn schedule_resyncs_instead_of_bursting() {
        let mut c = Clock::new(Duration::from_millis(10), Motion::Full);
        c.sync(true);
        // Pretend we stalled well past several deadlines.
        c.next = Some(TokioInstant::now() - Duration::from_secs(5));
        c.schedule();
        let next = c.next.expect("still armed");
        assert!(
            next > TokioInstant::now(),
            "a stalled clock must resync forward, not fire a burst of catch-up frames"
        );
    }

    #[test]
    fn sweep_holds_longer_at_its_endpoints() {
        // Sample the whole cycle and count how long each glyph is shown.
        let cycle: u64 = SWEEP_DWELL.iter().sum();
        let mut seen = [0usize; 6];
        for ms in 0..cycle {
            seen[sweep(Duration::from_millis(ms))] += 1;
        }
        assert!(
            seen[0] > seen[3] && seen[5] > seen[3],
            "endpoints should dwell longer than the middle: {seen:?}"
        );
        assert!(
            seen.iter().all(|n| *n > 0),
            "every glyph should appear: {seen:?}"
        );
    }

    #[test]
    fn sweep_never_shows_a_frame_faster_than_the_flicker_floor() {
        assert!(
            SWEEP_DWELL.iter().all(|d| *d >= 70),
            "a frame under ~70ms risks flicker in some terminals"
        );
    }

    #[test]
    fn easings_span_zero_to_one() {
        for f in [ease_out_cubic as fn(f32) -> f32, ease_in_out_sine] {
            assert!(f(0.0).abs() < 1e-4, "should start at 0");
            assert!((f(1.0) - 1.0).abs() < 1e-4, "should end at 1");
        }
    }

    #[test]
    fn ease_out_is_front_loaded() {
        // Half the time should already be most of the distance.
        assert!(
            ease_out_cubic(0.5) > 0.8,
            "ease-out should cover most ground early: {}",
            ease_out_cubic(0.5)
        );
    }

    #[test]
    fn reveal_catches_up_when_far_behind() {
        let dt = Duration::from_millis(75);
        let small = reveal_step(dt, 0, 30);
        // Stay under REVEAL_PANIC so we exercise the proportional catch-up, not
        // the snap.
        let big = reveal_step(dt, 0, 1_800);
        assert!(
            big > small,
            "a large backlog must advance faster or the reveal never finishes"
        );
        assert_eq!(reveal_step(dt, 40, 40), 40, "never overshoot the total");
    }

    #[test]
    fn reveal_snaps_past_the_panic_threshold() {
        // A huge backlog (a paste, a resumed transcript) is not worth animating:
        // it should jump straight to the end rather than crawl.
        let dt = Duration::from_millis(75);
        assert_eq!(
            reveal_step(dt, 0, 100_000),
            100_000,
            "an enormous backlog must snap to the total"
        );
        // Just over the threshold snaps; just under still animates in steps.
        assert_eq!(reveal_step(dt, 0, REVEAL_PANIC + 1), REVEAL_PANIC + 1);
        assert!(reveal_step(dt, 0, REVEAL_PANIC) < REVEAL_PANIC);
    }

    #[test]
    fn reveal_always_makes_progress() {
        // Even a zero-length frame must advance, or a slow terminal stalls it.
        assert!(reveal_step(Duration::ZERO, 0, 10) >= 2);
    }

    #[test]
    fn shimmer_band_travels_and_stays_bounded() {
        let p = Duration::from_millis(1000);
        let a = shimmer(20, Duration::from_millis(100), p);
        let b = shimmer(20, Duration::from_millis(500), p);
        assert_eq!(a.len(), 20);
        assert!(
            a.iter().all(|v| (0.0..=1.0).contains(v)),
            "brightness must stay in range"
        );
        let peak = |v: &Vec<f32>| {
            v.iter()
                .enumerate()
                .max_by(|x, y| x.1.partial_cmp(y.1).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };
        assert_ne!(peak(&a), peak(&b), "the band should move over time");
    }

    #[test]
    fn shimmer_is_soft_edged() {
        let v = shimmer(30, Duration::from_millis(500), Duration::from_millis(1000));
        let lit: Vec<f32> = v.iter().copied().filter(|x| *x > 0.0).collect();
        assert!(lit.len() > 2, "band should span several cells: {lit:?}");
        assert!(
            lit.iter().any(|x| *x < 0.9),
            "edges should be partial, not a hard block: {lit:?}"
        );
    }

    #[test]
    fn eighth_bar_is_constant_width() {
        let widths: Vec<usize> = (0..=100)
            .map(|p| eighth_bar(p as f32 / 100.0, 8, true).chars().count())
            .collect();
        assert!(
            widths.iter().all(|w| *w == 8),
            "a jittering gauge shifts everything beside it: {widths:?}"
        );
    }

    #[test]
    fn eighth_bar_uses_partial_cells() {
        // Half of one cell should be a partial block, not nothing and not full.
        let bar = eighth_bar(0.5, 1, true);
        assert!(
            bar != "█" && bar != "░",
            "expected a partial block, got {bar:?}"
        );
    }

    #[test]
    fn coarse_bar_is_pure_ascii() {
        let bar = eighth_bar(0.5, 8, false);
        assert!(
            bar.is_ascii(),
            "ascii mode must not emit box-drawing or shading glyphs: {bar}"
        );
        assert_eq!(bar.chars().count(), 8);
    }

    #[test]
    fn lerp_endpoints_are_exact() {
        let a = (10, 20, 30);
        let b = (200, 150, 100);
        assert_eq!(lerp_rgb(a, b, 0.0), a);
        assert_eq!(lerp_rgb(a, b, 1.0), b);
    }

    #[test]
    fn lerp_midpoint_stays_bright() {
        // Naive averaging in gamma space darkens a blend of two saturated
        // colours; squared space keeps the midpoint above it.
        let (r, _, _) = lerp_rgb((255, 0, 0), (0, 0, 255), 0.5);
        assert!(r > 127, "midpoint should not go muddy, got r={r}");
    }

    #[test]
    fn non_tty_disables_motion_entirely() {
        assert_eq!(Motion::resolve(true, false), Motion::Off);
    }

    #[test]
    fn config_off_still_updates_state() {
        assert_eq!(Motion::resolve(false, true), Motion::Reduced);
    }

    #[test]
    fn frame_budget_respects_the_flicker_floor() {
        assert!(frame_budget(false) >= Duration::from_millis(75));
        assert!(frame_budget(true) <= frame_budget(false));
    }
}
