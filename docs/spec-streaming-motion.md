# koda spec: streaming text motion, progress & accessibility

Scope: motion design for **streaming assistant text** and **progress/liveness
indicators** in koda (Rust / ratatui 0.29), plus accessibility. Complements
`spec-markdown-stream.md` (oh-my-pi's three-layer anti-reflow algorithm) with a
concrete, implementable motion + a11y layer.

## Established constraints (carried in from prior research)

- **~75 ms / ~13 fps is the practical frame ceiling** before flicker in some
  terminals. koda's current loop uses a fixed `tokio::time::interval(80 ms)`
  (`tui.rs:1946`) — i.e. it already sits right at the ceiling. This spec moves
  koda to **per-frame durations with easing** instead of a fixed interval.
- **Per-frame durations with easing beat a fixed interval.**
- **Semantic colour roles degrade better than literal RGB** (see the role table
  in `spec-markdown-stream.md` / `spec-glyphs-theme.md`).
- **Motion must be opt-out for accessibility.**
- **Static and dynamic regions must be separated** to limit redraws.
- **8.6 µs idle frame on a 4000-block transcript must not regress.** Every
  mechanism below is gated so that when nothing is streaming and no spinner is
  live, the frame does zero extra work: the reveal cursor, spinner phase, and
  freeze recompute are only touched while `busy`.

Current koda facts this spec builds on (verified in source):
- `App.busy` gates all idle repaints; the tick loop only sets `dirty` while busy
  (`tui.rs:1981`). Bursts are coalesced into one redraw (`tui.rs:2008`).
- Spinner is a glyph array on the theme: default Braille
  `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` (10 frames), ASCII fallback `|/-\` (`theme.rs:544,581`),
  advanced once per tick with a 200 ms `SPINNER_DELAY` before first show
  (`tui.rs:47,1391`).
- Reasoning/thinking items already carry `started: Instant` + `elapsed`
  (`view.rs:23,38,193`).

---

## Research summary (evidence feeding the recommendations)

**Reveal speed (the strongest external finding).** Zhou, Gallagher & Sterman,
*How Text Presentation Influences Perceptions of AI Writing Tools* (C&C '25,
arXiv:2504.20365, n=297) tested five reveal styles. Results:
- **Medium = 600 wpm ≈ 13 tokens/s ≈ ~50 char/s** was rated **most comfortable
  to read, highest perceived text quality, and (tied with slow) most
  human-like.** Ranking: medium > fast > slow > {backwards, random}.
- **Too slow is actively disliked**: "slower than I read and thus highly
  annoying" (P155). **Too fast overwhelms** slower readers. **Backwards/random
  are jarring**, "hurt my eyes," read as *erroring* and *untrustworthy*.
- Users **read along** with the stream; the win is matching human reading pace,
  not raw speed. Older VDU work (Tombaugh 1985) agrees: ~30 cps (~300 wpm) beat
  both 15 cps and 960 cps for comprehension.
- Authors' design takeaway: **give users control over appearance speed** (they
  cite RPG text-speed settings as precedent).

This is *reading* speed, not typing speed; matching koda's reveal to ~10–15
tok/s (near the low end of "good reader" range 200–400 wpm) is the sweet spot.

**Claude Code thinking spinner (two independent reverse-engineerings).**
- Glyph cycle: `· ✻ ✽ ✶ ✳ ✢` — interpunct then five asterisk/star glyphs
  (alexbeals.com, Feb 2026; Kyle Martinez, Oct 2025, confirmed via `script`
  capture of the live session). On ghostty / non-Darwin terminals Claude
  substitutes `*` as a larger centred element.
- **Easing**: Martinez screen-recorded the animation frame-by-frame and found
  *"very clever easing where the first and last character hold slightly longer
  than the rest."* The motion is a **triangle-wave sweep** across the glyph
  index (0→5→0), not a wrapping loop, and the two endpoints dwell longer — a
  raised-cosine ("breath") dwell. He also had to animate Y-position to keep the
  varying-width glyphs vertically centred.
- oh-my-pi's analogue (`spec-markdown-stream.md §4`) independently uses a
  raised-cosine dwell between **70 ms and 230 ms, mean ≈150 ms**. We adopt the
  same envelope.

**Streaming markdown without reflow.** Consensus across Streamdown, brookmd,
thetarnav/streaming-markdown, and the CommonMark streaming-guidance thread:
an **append-only model with stable block identities so unchanged blocks never
re-reconcile**, re-parsing only the grown tail, and *speculative closure* of
mid-stream constructs (an open code fence). This matches oh-my-pi Layer B.

**Accessibility conventions.**
- **`NO_COLOR`** (no-color.org): if present and **non-empty** (regardless of
  value), suppress *added* colour. Widely adopted (turbo, terraform, twitch-tui…).
- **`TERM=dumb`**: suppress colour/formatting/animation entirely.
- **`--color=WHEN`** with `always|never|auto` is the de-facto flag convention.
- **When stdout is not a TTY**: disable colour and all animation, emit plain
  linear text (Seirdy, *Best practices for inclusive CLIs*: "disable color when
  the output is not a TTY unless the user explicitly force-enables").
- **Spinners are hostile to screen readers** — every animation frame is a DOM/
  line mutation the reader may re-announce ("a disaster," tianpan.co). Seirdy:
  *"Nearly all animated spinners are extremely problematic for screenreaders. A
  simple progress meter and/or numeric percentage… is preferable."* Provide a
  simplified, low-frequency textual mode.
- No terminal-native `prefers-reduced-motion` exists; Claude Code has an **open
  issue (#57237)** requesting a runtime `prefersReducedMotion` toggle — so this
  is an unsolved area and koda should ship both an **env var and a config/slash
  toggle**.

---

## 1. The STABLE PREFIX algorithm

Goal: render streaming markdown so that **any line already shown on screen never
moves or changes bytes** as more tokens arrive. Only the volatile *tail* (the
block currently being written) may reflow. This is the reification of
`spec-markdown-stream.md` Layer B, specified as an algorithm for the Rust port.

### 1.1 Data model

```
StreamDoc {
    src: String,                 // full received markdown so far (append-only)
    frozen_len: usize,           // byte offset; src[..frozen_len] is settled
    frozen_lines: Vec<Line>,     // wrapped, styled, cached render of the prefix
    frozen_wrap_width: u16,      // width frozen_lines were wrapped at
    tail_cache: Option<TailRender>, // last tail render (transient)
}
```

Invariant (**never violated**): for a fixed width `w`, `frozen_lines` is a byte-
exact prefix of the lines that the *final* (post-stream) render of the whole
document would produce. Because of this, frozen lines can be blitted once and
skipped on every subsequent frame, and can safely retire into scrollback.

### 1.2 The freeze boundary

A prefix of `src` is safe to freeze iff its block tokenization is **independent
of anything that comes after it**. In CommonMark this holds at a **blank-line
boundary (`\n\n`) that is not inside an open code fence and not inside an open
container** (list item / blockquote whose continuation could still absorb the
next line). Formally we freeze up to the largest offset `b` such that:

1. `src[..b]` ends at a blank-line boundary (`\n\n` or start-of-doc..only-blanks),
2. the fence-depth of `src[..b]` is 0 (all ` ``` `/`~~~` fences balanced),
3. `b` is **strictly less than** the start of the last block (never freeze the
   block currently being appended — its markdown can still change),
4. there is at least one blank line of separation between `b` and current write
   head (guards against a trailing setext underline / lazy list continuation
   retro-actively re-typing the block just before `b`).

Rule (3)+(4) together are the **"one full block of slack"** rule: we keep the
last *completed* block un-frozen too, because a following line can still promote
it (e.g. a paragraph becomes a setext heading when an `===` line arrives, or a
line joins a loose/tight list). Freezing lags the write head by one blank-line-
bounded block. This costs one block of re-wrap per frame — bounded and cheap —
and is what makes the invariant hold under adversarial markdown.

### 1.3 Open code fence (still-open construct)

When the tail contains an **unclosed fence**, the tail is *not* freezable (rule
2 fails for any boundary inside it). Handle it with **speculative closure** for
rendering only:

- Detect the open fence in the tail (odd count of fence markers past `frozen_len`).
- Render the tail *as if* a synthetic closing fence were appended, so the body
  shows as a code block immediately.
- **Highlight only completed physical lines** of the fence body (lines ending in
  `\n`); the trailing partial line renders flat (`mdCodeBlock`), because its
  token class can still change as more characters arrive. (Mirrors oh-my-pi
  `createHighlightStream`.)
- **Do not freeze any line inside an open fence**, not even completed ones: a
  fence can be retro-actively reinterpreted (e.g. the "closing" ``` ``` `` turns
  out to have a language tag, or indentation changes it to an indented code
  block). Freeze the whole block only once the real closing fence is received and
  the block is re-rendered in final mode (byte-stable highlight).

### 1.4 Pseudocode

```
fn recompute_freeze(doc, width):
    # Only re-lex the region past the current frozen boundary — O(tail), not O(N).
    tail = doc.src[doc.frozen_len ..]
    b = doc.frozen_len
    depth = 0
    last_boundary = None
    i = doc.frozen_len
    for (line, byte_range) in physical_lines(tail):
        if is_fence_marker(line): depth ^= 1        # toggle open/closed
        if depth == 0 and is_blank(line):
            last_boundary = byte_range.end          # candidate freeze point
    # Apply the "one block of slack" rule: freeze up to the boundary that is
    # strictly before the last block start, only if fences are balanced there.
    new_frozen = choose_boundary(last_boundary, doc.src)   # rules 1–4
    if new_frozen > doc.frozen_len and width == doc.frozen_wrap_width:
        newly = render_final(doc.src[doc.frozen_len .. new_frozen], width)  # highlighted, cached
        assert is_line_prefix(doc.frozen_lines ++ newly, /* of full render */)  # debug-only guard
        doc.frozen_lines.extend(newly)
        doc.frozen_len = new_frozen

fn render_frame(doc, width, reveal_bytes):     # reveal_bytes from §2
    if width != doc.frozen_wrap_width:          # resize: everything re-wraps once
        rebuild_frozen(doc, width); doc.frozen_wrap_width = width
    recompute_freeze(doc, width)
    visible_src = doc.src[.. reveal_bytes]                 # typewriter cut
    tail_src    = visible_src[doc.frozen_len ..]
    tail_lines  = render_transient(tail_src, width)        # speculative fence close
    emit(doc.frozen_lines)                                 # blit once, cached
    emit(tail_lines)                                       # only volatile region
```

### 1.5 Region separation (protects the 8.6 µs idle frame)

- Frozen lines are a `Vec<Line<'static>>` owned per message; on an idle frame
  ratatui just re-renders already-built lines. No re-lex, no re-wrap.
- `recompute_freeze` and `render_transient` are called **only while that message
  is the streaming tail** (`busy && message.is_last && !message.finalized`).
  A finalized or historical message never runs any of this — it is pure frozen
  lines, so a 4000-block transcript pays nothing.
- Frozen lines whose screen position has scrolled above the viewport are eligible
  to retire into native scrollback (oh-my-pi Layer C) — out of scope here but the
  invariant in §1.1 is exactly what makes that safe.

---

## 2. Reveal pacing

**Recommendation: grapheme-paced reveal at a target of ~12 tok/s (≈ one row of
~30 fps ticks advancing a grapheme cursor), catch-up-bounded, with a hard
"snap" at structural boundaries. Default ON; user-configurable; auto-OFF for
accessibility and non-TTY.**

### 2.1 Per-token vs per-grapheme vs immediate

- **Immediate (paint every delta as it arrives)** — rejected as the *default*.
  Provider bursts are lumpy (dozens of tokens then a stall); painting each delta
  couples display cadence to network/inference jitter, which the HCI literature
  (Nielsen; the arXiv study §2.2) explicitly warns against ("UI updates should be
  timed to a clock, not to computation speed"). It also makes per-frame markdown
  work unbounded, threatening the frame ceiling.
- **Per-token** — rejected. Token boundaries are arbitrary (sub-word, whitespace-
  attached); revealing by token produces uneven, stuttery visual chunks and can
  split a grapheme cluster or an emoji ZWJ sequence.
- **Per-grapheme at a clock-driven rate** — **chosen.** Reveal a monotonically
  increasing prefix measured in **grapheme clusters** (via `unicode-segmentation`
  `graphemes(true)`), advanced by a timer, decoupled from delta arrival. This is
  smooth, never splits a cluster, and bounds per-frame work.

Reasoning: the arXiv study shows **~600 wpm (≈13 tok/s) maximizes comfort and
perceived quality**, and that *matching the user's reading pace* — not raw speed
— is what people reward. A clock-driven grapheme reveal is the only option that
lets us hit a chosen pace regardless of how the tokens actually arrive. Perceived
speed is *higher* than a dump-when-done approach because the user starts reading
immediately (the "slow reveal" perceived-performance effect), yet it never
outruns the reader.

### 2.2 Cadence (per-frame durations, not a fixed interval)

Drive reveal from an **eased frame scheduler** (§3.4) rather than koda's current
fixed 80 ms `interval`. Reveal budget per frame:

```
TARGET_CPS      = 50          # ≈ 600 wpm ≈ 12–13 tok/s  (arXiv medium)
MIN_STEP        = 2           # graphemes; keeps motion visible when nearly caught up
CATCHUP_FRAMES  = 8           # drain any backlog over ~8 frames

on each reveal frame (dt = time since last reveal, seconds):
    base    = ceil(TARGET_CPS * dt)                 # clock-paced baseline
    backlog = total_graphemes - revealed
    step    = max(MIN_STEP, base, ceil(backlog / CATCHUP_FRAMES))
    revealed = min(total_graphemes, revealed + step)
```

So: at steady state it reveals ~`TARGET_CPS` graphemes/s; when far behind (a big
burst just landed) it drains fast but smoothly over ~8 frames; when caught up it
idles (no reveal frame scheduled → no repaint → idle-frame cost preserved).

Grapheme counting/slicing is **memoized per block**: because the stream is
append-only, only the final cluster of the previous text can change, so re-
segment only the suffix from that cluster (oh-my-pi `BlockUnitCounter`).

### 2.3 Structural snap (no typewriter across boundaries)

Reveal jumps immediately to `total` (no typewriter) when:
- a **tool-call block** appears in the message (transcript-order boundary — finish
  the leading prose at once so the tool card renders below it), or
- the stream **finalizes** (seal the block in one final render), or
- reveal is **disabled** (accessibility / config / non-TTY), or
- the backlog exceeds a **panic threshold** (e.g. > 2000 graphemes behind, as on
  a resumed/replayed session) — snap rather than crawl.

### 2.4 No trailing caret

Do **not** append a blinking cursor/caret to streaming text. The reveal cadence
itself is the liveness cue (oh-my-pi §5), and the arXiv authors note a fake caret
implies "typing/thinking" that isn't happening. Liveness while *reasoning* (not
yet emitting text) is carried by the thinking indicator (§3).

### 2.5 Configurability

Expose reveal speed as a setting (the arXiv study's explicit recommendation):

```
[ui]
stream_reveal = "medium"   # off | slow | medium | fast | instant
```

| setting  | TARGET_CPS | ≈ wpm  | note |
|----------|-----------:|-------:|------|
| off      | (instant)  |   —    | reveal = total; identical to accessibility mode |
| slow     | 30         | ~360   | for slow readers; still above "annoying" floor |
| medium   | 50         | ~600   | **default** — arXiv comfort/quality optimum |
| fast     | 90         | ~1080  | skimmers; still forward-sequential |
| instant  | (instant)  |   —    | paint on finalize only |

`instant`/`off` also implicitly selected when reduced-motion or non-TTY (§4).

---

## 3. The thinking indicator (eased multi-frame spinner)

Shown while the model is **reasoning / before first text token**, and while a
tool is running (koda's `busy`). Replaces the fixed-interval Braille spinner with
an eased sweep modelled on Claude Code + oh-my-pi.

### 3.1 Glyphs

Adopt Claude Code's exact set for the "thinking" role (default / unicode preset):

```
frames = ["·", "✻", "✽", "✶", "✳", "✢"]   # 6 frames: interpunct + 5 stars
```

- Motion is a **triangle-wave sweep** over the index: `0 1 2 3 4 5 4 3 2 1` then
  repeat (a "breathing" bloom out to the fullest glyph and back to the dot), **not**
  a wrapping `0→5→0` jump. This matches the observed Claude behaviour.
- Render at a **fixed cell width** (all glyphs are width-1 in the unicode set;
  reserve 1 cell) so the following label never shifts. If a glyph renders
  double-width on a given terminal, pad to a stable 2-cell field.
- **Fallbacks** (mirror `spec-glyphs-theme.md` presets):
  - unicode: the 6 glyphs above.
  - nerd-font: may substitute a single bloom glyph if preferred; keep 6-frame
    sweep.
  - ASCII / `TERM=dumb` / narrow: fall back to the static word only, or the
    existing `|/-\` at a **slow, constant** cadence (no easing needed for ASCII).
  - Some terminals render the small glyphs off-centre; if koda ever uses a
    2-row sized glyph, apply Claude's vertical-centre compensation — otherwise
    N/A for single-cell.

### 3.2 Label & badge (koda already tracks `elapsed`)

```
<glyph> Thinking · <elapsed>s            # muted label + dim elapsed clock
<glyph> Thinking · <N> tok · <R> tok/s   # when live token deltas are flowing
```

- Label ` Thinking` in `muted`; elapsed clock and counts in `dim`.
- Optional rate badge (oh-my-pi §4): windowed avg over ~3 s, clamp `SPEED_MAX=200`,
  colour lerps `dim → accent` by `sqrt(rate/200)`; self-suppress if rate < 0.05
  or no live deltas. Purely additive; never widens/shifts the glyph field.
- koda can also surface a rotating "verb" (Claude ships 184: *Cogitating,
  Percolating, …*). Optional flavour; **must** be static text, not animated
  per-frame, to stay screen-reader-friendly.

### 3.3 Eased frame-duration table (concrete, ms)

Endpoints (the dot `·` and the fullest bloom `✽`, i.e. sweep positions 0 and the
turn-around) dwell **longer**; mid-sweep frames are quick. Envelope: raised-
cosine dwell between **70 ms (fastest, mid-sweep) and 230 ms (slowest, endpoints)**,
mean ≈150 ms — matching oh-my-pi and Claude's "first/last hold longer."

Full 10-step sweep cycle `0 1 2 3 4 5 4 3 2 1` (index → glyph → dwell):

| step | index | glyph | dwell (ms) | note                          |
|-----:|------:|:-----:|-----------:|-------------------------------|
| 1    | 0     | `·`   | **230**    | start endpoint — long hold    |
| 2    | 1     | `✻`   | 150        | accelerating out              |
| 3    | 2     | `✽`   | 90         | fast                          |
| 4    | 3     | `✶`   | 70         | fastest (mid-sweep)           |
| 5    | 4     | `✳`   | 90         | fast                          |
| 6    | 5     | `✢`   | **230**    | far endpoint — long hold      |
| 7    | 4     | `✳`   | 90         | fast                          |
| 8    | 3     | `✶`   | 70         | fastest (mid-sweep)           |
| 9    | 2     | `✽`   | 90         | fast                          |
| 10   | 1     | `✻`   | 150        | decelerating back to start    |

Cycle length ≈ 1260 ms. Every dwell ≥ 70 ms keeps the effective frame rate at
**≤ ~14 fps at its fastest** — under the ~75 ms / 13 fps flicker ceiling on the
slow frames and only momentarily at it on the two fastest, which is imperceptible
for a single-cell glyph swap (no full-screen repaint). The dwells are computed,
not hard-coded per glyph, via:

```
dwell(phase) = MIN_DWELL + (MAX_DWELL - MIN_DWELL) * (0.5 + 0.5*cos(2π*phase))
MIN_DWELL = 70 ms,  MAX_DWELL = 230 ms,  phase ∈ [0,1) around the sweep
```

so the endpoints (`phase` 0 and 0.5) get `MAX_DWELL` and the mid-sweep gets
`MIN_DWELL`. Keep the 200 ms `SPINNER_DELAY` before first paint so instant work
shows no spinner at all.

### 3.4 Scheduler (per-frame durations, not a fixed interval)

Replace `tokio::time::interval(80 ms)` with a **variable timer** that sleeps for
exactly the next glyph's dwell:

```
loop {
  select! {
    _ = sleep(dwell(next_phase)) if busy && motion_enabled => {
        spinner_phase = advance(spinner_phase);   // triangle-wave step
        reveal_step();                             // §2.2 (also clock-paced)
        dirty = true;
    }
    ev = events.recv() => { ... }                  // input/stream events
  }
  // when !busy: no timer armed → the select idles → 8.6 µs idle frame intact
}
```

Key property: **the timer is only armed while `busy && motion_enabled`.** When
idle or reduced-motion, there is no periodic wakeup, so the idle-frame cost is
unchanged (it never regresses the 8.6 µs figure — that path runs zero motion
code). Reveal and spinner share the frame so a streaming turn produces one
coalesced repaint per frame, not two.

### 3.5 Determinate progress (when a total is known)

Spinners are **indeterminate**: use them only when remaining work is unknown
(model reasoning, an open-ended tool). When a **total is known** (downloading N
bytes, applying k/M files, N/total test cases), show a **determinate** bar or
`k/total (pp%)`:
- Determinate reads as more responsive and honest; it lets the user estimate
  completion. Use it whenever a denominator exists.
- Keep the bar's motion clock-driven and update at ≤ 13 fps; never animate a
  determinate bar faster than its data changes (a "gaming" shimmer on a stalled
  bar reads as dishonest).
- If a task starts indeterminate and gains a total, switch bar type once, cleanly.
- Under reduced-motion / non-TTY, emit occasional textual `k/total (pp%)` lines
  instead of a redrawing bar (Seirdy: a simplified mode "can occasionally log a
  percentage-complete instead of a progress bar").

---

## 4. Accessibility

koda MUST honour the following, checked **once at startup** and re-checked on an
explicit runtime toggle. Precedence: **explicit force-on flag > env var >
config > TTY auto-detect**.

### 4.1 Colour

| signal | source | behaviour |
|---|---|---|
| `NO_COLOR` present and **non-empty** | env (no-color.org) | disable all *added* colour; keep layout/glyphs. Any value counts; only emptiness/absence means "not set". |
| `TERM=dumb` (or unset) | env | disable colour **and** all animation; plain linear output. |
| `--color=WHEN` (`always`\|`never`\|`auto`) | CLI flag | overrides env for colour; `auto` = enable only if stdout is a TTY. |
| `CLICOLOR=0` / `CLICOLOR_FORCE=1` | env (BSD convention) | honour as secondary: `CLICOLOR_FORCE` non-zero forces colour on even when piped; `CLICOLOR=0` disables. `NO_COLOR` wins over `CLICOLOR_FORCE`. |
| stdout not a TTY | `IsTerminal`/`libc::isatty` | disable colour unless force-on flag/`CLICOLOR_FORCE`. |

When colour is off, koda must still be legible via the **semantic-role → mono
mapping**: headings via bold/underline + blank lines, code via 2-space indent,
lists via `-`/`N.`, quotes via `▏`/`|`. (Colour roles degrade to attributes, per
the "semantic roles degrade better than literal RGB" constraint.)

### 4.2 Reduced motion

There is **no standard terminal `prefers-reduced-motion`** (Claude Code issue
#57237 is still open). koda ships all of:

| signal | source | behaviour |
|---|---|---|
| `KODA_REDUCED_MOTION` | env (any non-empty) | disable typewriter reveal (reveal = instant) and the eased spinner (static glyph or plain word); progress bars become periodic textual `pp%` lines. |
| `NO_MOTION` | env (any non-empty) | treated identically (community-convention alias; accept if present). |
| `TERM=dumb` | env | implies reduced motion (see above). |
| `[ui] motion = false` | config | same as `KODA_REDUCED_MOTION`. |
| `/motion off` (or `/reduce-motion`) | runtime slash cmd | toggle at runtime (Claude's requested feature); re-reads flags live, like `/think`. |
| stdout not a TTY | detect | force reduced motion (nothing to animate to a pipe). |

Reduced-motion behaviour, precisely:
- Reveal: `stream_reveal` forced to `instant` — text appears in whole blocks as
  finalized; **no per-grapheme animation**.
- Spinner: no eased sweep; show a **static** glyph + word (e.g. `✻ Thinking`) that
  updates only its **elapsed clock at ≤ 1 Hz**, or nothing at all in `dumb`.
- Progress: determinate bars stop redrawing; emit a textual `pp%` line at most
  every ~1 s (screen-reader-friendly cadence).
- The transcript remains fully legible; only motion is removed, never content.

### 4.3 Screen readers

- Animated spinners are hostile to screen readers (each frame is a fresh line
  mutation). Reduced-motion mode (which SR users will enable, or which koda picks
  when non-TTY) already removes them.
- Never encode state **only** in a spinner glyph or colour: always pair with a
  word (`Thinking`, `Running <tool>`, `Done`, `Error`) so a reader announces
  meaningful text.
- Prefer emitting **completed** lines once (frozen prefix) rather than re-mutating
  in place; SRs cope far better with append-only output than with lines that
  rewrite themselves.
- Provide a `--plain` / non-interactive path (Seirdy §CLIs) that streams linear
  text with no cursor movement, suitable for `espeak-ng` and log capture.

### 4.4 Not a TTY (piping / redirection / CI)

When `stdout` is not a terminal (piped, redirected to a file, or `CI` is set):
- **No colour, no animation, no cursor movement, no alt-screen.**
- Reveal = instant; spinner replaced by nothing (or a single `Thinking…\n` line);
  progress replaced by periodic `pp%` text lines.
- Emit **plain, append-only, line-buffered** output so it is byte-stable when
  redirected to a file or piped to another program (and readable by a screen
  reader over the pipe). This is the same code path as full reduced-motion, so
  there is one well-tested "static" renderer.

---

## 5. Anti-patterns (motion that annoys in a coding tool)

Be specific — these are the failure modes to avoid:

1. **Reveal slower than reading speed.** Anything below ~30 wpm equivalent reads
   as "highly annoying" (arXiv P155). Never gate a *completed* response behind a
   slow crawl; if the whole answer has arrived, don't make the user wait on a
   typewriter — cap catch-up (§2.2) and snap on finalize.
2. **Coupling reveal to token/network jitter.** Painting each provider delta makes
   text lurch (stall, then a wall of text). Always clock-pace (§2.2).
3. **Reflow of already-read lines.** If a line the user is reading suddenly
   re-wraps, shifts, or re-colours, they lose their place. The stable-prefix
   invariant (§1) exists precisely to forbid this; a code block that re-highlights
   its *earlier* lines mid-stream is the classic offender — highlight only
   completed lines, freeze only closed fences.
4. **A blinking caret / fake "typing" cursor on assistant text.** Implies human
   typing that isn't happening and adds motion with no information (§2.4).
5. **A spinner for instant work.** A spinner that flashes for a sub-200 ms
   operation is pure noise — koda already delays 200 ms; keep that.
6. **Over-fast spinner frames.** Cycling faster than ~12–14 fps flickers on slow
   terminals and reads as frantic; the eased envelope (§3.3) keeps every frame
   ≥ 70 ms.
7. **Full-screen redraws per frame.** Animating one glyph must not repaint the
   whole transcript. Separate static (frozen) from dynamic (tail + spinner)
   regions (§1.5); anything else risks regressing the 8.6 µs idle frame.
8. **Determinate-looking bar that lies.** A progress bar that shimmers/advances
   while the task is actually stalled destroys trust. Only advance a determinate
   bar from real data; use an indeterminate spinner when you don't know the total.
9. **Non-forward / decorative reveals** (backwards, random, "matrix rain",
   per-character colour cycling). Rated jarring, "hurt my eyes," and
   *untrustworthy* (arXiv). Never in a coding tool.
10. **Motion with no opt-out.** Any animation that can't be disabled via env,
    config, or runtime toggle is an accessibility failure (§4).
11. **State encoded only in motion or colour.** A colour-only or glyph-only status
    is invisible to `NO_COLOR`, reduced-motion, and screen-reader users; always
    pair with a word.
12. **Scroll-jank on new output.** Auto-scrolling/yanking the viewport on every
    token while the user is scrolled up reading history. Respect koda's `follow`
    flag; only auto-follow when already pinned to the bottom.
```
