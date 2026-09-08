# Delight in koda's TUI — the intro, and ambient motion during use

Status: research + design. Nothing here has landed. Two deliverables: (1) a
refined **intro** for the wordmark koda already draws, and (2) an **ambient
playful animation** for use, which is the hard half.

Every claim is tagged **[measured]** (run here, in this repo, commands inline),
**[reported]** (from a cited source — link in §11), or **[judgement]** (my
design call, argued but not evidenced). Untagged sentences are argument.

This sits under `spec-animation-engine.md` (the clock, easings, colour) and
`spec-streaming-motion.md` (§5 anti-patterns, §4 accessibility). Where it
disagrees with either, §10 says so. It does not supersede them.

---

## 1. The one-sentence answer

**Put delight inside the cells that are already moving for an informational
reason, and give the one genuinely blank row on screen a short, event-tied,
rate-limited visitor — never a loop, never a timer, never during a decision.**

Everything below is the argument for that sentence and the spec for building it.

---

## 2. What koda already has (read before designing)

Verified in source at `c30166b`.

| Fact | Where |
| --- | --- |
| `Clock` sleeps 24h when nothing is armed; `select!` guards the branch with `if clock.animating()`, so tokio never polls it when idle | `src/anim.rs:144`, `src/tui.rs:5060` |
| `wants_frames()` is the complete list of reasons to repaint: busy, compacting, log view, file scan, reveal catching up, welcome window | `src/tui.rs:1071` |
| `Motion::{Full,Reduced,Off}`; non-tty → `Off`; `KODA_REDUCED_MOTION`/`NO_MOTION`/`REDUCED_MOTION` → `Reduced`; `TERM=dumb` → `Off`; config off → `Reduced` | `src/anim.rs:41` |
| Frame budget 33 ms with trustworthy DEC 2026, else 75 ms | `src/anim.rs:84` |
| `sweep()` — 10-step out-and-back with dwells `[230,150,90,70,90,230,90,70,90,150]` ms, indices `[0,1,2,3,4,5,4,3,2,1]`, derived from elapsed time, no counter | `src/anim.rs:195` |
| `shimmer()` — travelling cosine-ish band with eased falloff, half-width 4, overshoots both ends by 4 | `src/anim.rs:272` |
| `lerp_rgb()` — gamma-2.0 blend; `theme::mix()` wraps it and **degrades to a 0.5 threshold on non-RGB palettes** | `src/anim.rs:256`, `src/theme.rs:502` |
| `WELCOME_ANIM = 1400 ms`, `REVEAL_FRACTION = 0.45`, `EDGE_WIDTH = 6.0` | `src/tui.rs:54` |
| `welcome_shimmer()` repaints only the 6 banner rows, over identical content, so stopping causes no jump | `src/tui.rs:4201` |
| `BANNER_ART` — 6 rows × 36 columns, "ANSI Shadow" face | `src/tui.rs:154` |
| `WORKING_MSGS` — 18 light-hearted status verbs, rotating every 10 s, derived from elapsed | `src/tui.rs:65,147` |
| `TIPS` — 15 real capability hints, first at 6 s, then every 14 s, session-seeded | `src/tui.rs:107,144` |
| `/motion` toggles `Full`↔`Reduced` live; `/reveal` is separate | `src/tui.rs:2610` |
| `Glyphs` has a full ASCII fallback set chosen from `LC_ALL`/`LC_CTYPE`/`LANG` containing `UTF-8` | `src/theme.rs:643` |

**[measured]** `cargo test --bin koda anim::` — 21 tests, 21 pass, 0.00 s. The
engine's invariants (no frame under 70 ms, clock cannot leak a claim, motion-off
never arms, gauge width constant) are already enforced by tests.

### 2.1 Where is the screen genuinely free?

`draw()` splits the frame into seven constraints (`src/tui.rs:3243`):

```
Min(1)                       transcript      -> content, never touch
Length(plan_h)               sticky plan     -> content, 0 when no plan
Length(1)                    hint / status   -> information, one animated glyph cell
Length(tip.is_some())        tip row         -> information, 0 unless a turn is long
Length(spacer)               blank           -> *** nothing is ever drawn here ***
Length(input_h)              composer        -> the user's own text
Length(1)                    powerline       -> information
```

The destructure is `(chunks[0], chunks[1], chunks[2], chunks[3], chunks[5],
chunks[6])` — **`chunks[4]`, the spacer, is allocated by the layout and never
rendered into.** It is one row tall, full width, present whenever `!m.tiny`
(i.e. terminal ≥ 64 columns), and its height does not depend on anything that
animates. **[measured]** by reading the layout: it is the only region on screen
that is simultaneously (a) always blank, (b) never content, (c) never resized by
the animation itself.

That is the entire free real estate. There is no other.

### 2.2 What does koda do when idle?

Nearly nothing, by design and by construction:

- `clock.sync(app.wants_frames())` disarms when nothing wants frames; `tick()`
  then resolves 24 hours out and the `if clock.animating()` guard means tokio
  does not even poll it (`src/anim.rs:144`, `src/tui.rs:5060`).
- `dirty` gates `term.draw` entirely, so an unchanged screen is not repainted.
- Two intervals do still tick regardless: `watch_tick` (`cfg.watch_interval_ms`,
  clamped 300–60 000 ms) and `web_tick` (400 ms, only when the web UI serves).
  Neither sets `dirty` unless something actually changed.

So idle koda wakes on the watch interval, does a cheap scan, and goes back to
sleep without drawing. **Any ambient animation that runs on a timer converts
this into a continuous render/diff/write loop, and that is the single most
defended property of the codebase.** §6 is built around not doing that.

### 2.3 Two defects found while reading

**[measured]** Every non-space glyph in `BANNER_ART` is East Asian **Ambiguous**
(`U+2588 █`, `U+2550 ═`, `U+2551 ║`, `U+2554 ╔`, `U+2557 ╗`, `U+255A ╚`,
`U+255D ╝`):

```
python3 -c "import unicodedata as u; print({c:u.east_asian_width(c) for c in
'█═║╔╗╚╝'})"
# {'█': 'A', '═': 'A', '║': 'A', '╔': 'A', '╗': 'A', '╚': 'A', '╝': 'A'}
```

`unicode-width`'s `width()` resolves Ambiguous to 1; `width_cjk()` resolves it
to 2 **[reported]**, and a terminal configured for CJK ambiguous-wide will
render the banner at 72 columns, not 36. `show_welcome` also uses
`row.chars().count()` for `cols` (`src/tui.rs:1112`), which is character count,
not display width — correct only because these characters happen to be single
`char`s.

**[measured]** `show_welcome` uses `BANNER_ART` unconditionally
(`src/tui.rs:1111`) — there is no ASCII branch, so on a terminal where
`theme::glyphs()` chose `ASCII` (non-UTF-8 locale) the very first thing koda
prints is mojibake.

Both are fixed in §5.

---

## 3. Prior art: what respected terminal programs actually do

### 3.1 Intros and splashes

| Tool | What it does | Verdict |
| --- | --- | --- |
| **Neovim** | "When Vim starts without a file name, an introductory message is displayed. It is removed as soon as the display is redrawn. To see the message again, use the `:intro` command. To avoid the intro message on startup, add the `I` flag to `'shortmess'`." **[reported]** | The reference design. Zero cost, self-clearing on first real work, one documented opt-out. |
| **lazygit** | `showRandomTip: true` — "show a random tip in the command log when Lazygit starts"; `disableStartupPopups: false` **[reported]** | Randomness carries *information*, not decoration. Both switchable. |
| **fastfetch / neofetch** | One-shot ASCII logo + system facts, then exit **[reported]** | Fine, because the logo *is* the program's output. Not a model for a long-lived TUI. |
| **btop** | No splash. `update_ms` default 2000 with the note "recommended 2000 ms or above"; `background_update` exists so users can stop menu-time flicker; `terminal_sync` default true "to reduce flickering on supported terminals" **[reported]** | A tool that repaints continuously by nature still ships three knobs to repaint *less*. |
| **helix, gitui, starship** | No splash screen at all **[reported]** | The modal default in the modern Rust TUI band. |
| **Shell splash-art scripts** (`ASCII-Art-Splash-Screen` and friends) | Random art per terminal launch **[reported]** | Personalisation for *your own shell*. A tool opened forty times a day should look the same every time; variation reads as noise, not identity. |

**Conclusion for koda's intro.** Having one at all puts koda above the modal
"nothing" — justified, because koda's first screen is not decoration: it carries
the model name, the mode keys, and how to attach a file. The animation earns its
place only if it (a) never blocks input, (b) ends fast, (c) is identical every
launch, and (d) disappears the moment the user starts working. Neovim's rule
("removed as soon as the display is redrawn") is the one koda is missing.

### 3.2 Delight during use

| Tool | What it does | Loved or switched off, and why |
| --- | --- | --- |
| **Claude Code spinner verbs** | ~187 whimsical present-tense words next to the thinking glyph ("Cerebrating", "Herding", "Noodling"). Community reverse-engineerings enumerate them; a 3 900-verb community pack exists; the vendor later shipped a `spinnerVerbs` settings block **[reported]** | **The most loved delight feature in this whole survey**, and it adds *zero* pixels of motion and zero extra frames — it varies the text of a row that was already going to be drawn. koda already does this (`WORKING_MSGS`). |
| **lazygit `animateExplosion`** | "If true, show a seriously epic explosion animation when nuking the working tree." Default true, one config key **[reported]** | Loved. Event-tied to a single dramatic, destructive, rare action. It is a *punctuation mark on a real event*, not ambience. |
| **lazygit spinner** | `spinner.frames: ["●∙∙","∙●∙","∙∙●","∙●∙"]`, `spinner.rate: 180` ms; index computed as `now.UnixMilli()/rate % len` **[reported]** | Same architecture as koda's `sweep()` — wall-clock-derived, stateless. Worth noting the convergence. |
| **charmbracelet/bubbles spinners** | Default `FPS` between `time.Second/12` (83 ms) and `time.Second/3` (333 ms); `MiniDot` is `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` at 12 fps **[reported]** | Corroborates koda's ≥70 ms floor from a widely-used library, independently. |
| **charmbracelet/harmonica** | A damped harmonic oscillator ported from Ryan Juckett's 2008/2012 C++; you supply the delta or `FPS(int)`, and "the framerate you set here matches your actual framerate" **[reported]** | Physics is a *nicer curve*, not a licence for more motion. koda's easings already cover its use cases; `spec-animation-engine.md §6` correctly declines the dependency. |
| **GitHub CLI** | Added `GH_SPINNER_DISABLED` because "the `spinner` package will continuously re-draw the screen… this is a problem because it interrupts screen readers and leaves no way to recover what the loading message was by scrolling up" **[reported]** | Even a *purely informational* spinner needed an opt-out. An ornamental one needs it more. |
| **VS Code Power Mode** | Typing triggers particle explosions and screen shake; `powermode.shake.enabled` exists so you can "disable screen shaking entirely", and the README recommends raising `explosionFrequency` when performance suffers **[reported]** | Popular as a novelty (its origin is the *Code in the Dark* competition), and the shake is the first thing people turn off. The lesson: the effect people disable first is the one that **moves content they are reading**. |
| **`sl`** | ASCII train crosses the screen when you mistype `ls`. "A cruel program made to punish users who mispell `ls`"; by default it **ignores Ctrl-C** **[reported]** | Beloved *as a joke program*. Its defining property — you cannot interrupt it — is precisely what a working tool must never do. |
| **cmatrix, pipes.sh, asciiquarium** | Full-screen looping animation, forever, until you quit **[reported]** | Beloved *because they are the whole program*. They are screensavers. None of them is a layer over work. |

**The pattern across all nine.** Delight that survives is either (a) content
variation in a cell that was already being drawn, or (b) a one-shot tied to a
specific meaningful event. Delight that gets disabled is motion that runs on a
timer, moves the thing you are reading, or cannot be stopped.

---

## 4. When motion helps, and when it harms

### 4.1 Duration

NN/g (Laubheimer, *Executing UX Animations: Duration and Motion
Characteristics*) **[reported]**:

- "the duration of most animations should be in the range of 100–500 ms,
  depending on complexity and on how far the element is traveling."
- Simple feedback: "roughly 100 ms… This duration feels **immediate**."
- Substantial screen changes: "a duration of 200–300 ms can be appropriate."
- "At 500 ms, animations start to feel like a real drag for users — they become
  cumbersome and annoying."
- "ease-out animation, that starts quickly but slows down… makes the animation
  feel responsive" — which is exactly why `anim.rs` defaults to
  `ease_out_cubic` for entrances.
- The load-bearing sentence for §6: **"the more frequent the animation, the more
  subtle and shorter you'll want it to be."**

clig.dev **[reported]**: "Print something to the user in <100 ms"; "A good
spinner or progress indicator can make a program appear to be faster than it
is."

Existing koda research already fixes the reveal rate at ~50 char/s from Zhou,
Gallagher & Sterman (arXiv:2504.20365) **[reported]**, and the 70–230 ms sweep
dwell envelope. Nothing here changes those.

### 4.2 Accessibility

**WCAG 2.2 SC 2.2.2 Pause, Stop, Hide (Level A)** **[reported]**: for moving,
blinking or scrolling content that "(1) starts automatically, (2) lasts more
than five seconds, and (3) is presented in parallel with other content", there
must be "a mechanism for the user to pause, stop, or hide it". The rationale for
the threshold is quoted directly: "Five seconds was chosen because it is long
enough to get a user's attention, but not so long that a user cannot wait out
the distraction if necessary to use the page."

This gives a hard, citable design number: **any ambient beat under 5 s is inside
Level A without needing its own control.** koda has `/motion` anyway, but the
budget is the budget.

**Vestibular triggers.** WebKit's *Responsive Design for Motion* **[reported]**
names the categories: scaling/zooming ("the illusion that the viewer is moving
forward or backward in physical space"); spiralling or spinning movements, which
"can cause some people with vestibular disorders to lose their balance or
vertical orientation"; multi-speed / parallax movement; 2D-planes-in-3D; and
peripheral motion — "horizontal movement in the peripheral field of vision can
cause disorientation or queasiness". Its guidance is *not* "remove everything":
"Only remove the animations you know to be vestibular triggers", and
user-initiated direct manipulation is fine. A List Apart's *Designing Safer Web
Animation For Motion Sensitivity* is the companion piece **[reported]**.

Read against that list, **a colour/brightness change with no positional movement
is not on it**, and a small localised traversal is much weaker than a
full-width peripheral sweep. That directly shapes §6.

### 4.3 Is there a terminal `prefers-reduced-motion`?

No standard exists. What exists is a scatter of per-tool conventions
**[reported]**:

| Signal | Source | Status |
| --- | --- | --- |
| `NO_COLOR`, present and non-empty regardless of value | no-color.org | The only genuinely settled convention. Colour, not motion. |
| `TERM=dumb` | de facto | Colour and animation off. |
| stdout not a TTY | de facto (clig.dev) | Everything off. |
| `CI=true` | GitHub Actions sets it on every workflow | Widely relied on; not motion-specific. |
| `GH_SPINNER_DISABLED` | GitHub CLI PR #10773 | Tool-scoped, accessibility-motivated. |
| `reduce_motion` config requests | e.g. herdr discussion #1316; anthropics/claude-code #22913 | Open asks, not conventions. |

koda's `Motion::resolve` already reads `KODA_REDUCED_MOTION`, `NO_MOTION` and
`REDUCED_MOTION`, plus `TERM=dumb` and non-tty. **That is more than any tool in
the table.** §7 adds exactly one signal (`CI`) and **no new env var** — inventing
a fourth spelling of "reduced motion" would be the opposite of `anim.rs`'s
stated stance of honouring conventions that already exist.

### 4.4 Is looping/idle animation ever recommended in a professional tool?

I found no source recommending it. I found three concrete arguments against.

1. **WCAG 2.2.2** requires a control past 5 s. A loop is unbounded by
   definition.
2. **It pins the host terminal.** A bug report against a terminal agent UI
   **[reported]** documents the mechanism precisely: an ~8 fps status animation
   produces "a diff frame written to the host pty roughly every 128 ms", and
   because terminals reset cursor-blink phase on output, "host terminal cursor
   blink is permanently suppressed" — plus continuous render/diff/write churn
   costing CPU and battery. The requested fix is a `reduce_motion` option.
   koda's `Clock` exists specifically so this cannot happen; a looping ambient
   would reintroduce it.
3. **NN/g's frequency rule.** "The more frequent the animation, the more subtle
   and shorter you'll want it to be" has a limit at frequency → always, where
   the only subtle-enough animation is none.

**[judgement]** A fourth, unevidenced but I believe true: a coding agent is
already asking for a lot of trust. Something moving on screen while koda claims
to be idle undermines "koda is doing nothing right now", which is a *status
claim*, not decoration.

### 4.5 What delight is, when it works

NN/g's *A Theory of User Delight* **[reported]** separates **surface delight**
("local and contextual… usually derived from largely isolated interface
features" — animations, microcopy, imagery) from **deep delight** ("holistic,
and is achieved once all user needs are met… functionality, reliability,
usability, and pleasurability"), and states the hierarchy plainly: "a product
can be delightful only if it is usable", with the warning that "if your product
or service lacks basic functionality and reliability, delightful features will
likely fail to provide any sustainable benefit."

The Kano model (Kano, Seraku, Takahashi & Tsuji, *Attractive Quality and
Must-Be Quality*, 1984) gives the same shape from the quality side: attractive
attributes "provide satisfaction when they are achieved fully but do not cause
dissatisfaction when they are not fulfilled" — and the model's later **reverse
quality** category names the failure mode exactly: a feature whose presence
*decreases* satisfaction **[reported]**. Ambient animation in a professional
tool is the textbook candidate for reverse quality. This is a framework, not
evidence about terminals; I use it only as vocabulary.

---

## 5. The intro

### 5.1 What stays

The current design is already right in three ways that matter, and this spec
keeps all three:

- **It does not block.** The card is written into the transcript immediately and
  the shimmer repaints over identical content. The composer is live from frame
  one. Never change this.
- **It repaints only 6 rows** — 167 non-space cells across a 36×6 region
  **[measured]** — which puts it in the top row of `spec-animation-engine.md`
  §2.1's frame-budget table.
- **The wordmark assembles rather than fades.** A left-to-right wavefront with
  eased motion and a bright leading edge is the memorable part. Keep it.

### 5.2 The beats

Total **1000 ms**, down from 1400. The change is **[judgement]**, argued from
NN/g's 100–500 ms band: an intro is not a state transition so the band does not
bind directly, but 1.4 s of decoration on a tool opened dozens of times a day is
past the point where NN/g's frequency rule ("the more frequent, the shorter")
bites. 1000 ms keeps a three-beat structure with each beat inside the band.

| Beat | Window | Easing | What moves | Cells touched |
| --- | --- | --- | --- | --- |
| **1. Arrival** | 0 – 420 ms | `ease_out_cubic` | Wavefront crosses 36 columns L→R; columns ahead of it are blank, not dimmed. Leading edge lifts toward white over `EDGE_WIDTH = 6` columns. | ≤ 167 |
| **2. Ignition** | 400 – 580 ms | `ease_out_cubic` on the way up, then down | The whole wordmark lifts to `mix(gradient, white, 0.55)` and settles back to the gradient. **Brightness only — nothing moves.** | 167 |
| **3. Sweep** | 560 – 1000 ms | `ease_in_out_sine` (already what `shimmer` uses) | The existing highlight band crosses once and exits. | ≤ 167 |
| **4. Settle** | 1000 ms | — | `welcome_at = None`, `wants_frames()` drops it, clock disarms. | 0 |

Beats overlap by 20 ms so they read as one gesture rather than three, which is
the same trick the current code plays with `REVEAL_FRACTION`.

Beat 2 is the addition, and it is the memorable one: it is a punctuation mark on
the arrival, it costs one extra `lerp_rgb` per cell per frame, and it is a pure
brightness change — **not on WebKit's vestibular trigger list** (§4.2), unlike
any scale, spin, bounce or shake.

The tagline and the four quick-start rows do **not** animate. They are the
information; they should be readable at t=0.

### 5.3 The Neovim rule

New, and the most valuable single change here:

> **Any keystroke, any agent event, or any scroll cancels the intro
> immediately** — set `welcome_at = None` and draw the settled banner on that
> frame.

Direct analogue of "it is removed as soon as the display is redrawn"
**[reported]**. Implementation: one line at the top of `handle_term_event` and
one in `on_event`. Consequence: a user who types instantly never sees an
animation at all, and never waits on one. This is what makes an intro
defensible in a tool rather than an indulgence.

### 5.4 Degradation

| Condition | Detected by | Behaviour |
| --- | --- | --- |
| Truecolor palette | `as_rgb(t.accent).is_some()` | All four beats. |
| 256 / 16 colour, or mono palette | `as_rgb` returns `None` | **Beat 1 only.** The arrival wavefront is a *visibility* change, so it needs no colour at all. Today `welcome_shimmer` returns early and these terminals get nothing (`src/tui.rs:4204`) — running beat 1 is strictly better and needs no new quantiser. Beats 2–3 are skipped because `theme::mix` collapses to a 0.5 threshold on non-RGB and a two-step "gradient" is a flicker, not a shimmer. |
| `NO_COLOR` | palette already resolves to mono | Same as above: beat 1 only. |
| ASCII glyphs (`theme::glyphs()` chose `ASCII`) | `!g.fine_blocks` | Use `BANNER_ART_ASCII` (§5.5). Beat 1 only — block-shading beats have nothing to shade. |
| Width < 40 columns | `area.width < 40` | No banner. One line: `koda — a fast terminal coding agent`. No animation. (36 art columns + 2 indent = 38; below 40 the art wraps and the whole design collapses.) |
| `Metrics::tiny` (< 64) but ≥ 40 | `m.tiny` | Banner drawn, **no animation** — a tiny terminal is a tiny terminal. |
| `Motion::Reduced` or `Off` | `!motion.animates()` | `welcome_at` never armed. Already correct (`src/tui.rs:1191`). |
| Not a TTY | `Motion::resolve(_, false) == Off` | Same. Already correct. |
| `CI` set and non-empty | new, §7.4 | `Motion::Off`. |

### 5.5 The ASCII banner

Fixes §2.3's second defect. **[measured]** five rows, 30 columns each, pure
ASCII (`[len(r) for r in rows] == [30]*5`, `all(r.isascii())`):

```
 _  __   ___    ____     _
| |/ /  / _ \  |  _ \   / \
| ' /  | | | | | | | | / _ \
| . \  | |_| | | |_| |/ ___ \
|_|\_\  \___/  |____//_/   \_\
```

(trailing spaces pad rows 1–4 to 30; they are load-bearing)

**[judgement]** on the exact face; the constraints that matter are that it is
`is_ascii()` and that every row is padded to equal length, so the wavefront
column index means the same thing on every row. Add a test asserting both,
mirroring `anim::tests::coarse_bar_is_pure_ascii`.

Consequence for `welcome_shimmer`: it currently closes over the module constant
`BANNER_ART` (`src/tui.rs:4207,4230`) while `show_welcome` picks the art
separately. Both must read the same `fn banner_art(g: &Glyphs) -> &'static
[&'static str]`, and the shimmer must derive its row count and column count from
that slice rather than from a hard-coded 6×36 — otherwise the ASCII path
shimmers a sixth row that does not exist.

### 5.6 The width fix

Replace `row.chars().count()` with `UnicodeWidthStr::width(row)` (the crate is
already a dependency — `src/tui.rs:48`), and add a test asserting every
`BANNER_ART` row has display width 36 and every glyph has width 1 under
`unicode_width`. This does not fix ambiguous-wide terminals — nothing in-process
can, since the terminal's resolution of UAX #11 Ambiguous is not observable — but
it makes the assumption explicit and testable. **[judgement]** Not worth
replacing the double-line face: it is the wordmark, and `spec-visual-language.md`
§6's ban on `╔═╗` is about *borders around content*, not about a logo. Worth a
comment saying so, since the two documents otherwise appear to contradict.

---

## 6. The ambient animation

### 6.1 The shape of the answer

Three tiers. All event-tied. None loops. None runs on a timer.

```
Tier 1  costume       varies the glyph set of a cell already animating   ~1 turn in 12
Tier 2  visitor       one-shot in the blank spacer row after real work   ~1 turn in 8, ≤1 per 10 min
Tier 3  easter egg    only when explicitly typed                          never automatic
```

Tier 1 is the one with the strongest evidence behind it. Tier 2 is the one the
user asked for ("some ANSI character that plays in the TUI funny and randomly").
Tier 3 is where anything bigger goes to live safely.

### 6.2 Tier 1 — the costume

**What.** `hint_row` already draws `g.thinking[sweep(elapsed) % len]` in a
single cell while a turn runs (`src/tui.rs:3646`). Once per turn — decided at
turn start, held for the whole turn — that six-glyph set is occasionally
swapped for an alternate.

**Why this is the right core.** It is the Claude Code spinner-verb pattern
applied to the glyph instead of the word, and that pattern is the single most
loved delight feature in §3.2 **[reported]**. It costs:

- zero additional cells,
- zero additional frames,
- zero additional wakeups,
- zero additional stop conditions (it ends when the turn ends, which is already
  handled),
- zero risk to layout (all frames are width 1, asserted by test).

**When.** Decided once, from `turn_started`, so it cannot flicker between
redraws — the same technique `working_message` and `tip_for` already use
(`src/tui.rs:147,144`). Probability **1 in 12** qualifying turns.

**Frequency rationale [judgement].** With `TIP_EVERY = 14 s` and
`WORKING_MSGS` rotating every 10 s, koda already has two rotating channels. A
third at 1-in-12 means a user doing thirty turns a day sees a costume about
twice a day: often enough to be noticed and talked about, rare enough that it
stays a small surprise rather than becoming chrome. Kano's attractive quality
depends on it being unexpected; a costume every turn is a theme, not a delight.

**The frames.** Six each, to match `SWEEP_INDEX`'s range 0..5. **[measured]**
every glyph below is East Asian **Neutral** — width 1 in both `width()` and
`width_cjk()`, so no terminal can widen them:

```
default (unchanged)  ·  ✻  ✽  ✶  ✳  ✢        (koda's UNICODE.thinking)
orbit                ⠁  ⠈  ⠐  ⠠  ⠄  ⠂
arc                  ◜  ◠  ◝  ◞  ◡  ◟
spark                ✦  ✧  ⋆  ∗  ⋆  ✧
corners              ▘  ▝  ▗  ▖  ▗  ▝
```

Verification:

```
python3 -c "import unicodedata as u; s='⠁⠈⠐⠠⠄⠂◜◠◝◞◡◟✦✧⋆∗▘▝▗▖';
print(sorted({u.east_asian_width(c) for c in s}))"
# ['N']
```

Note `·` (U+00B7) and `✽` (U+273D) in koda's *existing* default set **are**
Ambiguous **[measured]** — a pre-existing, low-severity issue the alternates
avoid. Worth a comment, not worth changing the default face.

**ASCII fallback.** `ASCII.thinking` is `[".","*","+","x","*","."]`. The
alternates: `["'","`",".",",",".","`"]`, `["-","\\","|","/","|","\\"]`,
`["o","O","o",".","o","O"]`. All `is_ascii()`.

**Off when:** `motion != Full`. Note carefully that `Motion::Reduced` still lets
the `Clock` arm (only `Off` disarms — `src/anim.rs:123`), because the elapsed
counter must keep updating; so the costume must gate on `motion.animates()`, not
on `clock.animating()`.

### 6.3 Tier 2 — the visitor

**Where.** `chunks[4]`, the spacer row (§2.1). Nothing is ever drawn there, its
height is fixed by the layout before any animation is considered, and it sits
between the tip row and the composer frame — adjacent to where the user's eye
already is, not in their peripheral vision, and not over any text.

**What.** A single braille cell bounces left-to-right across at most 24 columns,
starting at the row's left edge. The bounce is height-within-the-cell, using the
braille dot rows — the character changes, the cell does not move vertically, and
the row never reflows.

```
frames (row within the 2x4 braille cell, low -> high -> low)
  ⣀   U+28C0   dots 7,8      floor
  ⠤   U+2824   dots 3,6      low
  ⠒   U+2812   dots 2,5      mid
  ⠉   U+2809   dots 1,4      top
  ⠒   U+2812   dots 2,5      mid
  ⠤   U+2824   dots 3,6      low
```

Horizontal position: `x = ease_out_cubic(t) * span`, `span = min(24,
spacer.width - 2)`. Vertical frame: `hop = SWEEP_INDEX`-style triangle at
~6 hops across the span. Colour: `theme::mix(t.muted, t.accent, 0.35)` — which
degrades correctly on every palette because `mix` already handles non-RGB
(`src/theme.rs:502`). Not bold.

At the end of the span the cell is simply not drawn: it walks off, it does not
fade, it does not pop.

**Duration: 900 ms.** Inside NN/g's "larger motion" upper region, far under
WCAG 2.2.2's 5 s so no pause control is required by Level A **[reported]**, and
short enough that it is over before it can become the thing you are looking at.

**Trigger — this is the whole design.** It fires on the *transition* from busy
to not-busy, and only when all of these hold:

```
turn ended successfully           (no error event, not cancelled)
turn did real work                (>= 1 file written/edited this turn, OR turn ran >= 20s)
nothing is waiting on the user    pending.is_none() && asking.is_none()
no overlay is open                picker.is_none() && setup.is_none() && settings.is_none()
                                  && logs.is_none() && !plan_blocked
the user is looking at the bottom app.follow == true
the composer is empty             editor.is_empty()     (they are not mid-thought)
the row exists                    !m.tiny  (width >= 64, so spacer height == 1)
motion is full                    app.motion.animates()
ambient is enabled                cfg.ambient
rate limit not hit                last_ambient.elapsed() >= 10 min
dice                              1 in 8
```

Any keystroke during the 900 ms cancels it — `welcome_at`-style: clear the
state, redraw, done. Unlike `sl`, it is always interruptible **[reported]**.

**Why event-tied and not random.** This is the central research finding of the
document. lazygit's explosion fires when you nuke the working tree; Claude
Code's verbs appear while it thinks; `sl` fires when you mistype — every
surviving example of terminal delight is **caused by something the user did**.
Purely random interruption has no example I could find in a tool people keep
using, and it fails all three of §4.4's arguments at once. Tying the visitor to
"koda just finished something for you" also gives it *meaning*: it is a small
acknowledgement, not a screensaver.

**Why the frequency numbers [judgement].** 1-in-8 qualifying turns with a
10-minute floor works out to at most six sightings in a full working hour and
realistically two or three in a day of ordinary use. Rare enough to stay a
surprise; common enough that a new user meets it in their first session or two,
which is what makes it something they mention to someone else.

**Honest cost.** WebKit names peripheral horizontal movement as a vestibular
trigger **[reported]**. This animation *is* horizontal movement. Mitigations,
stated plainly rather than hand-waved: it is one cell, not a field; it spans
≤ 24 columns starting at the left, so on any terminal ≥ 64 columns it stays in
the foveal/parafoveal region rather than the periphery; it lasts 900 ms; it
never overlaps text; and `/motion` removes it along with everything else. I do
not claim it is free — I claim it is the cheapest form of "a character plays in
the TUI" that exists, and that everything cheaper is Tier 1.

**What it looks like on screen** (composer area, ≥ 64 columns):

```
  ✓ ready                                        @ file · ctrl+p mode · /keys
   ⠤                                                                            <- spacer row
  ╭──────────────────────────────────────────────────────────────────────────╮
  │ ❯                                                                        │
  ╰──────────────────────────────────────────────────────────────────────────╯
   koda · main · qwen2.5-coder ·  12%
```

**ASCII fallback.** `!g.fine_blocks` → hop frames `["_","-","~","^","~","-"]`,
same motion, same span. All `is_ascii()`.

### 6.4 Tier 3 — the deliberate easter egg

Anything bigger than Tier 2 goes behind an explicit command and is never
automatic. `/koda` (unlisted in `COMMANDS`, listed in `/keys` only after first
use) replays the intro's four beats plus a full-width sweep of the wordmark.
Precedent: `sl`, `cowsay`, `git`'s hidden porcelain — fun you go and fetch.

**[judgement]** This is optional and should be built last or never. It exists in
this document mainly to give the "wouldn't it be cool if…" ideas somewhere to
live that is not the main loop.

### 6.5 What was rejected, and why

| Rejected | Why |
| --- | --- |
| **Idle/looping ambience** (a fish, rain, a pet that wanders while you think) | Destroys the never-waking clock and the 8.6 µs idle frame; pins the host terminal's cursor blink solid and burns CPU/battery **[reported]**; unbounded, so WCAG 2.2.2 requires a control; and it contradicts koda's own status claim that it is idle. This is the single most important rejection. |
| **A timer-driven random interruption** | No surviving example in the prior art. Kano reverse quality. Interrupts thought at exactly the wrong moment by construction, because a timer knows nothing about what the user is doing. |
| **Anything drawn in the transcript** | `spec-streaming-motion.md` §1 makes it invariant that a line already on screen never changes bytes. Non-negotiable. |
| **Anything drawn in or near an approval prompt** (`pending`, `asking`) | A consent moment. Motion there is at best a misread risk and at worst manipulative. |
| **Screen shake, scale, spin, bounce-the-whole-panel** | Every one is on WebKit's vestibular trigger list **[reported]**, and Power Mode's shake toggle is the empirical proof that it is the first thing people turn off **[reported]**. |
| **Emoji** | Double-width under UAX #11 / Emoji_Presentation **[reported]**, so they break `truncate_line` and the powerline width maths; tofu on ASCII terminals; and `spec-visual-language.md` §6.9 already names emoji-as-decoration as a dated tell that makes a program "feel like a toy" (clig.dev's phrase). |
| **Confetti / particle bursts / full-width sweeps** | Large dynamic region → back at the 75 ms flicker ceiling per `spec-animation-engine.md` §2.1, and it repaints the transcript region, which §1.5 forbids. |
| **The blink attribute** | `spec-visual-language.md` §6.12: "Never." |
| **A randomised splash per launch** | §3.1. A wordmark is identity; identity should not be a dice roll. |
| **A persistent mascot** | The moment it is always there it is chrome: it must be laid out, it competes forever, and it can no longer be a surprise. |
| **A new `KODA_NO_AMBIENT` env var** | `anim.rs`'s stated stance is to honour conventions that exist rather than invent new ones, and koda already reads three reduced-motion spellings. One config key and `/motion` is enough. |
| **Depending on `tachyonfx` or `harmonica`** | `spec-animation-engine.md` §6 already settled this. Nothing here needs a spring; the existing easings cover every curve above. |

---

## 7. The rules that keep it safe

### 7.1 Idle stays free

`wants_frames()` gains exactly one clause:

```rust
|| self.ambient.is_some_and(|a| a.started.elapsed() < AMBIENT_LIFE)
```

`ambient: Option<Ambient>` where `Ambient { started: Instant, kind: AmbientKind }`.
Cleared in `draw` the same way `welcome_at` is (`src/tui.rs:3320`). When it
clears, `clock.sync(false)` on the next loop iteration returns the process to a
24-hour sleep. **No new interval, no new task, no new channel.** If the field is
`None` — which it is >99.9% of the time — the idle path is byte-identical to
today's.

### 7.2 No repaint when nothing moves

Frames are derived from `started.elapsed()` at draw time, as `sweep`, `shimmer`
and `welcome_shimmer` all already do. No per-frame state, no allocation in the
draw path, and a dropped frame skips ahead rather than falling behind.

### 7.3 No layout shift, ever

The spacer row is allocated by `Layout::vertical` before any ambient state is
consulted, and its height (`spacer`) depends only on `m.tiny`. Drawing into it
cannot move anything. Render with `Paragraph::new(Line::from(spans))` into
`chunks[4]` and **never** `Clear` — the panel background painted at
`src/tui.rs:3186` must survive, exactly as it does for `hint_row` and the tip
row. When the animation ends the row is blank again, which is its resting state,
so there is no visible jump when it stops — the same argument `welcome_shimmer`
already makes in its doc comment.

### 7.4 Motion, and one new signal

| Setting | Effect |
| --- | --- |
| `/motion` off (`Motion::Reduced`) | Tier 1 and Tier 2 both gone. Gate on `motion.animates()`, **not** on `clock.animating()` — `Reduced` still arms the clock (`src/anim.rs:123`). |
| `Motion::Off` (`TERM=dumb`, non-tty) | Gone; clock never arms. |
| `KODA_REDUCED_MOTION` / `NO_MOTION` / `REDUCED_MOTION` | → `Reduced`. Already handled. |
| **New:** `CI` present and non-empty | → `Motion::Off`. GitHub Actions sets `CI=true` on every workflow **[reported]**; a CI job with an allocated pty currently escapes the non-tty check. Four lines in `Motion::resolve`. |
| **New:** `[ui] ambient = true` | Turns Tier 2 (only) off without turning off spinners and gauges. Some people want a live status row and no visitor; `/motion` is too blunt for that. Tier 1 rides on `motion` alone. |

`NO_COLOR` is deliberately **not** wired to motion: it is a colour convention,
and no-color.org's text is specifically about "the addition of ANSI color"
**[reported]**. Colour off + motion on is a legitimate combination.

### 7.5 Non-interference, stated as invariants

1. Never renders outside `chunks[4]`.
2. Never renders when `pending`, `asking`, `picker`, `setup`, `settings` or
   `logs` is `Some`, or when `plan_blocked`.
3. Never renders when `!app.follow` (user is reading history).
4. Never renders when the composer is non-empty.
5. Cancelled by the first key event, and by any agent event that starts a new
   turn.
6. Never longer than 5 s (in fact 900 ms), so WCAG 2.2.2 Level A holds without
   a dedicated control.
7. Every glyph in every frame set has `unicode_width == 1`; every ASCII set is
   `is_ascii()`.
8. Rate-limited to at most one per 10 minutes regardless of dice.

### 7.6 Tests to add, in the existing style

```
ambient_never_selected_without_full_motion
ambient_span_never_exceeds_the_row
ambient_lifetime_is_under_wcag_five_seconds
ambient_frames_are_all_single_width          (unicode_width::width == 1)
ambient_ascii_frames_are_pure_ascii
ambient_rate_limit_blocks_a_second_turn
wants_frames_drops_ambient_after_its_lifetime
banner_rows_are_equal_display_width          (fixes §2.3)
banner_ascii_is_pure_ascii                   (fixes §2.3)
intro_cancels_on_first_key
```

Each mirrors an existing test in `anim.rs` — same naming voice, same
"assert the property the bug would violate" shape.

---

## 8. What NOT to build

Short list, for the reviewer who reads only this section:

- **No idle loop.** Not a pet, not rain, not a fish, not a matrix. If koda is
  idle, nothing on screen moves. This is the line.
- **No timer-driven surprise.** Every ambient beat is caused by something that
  happened.
- **Nothing in the transcript, the composer, or an approval prompt.**
- **No shake, scale, spin, or bounce of any region.**
- **No emoji.**
- **No full-width or full-screen effect.**
- **No blink attribute.**
- **No new environment variable.**
- **No new dependency.** Tier 1 and Tier 2 are together roughly 120 lines of
  `std` plus the existing `anim` primitives; `spec-animation-engine.md` §6's
  "net zero new dependencies" result stands.
- **No sound.** koda already has `notify_user` for the one case that warrants
  attention (`src/tui.rs:246`); delight does not get to ring the bell.

---

## 9. Build order

1. **Intro fixes** (§5.3, §5.5, §5.6) — the Neovim cancel rule, the ASCII
   banner, the width fix. All small, all pure wins, none of them animation.
2. **Intro beats** (§5.2) — retime to 1000 ms, add the ignition beat, run beat 1
   on non-truecolor.
3. **Tier 1 costume** (§6.2) — highest evidence, lowest risk, no new draw code.
4. **`CI` in `Motion::resolve`** (§7.4) — four lines, and it makes step 5 safe
   in the environment where animation is most useless.
5. **Tier 2 visitor** (§6.3) — the new state field, the trigger predicate, the
   spacer-row draw, the rate limiter, the tests.
6. **Tier 3** (§6.4) — optional, or never.

If only steps 1–3 ever land, koda has a better intro and a real piece of
delight, and it has taken on no new risk at all.

---

## 10. Where this disagrees with the existing specs

- `spec-animation-engine.md` §2.1 sets the default frame budget at 33 ms with
  sync and offers 16 ms; the shipped `anim::frame_budget` uses 33/75. **This
  document follows the shipped code, not the spec.** The ambient region is ≤ 24
  cells, comfortably inside the spec's "small dynamic region" row either way.
- `spec-visual-language.md` §6.1 names double-line box drawing as the #1 dated
  tell. `BANNER_ART` uses `╔═╗` as drop shadows. These do not actually conflict —
  §6.1 is about borders around content — but the code should say so, because the
  next reader will file it as a bug.
- `spec-streaming-motion.md` §5.9 forbids "decorative reveals" in a coding tool,
  citing arXiv:2504.20365 on backwards/random text reveal. The intro's wavefront
  is a *wordmark* entrance, not a text reveal, and the study's finding is about
  reading comprehension of streamed prose. I read that as out of scope rather
  than as permission — and it is why §8 forbids anything decorative in the
  transcript.

---

## 11. Citations

**Accessibility and motion**

- W3C. *Understanding Success Criterion 2.2.2: Pause, Stop, Hide* (WCAG 2.2).
  https://www.w3.org/WAI/WCAG22/Understanding/pause-stop-hide.html
- WebKit. *Responsive Design for Motion* (2017).
  https://webkit.org/blog/7551/responsive-design-for-motion/
- Val Head. *Designing Safer Web Animation For Motion Sensitivity.* A List
  Apart.
  https://alistapart.com/article/designing-safer-web-animation-for-motion-sensitivity/
- The A11Y Project. *A primer to vestibular disorders.*
  https://www.a11yproject.com/posts/understanding-vestibular-disorders/

**Animation timing and delight**

- Laubheimer, P. *Executing UX Animations: Duration and Motion Characteristics.*
  Nielsen Norman Group. https://www.nngroup.com/articles/animation-duration/
- Nielsen Norman Group. *A Theory of User Delight: Why Usability Is the
  Foundation for Delightful Experiences.*
  https://www.nngroup.com/articles/theory-user-delight/
- Nielsen Norman Group. *Three Pillars of User Delight.*
  https://www.nngroup.com/articles/pillars-user-delight/
- Kano, N., Seraku, N., Takahashi, F. & Tsuji, S. (1984). *Attractive Quality
  and Must-Be Quality.* Overview and the later reverse-quality category:
  https://en.wikipedia.org/wiki/Kano_model — used as vocabulary, not as
  evidence about terminals.

**CLI/TUI design guidance**

- *Command Line Interface Guidelines* (clig.dev). https://clig.dev/
- no-color.org. https://no-color.org/

**Prior art (source and docs)**

- Neovim. `runtime/doc/starting.txt`, "Intro message" / `:intro` / `'shortmess'`
  `I`.
  https://github.com/neovim/neovim/blob/master/runtime/doc/starting.txt ·
  https://neovim.io/doc/user/starting.html
- lazygit. `docs/Config.md` — `gui.animateExplosion`, `gui.showRandomTip`,
  `gui.spinner.{frames,rate}`, `disableStartupPopups`, `nerdFontsVersion`.
  https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md
- lazygit. `pkg/gui/presentation/loader.go` — wall-clock-derived spinner index.
  https://github.com/jesseduffield/lazygit/blob/master/pkg/gui/presentation/loader.go
- btop. README — `update_ms` (default 2000), `background_update`,
  `terminal_sync`, `graph_symbol`.
  https://github.com/aristocratos/btop
- GitHub CLI. PR #10773, *Introduce option to opt-out of spinners*
  (`GH_SPINNER_DISABLED`). https://github.com/cli/cli/pull/10773
- charmbracelet/bubbles. `spinner/spinner.go` — default frame sets and FPS.
  https://github.com/charmbracelet/bubbles/blob/master/spinner/spinner.go
- charmbracelet/harmonica. https://github.com/charmbracelet/harmonica
- hoovercj/vscode-power-mode. README — `powermode.shake.enabled`,
  `explosions.explosionFrequency`.
  https://github.com/hoovercj/vscode-power-mode
- mtoyoda/sl. https://github.com/mtoyoda/sl
- cmatrix. https://github.com/abishekvashok/cmatrix ·
  pipes.sh https://github.com/pipeseroni/pipes.sh ·
  asciiquarium https://robobunny.com/projects/asciiquarium/html/
- fastfetch. https://github.com/fastfetch-cli/fastfetch

**Claude Code spinner verbs** — community reverse-engineerings, **not** primary;
cited only as evidence that the feature is loved enough to have an ecosystem.

- https://github.com/levindixon/tengu_spinner_words
- https://github.com/claude-code-book/spinner-verbs-dictionary
- https://github.com/wynandw87/claude-code-spinner-verbs

**Terminal technique**

- Parpart, C. *Synchronized Output* (DEC private mode 2026) specification.
  https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036
- tmux `CHANGES` — "Add support for applications to use synchronized output mode
  (DECSET 2026) to prevent screen tearing during rapid updates"; earlier
  iTerm2-style synchronized updates in 3.4; DECRQM 2026 response.
  https://github.com/tmux/tmux/blob/master/CHANGES
- Unicode. *UAX #11: East Asian Width.* https://www.unicode.org/reports/tr11/
- Unicode. *UAX #29: Unicode Text Segmentation* (grapheme clusters).
  https://www.unicode.org/reports/tr29/
- `unicode-width` crate — `width()` vs `width_cjk()` resolution of Ambiguous.
  https://docs.rs/unicode-width/
- GitHub Changelog. *GitHub Actions sets the `CI` environment variable to true.*
  https://github.blog/changelog/2020-04-15-github-actions-sets-the-ci-environment-variable-to-true/

**Reduced-motion asks in agent TUIs** (open requests, not conventions)

- herdr discussion #1316 — continuous ~8 fps repaint suppresses host cursor
  blink; asks for `reduce_motion`.
  https://github.com/herdrdev/herdr/discussions/1316
- anthropics/claude-code #22913 — reduced motion configuration option.
  https://github.com/anthropics/claude-code/issues/22913
- anthropics/claude-code #37283 — TUI flicker in tmux from missing DECSET 2026.
  https://github.com/anthropics/claude-code/issues/37283

### A note on what is not verified

- The 1000 ms intro total, the 420/180/440 ms beat split, the 1-in-12 and 1-in-8
  probabilities, the 10-minute floor, the 24-column span and the 900 ms visitor
  lifetime are **[judgement]**, informed by NN/g's 100–500 ms band and WCAG's 5 s
  threshold but not measured against users. They are the numbers most likely to
  need tuning after the first week of living with it.
- The Claude Code spinner-verb sources are community extractions from a shipped
  binary. The *existence* of the ecosystem is the evidence I rely on; the exact
  verb counts (187 / 3 900) are as reported by those repositories and were not
  independently checked here.
- The herdr report's cursor-blink mechanism is a plausible, specific claim from a
  single bug report. It was not reproduced here.
- No usability testing of any kind was run for this document. Every "delightful"
  and "annoying" judgement is inference from the prior art in §3 plus the cited
  literature in §4.
