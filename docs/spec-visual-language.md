# Spec: Visual Language

Scope: what makes koda's TUI *look* modern in 2026. This is about visual design
language — layout, type hierarchy, spacing, colour, and the devices that read as
fresh versus dated. Animation/motion is specified separately; this spec is the
static skin those motions move across.

Grounding constraints (from prior research, restated so this spec stays inside
them):

- **Frame ceiling ~75ms / ~13fps** before flicker shows in some terminals. Any
  visual device that forces a full repaint on every keystroke is out.
- **Semantic colour roles** degrade better than literal RGB. koda already does
  this (`theme.rs`: `accent`, `muted`, `diff_add`…). This spec adds roles, never
  hardcoded colours.
- **Motion is opt-out**; the static design must stand on its own with motion
  disabled. Everything here works with zero animation.
- **Static vs dynamic regions separated** to bound redraws. Visual grouping
  devices (fills, rails) must not couple a static block's paint to a live one.
- **8.6µs idle frame on a 4000-block transcript must not regress.** Prefer
  devices that are cheap per cell: a background tint or a single rail glyph is
  one style write per cell; a full box border is a per-block perimeter walk plus
  two extra rows. koda already chose fills over boxes — keep that bias.

koda's current baseline is already good: exactly one border depth on screen
(the terminal edge frames the app), rounded corners, per-kind block fills,
semantic tokens, ANSI/mono/NO_COLOR fallbacks. This spec is mostly *sharpening*
that, not overturning it.

---

## 0. What the field converged on (evidence)

Sources read for this spec:

- **Charmbracelet lipgloss** (`charm.land/lipgloss/v2`) — the reference for the
  modern TUI look. Key takeaways: a CSS-like box model (padding, margin, border,
  align) treated as first-class; adaptive light/dark colours resolved at runtime
  (`LightDark`, `HasDarkBackground`); automatic colour *downsampling* to the best
  profile (truecolor → 256 → 16 → mono) rather than failing; rounded borders as
  the default "nice" border; `Blend1D`/`Lighten`/`Darken` for deriving a palette
  from a seed rather than hand-picking every value; whitespace placement
  (`Place`, `PlaceHorizontal`) as a layout primitive equal to borders.
- **Textual** (textualize.io) — a full CSS box model in the terminal:
  `padding`/`border`/`margin` with content-box vs border-box sizing, `fr`
  fractional units, `dock`, alpha-blended colours (`rgba`, translucent tints),
  `outline` (overlaps content, doesn't resize), border *titles/subtitles*
  embedded in the top/bottom edge, and themeable semantic colour variables.
  Textual promotes: alpha tints for layering, dock for persistent chrome,
  fractional sizing for responsive splits.
- **lobehub "tui-design" skill** (castle-x) — prescribes the four-section page:
  **Header → Info Bar → Content → Footer**, a single fixed separator width (60),
  symmetric logo padding, dim colour for hints/separators, visual-width-aware
  column alignment (pad first, style after), and a named palette
  (primary `#00D4AA`, secondary `#FF6B6B`, accent `#4ECDC4`, dim `#666666`).
- **clig.dev** — UX principles: human-first, "say just enough", use colour with
  *intention* (if everything is coloured, colour means nothing), disable colour
  when not a TTY / `NO_COLOR` / `TERM=dumb`, increase information density,
  responsive (<100ms) beats fast, tell the user when state changes.
- **Modern agent CLIs** — the 2026 design ranking that keeps recurring:
  **opencode** (most polished: inline diffs, session state, tool calls rendered
  as composed blocks), **Claude Code** (deliberately minimal/utilitarian — near
  plain-text, little chrome), **Codex CLI** (basic terminal styling), **aider**
  (git-native, plain, colour-coded diffs). The spread tells us the modern band
  runs from "minimal plain text with intentional colour" (Claude Code) to
  "composed blocks with tints and inline diffs" (opencode). Both ends read as
  modern; the dated look is neither of these (see §6).

The convergent conclusion across all five: **modern = restraint + rhythm +
semantic colour + whitespace grouping.** Dated = heavy boxes, rainbow colour,
inconsistent spacing, ALL-CAPS chrome everywhere.

---

## 1. LAYOUT

### 1.1 The pattern everyone converges on

Two archetypes dominate, and they are the *same* skeleton at different densities:

**A. Full-screen four-section page** (lobehub, Textual apps, opencode dashboards):

```
┌ Header ───────────────────────  persistent identity + global context
├ Info Bar ─────────────────────  context that changes per screen/state
│ Content ──────────────────────  the scrollable body (the only growable region)
│                                  ← whitespace lives here, not in chrome
└ Footer ───────────────────────  status + key hints
```

**B. Conversation transcript + composer** (Claude Code, aider, koda today):

```
Header line          persistent context, one row
Transcript           scrollable, grows; the whole visual budget
─────                one thin rule separating live input from history
Composer             input, 1–N rows, grows with content
Status/hints line    one row, merges state + contextual keys
```

koda is archetype B and should stay B. Map koda's existing regions onto the
lobehub four-section vocabulary so the two research streams stay coherent:

| lobehub section | koda region | Static or dynamic |
|---|---|---|
| Header | top context line | mostly static (model, cwd, branch) |
| Info Bar | *(fold into header right-align, or the turn's first line)* | dynamic |
| Content | transcript | dynamic tail, static body |
| Footer | rule + composer + status/hints line | dynamic |

**Do not** add a second full-width Info Bar row to koda. In a transcript app the
info bar's job (per-turn context) is better carried by the block header of the
active turn. Reserve horizontal chrome rows — each fixed row steals from the
transcript, which is the product.

### 1.2 Where status goes

- **Global, persistent** (model, working dir, git branch, token budget): the
  **header**, one row, top of screen. Right-align the volatile parts (token
  count, context %) so the eye finds them in a fixed spot.
- **Transient/state** (thinking, running a tool, error): the **footer** status
  line, bottom. This is where the eye already rests after typing.
- **Per-block** (which tool, exit status, duration): the block's own header row,
  inline in the transcript — never in global chrome.

Rule: **global context at the edges, local context inline.** Never make the user
scroll the transcript to learn global state, and never put per-turn detail in a
fixed bar where it fights for the one row it occupies.

### 1.3 Dividing content areas

- The transcript is the **only** region that grows. Everything else is fixed
  height or content-sized. This is what keeps layout math trivial and redraws
  bounded (satisfies static/dynamic separation).
- Use **fractional/`fr`-style thinking** (Textual) only if you ever add a side
  panel: content `1fr`, panel fixed or `min-width`. koda has no side panel today;
  don't add one for its own sake — a transcript app reads best full-width up to a
  **max line measure** (see §3.4).
- Overlays/modals are the *one* place a full border is allowed (koda already does
  this). A modal is a deliberate layer break; the box signals "this is not the
  document."

### 1.4 Use of whitespace

Whitespace is a first-class layout device, ranked #1 in §5. lipgloss ships
`Place`/`PlaceHorizontal` as peers of borders precisely because empty space
*is* the grouping. Budget:

- **Left gutter** of 1–2 cells on the transcript body. Content that starts at
  column 0 reads as "printed by a script"; a small consistent gutter reads as
  "composed." koda's rail glyph can live in this gutter.
- **Right margin**: cap the text measure (§3.4) rather than letting prose run to
  a 200-column terminal edge.
- **Vertical breathing room** between turns (§3). Never between tightly-related
  rows within a block.

---

## 2. TYPOGRAPHY WITHOUT FONTS

A terminal gives one font, one size. Hierarchy is built from four levers only:
**weight** (bold / normal / dim), **case**, **colour**, and **spacing/position**.
That's the whole toolbox. The craft is spending each lever deliberately and never
stacking all four on one element (that reads as shouting — see §6).

### 2.1 The four levers, ranked by how much hierarchy they buy

1. **Dim vs normal vs bold** (weight). The strongest, most portable signal.
   Survives `NO_COLOR` and mono. koda's `theme.rs` already models this:
   `body()`, `strong()` (bold), `dim()` (muted fg). This is the backbone.
2. **Colour** (semantic role, not decoration). Accent for the one thing that
   matters in a block; muted for everything ambient. Degrades to weight in mono.
3. **Case**. Reserve UPPERCASE for *labels/eyebrows* only (short, ≤ ~12 chars).
   Never body text, never headings longer than a couple words — long uppercase
   runs are the single most dated device in a TUI (§6).
4. **Spacing / indentation / position**. A blank line above and a left gutter do
   more for "this is a new section" than any glyph.

### 2.2 Concrete 4-level hierarchy for koda

| Level | Role | Recipe | koda token(s) |
|---|---|---|---|
| **L1 — Section / turn header** | "a new unit starts here" | **bold** + `accent`/`heading` colour + blank line above + left gutter | `strong()` fg `heading` |
| **L2 — Label / eyebrow** | naming a field, tool, or state | **UPPERCASE**, ≤12 chars, `muted` colour, *not* bold | `dim()`, uppercased |
| **L3 — Body** | the actual content | normal weight, `text` colour, default case | `body()` |
| **L4 — Meta / caption** | timestamps, counts, hints, paths | `muted` (dim) colour, normal weight, often right-aligned or trailing | `dim()` |

Worked example — a tool-call block header:

```
  READ  src/theme.rs                                  312 lines · 4ms
  └L2─┘ └────────L3────────┘                          └──────L4──────┘
  eyebrow  content (normal)                            meta (dim, right)
```

And a turn/section header:

```
                                    ← blank line (L1 spacing lever)
  ● Assistant                       ← L1: bold + accent glyph + bold text
  Here is the plan …                ← L3: body
```

Rules that keep this from degrading:

- **Never** combine bold + uppercase + colour + underline on one run. Pick at
  most two levers per element. L1 uses bold+colour; L2 uses case+colour(dim);
  they don't overlap.
- **Underline is not a hierarchy lever** here. Reserve it for hyperlinks
  (lipgloss/OSC-8) if supported; otherwise don't use it. Underlined headings
  read as 1990s.
- **Bold is precious.** If more than ~one element per block is bold, none of them
  read as bold. clig.dev's "use colour with intention" applies equally to weight.
- In mono/`NO_COLOR`, L1 collapses to bold, L2 to uppercase+dim(=faint), L4 to
  faint. Hierarchy survives because levers 1 and 3 (weight, case) don't need
  colour. This is why the recipe leads with weight, not colour.

---

## 3. SPACING RHYTHM

The single highest-leverage, lowest-cost modernizer. Consistency matters more
than the specific numbers: a UI with *wrong-but-consistent* spacing reads as
designed; a UI with *right-but-random* spacing reads as accreted. Pick a scale,
put it in one place, and never write a raw blank-line count anywhere else.

### 3.1 The vertical scale

Define one spacing unit = **1 blank row**. Allowed values: **0, 1, 2**. That's
the whole scale. Nothing gets 3+ blank rows (that reads as "something broke").

| Between | Blank rows |
|---|---|
| Rows *within* one block (header → body → footer of a tool card) | **0** |
| Consecutive blocks of the *same* turn (two tool calls) | **1** |
| Between *turns* (user ↔ assistant) | **1** (rely on fill/colour, not 2) |
| Around a modal/overlay vs the dimmed transcript | **1** each side |
| Above an L1 section header | **1** |

Note the lobehub skill's rule — *symmetric* padding around the logo (equal blank
lines above and below), and "never `\n\n` after logo, it creates asymmetric
padding." That asymmetry-is-a-bug principle is the whole point: **spacing must be
symmetric and derived from the scale, never eyeballed.**

### 3.2 The horizontal (indentation) scale

Indent step = **2 columns**. Allowed depths: 0, 2, 4, 6. Beyond depth 3 (6 cols),
don't indent further — switch to a different device (a rail, a collapsed
summary). Deep indentation in an 80-col terminal eats the measure.

| Element | Indent |
|---|---|
| Transcript body left gutter | 2 |
| Nested list / tree child | +2 per level |
| Code block inside prose | 2 (plus the block's own fill) |
| Continuation of a wrapped meta line | align under content start |

### 3.3 Why consistency beats the values

- The eye entrains to a rhythm. Once every block-gap is 1 row, a 0-row gap
  *means* "these belong together" and a 2-row gap *means* "hard break" — the
  spacing itself becomes signal, for free, with no glyphs or colour.
- Random spacing destroys that signal: the reader can't tell intentional grouping
  from noise, so they fall back to reading every glyph, which is slower and reads
  as amateur.
- Consistency is also cheap to enforce and cheap to render: a single
  `const SPACING: [u16; 3] = [0, 1, 2];` and a `gap(Kind)` helper means the
  values live in one place, and blank rows are the cheapest possible cell (no
  style, no glyph) — no risk to the 8.6µs idle frame.

### 3.4 The measure (line length)

Cap rendered prose at a **max measure of ~80–100 columns** even when the terminal
is wider. Reasons: readability (long lines lose the eye on return sweep) and
modern feel (full-bleed 200-col text is the most script-like, dated look). Code
blocks and diffs may run wider (they're scannable, not read left-to-right). This
matches lipgloss `Width()` + `Place` and Textual `max-width`.

---

## 4. COLOUR

### 4.1 How many roles

koda's `Theme` already defines a strong role set. The modern move is *fewer,
better-chosen roles applied sparingly*, not more colours. Target role count and
their jobs:

**Structural (5):**
`text` (body), `muted` (dim/meta/hints/rules), `surface` (panel bg),
`border`, `border_focus`.

**Accent (2):**
`accent` (the one attention colour — selection, active, primary action),
`accent_alt` (secondary highlight; use rarely).

**Status (4):**
`success`, `warning`, `error`, `info`.

**Domain fills (5):**
`bg_user`, `bg_tool`, `bg_tool_err`, `bg_panel`, `bg_selected` — subtle tints,
not saturated blocks.

**Diff (3) + Syntax (6):** already present; syntax should follow an established
editor palette (koda uses VS Code Dark+ values) so code looks familiar.

That's koda's current set and it's the right *shape*. The discipline is usage:
clig.dev — *"if everything is a different colour, then the colour means
nothing."* On any given screen, **accent should appear on at most one or two
things.** Everything ambient is `text` or `muted`.

### 4.2 Keeping contrast accessible

- **Body text vs background: aim for WCAG AA ~4.5:1.** For terminal chrome the
  practical rule: body `text` should be near-white on dark / near-black on light;
  `muted` must stay legible (don't let "dim" become "invisible" — koda's
  `muted rgb(119,125,136)` on `surface rgb(29,33,41)` is ~4.9:1, good).
- **Never encode meaning in colour alone** (colour-blind + mono users). Pair
  every status colour with a glyph: `✓ success`, `✗ error`, `● running`. koda's
  glyph set already does this — keep the pairing mandatory.
- **Tints must stay subtle.** Domain fills are ~5–10% lightness shift from
  `surface`, not a saturated block. koda's `bg_user rgb(34,29,26)` vs
  `surface rgb(29,33,41)` is exactly this — a warm nudge, readable body text on
  top. A saturated fill (bright block behind text) is the dated look.
- **Downsample, don't fail** (lipgloss): truecolor → 256 → 16 → mono. koda's
  ANSI theme (uses the terminal's own 16) and MONO theme already provide the
  bottom rungs. Keep semantic roles so downsampling is one lookup table.

### 4.3 Dark and light

- Ship both. Don't invert a dark palette to fake a light one — pick light values
  deliberately (koda's `SOLARIZED_LIGHT` is a real light theme, not an inversion).
- Prefer **runtime background detection** (lipgloss `HasDarkBackground` /
  Textual auto) where the terminal supports the query, falling back to configured
  theme. This is why koda defaults `"auto"` → a known dark palette: fills need a
  known background, and guessing wrong is worse than a fixed default.
- Light themes need *higher* border/tint contrast than dark ones to read (light
  backgrounds wash out subtle tints faster). Bump light-theme fills toward ~8–12%.

### 4.4 Reference palettes (actual hex)

Popular, battle-tested palettes — koda ships most of these; keep them as the
menu because users recognise their own theme:

| Palette | text | accent | success | error | notes |
|---|---|---|---|---|---|
| **Catppuccin Mocha** | `#CDD6F4` | `#89B4FA` | `#A6E3A1` | `#F38BA8` | soft, low-contrast, very 2026 |
| **Tokyo Night** | `#C0CAF5` | `#7AA2F7` | `#9ECE6A` | `#F7768E` | dim indigo base |
| **Nord** | `#D8DEE9` | `#88C0D0` | `#A3BE8C` | `#BF616A` | desaturated, calm |
| **Gruvbox Dark** | `#EBDBB2` | `#83A598` | `#B8BB26` | `#FB4934` | warm, higher contrast |
| **Dracula** | `#F8F8F2` | `#8BE9FD` | `#50FA7B` | `#FF5555` | vivid, high contrast |
| **Rosé Pine** | `#E0DEF4` | `#9CCFD8` | `#31748F` | `#EB6F92` | muted, muted, muted |
| **Solarized Light** | `#586E75` | `#268BD2` | `#859900` | `#DC322F` | the reference light theme |
| lobehub default | `#FFFFFF` | `#4ECDC4` | `#00D4AA` | `#FF6B6B` | teal/coral, dim `#666666` |

Construction tip (lipgloss `Lighten`/`Darken`/`Blend1D`): pick **one seed accent**
and derive `accent_alt`, hover, and selected states by lightness shifts rather
than hand-picking, so the family stays harmonious. Derive `muted` as `text`
blended ~45% toward `surface`.

---

## 5. VISUAL DEVICES, RANKED (modern → dated)

How each grouping device reads in 2026, best first:

1. **Whitespace-only grouping** — *most modern.* A blank line + shared indent is
   enough to say "these belong together." Zero glyphs, zero colour, cheapest to
   render, survives every terminal. This is the Claude Code / minimalist end and
   it reads as confident. Use as the default.
2. **Background fills / tints** — *modern.* A subtle per-kind tint (koda's
   `bg_user`/`bg_tool`) groups a block without spending two rows on a border and
   without a hard edge. Reads composed. Cost: one bg style write per cell (fine
   for 8.6µs). The opencode end. **Requires a known background** → only on themes
   with real colours (koda already gates this).
3. **A single left rail / accent bar** (`│` or `▌` in a gutter) — *modern.* One
   glyph column marks "this block is X." Cheap (one cell/row), scannable, works in
   mono. koda has `rail`/`user_bar`. Great for the *active* or *user* block.
4. **Thin horizontal rules** (`─`) — *neutral, use sparingly.* One dim rule to
   separate composer from transcript is fine (koda does this). A rule *between
   every block* is noise — that's what fills/whitespace are for. Full-width rules
   everywhere read as a 2010-era config screen.
5. **Full boxes / borders around content** — *dated when overused.* A border
   costs two rows + two columns + a perimeter walk, hard-edges everything, and
   stacks badly (a box in a box in a box is the single most dated TUI look).
   **Allowed only for a true layer break: modals/overlays.** Never box the
   transcript, never box every message. koda's "exactly one border depth" rule is
   exactly right — keep it.
6. **ASCII-art dividers, `====`, `####`, `****` separators, drop shadows,
   double-line borders `╔═╗`** — *dated, avoid.* These are the tells of a
   90s/2000s TUI (§6).

Ranking rationale ties back to constraints: the top three are the cheapest to
render *and* the most modern — there's no tradeoff. The device that's expensive
(full borders, #5) is also the one that reads dated. Design and performance agree.

---

## 6. WHAT MAKES A TUI LOOK DATED (be concrete)

Specific tells, roughly in order of how badly they age a UI:

1. **Double-line / heavy box-drawing borders around everything** (`╔══╗`, `║`),
   especially nested boxes. The #1 dated signal. Turbo Pascal, 1992.
2. **Long runs of ALL-CAPS text** beyond short labels — full sentences or
   multi-word headings in caps. Reads as a mainframe form.
3. **Rainbow colour / colour as decoration** — every field a different bright
   colour, saturated foregrounds, red+green+yellow+blue all on one screen. Violates
   clig.dev "colour with intention." Modern UIs are mostly `text` + `muted` with
   *one* accent.
4. **Saturated background blocks** behind text (bright blue bar with white text as
   a "header"). The modern version is a *subtle tint*, not a fill you'd see from
   across the room.
5. **Inconsistent spacing** — 0 blank lines here, 3 there, no discernible rhythm.
   Reads as accreted, not designed. (This is why §3 exists.)
6. **`=====`, `-----`, `*****`, `#####` ASCII dividers** and `>>>`/`<<<` arrow
   spam as decoration.
7. **Underlined headings** and underline used for emphasis (vs hyperlinks).
8. **Full-bleed text** running to a 200-column edge with no measure cap (§3.4).
9. **Emoji sprayed as bullets/decoration** on every line (a couple of *meaningful*
   status glyphs is modern; 🎉✨🚀 confetti is a toy). clig.dev: "easy to overdo
   it and make your program look cluttered or feel like a toy."
10. **Sharp `+`/`-`/`|` ASCII corners when Unicode is available.** Rounded
    (`╭╮╰╯`) reads current; koda already defaults to rounded and falls back to `+`
    only when it must.
11. **Progress shown as spinning `|/-\`** with no easing/label, or worse, a bar
    that redraws the whole line and flickers. (Motion spec covers the fix.)
12. **Blink attribute.** Never. It's supported, it's dated, it's an accessibility
    problem.

The through-line: **dated = loud, boxed, inconsistent, decorative. Modern =
quiet, grouped-by-space, consistent, semantic.**

---

## 7. BEFORE / AFTER SKETCHES

### 7.1 A tool-call result

**Before (dated):**

```
+==============================================================+
|| TOOL EXECUTION RESULT: READ_FILE                           ||
+==============================================================+
|| FILE: src/theme.rs                                         ||
|| STATUS: **** SUCCESS ****                                  ||
|| LINES READ: 312    TIME: 4ms                               ||
+==============================================================+
||  1: //! Theming: semantic tokens, not hardcoded colours.  ||
||  2:                                                        ||
||  3: use ratatui::style::{Color, Modifier, Style};          ||
+==============================================================+
>>> NEXT ACTION <<<
```

Tells: double-line box, nested boxes, ALL-CAPS everywhere, `****` decoration,
`>>>` arrows, everything boxed, no whitespace rhythm.

**After (modern):**

```
  READ  src/theme.rs                                  312 lines · 4ms ✓
    1  //! Theming: semantic tokens, not hardcoded colours.
    3  use ratatui::style::{Color, Modifier, Style};

  ● koda
```

Tells: L2 uppercase *eyebrow* only (`READ`), body normal-case, meta dim and
right-aligned with a status glyph, a subtle `bg_tool` tint (not drawn here) groups
the block, 2-col gutter, one blank row before the next turn. No border at all —
the terminal edge and the tint do the framing.

### 7.2 The overall screen

**Before (dated):**

```
╔════════════════════════ KODA v1.0 ════════════════════════╗
║ MODEL: gpt-4o | DIR: /home/u/proj | BRANCH: main           ║
╠════════════════════════════════════════════════════════════╣
║ USER> refactor the theme module                            ║
║ ------------------------------------------------------------ ║
║ ASSISTANT>                                                   ║
║ Sure! Here is what I'll do:                                  ║
║ 1. READ THE FILE                                            ║
║ 2. APPLY CHANGES                                            ║
╠════════════════════════════════════════════════════════════╣
║ [ENTER]=send [CTRL-C]=quit [TAB]=complete [ESC]=cancel      ║
╚════════════════════════════════════════════════════════════╝
```

Tells: full double border framing the whole app, `====` and `----` rules
everywhere, ALL-CAPS chrome and body, `USER>`/`ASSISTANT>` shouting labels, key
hints as a boxed `[KEY]=action` wall.

**After (modern):**

```
 koda · gpt-4o · ~/proj · main                        18k/200k · 9%

 ▌ refactor the theme module

 ● koda
 Here's the plan:
   1. Read the current theme module
   2. Derive muted from text instead of hardcoding it

   READ  src/theme.rs                                 312 lines · 4ms ✓

 ─────────────────────────────────────────────────────────────────
 ❯ ▏

 ready · enter send · ctrl+p mode · @ file · /help
```

Tells: header is one quiet row, volatile budget right-aligned; user turn marked
by a single accent rail (`▌`) — no box; assistant turn led by one `●` accent
glyph + body; tool block is an uppercase eyebrow + dim right-aligned meta;
exactly one thin rule separating composer from transcript; footer status line is
dim, verbs not `[KEYS]`. One border depth on screen: none (the edge frames it).
This is essentially koda-today, tightened per §2–§3.

---

## 8. koda-specific checklist (what to change vs keep)

**Keep (already modern):**
- One border depth; modals are the only boxed thing.
- Rounded corners with ASCII fallback.
- Per-kind subtle block fills gated to known-colour themes.
- Semantic token table with ANSI/mono/NO_COLOR fallbacks.
- Status glyph always paired with status colour.

**Sharpen:**
1. Formalise the **4-level type hierarchy** (§2.2) as helpers on `Theme`:
   `eyebrow()` (uppercase+dim), reuse `strong()` for L1, `body()`/`dim()` for
   L3/L4. Audit the codebase so no element stacks >2 levers.
2. Introduce a **spacing scale** constant `[0,1,2]` and a `gap(Kind)` helper;
   replace every literal blank-line push in `view.rs`/`panel.rs`/`tui.rs`.
3. Add a **left gutter (2 cols)** to the transcript body and standardise the
   user rail (`▌`) in it.
4. Cap prose at a **max measure (~80–100 cols)**; leave code/diffs full width.
5. Make **UPPERCASE strictly a ≤12-char eyebrow device**; verify no long caps
   runs exist (tool titles, headers).
6. Ensure **accent appears ≤1–2 times per screen** — audit for accent overuse.
7. Verify **muted contrast ≥ ~4.5:1** on every shipped theme's `surface`
   (add a test alongside the existing theme tests in `theme.rs`).

**Verify no regression:**
- All new grouping is whitespace/tint/rail (ranks 1–3, §5) — one style write per
  cell max, no new per-block perimeter walks. The 8.6µs idle frame budget holds
  because nothing here adds work proportional to *total* blocks; static blocks
  keep their cached styling.
