# Spec: Glyph Vocabulary & Semantic Colour System (oh-my-pi → koda)

Extracted from `refs/oh-my-pi/packages/coding-agent/src/modes/theme/`
(`symbols.ts`, `schema.ts`, `theme-class.ts`, `theme.ts`, `loader.ts`,
`color.ts`, `shimmer.ts`, `defaults/*.json`). Goal: give koda's Rust/ratatui TUI
enough to reach visual parity.

Literal characters are copied verbatim from source. Where a glyph is a Nerd Font
Private Use Area codepoint, the escape (`\uXXXX` / `\u{XXXXX}`) is reproduced
because the glyph will not render outside a patched font.

---

## 0. Architecture overview

Two orthogonal axes:

1. **Symbol preset** (`SymbolPreset = "unicode" | "nerd" | "ascii"`) — chooses
   the glyph vocabulary. Three complete `SymbolMap`s exist
   (`UNICODE_SYMBOLS`, `NERD_SYMBOLS`, `ASCII_SYMBOLS`), each keyed by the same
   `SymbolKey` union (~230 keys). A theme may also carry per-key `overrides` and
   `spinnerFrames` overrides.
2. **Theme colours** — a fixed schema of semantic colour roles (`ThemeColor` +
   `ThemeBg`), resolved from hex/256-index values via `vars` indirection.

The `Theme` class (`theme-class.ts`) holds a resolved `SymbolMap` plus resolved
fg/bg ANSI + hex tables and exposes category accessors (`theme.status`,
`theme.tree`, `theme.sep`, `theme.icon`, …) and `theme.fg(role, text)`.

koda already models both axes (`Glyphs` and `Theme` structs in
`src/theme.rs`), but with far fewer keys/roles. Sections 3–4 are the gap list.

---

## 1. Glyph vocabulary

### 1.1 How the three presets relate

- **unicode** (default) — plain Unicode + emoji. No special font required.
  This is the set koda should mirror first for parity.
- **nerd** — Nerd Font PUA icons for the icon-heavy keys; box-drawing / tree /
  progress glyphs are *identical to unicode*. Only meaningful if the user runs a
  patched font.
- **ascii** — pure ASCII fallback, `[ok]`, `>`, `|--`, etc. Many icon columns
  are intentionally blanked (`""`) in ASCII mode.

Every `SymbolKey` exists in all three maps (`SYMBOL_PRESETS`), so a lookup never
misses. koda's `Glyphs` has only two presets (UNICODE, ASCII) and no `nerd`.

### 1.2 Capability detection (important — differs from koda)

oh-my-pi does **not** probe the terminal for Nerd Font support. The preset is a
**user setting** (`symbolPreset`, default `"unicode"`), resolved in
`loader.ts::createTheme`:

```
symbolPreset = settingsOverride ?? themeJson.symbols?.preset ?? "unicode"
```

- Default is `"unicode"` always (`settings-schema.ts` line ~705).
- A setup-wizard "glyph" scene (`setup-wizard/scenes/glyph.ts`) shows sample
  glyphs and lets the user pick; there is no automatic Nerd Font sniffing.
- ASCII is offered as "Maximum compatibility".

Colour depth *is* auto-detected (`color.ts::detectColorMode`): `WT_SESSION` ⇒
truecolor; otherwise `getTerminalInfo(...).trueColor` decides
`truecolor` vs `256color`. Output is emitted as `ansi-16m` or `ansi-256`.

koda currently derives its glyph set from `LC_ALL/LC_CTYPE/LANG` containing
`UTF-8`. That is a reasonable, *different* heuristic. To match oh-my-pi's
behaviour, treat glyph preset as a config/setting with default unicode, and keep
the UTF-8 locale check only as the auto fallback. Consider adding a `nerd`
preset if parity with Nerd-Font users is wanted.

### 1.3 Core TUI glyphs (unicode / nerd / ascii)

Only the keys that actually surface in transcript, status line, selectors,
trees, diffs, gauges, and spinners. Nerd column shows the escape; the rendered
glyph is a private-use icon.

#### Status indicators (transcript tool headers, lists, toasts)

| Key | unicode | nerd | ascii | Used by |
|-----|---------|------|-------|---------|
| `status.success` | `✔` | `\uf00c` | `[ok]` | tool success header, checklists |
| `status.error` | `✘` | `\uf00d` | `[!!]` | tool error header |
| `status.warning` | `⚠` | `\uf12a` | `[!]` | warnings |
| `status.info` | `ⓘ` | `\uf129` | `[i]` | info rows |
| `status.pending` | `⏳` | `\uf254` | `[*]` | queued/pending |
| `status.disabled` | `⦸` | `\uf05e` | `[ ]` | disabled entries |
| `status.enabled` | `●` | `\uf111` | `[x]` | enabled entries |
| `status.running` | `⟳` | `\uf110` | `[~]` | in-progress |
| `status.shadowed` | `○` | `\uf10c` | `[/]` | shadowed/inactive |
| `status.aborted` | `⏹` | `\uf04d` | `[-]` | aborted run |
| `status.done` | `•` | `•` | `*` | completed marker |

#### Navigation / selectors

| Key | unicode | nerd | ascii | Used by |
|-----|---------|------|-------|---------|
| `nav.cursor` | `❯` | `\uf054` | `>` | selector cursor / prompt caret |
| `nav.selected` | `➤` | `\uf178` | `->` | selected list row |
| `nav.expand` | `▸` | `\uf0da` | `+` | collapsed tree/section |
| `nav.collapse` | `▾` | `\uf0d7` | `-` | expanded tree/section |
| `nav.back` | `⟵` | `\uf060` | `<-` | back nav |

#### Tree connectors (file trees, nested tool output)

| Key | unicode | nerd | ascii | Used by |
|-----|---------|------|-------|---------|
| `tree.branch` | `├─` | `├─` | `\|--` | non-last child |
| `tree.last` | `└─` | `└─` | `'--` | last child |
| `tree.vertical` | `│` | `│` | `\|` | rail continuation |
| `tree.horizontal` | `─` | `─` | `-` | horizontal run |
| `tree.hook` | `└` | `└` | `` `- `` | hook/corner |

#### Progress + context gauge

| Key | unicode | nerd | ascii | Used by |
|-----|---------|------|-------|---------|
| `progress.filled` | `━` | `━` | `=` | filled gauge cell |
| `progress.empty` | `─` | `─` | `-` | empty gauge cell |
| `context.speculation` | `╎` | `\u{f055d}` | `:` | context-gauge speculation boundary |
| `context.compaction` | `┃` | `\u{f0068}` | `\|` | context-gauge compaction boundary |

#### Box drawing — rounded (panels, cards)

| Key | unicode/nerd | ascii |
|-----|--------------|-------|
| `boxRound.topLeft` | `╭` | `+` |
| `boxRound.topRight` | `╮` | `+` |
| `boxRound.bottomLeft` | `╰` | `+` |
| `boxRound.bottomRight` | `╯` | `+` |
| `boxRound.horizontal` | `─` | `-` |
| `boxRound.vertical` | `│` | `\|` |

(Rounded boxes reuse `boxSharp.*` tee/cross glyphs for junctions — see
`theme-class.ts get boxRound`.)

#### Box drawing — sharp (tables, dividers)

| Key | unicode/nerd | ascii |
|-----|--------------|-------|
| `boxSharp.topLeft` `┌` / `topRight` `┐` / `bottomLeft` `└` / `bottomRight` `┘` | | `+` each |
| `boxSharp.horizontal` `─` / `vertical` `│` | | `-` / `\|` |
| `boxSharp.cross` `┼` | | `+` |
| `boxSharp.teeDown` `┬` / `teeUp` `┴` / `teeRight` `├` / `teeLeft` `┤` | | `+` each |

#### Separators (status line, powerline chips)

| Key | unicode | nerd | ascii | Notes |
|-----|---------|------|-------|-------|
| `sep.powerline` | `▕` | `\ue0b0` | `>` | right-pointing solid powerline |
| `sep.powerlineThin` | `┆` | `\ue0b1` | `>` | thin powerline |
| `sep.powerlineLeft` | `▶` | `\ue0b0` | `>` | |
| `sep.powerlineRight` | `◀` | `\ue0b2` | `<` | |
| `sep.powerlineThinLeft` | `>` | `\ue0b1` | `>` | |
| `sep.powerlineThinRight` | `<` | `\ue0b3` | `<` | |
| `sep.powerlineCapLeft` | `` (empty) | `\ue0b6` | `` (empty) | opening cap; unicode/ascii bands start flat |
| `sep.block` | `▌` | `█` | `#` | solid block |
| `sep.space` | ` ` | ` ` | ` ` | |
| `sep.asciiLeft` | `>` | `>` | `>` | |
| `sep.asciiRight` | `<` | `<` | `<` | |
| `sep.dot` | ` · ` | ` · ` | ` - ` | segment separator (note surrounding spaces) |
| `sep.slash` | ` / ` | `\ue0bb` | ` / ` | |
| `sep.pipe` | ` │ ` | `\ue0b3` | ` \| ` | |

#### Spinners (`SPINNER_FRAMES`, two `SpinnerType`s: `status`, `activity`)

| Preset | status frames | activity frames |
|--------|---------------|-----------------|
| unicode | `⣾ ⣽ ⣻ ⢿ ⡿ ⣟ ⣯ ⣷` | `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` |
| nerd | `󱑖 󱑋 󱑌 󱑍 󱑎 󱑏 󱑐 󱑑 󱑒 󱑓 󱑔 󱑕` (clock faces, `\u{F1456}…`) | `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` |
| ascii | `\| / - \\` | `- \\ \| /` |

koda's spinner today = the unicode **activity** braille set. oh-my-pi
distinguishes a heavier **status** spinner (`⣾⣽⣻…`) from the light activity
braille — add the status set if you want the two-tier distinction.

A theme can override frames via `symbols.spinnerFrames` (flat array = both
types, or `{status?, activity?}`).

#### Thinking-level indicators (composer / footer, paired with a label)

| Key | unicode | nerd | ascii |
|-----|---------|------|-------|
| `thinking.minimal` | `○ min` | `\u{F0A9E} min` | `[min]` |
| `thinking.low` | `◔ low` | `\u{F0A9F} low` | `[low]` |
| `thinking.medium` | `◑ med` | `\u{F0AA1} med` | `[med]` |
| `thinking.high` | `◒ high` | `\u{F0AA3} high` | `[high]` |
| `thinking.xhigh` | `◕ xhigh` | `\u{F0AA5} xhi` | `[xhi]` |
| `thinking.max` | `◉ max` | `\u{F06D} max` | `[max]` |
| `thinking.autoPending` | `⟳` | `\u{F074}` | `[~]` |

#### Checkboxes / radios (selectors, settings)

| Key | unicode | nerd | ascii |
|-----|---------|------|-------|
| `checkbox.checked` | `☑` | `\uf14a` | `[x]` |
| `checkbox.unchecked` | `☐` | `\uf096` | `[ ]` |
| `radio.selected` | `◉` | `\uf192` | `(o)` |
| `radio.unselected` | `○` | `\uf10c` | `( )` |

#### Text formatting / markdown (transcript body)

| Key | unicode | nerd | ascii | Used by |
|-----|---------|------|-------|---------|
| `format.bullet` | `•` | `\uf111` | `*` | inline bullet |
| `format.dash` | `—` | `–` | `-` | em dash |
| `format.bracketLeft` / `format.bracketRight` | `⟦` / `⟧` | `⟨` / `⟩` | `[` / `]` | chip brackets |
| `md.quoteBorder` | `▏` | `│` | `\|` | blockquote rail |
| `md.hrChar` | `─` | `─` | `-` | horizontal rule |
| `md.bullet` | `•` | `\uf111` | `*` | markdown list bullet |
| `md.colorSwatch` | `■` | `■` | `[]` | colour swatch preview |
| `advisor.rail` | `▎` | `▎` | `\|` | advisor-note left rail (heavier than quote) |

### 1.4 Icon / cmd / tool / lang / tab keys (breadth summary)

Beyond the core TUI glyphs above, the vocabulary includes large icon families.
These are mostly emoji in the unicode preset, PUA in nerd, and short text tags
in ascii. koda can adopt selectively; the status-line ones matter most.

- **`icon.*`** (~55 keys) — status-line + headers: `icon.model ⬢`,
  `icon.context ◫`, `icon.tokens 🪙`, `icon.cost 💲`, `icon.time ⏱`,
  `icon.git ⎇`, `icon.branch ⑂`, `icon.pr ⤴`, `icon.folder 📁`,
  `icon.agents 👥`, `icon.throughput ⚡`, `icon.host 🖥`, `icon.session 🆔`,
  `icon.omp π`, `icon.esc ⎋`, `icon.input ⤵`, `icon.output ⤴`,
  `icon.cache 💾`, `icon.cacheMiss ⊘`, `icon.auto ⟲`, `icon.fast ⚡`,
  `icon.rewind ↶`, `icon.pin 📌`, `icon.mic 🎤`, `icon.camera 📷` (compaction
  divider), plus `icon.extension*` for the extension registry.
- **`cmd.*`** (~50 keys) — slash-command autocomplete type indicators
  (`cmd.action ❯`, `cmd.prompt ✎`, `cmd.gear ⚙`, `cmd.rocket 🚀`, …).
  In **ascii** these are all `""` (icon column disabled).
- **`tool.*`** (~28 keys) — per-tool signature glyph on the success header:
  `tool.write ✎`, `tool.edit ✎`, `tool.bash ❯`, `tool.ssh ⇄`, `tool.lsp 💡`,
  `tool.webSearch ⌕`, `tool.browser 🌐`, `tool.eval ▶`, `tool.debug 🐞`,
  `tool.mcp 🔌`, `tool.task ⇶`, `tool.todo ☑`, `tool.memory 🧠`, `tool.ask ?`,
  `tool.resolve ✓`, `tool.delete 🗑`, `tool.move ➜`, …
- **`lang.*`** (~40 keys) — file-type icons for code-cell headers, emoji in
  unicode (`lang.rust 🦀`, `lang.python 🐍`, `lang.typescript 🟦` …), devicons
  in nerd, abbreviations in ascii (`rs`, `py`, `ts`). Four have brand tint
  colours (`LANG_BRAND_COLORS`): JS `#f7df1e`, Python `#3776ab`,
  Ruby `#cc342d`, Julia `#9558b2`; others fall back to `muted`.
- **`tab.*`** (10 keys), **`chip.image` / `chip.paste`** — settings tabs +
  composer attachment chips.

Full literal tables for these live in `symbols.ts`
(`UNICODE_SYMBOLS`/`NERD_SYMBOLS`/`ASCII_SYMBOLS`); reproduce as needed.

### 1.5 koda glyph gaps

koda's `Glyphs` covers frames, rail, pick, chevron, dot, gauge, ok/fail/
running/pending, prompt, user_bar, bullet, arrow, sep, tree, scroll, spinner,
ready. Missing vs oh-my-pi that likely matter:
- distinct **status vs activity spinner** sets,
- **powerline** separators (`sep.powerline*`) + `sep.block` for status-line chips,
- **thinking-level** glyph+label set,
- **checkbox/radio** glyphs,
- **context-gauge boundary** markers (`context.speculation`, `context.compaction`),
- **advisor rail** vs markdown quote rail distinction,
- the **icon/tool/lang** families (optional, but drive header identity).

---

## 2. ASCII fallback + capability detection (summary)

- Every glyph has an ASCII fallback (full `ASCII_SYMBOLS` map). Some columns
  (all `cmd.*`, and a few icons) are deliberately empty strings in ASCII mode.
- Preset is **user-chosen** (setting, default `unicode`), *not* auto-detected.
  No Nerd Font probe exists.
- Colour depth is auto-detected (truecolor vs 256) and colours degrade via
  `Bun.color(..., "ansi-256")` / 256-index (`\x1b[38;5;Nm`).
- `text`/empty-string colour tokens mean "terminal default fg/bg"
  (`\x1b[39m` / `\x1b[49m`); HTML export substitutes `#000000` (light) /
  `#e5e5e7` (dark).

---

## 3. Semantic colour roles (the schema) — full list

Source: `schema.ts` `themeColorsSchema` (required keys) + `ThemeBg`. This is the
authoritative role list. **69 roles total**: 61 foreground `ThemeColor` +
7 `ThemeBg` + 1 optional (`thinkingMax`). Plus an optional `export` block
(3 keys) and `vars` indirection.

`text`, `userMessageText`, `customMessageText`, `toolTitle` are commonly `""`
= terminal default.

### 3.1 Foreground roles (`ThemeColor`)

| Role | Meaning | Consumed by |
|------|---------|-------------|
| `accent` | Primary brand accent | headings, list bullets, focus, session accent seed |
| `border` | Default border | panel/card borders |
| `borderAccent` | Emphasised border | focused/active borders |
| `borderMuted` | Subtle border | dividers, low-emphasis frames |
| `success` | Success/positive | success headers, ok markers |
| `error` | Error/negative | error headers, failures |
| `warning` | Caution | warnings |
| `muted` | Secondary text | tool output, captions, de-emphasised text |
| `dim` | Tertiary/faint text | timestamps, URLs, hints |
| `text` | Body text (`""`=default) | transcript body |
| `thinkingText` | Reasoning/thinking body text | thinking transcript |
| `userMessageText` | User bubble text (`""`=default) | user message body |
| `customMessageText` | Custom message text | custom/system messages |
| `customMessageLabel` | Custom message label/badge | label on custom messages |
| `toolTitle` | Tool header title | tool execution header |
| `toolOutput` | Tool stdout/body | tool output block |
| `mdHeading` | Markdown headings | `#`/`##` lines |
| `mdLink` | Markdown link text | `[text]` |
| `mdLinkUrl` | Markdown link URL | `(url)` |
| `mdCode` | Inline code | `` `code` `` |
| `mdCodeBlock` | Fenced code text | code block body (base) |
| `mdCodeBlockBorder` | Code block border | fence rule/border |
| `mdQuote` | Blockquote text | `> quote` |
| `mdQuoteBorder` | Blockquote rail | left rail of quote |
| `mdHr` | Horizontal rule | `---` line |
| `mdListBullet` | List bullet | `- item` marker |
| `toolDiffAdded` | Diff added line | `+` diff lines (colour-blind ⇒ shifted to blue) |
| `toolDiffRemoved` | Diff removed line | `-` diff lines |
| `toolDiffContext` | Diff context line | unchanged diff lines |
| `syntaxComment` | Syntax: comment | code highlighting |
| `syntaxKeyword` | Syntax: keyword | code highlighting |
| `syntaxFunction` | Syntax: function | code highlighting |
| `syntaxVariable` | Syntax: variable | code highlighting |
| `syntaxString` | Syntax: string | code highlighting |
| `syntaxNumber` | Syntax: number | code highlighting |
| `syntaxType` | Syntax: type | code highlighting |
| `syntaxOperator` | Syntax: operator | code highlighting |
| `syntaxPunctuation` | Syntax: punctuation | code highlighting |
| `thinkingOff` | Thinking border, level off | thinking panel border |
| `thinkingMinimal` | Thinking border, minimal | thinking panel border |
| `thinkingLow` | Thinking border, low | thinking panel border |
| `thinkingMedium` | Thinking border, medium | thinking panel border |
| `thinkingHigh` | Thinking border, high | thinking panel border |
| `thinkingXhigh` | Thinking border, xhigh | thinking panel border |
| `thinkingMax?` | Thinking border, max (optional; falls back to xhigh) | thinking panel border |
| `bashMode` | Bash-mode accent | composer bash-mode border |
| `pythonMode` | Python-mode accent | composer python-mode border |
| `statusLineSep` | Status-line separator | segment dividers |
| `statusLineModel` | Status-line model segment | model name |
| `statusLinePath` | Status-line path segment | cwd path |
| `statusLineGitClean` | Git clean state | branch when clean |
| `statusLineGitDirty` | Git dirty state | branch when dirty |
| `statusLineContext` | Context-usage segment | context gauge/percent |
| `statusLineSpend` | Spend/budget segment | spend readout |
| `statusLineStaged` | Git staged count | staged files count |
| `statusLineDirty` | Git modified count | modified files count |
| `statusLineUntracked` | Git untracked count | untracked files count |
| `statusLineOutput` | Output tokens segment | output token count |
| `statusLineCost` | Cost segment | $ cost |
| `statusLineSubagents` | Subagents segment | active subagent count |

### 3.2 Background roles (`ThemeBg`)

| Role | Meaning | Consumed by |
|------|---------|-------------|
| `selectedBg` | Selection highlight | selected list rows, menus |
| `userMessageBg` | User bubble fill | user message block |
| `customMessageBg` | Custom message fill | custom/system message block |
| `toolPendingBg` | Tool pending fill | running tool block |
| `toolSuccessBg` | Tool success fill | succeeded tool block |
| `toolErrorBg` | Tool error fill | failed tool block |
| `statusLineBg` | Status-line background | status line + light/dark classification source |

### 3.3 Optional `export` block (HTML export only)

`pageBg`, `cardBg`, `infoBg` — page/card/info surfaces for HTML transcript
export. Not needed for the live TUI but present in every default theme.

### 3.4 Resolution & derived behaviour

- **`vars`**: themes define named colours then reference them; `resolveVarRefs`
  chases references (detects cycles). Values are hex (`#rrggbb`) or 256-index
  integers.
- **Light/dark classification**: by perceptual luma of **`statusLineBg`**
  (>0.5 ⇒ light), *not* `userMessageBg` (some light themes have dark bubbles).
- **Contrast helpers**: `getContrastFgAnsi` / `getFgOnBgAnsi` pick near-black
  (`#000000`) or near-white (`#e5e5e7`) fg over a fill by luma.
- **Colour-blind mode**: shifts `toolDiffAdded` green→blue via HSV
  (`{h:60, s:0.71}`).
- **Session accent**: a per-session accent is derived from `accent` + the set of
  "major" colours (must not hue-collide) + surface luminance.

---

## 4. Four representative themes — full hex values

Values below are as written in the JSON (after `vars` substitution where a role
points at a var). 256-index integers left as integers.

### 4.1 DARK (`dark.json`) — default dark

```
accent            #febc38    border            #178fb9    borderAccent      #0088fa
borderMuted       #3d424a    success           #89d281    error             #fc3a4b
warning           #e4c00f    muted             #777d88    dim               #5f6673
text              (default)  thinkingText      #777d88    customMessageLabel#b281d6
toolOutput        #777d88    mdHeading         #febc38    mdLink            #0088fa
mdLinkUrl         #5f6673    mdCode            #e5c1ff    mdCodeBlock       #9CDCFE
mdCodeBlockBorder #777d88    mdQuote           #777d88    mdQuoteBorder     #3d424a
mdHr              #3d424a    mdListBullet      #febc38    toolDiffAdded     #89d281
toolDiffRemoved   #fc3a4b    toolDiffContext   #777d88
syntaxComment #6A9955  syntaxKeyword #569CD6  syntaxFunction #DCDCAA  syntaxVariable #9CDCFE
syntaxString  #CE9178  syntaxNumber  #B5CEA8  syntaxType     #4EC9B0  syntaxOperator #D4D4D4
syntaxPunctuation #D4D4D4
thinkingOff #3d424a  thinkingMinimal #5f6673  thinkingLow #178fb9  thinkingMedium #0088fa
thinkingHigh #b281d6  thinkingXhigh #e5c1ff   bashMode #0088fa      pythonMode #e4c00f
statusLineBg      #121212    statusLineSep     244(256)   statusLineModel   #d787af
statusLinePath    #00afaf    statusLineGitClean#5faf5f    statusLineGitDirty#d7af5f
statusLineContext #8787af    statusLineSpend   #5fafaf    statusLineStaged  70(256)
statusLineDirty   178(256)   statusLineUntracked 39(256)  statusLineOutput  205(256)
statusLineCost    205(256)   statusLineSubagents #febc38
-- bg --
selectedBg #31363f  userMessageBg #221d1a  customMessageBg #2a2530
toolPendingBg #1d2129  toolSuccessBg #161a1f  toolErrorBg #291d1d  statusLineBg #121212
-- export -- pageBg #18181e  cardBg #1e1e24  infoBg #26262e
```

### 4.2 LIGHT (`light.json`) — default light

```
accent #5a8080  border #547da7  borderAccent #5a8080  borderMuted #b0b0b0
success #588458  error #aa5555  warning #9a7326  muted #6c6c6c  dim #767676
text (default)  thinkingText #6c6c6c  customMessageLabel #7e57c2  toolOutput #6c6c6c
mdHeading #9a7326  mdLink #547da7  mdLinkUrl #767676  mdCode #5a8080  mdCodeBlock #588458
mdCodeBlockBorder #6c6c6c  mdQuote #6c6c6c  mdQuoteBorder #6c6c6c  mdHr #6c6c6c
mdListBullet #588458  toolDiffAdded #588458  toolDiffRemoved #aa5555  toolDiffContext #6c6c6c
syntaxComment #008000  syntaxKeyword #0000FF  syntaxFunction #795E26  syntaxVariable #001080
syntaxString #A31515  syntaxNumber #098658  syntaxType #267F99  syntaxOperator #000000
syntaxPunctuation #000000
thinkingOff #b0b0b0  thinkingMinimal #767676  thinkingLow #547da7  thinkingMedium #5a8080
thinkingHigh #875f87  thinkingXhigh #8b008b  bashMode #588458  pythonMode #9a7326
statusLineBg #e0e0e0  statusLineSep #808080  statusLineModel #875f87  statusLinePath #005f87
statusLineGitClean #005f00  statusLineGitDirty #af5f00  statusLineContext #5f5f87
statusLineSpend #005f5f  statusLineStaged 28  statusLineDirty 136  statusLineUntracked 31
statusLineOutput 133  statusLineCost 133  statusLineSubagents #5a8080
-- bg -- selectedBg #d0d0e0  userMessageBg #e8e8e8  customMessageBg #ede7f6
toolPendingBg #e8e8f0  toolSuccessBg #e8f0e8  toolErrorBg #f0e8e8  statusLineBg #e0e0e0
-- export -- pageBg #f8f8f8  cardBg #ffffff  infoBg #fffae6
```

### 4.3 dark-tokyo-night (`defaults/dark-tokyo-night.json`) — popular

```
accent #bb9af7 (purple)  border #7aa2f7  borderAccent #7dcfff  borderMuted #363b54
success #9ece6a  error #db4b4b (darkRed)  warning #e0af68  muted #51597d (comment)  dim #51597d
text (default)  thinkingText #51597d  customMessageBg #221d2e  customMessageLabel #bb9af7
toolPendingBg #1a1e2e  toolSuccessBg #16191f  toolErrorBg #291d1d  toolTitle #bb9af7  toolOutput #51597d
mdHeading #bb9af7  mdLink #7dcfff  mdLinkUrl #51597d  mdCode #c0caf5  mdCodeBlock #a9b1d6
mdCodeBlockBorder #363b54  mdQuote #51597d  mdQuoteBorder #363b54  mdHr #363b54  mdListBullet #7dcfff
toolDiffAdded #9ece6a  toolDiffRemoved #f7768e  toolDiffContext #51597d
syntaxComment #51597d  syntaxKeyword #bb9af7  syntaxFunction #7aa2f7  syntaxVariable #c0caf5
syntaxString #9ece6a  syntaxNumber #ff9e64  syntaxType #0db9d7  syntaxOperator #a9b1d6
syntaxPunctuation #a9b1d6
thinkingOff #363b54  thinkingMinimal #51597d  thinkingLow #7aa2f7  thinkingMedium #7dcfff
thinkingHigh #bb9af7  thinkingXhigh #c9a0ff  bashMode #7dcfff  pythonMode #e0af68
statusLineBg #0f1019  statusLineSep 238  statusLineModel #bb9af7  statusLinePath #7dcfff
statusLineGitClean #9ece6a  statusLineGitDirty #e0af68  statusLineContext #7aa2f7
statusLineSpend #73daca  statusLineStaged 70  statusLineDirty 178  statusLineUntracked 39
statusLineOutput 205  statusLineCost 205  statusLineSubagents #ff9e64
-- bg -- selectedBg #2a2f41  userMessageBg #16161e  customMessageBg #221d2e
toolPendingBg #1a1e2e  toolSuccessBg #16191f  toolErrorBg #291d1d  statusLineBg #0f1019
-- export -- pageBg #16161e  cardBg #1a1b26  infoBg #2a2639
```
Note: koda's `tokyo-night` maps `error` to `#f7768e` (the bright red); oh-my-pi
uses `darkRed #db4b4b` for `error` and reserves `#f7768e` for diff-removed.

### 4.4 dark-monochrome (`defaults/dark-monochrome.json`) — high-contrast / grayscale

Single teal accent `#5fafaf` over a 9-step gray ramp (gray1 `#1a1a1a` →
gray9 `#e0e0e0`); status colours are desaturated (`errorRed #8a5555`,
`warningYellow #8a8a55`, `successGreen #558a55`).

```
accent #5fafaf  border #555555  borderAccent #5fafaf  borderMuted #3a3a3a
success #558a55  error #8a5555  warning #8a8a55  muted #8a8a8a  dim #707070
text (default)  thinkingText #707070  customMessageLabel #5fafaf  toolTitle #c0c0c0  toolOutput #8a8a8a
mdHeading #e0e0e0  mdLink #5fafaf  mdLinkUrl #707070  mdCode #c0c0c0  mdCodeBlock #a5a5a5
mdCodeBlockBorder #555555  mdQuote #8a8a8a  mdQuoteBorder #555555  mdHr #555555  mdListBullet #5fafaf
toolDiffAdded #558a55  toolDiffRemoved #8a5555  toolDiffContext #8a8a8a
syntaxComment #707070  syntaxKeyword #c0c0c0  syntaxFunction #e0e0e0  syntaxVariable #a5a5a5
syntaxString #8a8a8a  syntaxNumber #5fafaf  syntaxType #c0c0c0  syntaxOperator #a5a5a5
syntaxPunctuation #8a8a8a
thinkingOff #3a3a3a  thinkingMinimal #555555  thinkingLow #707070  thinkingMedium #8a8a8a
thinkingHigh #a5a5a5  thinkingXhigh #c0c0c0  bashMode #5fafaf  pythonMode #f0c040
statusLineBg #0d0d0d  statusLineSep #555555  statusLineModel #a5a5a5  statusLinePath #5fafaf
statusLineGitClean #558a55  statusLineGitDirty #8a8a55  statusLineContext #8a8a8a
statusLineSpend #5fafaf  statusLineStaged #558a55  statusLineDirty #8a8a55  statusLineUntracked #8a8a8a
statusLineOutput #a5a5a5  statusLineCost #a5a5a5  statusLineSubagents #5fafaf
-- bg -- selectedBg #3a3a3a  userMessageBg #2a2a2a  customMessageBg #3a3a3a
toolPendingBg #1a1a1a  toolSuccessBg #2a2a2a  toolErrorBg #2a1a1a  statusLineBg #0d0d0d
-- export -- pageBg #0d0d0d  cardBg #1a1a1a  infoBg #3a3a3a
```

(Bonus reference — dark-catppuccin, since koda ships `catppuccin-mocha`: accent
`peach #fab387`, border `blue #89b4fa`, borderAccent `lavender #b4befe`,
success `#a6e3a1`, error `#f38ba8`, warning `#f9e2af`, statusLineBg
`crust #11111b`, toolTitle `lavender`, mdHeading `peach`, mdListBullet `peach`.
koda maps accent to blue `#89b4fa` instead of peach — a divergence worth noting.)

---

## 5. Gradient / shimmer / animation handling

Source: `shimmer.ts`. Used for the animated "working…" line and progress
sweeps.

- **Modes** (`display.shimmer` setting): `classic`, `kitt`, `disabled`.
- **Three-tier palette** (`ShimmerPalette`): `low`, `mid`, `high` tiers, each a
  `ThemeColor` role (or raw ANSI), plus a `bold` flag for the crest.
  Default palette `DEFAULT_SHIMMER_PALETTE = { low:"dim", mid:"muted",
  high:"accent", bold:true }`.
- **Tier thresholds**: intensity ≥ 0.65 ⇒ `high`; ≥ 0.22 ⇒ `mid`; else `low`.
- **Classic** profile: a cosine bump band (`CLASSIC_BAND_HALF_WIDTH = 6`,
  `CLASSIC_PADDING = 10`) sweeps left→right.
- **KITT** profile: single bright head ping-pongs with a quadratic-decay trail
  (`KITT_HEAD_HALF = 0.6`, `KITT_TRAIL_LEN = 7`), no leading glow.
- **Velocity**: fixed `SHIMMER_SPEED_CELLS_PER_S = 30` cells/sec so the band
  advances ≤1 cell/frame at the 30fps redraw cadence regardless of text length.
- **Disabled** mode: no animation; text painted flat in its `mid` tier so the
  line stays legible.
- Performance: palettes compile to ready `open/close` ANSI pairs cached per
  `(theme, palette)`; same-tier runs coalesce into one escape pair per run.
- No multi-stop RGB gradient interpolation — shimmer is a 3-tier stepped
  colour cycle, not a continuous gradient. Session-accent derivation
  (`theme-class.ts`) is the only OKLCH/hue-aware colour math, and it is
  per-session, not per-frame.

koda has no shimmer today. To match: implement a classic cosine-band sweep over
the working line using three theme roles (`dim`/`muted`/`accent`) with a bold
crest, driven at a fixed cells/sec.

---

## 6. Summary — colour roles koda most likely lacks

koda's `Theme` has ~26 fields covering the common cases (text, muted, accent,
accent_alt, success/warning/error/info, border/border_focus/surface, 5 bg
fills, heading, 3 diff, 6 syntax). oh-my-pi defines **69 roles**. Gaps:

```
 1. thinkingText / thinkingOff / thinkingMinimal / thinkingLow /
    thinkingMedium / thinkingHigh / thinkingXhigh / thinkingMax  (8 reasoning-tier roles)
 2. statusLineModel / statusLinePath / statusLineContext / statusLineSpend  (status segments)
 3. statusLineGitClean / statusLineGitDirty / statusLineStaged /
    statusLineDirty / statusLineUntracked                        (git-state colours)
 4. statusLineOutput / statusLineCost / statusLineSubagents / statusLineSep   (status segments)
 5. statusLineBg (dedicated) — koda reuses `surface`, oh-my-pi has a distinct bg
 6. bashMode / pythonMode                                        (composer mode accents)
 7. customMessageBg / customMessageText / customMessageLabel      (custom-message role trio)
 8. userMessageText                                               (koda has bg_user but no fg)
 9. toolPendingBg (distinct) — koda folds pending into bg_tool
10. toolTitle / toolOutput                                       (tool header vs body split)
11. mdLink / mdLinkUrl / mdCode / mdCodeBlock / mdCodeBlockBorder (markdown fg roles)
12. mdQuote / mdQuoteBorder / mdHr / mdListBullet                 (markdown structure roles)
13. syntaxVariable / syntaxOperator / syntaxPunctuation           (3 extra syntax roles)
14. borderMuted (koda has border + border_focus, no muted tier)
15. dim as a distinct tier below muted (koda folds both into `muted`)
```

Highest-impact for parity: the **status-line segment roles** (§3.1, ~15 keys),
the **thinking-tier** roles (8 keys), and the **markdown fg roles** (~9 keys) —
these are what visibly differentiate oh-my-pi's status line, reasoning panels,
and rendered transcript from a flat palette.
