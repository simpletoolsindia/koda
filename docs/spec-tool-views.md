# Spec: Tool Call / Result Views (oh-my-pi → koda)

Extracted from `refs/oh-my-pi/packages/coding-agent/src`. Goal: byte-level visual
parity for how tool CALLS and RESULTS render in the transcript. All glyph
literals are copied from `modes/theme/symbols.ts` (unicode preset).

---

## 1. Global model

Each tool call is one `ToolExecutionComponent` (`modes/components/tool-execution.ts`).
It builds two possible sub-components: a **call** view and a **result** view. Most
built-in tools set `mergeCallAndResult: true` — once a result exists the call view
is suppressed and only the result renders. So in practice each finished tool is a
single card.

Three presentation families:

1. **Framed block** (`tui/output-block.ts`): a rounded box with a header bar.
   Used by: read, edit, write, bash/run, eval, web_search, lsp, todo, task, gh.
   Marked via `markFramedBlockComponent` / `framedBlock()`. Renders flush (no
   outer background tint).
2. **Inline / frameless** (`Text` component, `inline: true`): a status line plus
   an optional dim tree/bullet list. Used by: grep, glob, memory (retain/recall/
   reflect), simple single-line results.
3. **Generic fallback card** (`tools/default-renderer.ts`): a state-tinted
   background block for tools with no bespoke renderer. Padding `(0,1)`, background
   `toolPendingBg` / `toolSuccessBg` / `toolErrorBg`.

### 1.1 State → background/border color

| State                    | bg key          | border color (framed) |
|--------------------------|-----------------|-----------------------|
| partial / pending        | `toolPendingBg` | `accent`              |
| running (spinner active) | `toolPendingBg` | `accent`              |
| success (settled)        | `toolSuccessBg` | `dim` (gray)          |
| error                    | `toolErrorBg`   | `error` (red)         |
| warning                  | —               | `warning`             |

Many framed tools override `borderColor: "borderMuted"` (write, todo, edit result)
so the frame doesn't visually compete. `borderColor` for edit/write success is
`borderMuted`; error is `error`.

Diff/code frames use `contentPaddingLeft: 0` (flush) because the code gutter already
provides padding. Normal framed blocks use content padding 1 on each side.

### 1.2 Toggle / expand affordance

- Expansion is toggled by the keybinding action `app.tools.expand`, default
  **`Ctrl+O`** (`render-utils.ts` `expandKeyHint()` → falls back to `ctrl+o`).
- Expand hint text (`formatExpandHint`): dim, wrapped in theme brackets:
  ```
  [Ctrl+O: Expand]
  ```
  Bracket chars are `format.bracketLeft`/`format.bracketRight` (default `[` `]`).
  Hint is emitted only when collapsed AND there is more to show.

---

## 2. Glyphs (literal unicode preset)

Status icons (`symbols.ts` UNICODE_SYMBOLS, colored via `formatStatusIcon`):

```
status.success  ✔   (color: success/green)
status.error    ✘   (color: error/red)
status.warning  ⚠   (color: warning/yellow)
status.info     ⓘ   (color: accent)
status.pending  ⏳   (color: muted)
status.running  ⟳   (color: accent; replaced by spinner frame when animating)
status.done     •   (color: success)
status.aborted  ⏹   (color: error)
```

Tree connectors:
```
tree.branch    ├─
tree.last      └─
tree.vertical  │
tree.hook      └
```

Box (rounded — used by all framed blocks):
```
boxRound.topLeft ╭   topRight ╮   bottomLeft ╰   bottomRight ╯
horizontal ─     vertical │     teeRight ├ (as ┤/├ tees for section bars)
```
Header/separator bars are drawn as `╭───<space>label<space>────╮` — left glyph is
`topLeft` + `───` (3 horizontals) cap, then ` label `, then fill of `─`, then
`topRight`. Section separators reuse `teeRight`/`teeLeft` (`├`/`┤`).

Separators / formatting:
```
sep.dot        " · "   (space-dot-space; joins meta segments)
sep.slash      " / "
format.bullet  •
format.bracketLeft/Right  [ ]
```

Tool identity glyphs (`tool.*`, shown via `styledSymbol(key,"accent")` on settled
results in place of the ✔):
```
tool.write ✎   tool.edit ✎   tool.bash ❯   tool.lsp 💡   tool.gh ⎇
tool.webSearch ⌕   tool.task ⇶   tool.todo ☑   tool.memory 🧠
tool.delete 🗑   tool.move ➜   icon.search 🔍
```
(Nerd-font and ascii presets exist; ascii e.g. `tool.bash="$"`, `tool.edit="~"`,
`tool.webSearch="web"`.)

---

## 3. Status line format (`tui/status-line.ts`)

The single most important primitive. `renderStatusLine(options, theme)` produces:

```
<icon> <title>: <description> [<badge>] <meta · meta · meta>
```

Rules:
- `icon`: `formatStatusIcon(icon, spinnerFrame)` OR `iconOverride` (a pre-rendered
  glyph; takes precedence). If neither, no icon and no leading space.
- Space between icon and title. Title colored `titleColor` (**default `accent`**;
  many tools pass `titleColor: "toolTitle"`). Title is NOT bold here by default
  (bold only in the compact/1-line fallback path).
- `description`: prefixed with `": "`, colored `muted`.
- `badge`: ` ` + `[label]` in badge color.
- `meta[]`: ` ` + segments joined by `" · "` (`sep.dot`), colored `dim`.
- All fragments are flattened: CR/LF → single space (so a header is always 1 row).

Example rendered (collapsed grep call):
```
⏳ Grep: TODO in src, case:insensitive
```

---

## 4. Per-tool specs

Indentation convention: inline tools pass leftPad=1 to `Text(text, 1, 0)` (grep,
glob) or 0 (memory, todo, lsp). Framed blocks own their border so no extra indent.

### 4.1 read  (framed, mergeCallAndResult)

CALL line (`read-renderer.ts renderCall`):
```
⏳ Read: <path>
```
- icon `pending` (⏳), title `Read` (accent), description = clickable (OSC-8)
  shortened path (`~/…`), colored accent. Offset/limit appended as `:START-END`
  (e.g. `:10-40`).

RESULT: a framed **code cell** (or markdown cell if `contentType==text/markdown`).
Header via `renderStatusLine`:
```
Read <path> (summary: 2 elided spans) (⚠ 1 conflict)
```
- Success: NO leading status icon in the code-cell header form; title text is
  literally `Read <linked path>`. Correction suffix `(corrected from <oldpath>)`
  dim when a suffix-resolution fired.
- Byte/line truncation → warning line inside the block, wrapped in brackets, e.g.
  `[First line exceeds 256 KB limit. …]`.
- Error: framed `state:"error"` block, header `✘ Read <path>`, body = error text
  lines colored `error`, `Error:` prefix stripped.
- COLLAPSED vs EXPANDED: code cell caps rows; expanded shows full. Toggle Ctrl+O.

### 4.2 write  (framed, borderMuted, activitySummary)

CALL (`write.ts renderCall`): renders NOTHING until the streamed path settles
(guards against xd:// device writes). Then a `framedBlock`:
```
╭─── Write: <langIcon> <path> ────────────────────╮
│ <syntax-highlighted streaming content, tail window> │
│ ⟳ (streaming)                                    │
╰──────────────────────────────────────────────────╯
```
- Header: title `Write` (accent), description `<langIcon> <accent path>`. **No
  status icon** on the head row (would pin scrollback commit boundary). Liveness
  cue is the trailing `(streaming)` line, dim, prefixed by the running spinner
  glyph when active.
- state `pending`, border `borderMuted`.

RESULT: same frame; header settles to tool glyph:
```
✎ Write: <langIcon> <linked path>
```
- Success uses `iconOverride = tool.write (✎)` accent, state `success`.
- Error: `✘ Write: <path>` + framed error body.
- `activitySummary`: label `Write`, detail = shortened path (device writes read as
  the mounted tool, e.g. `LSP · references foo`).

### 4.3 edit / apply_patch  (framed, borderMuted, mergeCallAndResult)

CALL header (`renderEditHeader`), title from op:
```
⏳ Edit: <langIcon> <path>          (op=edit)
⏳ Create: <langIcon> <path>        (op=create)
⏳ Delete: <langIcon> <path>        (op=delete)
```
- Description = `<langIcon> <accent linked path>`. Rename shows `path → newpath`
  with dim `→`.
- Delete/move-only edits render as a single INLINE row (no empty frame): pending
  uses ⏳; completed uses `tool.delete 🗑` or `tool.move ➜`.
- Streaming diff preview: framed body shows a tail-window of the colored diff
  (`EDIT_STREAMING_PREVIEW_LINES = 12`), with a top marker `… (content above)`
  when clipped, and a trailing `⟳ (streaming)` / `(preview)` label line.

RESULT: `framedBlock`, `contentPaddingLeft: 0`, border `borderMuted` (success) /
`error`. Header:
```
✎ Edit: <langIcon> <linked path> [+12/-3]
```
- Success icon = `tool.edit ✎` accent. **Diff stats suffix** rides the header bar:
  ` [` + `+N` (toolDiffAdded/green) + dim `/` + `-M` (toolDiffRemoved/red) + `]`,
  brackets dim. Zero-change → no suffix.
- Body = colored diff section (see §5). No-op result → dim `No changes were made
  to <path>.`
- Diagnostics appended below (see §6).

COLLAPSED diff truncation: `truncateDiffByHunk(diff, DIFF_COLLAPSED_HUNKS=8,
DIFF_COLLAPSED_LINES=40)` then visual row slice to 40 rows. Remainder line:
```
… (2 more hunks, 15 more lines) [Ctrl+O: Expand]
```
colored `toolOutput`. Expanded shows full diff.

### 4.4 bash / run  (framed via `createShellRenderer`)

CALL: framed block. Header:
```
⟳ <title>            (title from resolveTitle, e.g. "Run" / command summary)
```
- icon `running` (spinner) if `spinnerFrame` set, else `pending` (⏳). state
  matches. Body = command preview lines from `formatBashCommandLines`:
  ```
  $ cd subdir && FOO=bar <highlighted command>
  ```
  `$` prefix + optional `cd <workdir> &&` + env assignments, all dim; command
  syntax-highlighted (bash). Multi-line: prefix only on first line.
- Preview capped by `capPreviewLines` (viewport tail window,
  `previewWindowRows() = rows-20, min 6`); top marker:
  ```
  … 12 earlier lines [Ctrl+O: Expand]
  ```

RESULT: framed block. Header success:
```
❯ <title>            (iconOverride = tool.bash ❯, accent)
```
- Failure: `✘ <title>` (error) or `⚠ <title>` (warning, on timeout). Partial: ⏳.
- Body = command lines + `Output` section. Exit failure appends
  `Command exited with code N`. Stats line collects: `Backgrounded: <jobId>`,
  `Wall: N.Ns`, `Timeout: Ns` / `Timeout: disabled` / `Timeout: Ns (requested Ms
  clamped)`.
- Output preview default 10 lines (`BASH_DEFAULT_PREVIEW_LINES`), expanded uncaps.

### 4.5 grep / search  (inline, mergeCallAndResult, leftPad=1)

CALL:
```
⏳ Grep: <pattern>  in src, case:insensitive · skip:5
```
- title `Grep` (`titleColor: toolTitle`), description = pattern (or `?`). meta:
  `in <paths>`, `case:insensitive`, `gitignore:false`, `skip:N`.

RESULT header:
```
🔍 Grep: <pattern>  <N> matches · <M> files · truncated
```
- iconOverride = `icon.search 🔍` colored `toolTitle`. On truncation the icon
  becomes `⚠` and a dim `truncated` meta appended.
- Counts via `formatCount` (auto-pluralized: `1 match` / `5 matches`,
  `1 file` / `3 files`).
- Zero matches: `⚠ Grep: <pattern>  0 matches` + `⚠ No matches found` (muted).
- Body = tree list of match groups (`renderTreeList`), collapsed to a limit,
  expanded shows all. Continuation lines use `├─`/`└─` tree glyphs, dim.

### 4.6 glob / find  (inline, mergeCallAndResult, leftPad=1)

CALL:
```
⏳ Glob: <pattern>  limit:100
```
RESULT:
```
🔍 Glob: <pattern>  <N> files · in <scope> · truncated
```
- iconOverride `icon.search 🔍` toolTitle; `⚠` on truncation/timeout.
- Empty: `⚠ Glob: … 0 files` + `⚠ No files found` (muted); timeout →
  `No matches before timeout (scan incomplete)` and meta `0 files · timed out`.
- Body = `renderFileList` (files hyperlinked, dirs marked with trailing `/`),
  collapsed limit `COLLAPSED_LIST_LIMIT`, expand for full. Truncation reasons
  appended as a warning line: `truncated: limit 100 results, line limit`.

### 4.7 ls (directory listing)

`ls` has no dedicated renderer entry — it flows through the **generic fallback
card** (§4.13): status line `<icon> ls` + inline args preview + first N output
lines (collapsed 4 / expanded 12).

### 4.8 task / delegate  (framed, mergeCallAndResult)

Custom renderer (`task/renderer.ts` → `task/render.ts`). Self-framing card.
- CALL streams a preview list of dispatched agents; suppressed once a result
  snapshot exists (`hasResult` in render context).
- RESULT: each dispatched agent drawn as its own progress/result line inside the
  frame. Live agents animate the spinner; parallel subagents share one 80ms
  ticker (`sharedSpinnerFrame`). Agent glyph `tool.task ⇶`.
- Uses `nowMs` from render context for elapsed timing.

### 4.9 todo  (framed, borderMuted, applyBg:false)

CALL (streaming): a lone status line (no frame):
```
⏳ Todo  update <task> <phase> 3 items
```
- icon `pending` w/ spinnerFrame, title `Todo`, meta = per-op summaries.

RESULT: `framedBlock`, `borderColor: borderMuted`, `applyBg: false`. Header:
```
☑ Todo  <N> tasks
```
- iconOverride = `tool.todo ☑` accent, meta `<N> tasks`.
- Body: phase blocks. Multi-phase: bold accent phase name + progress; each task a
  tree row (`renderTreeList`). Untouched phases collapse to a 1-line summary.
- Completed tasks get an animated **strikethrough reveal** (`TODO_STRIKE_TOTAL_
  FRAMES`, 65ms/frame). Collapsed shows a walking window (last-closed + active +
  next pending, limit `COLLAPSED_ITEMS=8`); expanded shows all.
- Error: framed `state:error`, header `✘ Todo`, body = error detail.
- Empty: `☑ Todo  0 tasks` + dim fallback line.

### 4.10 web_search  (framed, mergeCallAndResult)

CALL:
```
⏳ Web Search: <query>
```
RESULT: framed `CachedOutputBlock` with multiple labeled sections:
```
⌕ Web Search: <ProviderLabel>  <N> sources
├─ Query   <query>
├─ Answer  <markdown answer, collapsed to maxAnswerLines / full when expanded>
├─ Sources <tree list, 1 line per source: linked title  (domain) · age>
╰─ Metadata Provider: <model @ provider (OAuth)>   Usage: in 12 · out 34 · total 46
```
- Success iconOverride = `tool.webSearch ⌕` accent; no sources → `⚠` warning.
- Section labels colored `toolTitle`. Sources capped at `COLLAPSED_ITEMS=8`,
  expand for all (`… N more sources`). Error → framed error panel `✘ Web Search`.

### 4.11 memory: retain / recall / reflect  (inline, mergeCallAndResult)

All frameless (`inline: true`), status line + dim bullets/lines.

retain CALL: `⏳ Retain`. RESULT:
```
🧠 Retain  2 memories stored
  • <content line 1>
  • <content line 2>
  … 3 more [Ctrl+O: Expand]
```
- iconOverride `tool.memory 🧠` accent; summary as dim meta (period stripped).
- Bullet = `format.bullet •` (muted); content `toolOutput`; 2-space indent.
- Collapsed limit `COLLAPSED_ITEMS=8`; expand for all.

recall CALL: `⏳ Recall: <query>`. RESULT:
```
🧠 Recall: <query>  5 found
  [Ctrl+O: Expand]
```
- Success `🧠` + `N found`; zero → `⚠` + `no matches` (header only). Expanded shows
  recalled memory body lines (2-space indent, muted), up to `OUTPUT_EXPANDED=10`.

reflect CALL: `⏳ Reflect: <query>`. RESULT: `🧠 Reflect: <query>` + answer lines
(`toolOutput`, 2-space indent), collapsed `OUTPUT_COLLAPSED=3`, expanded
`OUTPUT_EXPANDED=10`, trailing `… N more lines [Ctrl+O: Expand]`.

Query previews truncated to 80 cells (unicode ellipsis).

### 4.12 lsp  (framed, inline, mergeCallAndResult)

CALL:
```
⏳ LSP: <action> <target> query:foo · new:bar · apply:true
```
- title `LSP`, description = action label + target, meta from args.

RESULT: framed `CachedOutputBlock`; header is a plain string (NOT renderStatusLine):
```
💡 LSP <action label>
```
- Success icon = `tool.lsp 💡` accent; partial → spinner; error → `✘`.
- Body auto-detected by content: Hover (```code```), Diagnostics
  (`N error(s)`/`N warning(s)`), References (`N reference(s)`), Symbols
  (`Symbols in <file>:`), else Response. Diagnostics state colored by severity
  (error > warning > success). Request info (file / `line N` / `symbol:` / `query:`)
  shown dim.

### 4.13 Generic fallback card  (`default-renderer.ts`)

State-tinted background block (no frame). For any tool without a bespoke renderer.
```
<icon> <label>
 └─ key=val, key2=val2                 (collapsed: dim inline args preview)

Args                                    (expanded only)
<json tree>
…

<output lines>                          (toolOutput; collapsed 4, expanded 12)
… 6 more lines [Ctrl+O: Expand]
```
- icon: partial → `running` if spinner else `pending`; skipped → `info`; error →
  `error`; else `done` (•). Skipped titles colored `muted`.
- Collapsed inline args: ` └─ ` (dim tree.last) + dim `formatArgsInline` preview.
- JSON-looking output rendered as a JSON tree (`renderJsonTreeLines`) with
  collapsed/expanded depth+line+scalar caps.
- `(no output)` dim when empty.

### 4.14 Compact / squeezed forms (`#renderCompact`)

When the transcript allocator squeezes a block below 3 rows:
- allocation 1 row:
  ```
  • <bold accent label> · <muted detail> <dim elapsed>s
  ```
  glyph = spinner frame if active else `•` (dim).
- allocation 2 rows:
  ```
  ╭─ <bold accent label> · <detail>
  ╰
  ```
  `╭─` and `╰` dim. Label is bold+toolTitle here (only place title is bold).

---

## 5. Diff rendering  (`modes/components/diff.ts`)

Input diff line formats accepted:
- canonical `"+123|content"` — marker, line number, `|`, content
- legacy `"+123 content"` or `"+ content"`

Rendered line format (`formatCodeFrameLine`):
```
<marker><lineNum padded>│<content>
```

### Gutter
- **Line-number gutter width is constant 3 digits minimum** (reserved through 999
  lines) so streamed re-renders stay byte-identical: `lineNumberWidth =
  max(3, longest line number)`.
- Gutter text = marker + number, right-padded to `lineNumberWidth + 1`, then `│`.
  Marker column holds `+`, `-`, or space.

Example (width 3 gutter):
```
 313│    unchanged context line
-314│    const old = 1
+314│    const neu = 2
 315│    trailing context
```

### Markers & colors
- Added `+` lines: color `toolDiffAdded` (green).
- Removed `-` lines: color `toolDiffRemoved` (red).
- Context ` ` lines: color `toolDiffContext` (dim/gray), syntax-highlighted in
  batches when file language is known.
- Duplicate line numbers blanked: a `-N` / `+N` single-line replacement blanks the
  repeated `+N` gutter number (shows marker only) to avoid visual noise.

### Intra-line word diff
- ONLY when a hunk is exactly one removed + one added line. Uses `diffWords`
  (`@oh-my-pi/pi-natives`). Changed tokens rendered with **inverse video**
  (`theme.inverse`). Leading whitespace of the first changed part is stripped from
  the inverse span (so indentation isn't highlighted).
- Multi-line hunks: no intra-line highlight; all removed lines then all added.

### Whitespace visualization (`visualizeIndent`)
Leading indentation only:
- tab → dim `  →  ` (padded to tab width, arrow centered)
- space → dim `·`
Remaining tabs in content replaced with spaces (`replaceTabs`).

### Gap / non-contiguous regions
Blank rows and legacy `...` / `…` markers render as a single dim `…`
(`toolDiffContext`).

### Large-diff truncation (`render-utils.ts truncateDiffByHunk`)
- Collapsed caps: `DIFF_COLLAPSED_HUNKS = 8`, `DIFF_COLLAPSED_LINES = 40`.
- Keeps whole change hunks up to the hunk budget; distributes remaining line
  budget across context segments (splitting long context blocks head/tail with a
  blank middle line).
- Streaming previews use `fromTail: true` (reverse the buffer) to keep the newest
  hunks visible.
- Overflow marker (edit result): `… (2 more hunks, 15 more lines) [Ctrl+O: Expand]`
  colored `toolOutput`.

### Diff stats suffix (header)
`formatDiffStatsSuffix`: ` [` + green `+N` + dim `/` + red `-M` + `]` (brackets
dim). `formatDiffStats` (used elsewhere) additionally appends dim `N hunks`,
segments joined by dim ` / `.

---

## 6. Diagnostics block  (`render-utils.ts formatDiagnostics`)

Appended below edit/write results when LSP diagnostics exist:
```

✘ Diagnostics (2 errors, 1 warning)
 ├─ <langIcon> <file path (accent)>
 │  ├─ ✘:12:5 <error message> (code)
 │  └─ ⚠:20:1 <warning message>
 └─ … 3 more [Ctrl+O: Expand]
```
- Header icon `✘` (error) or `⚠` (warning); title `Diagnostics` (toolTitle) + dim
  `(summary)`.
- File nodes use `├─`/`└─`; diag nodes nest with `│ ├─` / `  └─` (dim tree glyphs).
- Severity icons: `✘` error, `⚠` warning, `ⓘ`/info glyph else. Location `:line:col`
  dim; code `(code)` dim; message colored by severity.
- Collapsed shows max 5 diagnostics; expanded shows all. Overflow `… N more`.

---

## 7. Pluralization & counts

- `formatCount(word, n)` → `N word` / `N words` (auto-plural).
- Ad-hoc: `${n} hunk${n!==1?"s":""}`, `${n} file${n>1?"s":""}`, `${n} conflict${
  n===1?"":"s"}`, `${remaining} more ${pluralize(itemType, remaining)}`.
- `formatMoreItems(n, type)` → `… N more <plural type>`.
- Elided spans: `(summary: N elided span[s])`.

---

## 8. Spinner

- Frames from `theme.spinnerFrames`. Advance every **80ms**
  (`SPINNER_GLYPH_ADVANCE_MS`), render tick 80ms (`SPINNER_RENDER_INTERVAL_MS`).
- All live blocks share one global ticker; phase-locked via
  `sharedSpinnerFrame(frameCount, now) = floor(now/80) % frameCount`.
- Spinner glyph replaces the `running` status icon (⟳) via `formatStatusIcon
  ("running", theme, spinnerFrame)`. When no frame: falls back to `•` (dim) in
  compact mode or the static ⟳.

---

## 9. Indentation / nesting summary

- Framed blocks: border `│` owns left edge; content padded 1 (0 for diff/code
  frames). Section labels drawn as tee-bars.
- Inline tools: `Text(text, leftPad, 0)` — grep/glob use leftPad 1; memory/todo/
  lsp use 0. Bullets and list items get a 2-space body indent under the header.
- Tree lists: ` ├─ ` / ` └─ ` (leading space + branch), dim; nested diagnostics add
  a `│ ` or `  ` prefix column.
- Generic card: collapsed args on ` └─ ` line; expanded args under a dim `Args`
  heading.
