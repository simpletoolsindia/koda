# Spec: Status Line & Input Composer (koda visual parity with oh-my-pi)

Source of truth: `oh-my-pi/packages/coding-agent/src/modes/components/status-line/*`,
`oh-my-pi/packages/tui/src/components/composer/*`, `.../components/editor.ts`,
`.../coding-agent/src/modes/theme/symbols.ts`, `.../config/keybindings.ts`,
`.../tui/src/keybindings.ts`. Line/file references below are to that tree.

This document describes the **runtime behaviour to reproduce**, not the TS
implementation. Everything here is exact: glyphs are the literal code points
oh-my-pi emits, thresholds are the literal constants.

Defaults: `composer.shape = "band"`, `statusLine.preset = "default"`,
`statusLine.separator = "powerline-thin"`, `statusLine.contextLine = "embedded"`,
symbol preset = `unicode` (the tables below give unicode / nerd / ascii).

---

## 1. Status line segment model

A status line is two ordered groups of **segments**: a left group and a right
group. Each segment renders to `{ content: string, visible: bool }`; invisible
or empty segments are dropped before layout. Segment content already carries its
colour (an ANSI foreground); the bar frame supplies background + separators.

### 1.1 Segment catalogue

Every segment id, what it shows, its colour role (theme colour name), and its
visibility rule. `withIcon(icon, text)` = `"{icon} {text}"` when the icon is
non-empty, else just `text`. Icons are per symbol-preset (see §3.4).

| id | Content (unicode preset) | Colour role | Visible when |
|----|--------------------------|-------------|--------------|
| `pi` | Brand: `π ` idle. While a turn runs: `{spinner} {timer} ` (e.g. `⠹ 12s `). Focused-subagent proxy: `👻 {agentId} `. | `dim`→accent fade via `brandFgAnsi` (idle=dim); `warning` when proxied | always |
| `model` | `⬢ {modelName}` + optional ` ⚡` fast-mode + `· {thinkingDisplay}` + advisor `👁` badge. `Claude ` prefix stripped from name. Compact mode: thinking glyph replaces `⬢`, drops `· level` tail. | `statusLineModel` (aliased to `accent`; session-accent hash tint if enabled). advisor badge coloured by worst advisor status: error/warning/success/dim | always |
| `mode` | First active of: Plan `🗺 Plan` / `🗺 Plan ⏸`; Prewalk `🏃 Prewalk`; Goal `🎯 Goal [used/budget]` (status-driven icon+colour); Vibe `👥 Vibe`; Loop `↻ Loop {state} [limit]`. | accent (session tint); `warning` when paused; loop uses `customMessageLabel` | any mode active |
| `path` | `📁 {pwd}` (home→`~`, abbreviated, left-clamped to maxLength with leading `…`). Worktree: `🌳 {project[/worktree]}`. Scratch dir: `🗑`. Active-repo suffix ` ↳ {repoRoot}`. | `statusLinePath` | always |
| `git` | `⑂ {branch}` + dirty markers `*{n}` unstaged, `+{n}` staged, `?{n}` untracked (space-joined). | `statusLineGitClean` / `statusLineGitDirty` when dirty. markers: `statusLineDirty`/`statusLineStaged`/`statusLineUntracked` | branch or status present |
| `pr` | `⤴ #{number}` (OSC-8 hyperlink to PR url when terminal supports it) | accent (session tint) | PR resolved |
| `subagents` | `👥 {count}` | `statusLineSubagents` | count > 0 |
| `token_in` | `⤵ {n}` | `statusLineSpend` | input > 0 |
| `token_out` | `⤴ {n}` | `statusLineOutput` | output > 0 |
| `token_total` | `🪙 {n}` (input+output+cacheWrite+orchIn+orchOut; **excludes cacheRead**) | `statusLineSpend` | total > 0 |
| `token_rate` | `⚡ {x.x} tok/s` | `statusLineOutput` | rate present |
| `cost` | `💲{x.xx}` (or `S{x.xx}`/subscription icon on OAuth) + `★ {premiumReqs}` + `+ {advisorSpend} (adv)` | `statusLineCost` | any cost/sub/premium |
| `context_pct` | `◫ {pct}%/{window}` + auto-compact icon `⟲`. Unknown window → `{tokens}/?`. | context level colour (§5) | always |
| `context_total` | `◫ {window}` | `statusLineContext` | window > 0 |
| `time_spent` | `⏱ {duration}` (active processing time, not wall clock) | default text | activeMs ≥ 1000 |
| `time` | `⏱ {H:MM[:SS][am/pm]}` | default text | always |
| `session` | `🆔 {sessionId[:8] or "new"}` | default text | always |
| `hostname` | `🖥 {host}` (first dot-label) | session tint or default | always |
| `cache_read` | `💾 {n}` | `statusLineSpend` | cacheRead > 0 |
| `cache_write` | `💾 {n}` | `statusLineOutput` | cacheWrite > 0 |
| `cache_hit` | `💾 {rate}%` (cacheRead/(cacheRead+cacheWrite+input)) | `statusLineSpend` | cacheRead > 0 |
| `session_name` | `{sanitized title}` | accent (session tint) | title or previewTitle set |
| `usage` | `⏱ 5h {p}% (reset) · 1d … · 7d … · mo …` | per-window: muted/warning/error at 50/80% | any window present |
| `collab` | `⇄ collab:{n}` (host) / `⇄ collab guest:{n}` | accent (session tint) | collab active |

Extra transient right-group prepends (unshifted, before `session_name` etc.):
- Running background jobs: `⚙ {n}` in `statusLineSubagents`.
- Subagent badge (if any): the badge text.

### 1.2 Preset segment orders

```
default : L=[pi model mode collab path git pr context_pct cost]   R=[session_name]        sep=powerline-thin
minimal : L=[path git]                                            R=[session_name mode context_pct]  sep=slash
compact : L=[model mode git pr]                                   R=[session_name cost context_pct]  sep=powerline-thin
full    : L=[pi hostname model mode path git pr subagents]        R=[session_name cache_hit token_in token_out token_rate cache_read cost context_pct time_spent time]  sep=powerline
nerd    : L=[pi hostname model mode path git pr session subagents] R=[session_name token_in token_out cache_read cache_write token_rate cost context_pct context_total time_spent time]  sep=powerline
ascii   : L=[model mode path git pr]                              R=[session_name token_total cost context_pct]  sep=ascii
```

`default` is the shipped default. Left group is left-aligned from column 0;
right group is right-aligned to the terminal edge; the gap between them is the
context-gauge fill (box/band) or plain padding (bottom-bar composers).

---

## 2. Priority truncation

When `leftWidth + rightWidth + gap > terminalWidth`, segments are shrunk then
dropped **in this exact order** (from `#buildStatusLine`):

1. **Shrink `session_name`** (right group, the only elastic right segment).
   Truncate down to a floor of **8 visible cells** (`minNameVW = 8`), taking
   only as many cells as needed to fit.
2. **Drop right-group segments** from the tail (`right.pop()`) one at a time
   until it fits or the right group is empty.
3. **Shrink `path`** (left group, the only elastic left segment). Reduce its
   `maxLength` toward a floor of **8 visible cells** (`minPathVW = 8`, icon +
   `…` + a few chars). Iterates up to 8× to compensate for the icon prefix.
4. **Drop left-group segments** — but **path is preserved last**. The drop index
   walks from the tail and skips `path`; every other left segment
   (model/mode/collab/pr/git/pi…) is removed before path. Path only goes when it
   is the sole survivor.

Width accounting per group: `sum(visibleWidth(parts)) + (n-1)*(sepWidth+2) + 2 +
capWidth`. Powerline end-caps add their width; transparent mode drops caps from
the budget. Minimum gap = 1 cell when both groups non-empty (else the embedded
gauge min width, see §5.4).

Embedded context mode removes the standalone `context_pct`/`context_total`
segments up front and folds their numbers into the gauge, reserving
`embeddedContextGaugeMinWidth = len(pct%) + len(window) + 4` cells for the gauge
labels before overflow handling runs.

There are no hard column breakpoints; truncation is purely width-driven. The
observable order at shrinking widths for the `default` preset:

```
wide      pi ▏ model ▏ mode ▏ collab ▏ path ▏ git ▏ pr ▏ context …… title
narrower  (title shrinks to ≥8 cells)
narrower  (title dropped; right group empty)
narrower  (context_pct folded into gauge / dropped)
narrower  (path shrinks toward 8 cells)
narrow    (pr, git, collab, mode, model dropped tail-first, path skipped)
narrowest path   (path alone, possibly clamped)
```

---

## 3. Separators & glyphs

### 3.1 Separator styles (`getSeparator`)

Each style has a left→right glyph and a reversed right→left glyph, and optional
powerline end-caps. Rendering inserts a separator as ` {sep} ` (one space each
side) between adjacent same-group parts. `slash`/`pipe` are trimmed before use.

| style | left glyph | right glyph | end-caps |
|-------|-----------|-------------|----------|
| `powerline` | `sep.powerline` | `sep.powerlineRight` | yes (`powerlineRight`/`powerlineLeft`, useBgAsFg) |
| `powerline-thin` (default) | `sep.powerlineThin` | `sep.powerlineThinRight` | yes |
| `slash` | `/` | `/` | none |
| `pipe` | `│` | `│` | none |
| `block` | `sep.block` | `sep.block` | none |
| `none` | space | space | none |
| `ascii` | `>` | `<` | none |

### 3.2 Separator glyphs per preset

| symbol key | unicode | nerd | ascii |
|-----------|---------|------|-------|
| `sep.powerline` | `▕` | `` (U+E0B0) | `>` |
| `sep.powerlineThin` | `┆` | `` (U+E0B1) | `>` |
| `sep.powerlineLeft` | `▶` | `` (E0B0) | `>` |
| `sep.powerlineRight` | `◀` | `` (E0B2) | `<` |
| `sep.powerlineThinLeft` | `>` | `` (E0B1) | `>` |
| `sep.powerlineThinRight` | `<` | `` (E0B3) | `<` |
| `sep.powerlineCapLeft` (band opening cap) | `` (empty) | `` (E0B6) | `` (empty) |
| `sep.block` | `▌` | `█` | `#` |
| `sep.slash` | ` / ` | `` (E0BB) | ` / ` |
| `sep.pipe` | ` │ ` | `` (E0B3) | ` \| ` |
| `sep.dot` (model/thinking joiner, usage joiner) | ` · ` | ` · ` | ` - ` |

### 3.3 Gauge / boundary glyphs

| symbol key | unicode | nerd | ascii |
|-----------|---------|------|-------|
| `boxRound.horizontal` (gauge fill / rules) | `─` | `─` | `-` |
| `boxRound.topLeft/topRight` | `╭` / `╮` | `╭` / `╮` | `+` / `+` |
| `boxRound.bottomLeft/bottomRight` | `╰` / `╯` | `╰` / `╯` | `+` / `+` |
| `boxRound.vertical` | `│` | `│` | `\|` |
| `context.speculation` (gauge tick) | `╎` | `󱕝` (U+F055D) | `:` |
| `context.compaction` (gauge tick) | `┃` | `󰁨` (U+F0068) | `\|` |
| `progress.filled` / `progress.empty` | `━` / `─` | `━` / `─` | `=` / `-` |

### 3.4 Segment icons per preset

| icon key | unicode | nerd | ascii |
|----------|---------|------|-------|
| `icon.omp` (pi brand) | `π` | `󰵗` (U+F0D57) | `pi` |
| `icon.model` | `⬢` | `` (U+EC19) | `[M]` |
| `icon.plan` | `🗺` | `` (U+F2D2) | `plan` |
| `icon.prewalk` | `🏃` | `` (U+F29D) | `prewalk` |
| `icon.goal` | `🎯` | `` (U+F140) | `goal` |
| `icon.pause` | `⏸` | `` (U+F04C) | `\|\|` |
| `icon.loop` | `↻` | `` (U+F021) | `loop` |
| `icon.folder` | `📁` | `` (U+F115) | `[D]` |
| `icon.worktree` | `🌳` | `` (U+F0E8) | `[wt]` |
| `icon.scratchFolder` | `🗑` | `` (U+F014) | `[T]` |
| `icon.git` | `⎇` | `` (U+F1D3) | `git:` |
| `icon.branch` | `⑂` | `` (U+F126) | `@` |
| `icon.pr` | `⤴` | `` (U+EA64) | `PR` |
| `icon.tokens` | `🪙` | `` (U+E26B) | `tok:` |
| `icon.context` | `◫` | `` (U+E70F) | `ctx:` |
| `icon.cost` | `💲` | `` (U+F155) | `$` |
| `icon.subscription` | `(sub)` | `󰙺` (U+F067A) | `(sub)` |
| `icon.advisor` | `👁` | `` (U+EA70) | `(adv)` |
| `icon.time` | `⏱` | `` (U+F017) | `t:` |
| `icon.ghost` (proxy) | `👻` | `󰊠` (U+F02A0) | `@` |
| `icon.agents` | `👥` | `` (U+F0C0) | `AG` |
| `icon.job` | `⚙` | `` (U+F013) | `bg` |
| `icon.cache` | `💾` | `` (U+F1C0) | `cache` |
| `icon.input` | `⤵` | `` (U+F090) | `in:` |
| `icon.output` | `⤴` | `` (U+F08B) | `out:` |
| `icon.throughput` | `⚡` | `` (U+F0E4) | `tok/s:` |
| `icon.host` | `🖥` | `` (U+F109) | `host` |
| `icon.session` | `🆔` | `󰁑` (U+F0051) | `id` |
| `icon.auto` (auto-compact) | `⟲` | `󰁨` (U+F0068) | `[A]` |
| `icon.fast` | `⚡` | `` (U+F0E7) | `>>` |

### 3.5 Thinking-level display strings

| level | unicode | nerd | ascii |
|-------|---------|------|-------|
| minimal | `○ min` | `󰩞 min` | `[min]` |
| low | `◔ low` | `󰩟 low` | `[low]` |
| medium | `◑ med` | `󰩡 med` | `[med]` |
| high | `◒ high` | `󰩣 high` | `[high]` |
| xhigh | `◕ xhigh` | `󰩥 xhi` | `[xhi]` |
| max | `◉ max` | `󰛭 max` | `[max]` |
| auto pending | `⟳` (+ ` auto`) | `󰀴` | `[~]` |
| off | `⦸ off` (`status.disabled` glyph) | | |

Compact mode promotes the leading glyph (before the first space) to the model
icon and drops the ` · level` tail.

---

## 4. Spinner / progress

### 4.1 Cadence

`SPINNER_ADVANCE_MS = 80` ms per frame (`tui/src/components/loader.ts`). All
time-derived spinners tick on the shared clock:
`frame = frames[floor(Date.now() / 80) % frames.length]`.

### 4.2 Frame sets (`SPINNER_FRAMES`, two types: `status`, `activity`)

| preset | activity frames | status frames |
|--------|-----------------|---------------|
| unicode | `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` | `⣾ ⣽ ⣻ ⢿ ⡿ ⣟ ⣯ ⣷` |
| nerd | `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` | `󱑖 󱑋 󱑌 󱑍 󱑎 󱑏 󱑐 󱑑 󱑒 󱑓 󱑔 󱑕` |
| ascii | `- \ \| /` | `\| / - \` |

The Loader's own default frame set is the braille activity set
`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`.

### 4.3 Brand spinner (pi segment while a turn is running)

- Uses the **activity** frame set.
- Rendered as `{spinnerFrame} {turnTimer} ` (trailing space), replacing the
  static `π ` brand.
- Turn timer format (whole units): `<60s` → `{n}s`; `<3600s` → `{m}m`;
  else `{h}h` capped at `99h`. Examples: `3s`, `47s`, `2m`, `1h`.
- Idle foreground = `dim`; while working the brand fades toward the accent
  (tweened via `brandFgAnsi`).

### 4.4 Async-compaction indicator (context_pct segment)

The auto-compact icon `⟲` beside the context percent pulses on background
speculation:
- `running` + blink-on → accent; blink-off → `muted` (pulse).
- `armed` → steady accent.
- `idle` → context-level colour.

---

## 5. Token / context display

### 5.1 Format

`formatContextUsage(pct, window, tokens)`:
- Known window: `{pct.toFixed(1)}%/{formatNumber(window)}` e.g. `42.3%/200K`.
- Unknown pct (post-compaction) but known window: `?/{window}`.
- Unknown window: `{formatNumber(tokens)}/?`.

Segment content = `withIcon(◫, "{formatted}{autoIcon}")`.

### 5.2 Colour thresholds (`context-thresholds.ts`)

Level is reached when `pct ≥ min(percentThreshold, tokenThreshold/window*100)`
— whichever fires first. Constants:

| level | percent threshold | token threshold | theme colour |
|-------|-------------------|-----------------|--------------|
| normal | — | — | `statusLineContext` |
| warning | 50% | 150,000 | `warning` |
| purple | 70% | 270,000 | `thinkingHigh` |
| error | 90% | 500,000 | `error` |

Evaluated highest-first (error → purple → warning → normal). With no valid
window, the raw percent thresholds apply.

### 5.3 When a gauge appears

The **gauge** is the fill line between the left and right groups, only in box
(`getTopBorder`) and band (`getBandTopBorder`) layouts — never in the plain
bottom-bar composers (they use plain padding). Driven by
`statusLine.contextLine`:

- `off`: solid session-accent line (`─` repeated), no context feedback.
- `percentage`: used portion in session-accent colour, remainder in `border`.
- `annotated`: percentage + boundary ticks where speculative compaction starts
  (`context.speculation` glyph `╎`, `muted`) and where auto-compaction fires
  (`context.compaction` glyph `┃`, dimmed-accent). Ticks only when
  `autoCompactEnabled` and `gapWidth ≥ 8`.
- `embedded` (default): annotated + absorbed percent/window labels from the
  context segments, drawn into the gauge.

### 5.4 Gauge fill construction

- Fill glyph: `boxRound.horizontal` (`─`).
- Used cells: `min(scaleWidth, max(1, round(pct/100 * scaleWidth)))` — always at
  least one accent cell so a fresh session shows the accent starting left.
- Used colour = session-accent (hash-derived) or `borderAccent`; unused =
  `border`.
- Embedded labels only render when
  `gapWidth ≥ embeddedContextGaugeMinWidth = len("{pct}%") + len(window) + 4`.
  Window label docks at the right end (`windowStart = gap - len(window) - 1`);
  percent label placed near the used-cell boundary, nudged to avoid overlapping
  boundary ticks.
- Percent formatting in gauge: `<1%` → one decimal (`0.4%`); else rounded int.
- `>100%` overflow: bar clamps full, percent label uses `error` colour and
  anchors to the far right (`──200K─120%`).

---

## 6. The composer

The composer is an editor wrapped in chrome. A `ComposerStyle` owns the frame;
the editor owns text, caret, wrapping, autocomplete. `composer.shape` default is
**`band`**. Eight built-in shapes: `box band claude pi borderless rule field
rail`.

### 6.1 Shape chrome summary

| shape | side borders | vertical chrome | status attach | bottom bar | gap | prompt gutter | paddingX |
|-------|-------------|-----------------|---------------|------------|-----|---------------|----------|
| `box` | yes | 2 | top-border (embedded) | none | no | none | 2 |
| `band` (default) | no | 1 | top-band | none | no | `╰─ ` | 0 |
| `claude` | no | 2 | top-rule-chip | left | no | `❯ ` | 0 |
| `pi` | no | 2 | none | full | no | none | 1 |
| `borderless` | no | 0 | none | full | no | `❯ ` | 0 |
| `rule` | no | 1 | top-rule-chip | left | yes | `❯ ` | 0 |
| `field` | yes | 0 | none | full | yes | none | 1 |
| `rail` | yes | 0 | none | full | yes | none | 1 |

- **statusAttachment**: where the status bar lives — embedded in the box top
  border; a flush soft-capped powerline band above the input; docked as a chip
  on a top rule (right group only, left group on the bottom bar); or fully
  detached to a standalone bottom bar.
- **bottomBar**: `full` = both groups on the bottom bar; `left` = left group on
  the bottom bar (right group rides the rule); `none` = no bottom bar.
- **bottomBarGap**: insert one blank spacer row between editor and bottom bar
  (styles with no bottom chrome need it).

### 6.2 Prompt glyphs (gutter)

- `band`: `╰─ ` (border-coloured curved cue).
- `claude` / `rule` / `borderless`: `❯ ` (also the effective glyph whenever a
  host disables the border).
- `box` / `pi` / `field` / `rail`: no gutter.

`box` bottom border merges the last content row into `╰─ … ─╯`.

### 6.3 Frame glyphs per shape

- `box`: top `╭──…status…──╮`, sides `│ … │`, bottom-left `╰─`, bottom-right
  `─╯`. Uses `boxRound`. Right border becomes `█` in the scrollbar thumb range.
- `band`: single top row = the full status band (flush left, powerline soft cap
  `` when nerd, empty otherwise), no bottom chrome. Input row prefixed by
  `╰─ `.
- `claude` / `rule`: top rule `───────── {chip} ─` (chip docks right, `─` fill
  left; over-wide chip truncated keeping one rule cell each side). `claude` adds
  a closing full-width rule at the bottom; `rule` has no closing rule.
- `pi`: full-width `─` rule above and below plain padded text.
- `borderless`: bare `❯ ` prompt, no rules.
- `field`: one-row filled field with accent end caps `▐` (left) and `▌` (right),
  subtle surface fill; scrollbar thumb replaces right cap with `█`.
- `rail`: filled surface with a single left accent rail `▎`; scrollbar thumb
  appends `█` on the right.

### 6.4 Placeholder text

The editor has **no built-in placeholder string**; empty input renders as an
empty row with just the caret and gutter. (The editor exposes atomic-token
placeholders like `[Image #1]` for pasted attachments, but there is no "Type a
message…" ghost prompt in the main composer. Any hint text is carried by the
status/hint rows, not inside the editor.)

### 6.5 Multi-line behaviour

- Text soft-wraps to the content width (terminal width minus side chrome). Each
  wrapped visual line is a row.
- `maxHeight` budgets rows; `verticalChrome` rows are reserved for top/bottom
  chrome. When content overflows `maxHeight`, side-bordered shapes show a
  scrollbar in the right border (`█` thumb over `│` track).
- Newlines are inserted by `shift+enter` / `ctrl+j` (see §7); plain `enter`
  submits.
- Sticky preferred column is kept across vertical cursor moves.

### 6.6 Caret rendering

Two modes (`setUseTerminalCursor`):
- **Software caret (default)**: the grapheme under the cursor is drawn in
  reverse video `\x1b[7m{grapheme}\x1b[0m`. At end-of-line (no grapheme), a
  standalone caret glyph `inputCursor` is emitted: `▏` (U+258F) in unicode/nerd,
  `|` in ascii. A zero-width row keeps the prompt glyph visible and reverse-
  videos it if the gutter consumes the whole row.
- **Terminal caret**: a `CURSOR_MARKER` is emitted at the cursor position and the
  host positions the real terminal cursor there; no reverse-video glyph.
- IME-safe last row (box, side-bordered): the end-of-input caret row is kept
  empty to its right so local IME preedit can't shift chrome onto the next row.
- `cursorOverride` (e.g. mic glyph during voice input) replaces the end-of-text
  caret with an ANSI-styled string of a declared width.

### 6.7 Hint rows

- There are no fixed hint rows baked into the composer chrome. Contextual hints
  are separate `Text` rows rendered by controllers, dim-key + muted-desc styled
  via `keyHint(action, desc)` = `dim(key) + muted(" " + desc)`.
- The status line's **bottom bar / band / top border** is the persistent info
  row (segments from §1). Hook status rows (if enabled) append below.
- Transient status messages (controllers) use ` (esc to cancel)` suffixes,
  e.g. `Compacting context... (esc to cancel)`, `Auto-compacting context...
  (esc to cancel)`, and an idle `F5 to Retry` affordance after a failed turn.

---

## 7. Composer keybindings

Editor + input bindings from `tui/src/keybindings.ts` (TUI layer) and
app-level bindings from `coding-agent/src/config/keybindings.ts`. Keys are
canonicalised (modifier order ctrl+shift+alt+super).

### 7.1 Cursor movement

| action | default keys | description |
|--------|-------------|-------------|
| `tui.editor.cursorUp` | `up` | Move cursor up |
| `tui.editor.cursorDown` | `down` | Move cursor down |
| `tui.editor.cursorLeft` | `left`, `ctrl+b` | Move cursor left |
| `tui.editor.cursorRight` | `right`, `ctrl+f` | Move cursor right |
| `tui.editor.cursorWordLeft` | `alt+left`, `ctrl+left`, `alt+b` | Word left |
| `tui.editor.cursorWordRight` | `alt+right`, `ctrl+right`, `alt+f` | Word right |
| `tui.editor.cursorLineStart` | `home`, `ctrl+a` | Line start |
| `tui.editor.cursorLineEnd` | `end`, `ctrl+e` | Line end |
| `tui.editor.jumpForward` | `ctrl+]` | Jump forward to char |
| `tui.editor.jumpBackward` | `ctrl+alt+]` | Jump backward to char |
| `tui.editor.pageUp` | `pageUp` | Page up |
| `tui.editor.pageDown` | `pageDown` | Page down |

### 7.2 Editing / kill-ring

| action | default keys | description |
|--------|-------------|-------------|
| `tui.editor.deleteCharBackward` | `backspace` | Delete char backward |
| `tui.editor.deleteCharForward` | `delete`, `ctrl+d` | Delete char forward |
| `tui.editor.deleteWordBackward` | `ctrl+w`, `alt+backspace`, `ctrl+backspace`, `super+alt+backspace` | Delete word backward |
| `tui.editor.deleteWordForward` | `alt+delete`, `alt+d`, `super+alt+delete`, `super+alt+d` | Delete word forward |
| `tui.editor.deleteToLineStart` | `ctrl+u` | Kill to line start |
| `tui.editor.deleteToLineEnd` | `ctrl+k` | Kill to line end |
| `tui.editor.yank` | `ctrl+y` | Yank (paste kill-ring) |
| `tui.editor.yankPop` | `alt+y` | Yank pop |
| `tui.editor.undo` | `ctrl+-`, `ctrl+_` | Undo |
| `tui.editor.spellingSuggestions` | `ctrl+.` | Show spelling replacements |

### 7.3 Input / submit

| action | default keys | description |
|--------|-------------|-------------|
| `tui.input.newLine` | `shift+enter`, `ctrl+j` | Insert newline |
| `tui.input.submit` | `enter` | Submit input |
| `tui.input.tab` | `tab` | Tab / autocomplete |
| `tui.input.copy` | `ctrl+c` | Copy selection |

Legacy `shift+enter` variants also accepted: bare `\n` (LF) and `\x1b[13;2~`.

### 7.4 App-level (active while the composer is focused)

| action | default keys | description |
|--------|-------------|-------------|
| `app.interrupt` | `escape` | Interrupt current operation |
| `app.clear` | `ctrl+c` | Clear screen / cancel |
| `app.exit` | `ctrl+d` | Exit application |
| `app.suspend` | `ctrl+z` | Suspend |
| `app.display.reset` | `alt+l` | Reset terminal display |
| `app.thinking.cycle` | `shift+tab` | Cycle thinking level |
| `app.thinking.toggle` | `ctrl+t` | Toggle thinking mode |
| `app.model.cycleForward` | `ctrl+p` | Next model |
| `app.model.cycleBackward` | `shift+ctrl+p` | Previous model |
| `app.model.select` | `alt+m` | Select model |
| `app.model.selectTemporary` | `alt+p` | Temp model this session |
| `app.tools.expand` | `ctrl+o` | Expand tools |
| `app.tools.toggleVisibility` | `ctrl+shift+o` | Show/hide tool activity |
| `app.editor.external` | `ctrl+g` | Open external editor |
| `app.message.followUp` | `ctrl+q`, `ctrl+enter` | Send follow-up message |
| `app.retry` | `f5`, `alt+r` | Retry last failed turn |
| `app.message.dequeue` | `alt+up`, `shift+up` | Dequeue queued message |
| `app.clipboard.pasteImage` | (platform default) | Paste image/text |
| `app.clipboard.pasteTextRaw` | `ctrl+shift+v`, `alt+shift+v` | Paste raw text |
| `app.clipboard.copyLine` | `alt+shift+l` | Copy current line |
| `app.clipboard.copyPrompt` | `alt+shift+c` | Copy prompt |
| `app.agents.hub` | `alt+a` | Open agent hub |

Note the intentional overloads: `ctrl+c` = editor copy-selection **and**
app clear/cancel; `ctrl+d` = delete-char-forward **and** app exit (context
decides); `enter` submits while `shift+enter`/`ctrl+j` insert newlines.

---

## 8. Example layouts

Illustrative renders of the **default** preset + **band** composer, unicode
preset, session titled "auth-refactor", model "Sonnet 4.5", branch "main" with
3 unstaged, context 42.3%/200K, cost $1.87. Powerline-thin separators shown as
`┆`; the band's top row is the status band, the input row is prefixed `╰─ `.
The line between left/right groups is the embedded context gauge (`─` fill with
percent/window labels docked right). These are visual approximations at the
given column widths.

### 8.1 — 80 columns

```
π ┆ ⬢ Sonnet 4.5 · ◒ high ┆ 📁 ~/koda ┆ ⑂ main *3───42.3%─200K auth-refactor
╰─ ▏
```

Under pressure at 80 cols the title has already shrunk toward its 8-cell floor;
`pr`/`collab` absent (no PR, no collab). Caret shown as `▏` at end of empty line.

### 8.2 — 100 columns

```
π ┆ ⬢ Sonnet 4.5 · ◒ high ┆ 📁 ~/koda ┆ ⑂ main *3 ┆ 💲1.87────42.3%──200K────auth-refactor
╰─ explain the retry back-off logic▏
```

At 100 cols `cost` survives in the left group; the gauge fill widens; the title
renders in full.

### 8.3 — 120 columns

```
π ┆ ⬢ Sonnet 4.5 · ◒ high ┆ 📁 ~/koda ┆ ⑂ main *3 ┆ ◫ 42.3%/200K ⟲ ┆ 💲1.87──────────42.3%───200K──────auth-refactor
╰─ explain the retry back-off logic and add a jitter cap▏
```

At 120 cols nothing is dropped: `context_pct` shows inline (`◫ 42.3%/200K ⟲`)
in the left group *and* the gauge shows the embedded percent/window labels; the
right group carries the full session title against the edge.

### 8.4 — box composer, 100 columns (for contrast)

```
╭─ π ┆ ⬢ Sonnet 4.5 · ◒ high ┆ 📁 ~/koda ┆ ⑂ main *3 ┆ 💲1.87───42.3%──200K──auth-refactor ─╮
│  explain the retry back-off logic                                                          │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
```

Box embeds the status in the top border and merges the last input row into the
bottom border on a single-line prompt.

### 8.5 — claude composer, 100 columns (for contrast)

```
──────────────────────────────────────────────────────────────────────────── auth-refactor ─
❯ explain the retry back-off logic▏
──────────────────────────────────────────────────────────────────────────────────────────
π ┆ ⬢ Sonnet 4.5 · ◒ high ┆ 📁 ~/koda ┆ ⑂ main *3 ┆ 💲1.87 ┆ ◫ 42.3%/200K ⟲
```

The right group (title) rides the top rule as a chip; the left group is the
standalone bottom bar (which yields its row to the autocomplete menu when open).

Colour-role note for koda: map theme roles to concrete colours —
`statusLineModel`≈accent, `statusLinePath`, `statusLineGitClean/Dirty`,
`statusLineDirty/Staged/Untracked`, `statusLineSpend/Output/Cost/Context/
Subagents`, `warning`, `error`, `thinkingHigh` (purple), `muted`, `dim`,
`border`, `borderAccent`. Session-accent is a hash of the session title used as
an optional foreground tint for pi/model/pr/mode/title/gauge.
