# Spec: Animation Engine for koda (Rust + ratatui 0.29)

Status: draft · Scope: how to implement animation correctly in Rust + ratatui 0.29 for koda's
tokio-driven TUI. This spec is implementation-level; every Rust snippet is written to compile
against `ratatui = "0.29"`, `crossterm` (re-exported by ratatui), and `tokio 1`.

## 0. Context: what koda already does right

`src/tui.rs::run` already establishes most of the correct skeleton, and this spec builds on it
rather than replacing it:

- A single `dirty: bool` gates `term.draw`. Frames are only presented when something changed.
- `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` wrap each draw (DEC 2026, see §7).
- Terminal events arrive on a dedicated OS thread → `key_rx`; agent events on `ev_rx`.
- A `tokio::time::interval(80ms)` ticker only marks `dirty` when `app.busy`.
- After the `select!`, a coalescing drain loop merges token/keystroke bursts into one redraw.
- `src/view.rs` computes a per-block `signature(...)` hash, so unchanged transcript blocks are
  not re-laid-out. This is the "separate static from dynamic regions" constraint already met:
  the 4000-block transcript hashes to identical signatures each idle frame, which is why the
  idle frame is 8.6µs.

The gaps this spec closes:

1. The ticker is a **fixed 80ms wall-clock interval** whether or not anything animates. It wakes
   the runtime 12.5×/s even when idle-but-not-busy edge cases exist, and it cannot express
   per-animation durations or easing. We replace it with a demand-driven **animation clock**.
2. There is no easing / interpolation layer; `spinner` is an integer index.
3. No sub-cell precision helpers (eighth blocks, braille).
4. Colour interpolation for shimmer/gradient is not implemented and must degrade cleanly.

The non-negotiable constraint: **the 8.6µs idle frame must not regress.** Every mechanism below
is designed so that when no animation is registered, the code path is byte-identical to today's
idle path — the clock sleeps forever, `dirty` stays false, and `signature()` short-circuits layout.

---

## 1. Animation clock architecture (wake only when animating, else fully idle)

### 1.1 Principle

Follow the ratatui maintainers' guidance from discussion #579 (joshka): *treat time as the input
to the render, not the trigger.* Instead of "sleep N ms then advance one step", ask "given the
current `Instant`, what does every active animation look like now?" This decouples animation
smoothness from event cadence and lets one clock serve many animations.

The clock has exactly two states:

- **Idle**: no active animations. The timer future is `sleep_until(far future)` — it never fires.
  The `select!` blocks purely on `key_rx` / `ev_rx`, identical to koda today with the interval
  removed. Zero wakeups, zero CPU, idle frame unchanged.
- **Animating**: ≥1 animation registered. The timer is `sleep_until(now + frame_budget)`. Each
  wake advances all animations by real elapsed time, sets `dirty`, and reschedules only if
  animations remain.

### 1.2 The `AnimationClock`

```rust
use std::time::{Duration, Instant};
use tokio::time::{sleep_until, Instant as TokioInstant, Sleep};

/// Minimum time between animated frames. See §2 for the 75ms discussion; the
/// default target is 16ms (60fps ceiling) but the *scheduler* never wakes faster
/// than the terminal can present, and animations that need less are throttled.
const FRAME_BUDGET: Duration = Duration::from_millis(33); // ~30fps default; see §2

pub struct AnimationClock {
    /// Deadline for the next animated frame, or None when fully idle.
    next: Option<TokioInstant>,
    /// Number of live animations. When this hits 0 we go idle.
    active: u32,
    frame_budget: Duration,
}

impl AnimationClock {
    pub fn new(frame_budget: Duration) -> Self {
        Self { next: None, active: 0, frame_budget }
    }

    /// Called when an animation starts. Idempotent-friendly: increments a refcount.
    pub fn arm(&mut self) {
        self.active += 1;
        if self.next.is_none() {
            self.next = Some(TokioInstant::now() + self.frame_budget);
        }
    }

    /// Called when an animation completes. When the last one ends, the clock
    /// stops scheduling wakeups and the select! goes back to pure event-wait.
    pub fn disarm(&mut self) {
        self.active = self.active.saturating_sub(1);
        if self.active == 0 {
            self.next = None;
        }
    }

    pub fn is_animating(&self) -> bool {
        self.active > 0
    }

    /// A future that resolves at the next frame deadline, or *never* when idle.
    /// `select!` polls this; when idle it is a pending future that consumes no CPU.
    pub fn tick(&self) -> Sleep {
        match self.next {
            // Real deadline while animating.
            Some(at) => sleep_until(at),
            // Idle: sleep for effectively forever. The future stays Pending and
            // is dropped/rebuilt cheaply on the next select! iteration. No wakeup.
            None => sleep_until(TokioInstant::now() + Duration::from_secs(86_400)),
        }
    }

    /// After a frame is drawn, advance the deadline if still animating.
    pub fn schedule_next(&mut self) {
        if self.active > 0 {
            // Anchor to previous deadline (not `now`) to avoid drift; clamp so a
            // stalled runtime doesn't try to "catch up" with a burst of frames.
            let base = self.next.unwrap_or_else(TokioInstant::now);
            let mut nxt = base + self.frame_budget;
            let now = TokioInstant::now();
            if nxt <= now {
                nxt = now + self.frame_budget; // dropped frames: resync, don't spiral
            }
            self.next = Some(nxt);
        }
    }
}
```

### 1.3 Integration into koda's `run` loop

This replaces the `ticker`/`select!` block in `src/tui.rs`. The diff is small and preserves the
existing coalescing drain and `dirty` semantics.

```rust
let mut clock = AnimationClock::new(FRAME_BUDGET);
let mut dirty = true;

let result = loop {
    if dirty {
        let sync = app.sync_output;
        if sync { let _ = execute!(io::stdout(), BeginSynchronizedUpdate); }
        let r = term.draw(|f| draw(f, &mut app));
        if sync { let _ = execute!(io::stdout(), EndSynchronizedUpdate); }
        if let Err(e) = r { break Err(anyhow::Error::from(e)); }
        dirty = false;
    }
    if app.quit { break Ok(()); }

    tokio::select! {
        // biased so input always wins a tie against the animation tick, keeping
        // the UI responsive under heavy animation load.
        biased;

        maybe = key_rx.recv() => match maybe {
            Some(ev) => dirty |= handle_term_event(&mut app, ev),
            None => break Ok(()),
        },
        maybe = ev_rx.recv() => match maybe {
            Some(ev) => { app.on_event(&mut clock, ev); dirty = true; }
            None => break Ok(()),
        },
        // Idle: this branch is a 24h sleep — it never fires, costs nothing.
        // Animating: fires on the frame deadline.
        _ = clock.tick(), if clock.is_animating() => {
            let now = Instant::now();
            // Advance every animation by real elapsed time and let each decide
            // whether it finished (calling clock.disarm()).
            dirty |= app.advance_animations(now, &mut clock);
            clock.schedule_next();
        }
    }

    // Existing coalescing drain — unchanged.
    loop {
        let mut progressed = false;
        while let Ok(ev) = key_rx.try_recv() { progressed |= handle_term_event(&mut app, ev); }
        while let Ok(ev) = ev_rx.try_recv() { app.on_event(&mut clock, ev); progressed = true; }
        if !progressed { break; }
        dirty = true;
    }
};
```

Key points:

- The `if clock.is_animating()` guard on the `select!` branch means that when nothing animates,
  tokio does not even poll the sleep — the loop is a pure two-channel wait, exactly like koda
  with the interval deleted. **This is what protects the 8.6µs idle frame.**
- `biased;` makes input strictly higher priority than the animation tick.
- `app.busy` (spinner) is now expressed as an animation that calls `clock.arm()` when a turn
  starts and `clock.disarm()` when it ends. The old `if app.busy { ... dirty = true }` logic in
  the tick body moves into `advance_animations`.

### 1.4 `advance_animations`

A single registry keyed by a small enum keeps the hot path allocation-free. Each animation stores
its own start `Instant` and duration, so easing and per-frame timing are per-animation (§3).

```rust
pub enum AnimId { Spinner, ShimmerThinking, ToolPulse(u64) /* block id */ }

pub struct Animation {
    id: AnimId,
    start: Instant,
    /// None = indefinite (spinner, shimmer loop); Some = one-shot with a deadline.
    duration: Option<Duration>,
    easing: Easing,
}

impl App {
    /// Returns true if any animation changed visible state (→ redraw needed).
    /// Calls clock.disarm() for each one-shot that just completed.
    pub fn advance_animations(&mut self, now: Instant, clock: &mut AnimationClock) -> bool {
        let mut changed = false;
        self.anims.retain(|a| {
            match a.duration {
                Some(d) if now.duration_since(a.start) >= d => {
                    clock.disarm();
                    changed = true; // final frame at progress = 1.0
                    false           // drop it
                }
                _ => { changed = true; true }
            }
        });
        // Indefinite animations (spinner) advance a phase but never disarm here.
        changed
    }
}
```

The rendering code reads `now` and each animation's `start`/`duration`/`easing` to compute a
progress value (§3) at draw time — nothing is stored per-frame.

---

## 2. Frame-rate guidance

### 2.1 Verdict on the 75ms/13fps ceiling

The prior "~75ms/13fps is the ceiling before flicker" figure is **real but specific**: it is the
point at which *full-screen, non-atomic* redraws start to tear/flicker on terminals **without**
synchronized output (notably macOS Terminal.app). It is not a universal frame cap. The mechanism,
confirmed by the ratatui #579 profiling and multiple synchronized-output bug reports
(spring-shell #1361, ghostty docs, claude-code #55613): tearing happens when the renderer samples
the screen buffer mid-write. The bottleneck is the `write()` syscall to the tty, and its cost
scales with **number of changed cells**, not frame count.

Refined guidance:

| Situation | Safe frame budget | Why |
|---|---|---|
| Small dynamic region (spinner, a few shimmer chars, one progress bar), DEC 2026 on | 16ms (60fps) | Only a handful of cells diff; write is tiny; sync-update makes it atomic. |
| Small dynamic region, no DEC 2026 | 33ms (30fps) | Safe; the diff is small enough that a mid-write sample is rarely visible. |
| Large dynamic region (≥30% of a big terminal changes/frame), no DEC 2026 | 66–75ms (13–15fps) | This is where the 75ms figure applies. Larger writes are more likely to be sampled mid-flight → flicker. |
| Large dynamic region, DEC 2026 on | 33ms | Sync-update makes even large writes atomic; the ceiling relaxes. |

**koda's case**: the dynamic region is tiny (spinner + thinking shimmer + running-tool cards),
and `signature()` guarantees the static 4000-block transcript never re-diffs. So koda is in the
top row: **60fps is safe when sync_output is on, 30fps otherwise.** The `FRAME_BUDGET` default is
set to **33ms (30fps)** as a conservative cross-terminal choice, with an opt-in to 16ms when
`sync_output` is enabled and true-color is detected.

```rust
fn frame_budget(sync_output: bool) -> Duration {
    if sync_output { Duration::from_millis(16) } else { Duration::from_millis(33) }
}
```

### 2.2 Why per-frame durations beat a fixed interval (prior constraint, validated)

The fixed 80ms interval couples *every* animation to one cadence and cannot express "this fade
lasts 250ms with ease-out". By storing `start`/`duration` per animation and sampling progress at
draw time (§3), a 250ms fade and an indefinite spinner coexist under one clock; the clock only
governs *how often we sample*, never *how far each animation has travelled*. Under load, dropped
frames degrade smoothly: a fade that should take 250ms still ends at 250ms of wall time even if
only 4 frames were drawn, because progress is `elapsed / duration`, not `frames × step`.

### 2.3 Never render faster than the terminal presents

`schedule_next` anchors to the prior deadline and resyncs (rather than bursting) after a stall, so
a slow `write()` cannot cause a catch-up spiral. Combined with the `dirty` gate, koda never issues
two draws for one logical change.

---

## 3. Easing

f32 easings over normalised progress `t ∈ [0,1]`. These are `const fn`-friendly, branchless where
possible, allocation-free, and depend on nothing but `libm`/`std` (`sin`, `cos` are in `std` for
`f32`). No crate needed.

```rust
#[derive(Copy, Clone, Debug)]
pub enum Easing { Linear, OutCubic, InOutSine, OutBack }

impl Easing {
    #[inline]
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear    => linear(t),
            Easing::OutCubic  => ease_out_cubic(t),
            Easing::InOutSine => ease_in_out_sine(t),
            Easing::OutBack   => ease_out_back(t),
        }
    }
}

#[inline]
pub fn linear(t: f32) -> f32 { t }

/// Decelerates to the end. Good for entrances (a card sliding in and settling).
#[inline]
pub fn ease_out_cubic(t: f32) -> f32 {
    let f = 1.0 - t;
    1.0 - f * f * f
}

/// Symmetric ease. Good for looping shimmer / pulses — no visible seam at the wrap.
#[inline]
pub fn ease_in_out_sine(t: f32) -> f32 {
    use std::f32::consts::PI;
    -( (PI * t).cos() - 1.0 ) / 2.0
}

/// Overshoots slightly past 1.0 then settles. Good for a "pop" on completion ticks.
/// Note: returns values >1.0 mid-curve; callers that index arrays must clamp the
/// *output* (e.g. positions), not the input.
#[inline]
pub fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    let f = t - 1.0;
    1.0 + C3 * f * f * f + C1 * f * f
}
```

### 3.1 Mapping an elapsed `Duration` to eased progress

```rust
/// Raw linear progress of a one-shot animation, clamped to [0,1].
#[inline]
pub fn progress(start: Instant, duration: Duration, now: Instant) -> f32 {
    if duration.is_zero() { return 1.0; }
    (now.duration_since(start).as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// Eased progress for a one-shot animation.
#[inline]
pub fn eased(start: Instant, duration: Duration, now: Instant, e: Easing) -> f32 {
    e.apply(progress(start, duration, now))
}

/// Looping phase in [0,1) for indefinite animations (spinner, shimmer). `period`
/// is the loop length. Eased *within* the loop with InOutSine for a seamless wrap.
#[inline]
pub fn loop_phase(start: Instant, period: Duration, now: Instant) -> f32 {
    let p = (now.duration_since(start).as_secs_f32() / period.as_secs_f32()).rem_euclid(1.0);
    p
}
```

Usage example (a tool card that fades its border in over 200ms with ease-out):

```rust
let t = eased(anim.start, Duration::from_millis(200), now, Easing::OutCubic);
let border = lerp_rgb(theme.surface_rgb, theme.accent_rgb, t); // §5
```

---

## 4. Sub-cell precision

Terminal cells are ~2:1 tall:wide. Two Unicode families give fractional resolution.

### 4.1 Eighth blocks — horizontal bars, progress, meters (pick this by default)

`U+2588..U+258F` are full→left-eighth block; `U+2589..` fill from the left in 1/8 steps. For a
horizontal bar this gives **8× the width resolution** of whole cells and animates smoothly at low
frame rates because a 1px advance is a visible, non-flickery change.

```rust
/// Eighth blocks filling from the LEFT: index 0 = empty, 8 = full cell.
const LEFT_EIGHTHS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// Render `ratio` (0..1) as a bar `width` cells wide into a String.
pub fn eighth_bar(ratio: f32, width: u16) -> String {
    let ratio = ratio.clamp(0.0, 1.0);
    let total_eighths = (ratio * width as f32 * 8.0).round() as u32;
    let full = (total_eighths / 8) as usize;
    let rem = (total_eighths % 8) as usize;
    let mut s = String::with_capacity(width as usize * 3);
    for _ in 0..full { s.push('█'); }
    if rem > 0 && full < width as usize { s.push_str(LEFT_EIGHTHS[rem]); }
    let drawn = full + usize::from(rem > 0 && full < width as usize);
    for _ in drawn..width as usize { s.push(' '); }
    s
}
```

For vertical bars use `U+2581..U+2588` (`▁▂▃▄▅▆▇█`, fill from bottom) — same idea, indexed by
`(ratio*8).round()`.

### 4.2 Braille — fine dot-matrix detail (sparklines, waveforms, dense spinners)

A braille cell (`U+2800` base) packs a **2×4 dot grid** = 8 independently addressable sub-pixels,
giving the highest spatial resolution the terminal offers. Bit layout per Unicode:

```rust
/// Braille dot bit positions within a 2x4 cell:
///   (col, row) -> bit
///   (0,0)=0x01 (0,1)=0x02 (0,2)=0x04 (1,0)=0x08
///   (1,1)=0x10 (1,2)=0x20 (0,3)=0x40 (1,3)=0x80
const BRAILLE_BITS: [[u8; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

/// Set dot (col in 0..2, row in 0..4) on a braille cell mask.
#[inline]
pub fn braille_set(mask: u8, col: usize, row: usize) -> u8 {
    mask | BRAILLE_BITS[row][col]
}

#[inline]
pub fn braille_char(mask: u8) -> char {
    char::from_u32(0x2800 + mask as u32).unwrap_or('⠀')
}
```

Note: ratatui 0.29 already ships `symbols::braille` and a `Canvas`/`Marker::Braille` for plotting;
prefer those for charts. Hand-roll only for a bespoke spinner.

### 4.3 How to pick

| Need | Use | Reason |
|---|---|---|
| Progress bar, token meter, context gauge | **Eighth blocks** | Solid fill reads clearly at a glance; 8× horizontal res is plenty; robust in every font. |
| Fine spinner, sparkline, waveform, scrubber | **Braille** | 8 sub-dots/cell = smoothest motion; but thin glyphs, weaker in some fonts/low-contrast themes. |
| Coarse spinner, status dot | Single rotating glyph set | Cheapest; one cell diff/frame. |

Rule of thumb: **eighth blocks for magnitude, braille for shape/motion.** Both degrade to a plain
`#`/`*` fill if a NerdFont/Unicode-hostile terminal is detected (koda already has a glyph-fallback
path in `theme::glyphs`).

---

## 5. Colour interpolation (gradient / shimmer) with clean degradation

### 5.1 Perceptually-acceptable RGB lerp without a heavy dependency

Naïve linear lerp in sRGB darkens midpoints of saturated gradients (the classic "muddy purple"
between red and green). A full CIELAB/OKLab conversion is correct but pulls in trig/cbrt and code
weight. The **cheap, good-enough** middle ground used by games and terminals is to interpolate in
**gamma-2.0 (squared) space**: lerp the *squares* of the channels and take the square root back.
This removes almost all of the midpoint darkening for one `mul`+`sqrt` per channel and **no
dependency**.

```rust
pub type Rgb = (u8, u8, u8);

#[inline]
fn srgb_to_lin(c: u8) -> f32 { let f = c as f32 / 255.0; f * f }   // gamma ~2.0 approx
#[inline]
fn lin_to_srgb(f: f32) -> u8 { (f.max(0.0).sqrt() * 255.0).round().clamp(0.0, 255.0) as u8 }

/// Perceptually-acceptable lerp: interpolate in linear-ish (squared) space.
/// t in [0,1]. Cost: 3 sqrt + a few muls. No allocation, no crate.
#[inline]
pub fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| lin_to_srgb(srgb_to_lin(x) * (1.0 - t) + srgb_to_lin(y) * t);
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}
```

If a gradient needs true perceptual uniformity (rare in a coding TUI), OKLab is the upgrade path;
gate it behind a feature flag so it never adds weight to the default build. For koda's shimmer and
accent fades, gamma-2.0 lerp is indistinguishable from OKLab to the eye and far cheaper.

Shimmer band (adapting tui-shimmer's approach: a cosine-windowed highlight sweeping across text):

```rust
/// Intensity of the shimmer highlight at character `i`, sweep centre at `pos`,
/// half-width `hw`. Cosine window → smooth, seamless band. Precompute per-frame.
#[inline]
fn shimmer_intensity(i: isize, pos: isize, hw: isize) -> f32 {
    let d = (i - pos).abs();
    if d > hw { 0.0 } else {
        let x = std::f32::consts::PI * (d as f32 / hw as f32);
        0.5 * (1.0 + x.cos())     // 1.0 at centre → 0.0 at edges
    }
}

/// Per-character shimmer colour: blend base→highlight by the windowed intensity.
pub fn shimmer_fg(base: Rgb, highlight: Rgb, i: isize, pos: isize, hw: isize) -> Rgb {
    lerp_rgb(base, highlight, shimmer_intensity(i, pos, hw))
}
```

Drive `pos` from `loop_phase` (§3): `let pos = (loop_phase(start, period, now) * period_cells) as isize;`

### 5.2 Degrading to 256-colour and 16-colour terminals

Detect capability once, cache it, and choose the `Color` variant at span-build time. This mirrors
tui-shimmer's `supports_true_color()` + fallback and respects `NO_COLOR`.

```rust
use std::sync::OnceLock;
use ratatui::style::{Color, Modifier, Style};

#[derive(Copy, Clone, PartialEq)]
pub enum ColorDepth { TrueColor, Ansi256, Ansi16, NoColor }

pub fn color_depth() -> ColorDepth {
    static CACHE: OnceLock<ColorDepth> = OnceLock::new();
    *CACHE.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() { return ColorDepth::NoColor; }
        let ct = std::env::var("COLORTERM").unwrap_or_default();
        if ct.contains("truecolor") || ct.contains("24bit") { return ColorDepth::TrueColor; }
        let term = std::env::var("TERM").unwrap_or_default();
        if term.contains("256color") { return ColorDepth::Ansi256; }
        if term.is_empty() || term == "dumb" { return ColorDepth::NoColor; }
        ColorDepth::Ansi16
    })
}

/// Quantise an interpolated RGB to the best representable Color for this terminal.
pub fn quantize(rgb: Rgb, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        ColorDepth::Ansi256   => Color::Indexed(rgb_to_xterm256(rgb)),
        ColorDepth::Ansi16    => Color::Indexed(rgb_to_ansi16(rgb)),
        ColorDepth::NoColor   => Color::Reset,
    }
}

/// Map RGB to the xterm 6x6x6 cube (indices 16..231) or greyscale ramp (232..255).
pub fn rgb_to_xterm256((r, g, b): Rgb) -> u8 {
    // Greyscale shortcut when channels are close: the 24-step ramp looks better
    // than the colour cube for near-grey shimmer.
    let max = r.max(g).max(b) as i32;
    let min = r.min(g).min(b) as i32;
    if max - min < 8 {
        let l = ((r as u32 + g as u32 + b as u32) / 3) as u8;
        if l < 8 { return 16; }
        if l > 248 { return 231; }
        return 232 + ((l as u32 - 8) * 24 / 240) as u8;
    }
    let q = |c: u8| -> u32 { // 0..5 with the xterm cube's non-linear steps
        if c < 48 { 0 } else if c < 115 { 1 } else { ((c as u32 - 35) / 40).min(5) }
    };
    (16 + 36 * q(r) + 6 * q(g) + q(b)) as u8
}

/// Map RGB to the 16 ANSI colours by nearest hue+brightness. Cheap and stable.
pub fn rgb_to_ansi16((r, g, b): Rgb) -> u8 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let bright = r.max(g).max(b) > 128;
    let bit = |c: i32| -> u8 { u8::from(c > 96) };
    let base = bit(r) | (bit(g) << 1) | (bit(b) << 2); // 0..7 (R=1,G=2,B=4)
    if bright { 8 + base } else { base }
}
```

The critical accessibility rule (prior constraint): **animate semantic roles, not literal RGB.**
The animation code fades between `theme.surface` and `theme.accent` *as roles*; only at the final
`quantize` step does it become a concrete `Color`. On a 16-colour terminal the fade collapses to at
most two steps, which is correct and legible, whereas a literal RGB ramp would smear into
indistinguishable indices. koda's `theme.rs` already stores roles — expose their RGB so the
animation layer can interpolate, then re-quantise.

### 5.3 Reduced-motion / opt-out

Motion must be opt-out. A single flag short-circuits every animation to its resting state:

```rust
impl App {
    #[inline]
    fn motion_enabled(&self) -> bool {
        // false when user set reduce_motion, or NO_COLOR-style env, or a TTY that
        // reported no sync support AND a large dynamic region (flicker risk).
        self.cfg.reduce_motion == false
    }
}
```

When motion is disabled: `arm()` is never called (clock stays idle → no wakeups → idle frame
protected), shimmer renders at its base colour, bars snap to final ratio, spinner becomes a static
glyph. This is both the accessibility path and the safest performance path.

---

## 6. Dependencies: depend vs hand-roll (binary budget 6.1MB→ keep tight)

Current release binary: **6.4MB** (`target/release/koda`, `strip=true`, `lto="thin"`). Every
addition must earn its bytes.

| Candidate | Verdict | Justification |
|---|---|---|
| **tachyonfx** (`ratatui/tachyonfx`) | **Do not depend (default).** Mine it for design. | 50+ effects, spatial patterns, DSL — powerful but far more than koda needs and it pulls a DSL compiler + large effect table. Its architecture *validates* this spec: stateful effects created once and `process(delta, buf, area)` every frame, `EffectTimer::from_ms(ms, Interpolation)` — identical to our clock+easing model. Adopt the *pattern*, not the crate. Reconsider only if koda later wants transitions/dissolves across many regions. |
| **tui-shimmer** | **Do not depend; port ~40 lines.** | The whole crate is one function: a cosine-windowed highlight band with a truecolor→grey fallback. §5.1 reproduces it. Adding a crate + its `OnceLock` caches for 40 lines we already have is not worth the dependency edge. |
| **easing/tween crates** (`keyframe`, `simple-easing`, `interpolation`) | **Hand-roll (§3).** | The four easings are ~20 lines of `std`-only math. A crate adds a dependency for functions that fit on a screen. |
| **colour crates** (`palette`, `csscolorparser`, `colorgrad`) | **Hand-roll (§5).** | `palette` is excellent but heavy (generic colour-space machinery, ~pulls `approx`, `fast-srgb8`). koda needs one gamma-2.0 lerp + two quantisers = ~60 lines, zero deps. |
| **libm** | **Not needed.** | `f32::sin/cos/sqrt` are in `std` on all koda targets. |
| **tokio** | **Already present.** | The clock reuses `tokio::time::{sleep_until, Instant}` and `select!` — no new deps. |
| **crossterm** DEC 2026 | **Already present via ratatui.** | koda already calls `BeginSynchronizedUpdate`/`EndSynchronizedUpdate`. |

**Net: zero new dependencies.** The entire animation engine is ~250 lines of `std` + existing
tokio/ratatui, so the 6.4MB binary is essentially unchanged (the code is tiny and `strip`ped).
This is the strongest possible outcome for the binary-size constraint.

---

## 7. Synchronized output (DEC private mode 2026) — does it help?

**Yes, for correctness; not for speed.** Findings:

- DEC 2026 wraps a screen update in `CSI ? 2026 h` (begin) / `CSI ? 2026 l` (end). The emulator
  buffers everything between and presents it atomically, so the renderer never samples a
  half-written frame. This eliminates **tearing**, which is the dominant cause of perceived
  "flicker" during animation (spring-shell #1361, ghostty docs, claude-code #55613).
- It does **not** make writes faster — ratatui #579 profiling showed the `write()` syscall is the
  bottleneck and sync-update adds two short control sequences per frame, a negligible cost. A quick
  test in #579 found "no speed effect."
- Support is not universal: many modern emulators (Ghostty, WezTerm, iTerm2, Kitty, Windows
  Terminal) support it; **macOS Terminal.app and GNU screen do not** (screen silently ignores mode
  2026). Sending the sequences to a non-supporting terminal is harmless — unknown DECSET modes are
  ignored — so koda can always emit them.

**Implication for koda:** keep wrapping each `term.draw` in Begin/End (already done). This is what
*raises the flicker ceiling* from 75ms toward 33/16ms (§2): on terminals that honour 2026, even a
larger dynamic region redraws atomically, so higher frame rates become safe. On Terminal.app it is
a no-op and the conservative 33ms budget applies. Recommendation: **detect 2026 support and pick
the frame budget accordingly.**

```rust
/// Best-effort: assume sync support unless we're clearly on a terminal that lacks it.
/// (A DA/DECRQM query is more precise but adds a round-trip at startup.)
fn sync_supported() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if term.starts_with("screen") { return false; }        // GNU screen ignores 2026
    if prog == "Apple_Terminal" { return false; }           // Terminal.app: no 2026
    true
}

fn choose_frame_budget(cfg_sync: bool) -> Duration {
    if cfg_sync && sync_supported() { Duration::from_millis(16) }
    else { Duration::from_millis(33) }
}
```

---

## 8. Summary of the design contract

- One `AnimationClock` drives all animation via `sleep_until`; **idle = a never-firing sleep**, so
  the 8.6µs idle frame and today's pure event-wait loop are preserved byte-for-byte when nothing
  animates.
- Animations store `start`/`duration`/`easing`; progress is `elapsed/duration` sampled at draw
  time, so per-animation durations + easing coexist and degrade gracefully under dropped frames.
- Frame budget: 30fps default, 60fps when DEC 2026 is supported; the 75ms figure applies only to
  large non-atomic redraws, which koda avoids via `signature()` region separation + sync output.
- Sub-cell: eighth blocks for magnitude, braille for motion/shape, with glyph fallback.
- Colour: gamma-2.0 lerp (no deps) on **semantic roles**, quantised to truecolor/256/16/none at the
  last step; motion is opt-out and collapses cleanly.
- **Zero new dependencies**; tachyonfx and tui-shimmer are mined for design, not linked.
