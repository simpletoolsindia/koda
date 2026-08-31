# koda spec: assistant message rendering, markdown, streaming

Extracted from oh-my-pi (TypeScript). Target: Rust/ratatui reimplementation with
visual parity. Source files cited inline as `path:line`.

Primary sources:
- `packages/tui/src/components/markdown.ts` — the `Markdown` component (block + inline render, streaming lex/highlight caches).
- `packages/coding-agent/src/modes/components/assistant-message.ts` — assistant turn component (thinking, images, errors, stable-row publication).
- `packages/coding-agent/src/modes/controllers/streaming-reveal.ts` — grapheme reveal cadence.
- `packages/coding-agent/src/utils/thinking-display.ts` — thinking text folding.
- `packages/coding-agent/src/modes/theme/tui-adapters.ts`, `dark.json`, `symbols.ts` — colors + glyphs.

Colors below are from the built-in `dark` theme (`dark.json`). Roles matter more than
exact hex; koda should define a theme table keyed by these role names.

---

## 0. The streaming algorithm (KEY FINDING)

oh-my-pi avoids whole-message reflow with **three cooperating layers**. Reimplement all three.

### Layer A — grapheme-paced reveal (typewriter cadence)
`StreamingRevealController` (`streaming-reveal.ts`). The provider delivers the *entire
current message* each delta (content blocks with growing text). The controller does NOT
render each delta directly; it reveals a monotonically increasing prefix measured in
**graphemes** (Intl.Segmenter clusters, not bytes/codepoints).

- Frame timer: `setInterval` at `STREAMING_REVEAL_FRAME_MS = 1000/30` (33.3 ms, ~30 fps).
- Per tick advance: `revealed += nextStep(backlog)` where
  `nextStep(backlog) = max(MIN_STEP=3, ceil(backlog / CATCHUP_FRAMES=8))`.
  So it always reveals ≥3 graphemes/frame, and drains any backlog over ~8 frames
  (fast when far behind, smooth when near the tip). Clamp to `total`.
- `visibleUnits(message)` = sum of grapheme counts of every `text` block plus every
  visible `thinking` block (after fold formatting). `buildDisplayMessage(target, revealed,…)`
  walks blocks, spending `revealed` budget across them in order, slicing the block that
  straddles the boundary to its first N graphemes.
- Grapheme counting/slicing is memoized per block (`BlockUnitCounter`): because streaming
  is append-only, only the final grapheme cluster of the previous text can change, so only
  the suffix from that cluster is re-segmented. Reuse this optimization.
- Boundary rule: if the message contains a `toolCall` block, reveal jumps straight to
  `total` (no typewriter) — a tool call is a transcript-order boundary; finish the leading
  text immediately so the tool card can render after it.
- Catch-up coalescing: while behind, per-delta renders are skipped (the tick renders the
  latest target at 30 fps). When caught up, a new delta sets `targetDirty` and defers to
  the next tick, so a burst of post-catch-up tokens coalesces into one render.
- `getSmoothStreaming() === false` disables the typewriter: reveal = total immediately.

### Layer B — frozen block prefix (stops re-lex/re-wrap of settled content)
`Markdown` component. marked has no resumable lexer, but block tokenization is local across
a blank-line (`\n\n`) boundary when fences are balanced:
`lex(prefix) ++ lex(tail) === lex(prefix + tail)`.

- `#streamPrefixText` / `#streamPrefixTokens`: the largest blank-line-bounded prefix whose
  block tokens are frozen. On append-only growth, re-lex **only the grown tail**, turning
  O(N²) reveal into O(N). See `stableBlockBoundary` (`markdown.ts:1161`) and `lexWindowed` (`:1220`).
- `setText(text)` (`markdown.ts:1745`) detects append via `text.startsWith(this.#text)` and
  keeps caches; a non-append edit drops them.
- `transientRenderCache` flag marks in-flight renders. Completed (earlier) blocks render in
  **final** mode (syntax-highlighted, LRU-cached, byte-stable) even mid-stream; only the
  actively streaming tail renders transiently. This is what makes settled rows byte-identical
  to the eventual finalized render, so they can retire into the terminal scrollback safely.

### Layer C — line-level / append-only stable-row publication
`assistant-message.ts` (`AssistantMessageComponent`, `transcriptBlockMode = "appendOnly"`).
Rendered rows that are guaranteed never to change are "published" as `TranscriptStableRow`s
and pushed into native terminal scrollback, leaving only the mutable tail in the live viewport.

- Only the **leading run of visible thinking blocks** publishes this way, because raw thinking
  only ever appends (its markdown prefix cannot be revised by later deltas), whereas streamed
  **text** deltas *can* revise earlier markdown, so text never publishes mid-stream.
- `Markdown.getLastRenderStableText()` returns the frozen source prefix (Layer B) of the
  streaming block. A snapshot publishes only when: render is transient, no marker rows,
  fast-path items exist, and non-blank content exists *past* the frozen boundary (so the
  prose-ellipsis tail can't leak into published bytes).
- Guards enforce append-only: `isSnapshotExtension` (each prior part unchanged except the last
  which may only gain a suffix), `isRowPrefix` (published rows must be a prefix of the current
  blank-trimmed render), and each new snapshot must add ≥1 physical row at every width.
  Nothing is ever retracted.

### Fast path (shape-stable in-place update)
`assistant-message.ts` `#tryFastPathUpdate` / `#computeShapeKey`: when the message *shape*
(block types + which are visible) is unchanged, reuse the existing `Markdown` children and
call `setText` only on the child whose source grew. Only the **last** (streaming) child may
mutate in place; a delta into an earlier child forces a full teardown+rebuild.

**koda implementation guidance:** model each block as a widget; keep a byte offset of the
"committed prefix" per block; only re-parse/re-wrap from the last blank-line boundary; render
committed lines once and cache; drive reveal from a 30 fps tick advancing a grapheme cursor.

---

## 1. Markdown element treatment

Renderer: `Markdown.#renderToken` (`markdown.ts:2826`). `paddingX`/`paddingY` are set by the
caller (assistant text uses `paddingX=1, paddingY=0`). Code block content indent = 2 spaces
(`codeBlockIndent`, constructor default 2).

Blank-line rule (uniform): after most block tokens a single empty row is appended **unless the
next token is a `space` token** (markdown blank line) — i.e. don't double a blank line. Lists
also suppress the trailing blank when a `space`/`list` follows.

| Element | Glyph / prefix | Color role | Indent | Surrounding blanks |
|---|---|---|---|---|
| **H1** | `bold(underline(text))`, no `#`. If terminal supports text-sizing, 2× sized glyphs + reserved 2nd row; else plain. | `mdHeading` = `#febc38` (amber). Also bold + underline. | none | 1 blank row after (unless space follows) |
| **H2** | `bold(text)`, no `#` prefix | `mdHeading` amber, bold | none | 1 blank after |
| **H3** | `bold("### " + text)` — hash prefix kept | `mdHeading` amber, bold | none | 1 blank after |
| **H4** | `bold("#### " + text)` (same branch as H3: any depth ≥3 keeps the `#`×depth prefix + space) | `mdHeading` amber, bold | none | 1 blank after |
| **Bold** | inline, no marker | `theme.bold()` (SGR bold; no color change) | inline | — |
| **Italic** | inline, no marker | `theme.italic()` (SGR italic) | inline | — |
| **Strikethrough** | inline (`~~`) | `chalk.strikethrough` | inline | — |
| **Inline code** | no backticks shown; may render a color swatch `■` if the span is a hex/named color (`codespanSwatch`) | `mdCode` = `#e5c1ff` (light violet) | inline | — |
| **Fenced code** | opening line literal `` ```lang `` and closing `` ``` `` (see §2) | border lines `mdCodeBlockBorder` = `gray` (`#777d88`); body `mdCodeBlock` = `#9CDCFE` or syntax colors | body indented 2 spaces | 1 blank after |
| **Bullet list** | `- ` (literal hyphen + space) | bullet styled `mdListBullet` = `accent` `#febc38` | nested lists add `"  "` (2 spaces) per depth; continuation rows hang-indent to bullet width | no blank between items; list spacing handled by following space token |
| **Numbered list** | `N. ` (e.g. `1. `, `10. `), starts at `token.start` | `mdListBullet` accent | continuation hangs to full number width (`"10. "` = 4 cells) | as above |
| **Block quote** | left border `▏` + space, prepended per wrapped line | border `mdQuoteBorder` = `darkGray` `#3d424a`; content `mdQuote` = `gray`, italic | content width = width−2 | trailing blank rows inside quote trimmed; 1 blank after quote |
| **Table** | sharp box-drawing (see §1 note) | header cells bold; borders unstyled (default fg) | full width | 1 blank after |
| **Horizontal rule** | fill char repeated `min(width, 80)`; char = source char if present else `─` (`md.hrChar`) | `mdHr` = `darkGray` `#3d424a` | none | 1 blank after |
| **Link** | if text==href: just the styled+underlined text. Else: `text (url)` — text underlined + OSC-8 hyperlink, then space, then `(href)`. | link text `mdLink` = `#0088fa` (blue), underlined; url suffix `mdLinkUrl` = `dimGray` `#5f6673` | inline | — |

Table border glyphs (`boxSharp` / `symbols.table`, unicode preset):
corners `┌ ┐ └ ┘`, `horizontal ─`, `vertical │`, `teeDown ┬`, `teeUp ┴`, `teeLeft ┤`, `teeRight ├`, `cross ┼`.
Row layout: `│ cell │ cell │`; overhead per row = `3*cols + 1`. Top border, header rows (bold),
separator (`├─┼─┤`), body rows. Cell lines are terminated with `\x1b[22m\x1b[23m\x1b[39m` so
open styles don't bleed into borders. If too narrow (`availableForCells < cols`), falls back to
raw wrapped markdown text.

ASCII symbol preset (fallback when no unicode): quoteBorder `|`, hrChar `-`, bullet `•`→`*`-ish,
table uses `+ - |`. Nerd-font preset uses `` (U+F111) for bullets. koda should default to the
unicode preset glyphs above.

---

## 2. Fenced code blocks

Two distinct renderers — **do not confuse them**:

**(a) Assistant markdown prose fences** (`Markdown.#renderToken` case `"code"`, `:2917`):
- **No box border, no fill.** Rendered as three parts:
  1. opening line: literal `` ```language `` styled `mdCodeBlockBorder` (gray). Language string
     is whatever followed the opening fence (may be empty → just `` ``` ``).
  2. body lines: each prefixed by 2 spaces (`codeBlockIndent`), styled either by syntax
     highlighter or flat `mdCodeBlock` (`#9CDCFE`).
  3. closing line: literal `` ``` `` styled gray.
- **Language shown?** Yes — echoed verbatim in the opening `` ``` `` line. No separate label chip.
- **Line numbers?** No (prose fences have no gutter).
- **Syntax highlighting:** `highlightCode(code, lang)` via `MarkdownTheme.highlightCode`
  (native highlighter; VS-Code-dark-like `syntax*` colors, e.g. keyword `#569CD6`, string
  `#CE9178`, comment `#6A9955`, number `#B5CEA8`, type `#4EC9B0`, function `#DCDCAA`).
  Unsupported language → flat `mdCodeBlock` color.
- **Streaming highlight:** open (unclosed) fence uses a stateful incremental highlighter
  (`createHighlightStream`) that highlights *completed* lines only; the trailing partial line
  stays flat `mdCodeBlock`. Once the closing fence arrives, the whole block is re-highlighted
  via `highlightCode` so bytes match the finalized render. Diff-family langs fall back to
  per-line highlighting. (`#renderCodeBodyLines` `:2576`, `#highlightStreamingLines`.)
- **Overflow:** long lines wrap at content width (ANSI-aware wrap). Mermaid fences with a
  resolver render as ASCII art clipped to width.

**(b) Tool/eval code cells** (`tui/code-cell.ts` `renderCodeCell` → `output-block.ts`
`renderOutputBlock`): these DO get a bordered/boxed panel with a header (status icon,
`[i/total]`, optional language icon, title), optional line-number gutter
(`String(n).padStart(max(2,width))` styled `dim`), collapse to `codeMaxLines=12`
(default) with a `… +N more lines (ctrl+o to expand)` footer, and an optional `Output`
section (`outputMaxLines=6`). Use this style for koda's *tool output* cells, not for
assistant prose fences.

Expand hint: `formatExpandHint` / `expandKeyHint` → `ctrl+o`. More-items footer:
`… +N more line(s)` in `dim`.

---

## 3. Streaming — see §0. Summary of anti-reflow mechanisms

1. **Grapheme-paced reveal** at 30 fps, step `max(3, ceil(backlog/8))` — content appears at a
   bounded rate regardless of provider burst size, so markdown work per frame is bounded.
2. **Frozen block prefix** — only the tail past the last blank-line boundary is re-lexed/
   re-wrapped; settled blocks render in final (cached, highlighted, byte-stable) mode.
3. **Append-only stable rows** — leading thinking prefix is committed to native scrollback and
   never rewritten (verified by prefix/extension guards). Text never commits early (its
   markdown can be revised by later deltas), which is precisely why text does not reflow into
   scrollback but the visible viewport tail does.
4. **Fast path** — shape-stable updates mutate only the last child's text in place.

Net effect: the terminal shows a stable, growing transcript; only the last few lines (the
active block's tail) are volatile and re-rendered per frame.

---

## 4. Reasoning / thinking blocks

Config: `hideThinkingBlock` (toggles visible vs collapsed), `proseOnlyThinking` (folds fenced
code to `…`). Formatting: `formatThinkingForDisplay` (`thinking-display.ts`).

- **Expanded form (hideThinkingBlock = false):** thinking renders as markdown via `Markdown`,
  styled `thinkingText` = `gray` (`#777d88`) and **italic** (`color: fg("thinkingText"), italic:true`).
  Prepended in transcript order before/after text with a `Spacer(1)` separating it from following
  visible content. In `proseOnly` mode, fenced code inside thinking is elided to a trailing
  ellipsis (`…`); empty HTML-comment noise (`<!-- -->`, GPT-5 reasoning padding) is dropped.
- **Collapsed form (hideThinkingBlock = true, block still streaming):** the block is replaced by
  an animated single-line **pulse** placeholder:
  - Glyph cycles through starburst frames `✻ ✼ ❉ ❊ ✺ ✹ ✸ ✶` (`THINKING_DOTS_FRAMES`), colored
    `thinkingText` (gray). Fixed width so the line doesn't shift.
  - Label: ` Thinking` in `muted`.
  - Optional badge when genuinely streaming tokens: ` · <total>` (dim) + ` · <rate> toks/s`
    where rate color lerps dim-gray → accent (`#febc38`) by `sqrt(rate/200)`. Windowed average
    over 3 s (`SpeedTracker`), clamped to `SPEED_MAX=200`. Badge self-suppresses if rate < 0.05
    or provider reports no live token deltas.
  - Animation cadence: self-rescheduling timeout with eased (raised-cosine "breath") dwell
    between `70 ms` and `230 ms` per frame, mean ≈150 ms.
  - Only animates while: block not finalized, thinking hidden, no tool call started, and the
    active tail block is a thinking block. Once text starts / tool call streams / block seals,
    the pulse ends.
- **Key toggles:** `ctrl+t` = `app.thinking.toggle` (toggle thinking mode);
  `shift+tab` = `app.thinking.cycle` (cycle thinking level: off/min/low/med/high/xhigh/max,
  glyphs `○ ◔ ◑ ◒ ◕ ◉`). Changing level mid-stream calls `resyncVisibility()` to re-read flags.

---

## 5. "Still generating" indicator

There is **no trailing text cursor / caret** appended to streaming assistant *text* — the reveal
cadence itself is the liveness cue (text grows visibly at ~30 fps).

The only end-of-stream "generating" indicator is the **thinking pulse** described in §4 (the
animated `✻ Thinking · N · R toks/s` line), shown while the model is reasoning with thinking
hidden. When text is streaming, no separate spinner is drawn on the message; a global status
line/footer spinner exists elsewhere (out of scope here). Finalization performs the single
non-transient render that seals the block (`markTranscriptBlockFinalized`).

---

## Appendix: color role table (dark theme)

| role | hex / ref | use |
|---|---|---|
| `mdHeading` | `#febc38` | headings (also bold; H1 underline) |
| `mdLink` | `#0088fa` | link text (underlined) |
| `mdLinkUrl` | `#5f6673` (dimGray) | `(url)` suffix |
| `mdCode` | `#e5c1ff` | inline code |
| `mdCodeBlock` | `#9CDCFE` | code body (unhighlighted) |
| `mdCodeBlockBorder` | `#777d88` (gray) | `` ``` `` fence lines |
| `mdQuote` | `#777d88` (gray) | blockquote text (italic) |
| `mdQuoteBorder` | `#3d424a` (darkGray) | quote `▏` |
| `mdHr` | `#3d424a` (darkGray) | `─` rule |
| `mdListBullet` | `#febc38` (accent) | `-` / `N.` |
| `thinkingText` | `#777d88` (gray) | thinking (italic) |
| `accent` | `#febc38` | pulse rate peak, list bullets |
| `muted` | `#777d88` | " Thinking" label |
| `dim` | `#5f6673` | token count, more-lines footer |
| syntax | `syntaxKeyword #569CD6`, `syntaxString #CE9178`, `syntaxComment #6A9955`, `syntaxNumber #B5CEA8`, `syntaxType #4EC9B0`, `syntaxFunction #DCDCAA`, `syntaxVariable #9CDCFE` | code highlighting |

Glyphs (unicode preset): bullet `•` (prose uses literal `-`), quote `▏`, hr `─`, color swatch `■`,
table `┌┐└┘─│┬┴┤├┼`, thinking pulse `✻✼❉❊✺✹✸✶`.
