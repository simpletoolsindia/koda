# Overlays, Selectors & Modal UI — Implementation Spec

Source of truth: `oh-my-pi/packages/coding-agent/src/modes/{components,controllers}` and
`oh-my-pi/packages/tui/src`. This spec targets visual parity for a Rust/ratatui
port (koda). All measurements and glyphs below are extracted from the TS source,
not approximated.

---

## 0. Symbol vocabulary (theme presets)

Every glyph is theme-selectable across three presets: `unicode` (default), `nerd`
(Nerd Font), `ascii`. koda should implement the same three-preset switch. The
values below are the `unicode` / `ascii` pair (nerd uses private-use glyphs).

| Role | key | unicode | ascii |
|------|-----|---------|-------|
| Selection cursor | `nav.cursor` | `❯` | `>` |
| Selected marker (alt) | `nav.selected` | `➤` | `->` |
| Expand / collapse | `nav.expand` / `nav.collapse` | `▸` / `▾` | `+` / `-` |
| Radio selected / unselected | `radio.selected` / `radio.unselected` | `◉` / `○` | `(o)` / `( )` |
| Checkbox checked / unchecked | `checkbox.checked` / `checkbox.unchecked` | `☑` / `☐` | `[x]` / `[ ]` |
| Box round corners | `boxRound.{topLeft,topRight,bottomLeft,bottomRight}` | `╭ ╮ ╰ ╯` | `+ + + +` |
| Box round edges | `boxRound.{horizontal,vertical}` | `─` `│` | `-` `|` |
| Tees (dividers) | `boxSharp.{teeRight,teeLeft,teeDown,teeUp}` | `├ ┤ ┬ ┴` | `+ + + +` |
| Tree branch / last / vertical | `tree.{branch,last,vertical}` | `├─` `└─` `│` | `|--` `'--` `|` |
| Status enabled/running/parked/aborted | `status.*` | `●` `⟳` `○` `⏹` | `[x]` `[~]` `[/]` `[-]` |
| Dot separator | `sep.dot` | ` · ` | ` - ` |

Theme colors referenced by name (map to koda's palette): `accent` (selection /
titles), `text` (normal row), `dim` (chrome, hints, disabled), `muted`
(descriptions, secondary), `border`/`borderMuted` (box glyphs), `success`,
`warning`, `error`, `toolOutput` (checked-but-not-selected rows).

---

## 1. The unified selector pattern

There are **two** frame styles that share the same box chrome, colors, list
behavior, and footer grammar:

- **Inline overlay** — `OverlayPanel` in the editor slot (replaces the composer).
  Used by small selectors (history search, copy, hooks, plan-save, move).
- **Fullscreen overlay** — mounted via `showOverlay({ fullscreen:true,
  width:"100%", maxHeight:"100%", margin:0, anchor:"bottom-center" })` on the
  alternate screen. Used by Settings, Model Hub, Agent Hub, MCP wizard, /copy.

### 1.1 Box chrome primitives (`overlay-box.ts`)

All overlays paint with `boxRound` glyphs, border color for the frame, `accent`
+ `bold` for the title. Content is inset **2 columns** on each side (`│ … │` with
one leading + one trailing space, so usable content width = `width - 4`).

```
╭─ Title ──────────────────────────────────────────────────╮   topBorder(width, "Title")
│ content row inset two columns                             │   row(content, width)
├──────────────────────────────────────────────────────────┤   divider(width)  (tee-right … tee-left)
│ more content                                              │
├──────────────────────────────────────────────────────────┤
│ ↑↓ move · Enter copy · Esc quit                           │   footer row (dim)
╰──────────────────────────────────────────────────────────╯   bottomBorder(width)
```

Title rule construction (`topBorder`): `╭─` + bold-accent ` Title ` +
`─…─` fill + `╮`. Title is collapsed to a single line (whitespace folded),
truncated to `width - 4`.

### 1.2 Two-column (split) fullscreen frame

Model Hub / Agent Hub use a sidebar + body split. Chrome adds `┬`/`┴` junctions
over the column divider. Sidebar content width = `sidebarWidth`; body width =
`width - sidebarWidth - 7`; divider vertical sits at column `sidebarWidth + 3`.

```
╭─ Models ───────────────┬─────────────────────────────────────────╮   topBorderSplit
│ ❯ Anthropic            │ claude-sonnet-4-6            ◒ high       │   splitRow(sidebar, body)
│   OpenAI               │ claude-opus-4-1              ◉ max        │
│   Google               │ ...                                      │
├────────────────────────┴─────────────────────────────────────────┤   dividerSplit
│ ↑↓ move · Tab switch · Enter select · Esc close                   │
╰────────────────────────────────────────────────────────────────────╯
```

### 1.3 Fixed-height / full-viewport behavior

- Fullscreen overlays compute rows from terminal height: `contentRows = max(10,
  rows - 4)` (Model Hub) or a chrome budget (`/copy`: `CHROME_ROWS = 5` = top +
  2 dividers + footer + bottom).
- The list is padded with blank rows to the full viewport height
  (`padLinesToHeight`) so the transcript never peeks through below.
- The **AskDialog** (confirmation) is height-stable: it measures the tallest tab
  once at spawn, clamps to `0.7 * rows` (min 12 rows), and never resizes on tab
  switch or cursor move — content that outgrows it scrolls.

### 1.4 Row layout (the `SelectList` primitive — `tui/src/components/select-list.ts`)

This is the canonical row engine used everywhere. Columns, left to right:

```
<cursor><icon-col><primary-label><gap><description>
```

- **cursor prefix**: selected → `"❯ "` painted `accent`; unselected → 2 spaces
  (`padding(visibleWidth(cursor)+1)`), keeping alignment.
- **icon column**: optional, width = widest item icon; every row reserves it so
  labels stay aligned. Unselected icons use the theme's `icon` style.
- **primary column**: label. Width = clamp(widest label + 2, min, max);
  default column width `32`, gap `PRIMARY_COLUMN_GAP = 2`.
- **description**: only rendered when width > 40 and remaining width >
  `MIN_DESCRIPTION_WIDTH (10)`. Aligned into a second column; may wrap onto
  indented continuation rows (`wrapDescription`) or be truncated with `…`.

Row states:

| State | Rendering |
|-------|-----------|
| Normal | `  label` + `dim`/`muted` description |
| Selected | `❯ ` (accent) + whole line via `selectedText` (accent, often bold) |
| Checked, not selected | marker in `success`, label in `toolOutput` color |
| Disabled | label + marker in `dim`; cursor still `dim` not `accent` |
| Hovered (mouse) | full row wrapped in `hovered` band style |

Multi-select / radio rows (ask-dialog, settings multiselect) prepend a marker
**before** the label:

```
❯ ◉ Selected radio option          (selected + checked single-choice)
  ○ Other option
  ☑ Checked multi option           (multi)
  ☐ Unchecked multi option
```

Marker color: checked → `success`, unchecked → `dim`. In ordered multiselect the
marker is a 1-based two-digit position: `" 1."`, `" 2."` (accent) or `" · "`/
`" ○ "` when unselected.

### 1.5 Grouped sections & headers

Groups are expressed as **section titles rendered as rows**, not as separate
boxes. A group header is a bold-accent `Text` row, optionally preceded by a
`Spacer(1)`; a `├───┤` divider (`PanelDivider` in an `OverlayPanel`, or
`divider(width)`) separates major regions (list vs. preview vs. footer). Example
from the settings multiselect submenu:

```
Providers                          ← theme.bold(theme.fg("accent", title))
                                   ← Spacer(1)
Reorder with ←/→ ...               ← theme.fg("muted", description)
                                   ← Spacer(1)
❯  1. anthropic
   2. openai
   · groq
```

The welcome box uses inline section headers in its right column: bold-accent
label (`Tips`, `LSP Servers`, `Recent sessions`) each followed by a dim `─────`
separator line.

### 1.6 Scroll indicators

- `SelectList` renders through a `ScrollView` with `scrollbar:"auto"`: a 1-column
  gutter on the right, `track` painted `muted`/`scrollInfo`, `thumb` painted
  `accent`/`selectedPrefix`. When the list overflows, content width shrinks by 1
  to reserve the scrollbar column.
- The visible window is **centered on the selection** (`centeredWindow` / the
  `pickWindow` algorithm: expand up ⌊budget/2⌋, then fill down, then back up).
- A **search/status line** appears under the list when `items > maxVisible` or a
  query is active: `  Type to search` (idle) or `  Search: <query>` (typing),
  painted `scrollInfo`.
- Preview panes show `… N more lines` (dim) when truncated (`/copy`, ask-dialog).

### 1.7 Footer key hints (`keybinding-hints.ts`)

Footer is one dim row. Each hint = `dim(key)` + `muted(" description")`, joined
by `dim(" · ")`. Keys are looked up from the keybinding manager so remaps show
correctly; raw hints (`↑↓`) use `rawKeyHint`. Examples verbatim:

```
↑↓ move · Enter copy · Esc quit                         (/copy)
↑/↓ move · Tab switch · Enter select · Esc close        (model hub)
Enter submit · ↑/↓ scroll · Esc cancel                  (ask, submit tab)
Space toggle · Enter next · Tab/←/→ · Esc cancel · ^o expand   (ask, multi)
Enter select · n note · ↑/↓ move · Esc cancel           (ask, single)
```

---

## 2. Row numbering & selection marker

- Rows are **not** globally numbered. The default cursor `❯` (ascii `>`) is the
  sole selection marker; unselected rows get 2 blank cells so text stays aligned.
- **Ordered multiselect** rows are the exception: they show a **1-based**
  position number (`" 1."`, `" 2."`, zero-padded to width 2) for selected members
  only; unselected show `" · "`.
- Number keys `1`–`9` are a *placement* shortcut in ordered lists (place the
  highlighted item at that 1-based slot), not row selectors elsewhere.

---

## 3. Filtering

- **Fuzzy**, via `tui/src/fuzzy.ts` (`fuzzyFilter`/`fuzzyRank`). Lower score =
  better; results are re-sorted best-first on every keystroke.
- Tokenized: query split on spaces, **all tokens must match** (AND). Matching is
  word-local (camelCase and separators split into words) so "image provider"
  won't match unrelated scattered letters.
- Scoring favors, in order: exact word, word-prefix, whole-phrase (word-boundary,
  bonus `-1000`), compact-phrase (`-1200`), substring, acronym (initials of 2–4
  words), then char-subsequence with a span cap. Alphanumeric transposition
  (`4o`↔`o4`) is tried with a `+5` penalty.
- **Matches are NOT highlighted.** There is no per-character emphasis on matched
  runs — filtering only *reorders and hides* rows. The active query is echoed in
  the status line (`Search: <query>`). koda should replicate: fuzzy filter +
  reorder, show the query, do **not** bold matched substrings.
- Filter editing: printable chars append; `deleteCharBackward` (Backspace) pops.
  Search is only editable when `items.length > maxVisible` (`overflowSearch`,
  default on). `j`/`k` are reserved for navigation and never enter the query.

---

## 4. Keyboard model

Default bindings (from `tui/src/keybindings.ts`, all remappable):

| Action | Default key(s) | Effect |
|--------|---------------|--------|
| `tui.select.up` | `↑` | Move selection up; **wraps** to bottom at top |
| `tui.select.down` | `↓` | Move selection down; **wraps** to top at bottom |
| `tui.select.pageUp` | `PageUp` | Jump up by `maxVisible` rows (no wrap; clamps) |
| `tui.select.pageDown` | `PageDown` | Jump down by `maxVisible` rows |
| `tui.select.confirm` | `Enter` (also raw `\n`) | Confirm highlighted item → `onSelect` |
| `tui.select.cancel` | `Esc`, `Ctrl+C` | Dismiss → `onCancel` |

Additional, per component:

- Type-to-filter: any printable char (except reserved `j`/`k`) when overflow
  search is active; `Backspace` deletes.
- `Tab` / `→` → next tab/pane; `Shift+Tab` / `←` → previous
  (`handleTabSwitchKey`). In hubs `←/→` are **spatial** pane switches (sidebar ↔
  list), not slider moves.
- `Ctrl+O` (`app.tools.expand`) → expand/collapse a truncated question header
  (ask-dialog) or expand tool output.
- Multi-select: `Space` toggles membership; `Enter` advances to next question /
  Submit tab (never submits directly). `Enter` on Submit tab submits.
- Ordered multiselect: `←/→` reorder highlighted member; `1`–`9` place at slot.
- `n` → attach a note to the highlighted answer (ask-dialog single-choice).
- Mouse: wheel steps selection, hover lights the row, left-click selects +
  confirms (routed via `routeSelectListMouse`).

The **PauseScreen** consumes `Esc`, `Enter`, `Space`, `Ctrl+C` — every key
resumes (never aborts).

---

## 5. Approval / confirmation prompt for risky actions

Risky-action confirmation reuses the **AskDialog** (`ask-dialog.ts`) — the same
box chrome as selectors, mounted inline rising from the bottom. It is a
radio/checkbox question, not a bespoke dialog.

- Title row: `Ask` or, when a timeout is set, `Ask (12s)` (live countdown).
- Options are radio rows (single-choice) or checkbox rows (multi). Canonical
  binary wording (from `agent-session.ts` / extension `select`): **`Yes` / `No`**;
  extension safety prompts use **`Approve` / `Deny`**. Simpler yes/no selectors
  add descriptions, e.g. `Yes — Show images inline`, `No — Show placeholder`.
- Approval **policy** vocabulary underneath is `allow` / `deny` / `prompt`
  (per-tool, per-bash-pattern). The UI surfaces these as options; "always /
  session" style options appear as additional radio choices when offered.
- The recommended option is pre-highlighted (`question.recommended`, clamped).
- A free-form escape hatch row `Other (type your own)` (`◉/○` marker) opens a
  prompt editor when chosen.

Exact layout (single-choice approval):

```
╭─ Ask (28s) ──────────────────────────────────────────────╮
│ Run `rm -rf build/` in /repo?                             │   ← question header (inline markdown, ≤4 rows, ^o to expand)
├──────────────────────────────────────────────────────────┤
│ ❯ ◉ Yes                                                   │   ← selected radio, accent
│   ○ No                                                    │
│   ○ Other (type your own)                                 │
├──────────────────────────────────────────────────────────┤
│ Enter select · n note · ↑/↓ move · Esc cancel · ^o expand │
╰──────────────────────────────────────────────────────────╯
```

Multi-question / multi-select variant adds a `TabBar` header (question chips +
a final `Submit` tab) and a `Review answers` submit body listing each answer;
unanswered questions show `unanswered` in `warning` color.

Keys: `↑/↓` move, `Space` toggle (multi), `Enter` select/next/submit, `n` note,
`Esc`/`Ctrl+C` cancel, `Tab`/`←`/`→` switch tab, `^o` expand header. Cancel key
label in the footer is normalized to `Esc`.

---

## 6. Welcome screen (`welcome.ts`)

Rendered as the **first transcript block** (not an overlay) — it scrolls with
history. A rounded box, max width `100`, `boxWidth = min(100, termWidth-2)`,
painted with **dim** border glyphs. Two columns when wide enough
(`leftCol ≈ 35%` clamped to ≥ `"Welcome back!"` width, min right col 20);
collapses to single column on narrow terminals.

- **Top border** carries an embedded title: `╭───` + muted ` <APP_NAME> vX.Y.Z `
  + dim fill + `╮`.
- **Left column** (centered): blank, bold `Welcome back!`, blank, the block-grid
  `π` logo (5 rows, diagonal pink→purple→cyan gradient with a one-shot intro
  sweep animation), blank, muted `modelName`, borderMuted `providerName`.
- **Right column** sections, each a bold-accent header:
  - `Tips` + four command hints: `# for prompt actions`, `/ for commands`,
    `! to run bash`, `$ to run python` (key glyph dim, text muted).
  - dim `─────` separator.
  - `LSP Servers` — up to 4 fixed slots: status glyph (`●` ready/success,
    `●` available/dim, `⏳` connecting/muted, `✘` error) + muted name + dim exts.
    Padded to 4 rows for stable height.
  - separator, then `Recent sessions` — up to 4 slots: dim `•` bullet + muted
    name + dim ` (timeAgo)`; time is width-reserved so the name truncates first.
    `No recent sessions` (dim) when empty.
- **Bottom border**: `╰──…┴──…╯` (`teeUp` under the column split when two-column).
- **Below the box**: a single italic tip line: `customMessageLabel`-colored
  `Tip: ` + muted body; `[NEW]`-tagged tips append an animated rainbow `NEW!`.

```
╭─── omp v1.2.3 ─────────────────────────────────────────────────────────╮
│                              │ Tips                                     │
│        Welcome back!         │ # for prompt actions                     │
│                              │ / for commands                           │
│         ████████████         │ ! to run bash                            │
│            ██  ██            │ $ to run python                          │
│            ██  ██            │ ─────────────────────                    │
│            ▒▒  ██            │ LSP Servers                              │
│                ██            │ ● typescript ts tsx js                   │
│                              │ ● rust rs                                │
│      claude-sonnet-4-6       │ ─────────────────────                    │
│         anthropic            │ Recent sessions                          │
│                              │ • refactor auth (2h ago)                 │
│                              │ • fix parser (yesterday)                 │
╰──────────────────────────────┴──────────────────────────────────────────╯
 Tip: Press # for prompt actions.
```

---

## 7. How overlays compose with the transcript

Three composition modes, all leaving the transcript **intact** underneath:

1. **Inline / editor-slot replacement** (`showSelector`, `OverlayPanel`): the
   selector is swapped **into the editor container in place of the composer**
   (`editorContainer.clear(); addChild(component)`). The transcript above is
   untouched and does not scroll; the overlay occupies exactly the composer's
   region and grows upward. On dismiss, the editor is restored and focus returns
   to it (`focusActiveEditorArea`). Used by history search, copy, hooks, move,
   plan-save.

2. **Fullscreen on the alternate screen** (`#showFullscreenMenu` /
   `showOverlay({ fullscreen:true })`): the overlay **borrows the terminal's
   alternate buffer** and enables mouse tracking for its lifetime. The transcript
   is neither replaced in the data model nor pushed — it simply sits on the main
   buffer and reappears verbatim when the overlay hides. Lines are padded to full
   viewport height so nothing shows through. Used by Settings, Model Hub, Agent
   Hub, MCP wizard, Extensions dashboard, /copy, PauseScreen.

3. **Transcript block** (welcome, compaction markers): rendered **as a normal
   transcript entry** that scrolls with history; not an overlay at all.

Overlays do **not** float as a centered popup over a dimmed transcript. They
either take the composer's slot (inline) or the whole alternate screen
(fullscreen). Focus is explicitly set to the overlay on open and restored to the
visible editor-slot owner on close (handling the case where an approval prompt
swapped the editor out mid-overlay).

For koda/ratatui: model inline overlays as a bottom-anchored region replacing the
composer widget; model fullscreen overlays as an alternate-screen `Frame` that
fully repaints and restores the prior transcript buffer on exit. Keep the
transcript buffer immutable across overlay lifetimes.
