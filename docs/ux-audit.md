# koda TUI — UX Audit

A prioritized, code-grounded design review of the koda terminal UI. Every
finding cites the file and function so it can be actioned directly. Severity:
**P0** blocker · **P1** major · **P2** polish.

Scope reviewed: `src/tui.rs`, `src/view.rs`, `src/md.rs`, `src/panel.rs`,
`src/theme.rs`, `src/setup.rs`, `src/settings.rs`.

Already-fixed items (table inline formatting, centered/dimmed approval popup,
sticky ctrl+t/ctrl+r, tool progress bar, rotating working messages, /fam,
ask_user card, enlarged input + spacer, setup caret/scroll, friendly probe
failures) are **excluded** and not re-reported.

---

## Top findings, prioritized

| # | Severity | Finding | Where |
|---|----------|---------|-------|
| 1 | P1 | Overlays don't look like one product (border type/color/title style diverge) | `setup.rs`, `settings.rs`, `tui.rs` |
| 2 | P1 | Markdown headings have zero visual hierarchy (h1–h6 identical) | `md.rs::render` |
| 3 | P1 | Inline `code` is nearly invisible and collides with diff/string colors | `md.rs::inline` |
| 4 | P1 | First-run/empty transcript is a bare logo — no orientation | `tui.rs::show_welcome` |
| 5 | P1 | Keybindings are undiscoverable; the hint row under-teaches | `tui.rs::hint_row`, `show_help` |
| 6 | P2 | NEON default theme has contrast/legibility risks; muted tier too dim | `theme.rs::NEON`, `resolve` |
| 7 | P2 | Narrow-terminal (64/92 col) degradation drops important status silently | `tui.rs::Metrics`, `powerline` |
| 8 | P1 | Expand affordance (`ctrl+r`/`ctrl+t`) is inconsistent and self-contradicting | `panel.rs::expand_hint`, `view.rs` |
| 9 | P2 | Error state is a flat red line — no structure, no recovery path | `view.rs::render_item` (Error) |
| 10 | P2 | Powerline right cluster is cryptic and silently truncates | `tui.rs::powerline`, `panel.rs::status_bar` |
| 11 | P2 | Settings overlay is a 21-row wall with no grouping | `settings.rs::Row::ALL`, `draw` |
| 12 | P2 | Spinner vocabulary is inconsistent across the app | `theme.rs::Glyphs`, `view.rs`, `hint_row` |

---

## 1. Overlays don't read as one product — P1

**Where:** `setup.rs::draw`, `settings.rs::Settings::draw`, `tui.rs::approval_popup`,
`tui.rs::asking_popup`, `tui.rs::session_picker`, `tui.rs::log_overlay`.

**What the user experiences.** Every modal is styled by a different hand:

- `approval_popup`: `BorderType::Thick`, border colored by risk (`accent`/error/warning), title `REVERSED`.
- `asking_popup`: `BorderType::Rounded`, `info` border, title `REVERSED`.
- `settings::draw`: `BorderType::Rounded`, `border_focus` border, plain bold title.
- `setup::draw`: default (sharp) `Borders::ALL`, `border_focus` border, plain bold title.
- `session_picker`: default (sharp) borders, `border_focus`, plain bold title.
- `log_overlay`: default (sharp) borders, dim `border` (not focus), dim title.

So within one session the user sees sharp corners, rounded corners, and thick
corners; three different title treatments; and borders that variously mean "this
is focused," "this is risky," and nothing. It reads as several apps bolted
together — which is exactly the "UI is the weakest part" symptom.

**Fix (minimal).** Define one modal chrome helper (e.g. `panel::modal_block(title, accent, glyphs, theme)`)
that fixes: `BorderType::Rounded` everywhere; title always
`fg(accent).bold()` (drop `REVERSED`, which inverts inconsistently across
terminals); `title_bottom` always the dim key-hint line. Let callers pass only
the accent color (risk-red for approval, `info` for ask, `border_focus` for the
rest). Route all six overlays through it. This is the single highest-leverage
change for perceived quality.

---

## 2. Markdown headings have no visual hierarchy — P1

**Where:** `md.rs::render`, the `strip_heading` branch (~line 66).

**What the user experiences.** A `#`, `##`, and `######` all render identically:
`Style::default().fg(t.heading).add_modifier(BOLD)`. An assistant reply that uses
heading levels to structure a long answer (very common) comes out flat — the
reader can't tell a section title from a sub-point. Body text and headings differ
only by color+bold, and inline **bold** in prose *also* is bold, so headings don't
even reliably stand out from emphasized sentences.

**Fix (minimal).** `strip_heading` already knows the hash count — return it.
Then in `render`: for h1 render as bold heading color; for h2 prefix with a
subtle glyph or underline the text; for h3+ drop to `t.accent`/plain-bold and
smaller emphasis. Even a two-tier split (h1/h2 = heading color bold; h3+ =
accent bold) restores scannability at near-zero cost.

---

## 3. Inline `code` is almost invisible — P1

**Where:** `md.rs::inline`, the backtick branch (~line 417):
`spans.push(Span::styled(code, base.fg(t.syn_string)))`.

**What the user experiences.** Inline code gets only a foreground color swap to
`syn_string`. Problems:

- No background/box, so `foo()` in a sentence doesn't visually separate from prose
  the way a real code span should — the reader loses the "this is a literal" signal.
- `syn_string` is reused: in DARK it's `rgb(206,145,120)` (a salmon), in several
  themes it equals `diff_add`/`success` green, so inline code reads as "added" or
  "success" rather than "code."
- In MONO theme `syn_string == Color::Reset`, so inline code is *indistinguishable*
  from surrounding prose — the code marker vanishes entirely.

**Fix (minimal).** Give inline code its own token — a dim background tint where
available (`bg_tool`) plus `t.text` fg, falling back to `REVERSED` or bracketing
with a subtle glyph on fill-less/mono themes. At minimum, use a dedicated color
that is not shared with diff/success, and in MONO add a modifier (e.g.
`UNDERLINED`) so the span survives with no color.

---

## 4. First-run / empty transcript gives no orientation — P1

**Where:** `tui.rs::show_welcome`.

**What the user experiences.** When a model *is* configured, `show_welcome`
prints the KODA banner art and two blank lines — nothing else. A first-time user
stares at a logo and an empty prompt with no idea what to type, what model is
loaded, or that `/help` exists. (The "no model" branch is handled nicely; the
happy-path branch is barren.) The `ready` state and hints only appear once you
know to read the one-line row, and the composer placeholder "ask, or /help for
commands" is dim and easy to miss on a colorful banner.

**Fix (minimal).** After the banner, add 2–3 dim orientation rows:
- `model · cwd · branch` (the same facts as the powerline, but stated once in prose)
- a one-line "Try: `explain this repo` · `/help` for commands · `@` to attach a file"

This is a `Panel`-free `transcript.raw(...)` append, costs ~3 lines, and turns a
cold open into a guided one. Only show it on the true first frame (empty
transcript), not on every `/clear`.

---

## 5. Keybindings are undiscoverable — P1

**Where:** `tui.rs::hint_row` (the contextual right-side hints) and
`tui.rs::show_help`.

**What the user experiences.** The persistent hint row shows at most ~3 keys, and
in the idle state only `@ · ctrl+p · /keys`. Core, high-value shortcuts — `ctrl+r`
(expand output), `ctrl+t` (expand reasoning), `pgup/pgdn`, `!cmd` shell escape,
`up/down` history — are never surfaced except inside `/help`, which a user has to
already know to run. The reasoning block *does* inline "ctrl+t expand", which is
good; nothing else does. A power user won't find `!git status` or `/orc`; a
beginner won't discover expansion at all and will assume tool output is just
truncated.

**Fix (minimal).**
- Rotate the idle hint set so it occasionally advertises `ctrl+r`, history, and
  `!cmd` rather than the same three every frame.
- When a collapsed tool block is the newest item, make its `expand_hint` reflect
  the *current* sticky state ("ctrl+r expand all" vs "ctrl+r collapse") — see #8.
- Add a one-time toast on first tool collapse: "output trimmed — ctrl+r to expand."

---

## 6. NEON default theme risks contrast and legibility — P2

**Where:** `theme.rs::NEON`, `theme.rs::resolve` (defaults `"" | "auto"` → NEON).

**What the user experiences.** NEON is the shipped default. Concerns grounded in
the values:

- `muted: rgb(122,130,180)` on `bg_panel: rgb(24,26,58)` is a low-contrast
  blue-on-blue; dim text (hints, timings, the "working" label base color `t.muted`)
  is hard to read — and dim text is *everywhere* in this UI (all `t.dim()` calls).
- High-saturation cyan/magenta everywhere is fatiguing for a tool users stare at
  for hours, and several accents sit close in hue to `info`/`accent_alt`, muddying
  the powerline's "each field its own color" premise.
- A neon default also means the *screenshot* new users first see is the least
  conservative option, which can read as "toy" to the skeptic persona (below).

**Fix (minimal).** Default to `DARK` (the code comments even call DARK "the
default because the block fills… need known colours" — the `resolve` default
contradicts that comment). Keep NEON as an opt-in palette. If NEON must stay
default, lift `muted` toward `rgb(150,158,205)` and verify contrast ≥ 4.5:1
against `bg_panel`.

---

## 7. Narrow-terminal degradation drops status silently — P2

**Where:** `tui.rs::Metrics::of` (`compact = width<92`, `tiny = width<64`),
`tui.rs::powerline`, `panel.rs::status_bar`.

**What the user experiences.** At the two widths the brief calls out:

- **92 cols (`compact`)**: `powerline` drops the endpoint host and the token
  gauge bar (keeps a bare `%`). Reasonable, but the *branch* is still shown and
  the right cluster (`model  mode  web  auto  tokens`) is space-separated with no
  separators (see #10), so at 92 it's already a cramped run-on.
- **64 cols (`tiny`)**: branch is dropped; the hint row collapses to just
  `/help`; the input spacer is removed. But `status_bar` also *silently drops the
  entire right cluster* when `width <= lw+rw+3` — so on a narrow/long model name
  the user loses model, mode, and the full-auto warning with no indication. Losing
  the `full-auto` (red) indicator specifically is a safety-relevant silent drop.

**Fix (minimal).** In `powerline`, when compact/tiny, *prioritize* segments:
always keep `mode` and `auto_tier` (safety), shorten model harder, drop dir/branch
first. In `status_bar`, if the right side can't fit, drop *left* segments before
sacrificing the whole right cluster, and never drop the auto-tier warning.

---

## 8. Expand affordance is inconsistent and self-contradicting — P2→P1

**Where:** `panel.rs::expand_hint` (hardcoded `"  ctrl+r expand"`),
`view.rs::render_tool` (uses it as `tail`), `view.rs` Reasoning branch
(`"ctrl+t hide" / "ctrl+t expand"`), and the sticky global prefs `expand_tools`/
`expand_reasoning` passed into `render_item`.

**What the user experiences.** `ctrl+r`/`ctrl+t` are now *sticky global*
preferences (per the fixed list), but the per-block hints were written for the
old per-block toggle:

- `expand_hint` always says "ctrl+r expand" even when everything is *already*
  expanded via the sticky pref — so pressing it collapses, contradicting the label.
- Collapsed tools show "ctrl+r expand"; the reasoning block correctly flips
  between "expand"/"hide"; tools never show the "collapse" affordance. Two blocks,
  two grammars.
- The docstring in `expand_hint` still references `[Ctrl+O: Expand]` (a third,
  dead keybinding), evidence the affordance drifted.

**Fix (minimal).** Thread the current sticky state into `expand_hint(expanded, t)`
and render "ctrl+r collapse all" / "ctrl+r expand all" to match reality; align the
tool and reasoning grammars to the same verb pair; remove the stale `Ctrl+O` doc.

---

## 9. Error state is a flat red line with no recovery path — P2

**Where:** `view.rs::render_item`, the `Item::Error` branch.

**What the user experiences.** An agent/system error renders as `fail` glyph +
red text on the `bg_tool_err` fill — one undifferentiated blob. Compared to the
care lavished on tool cards (rails, caps, headers), errors are the *least*
structured element, yet they're the moments the user most needs guidance. There's
no title ("Request failed"), no separation of cause vs detail, and no "what next"
(retry? `/logs`? `/setup`?). The powerline does surface `N issue /logs`, which is
good, but the inline error itself points nowhere.

**Fix (minimal).** Give errors the same `panel::railed(..., Frame::Failed)`
treatment tools already use: a header line ("Error: <short cause>"), the detail
wrapped beneath, and a dim tail hint ("/logs for detail · /setup if this is a
connection issue"). Reuse the existing rail machinery — no new primitive needed.

---

## 10. Powerline right cluster is cryptic and silently truncates — P2

**Where:** `tui.rs::powerline`, `panel.rs::status_bar`.

**What the user experiences.** The left segments are chevron-separated and
colored (good). The right cluster is *two-space* separated with no chevrons:
`gpt-4o  execute  web  full-auto  12.4k tok  ▓▓░░ 62%`. Issues:

- No separators means fields run together at a glance; the "each field its own
  color" reasoning that justifies the left side isn't applied to the right.
- `full-auto` in red is the single most important safety signal and it's buried
  mid-cluster with equal weight to `web`.
- `web` / `full-auto` / `1 warn /logs` are unlabeled jargon to a newcomer.
- Silent truncation (see #7) can drop the whole cluster.

**Fix (minimal).** Use the same chevron/dot separator on the right cluster;
promote `auto_tier` to a bold, always-kept, leftmost-of-right position when it's
not `Ask`; and consider a tiny leading glyph for web (`🌐`/`web:` label). Keeps
the "scan by color+separator" grammar consistent end to end.

---

## 11. Settings overlay is a 21-row wall — P2

**Where:** `settings.rs::Row::ALL` (21 rows), `settings.rs::Settings::draw`.

**What the user experiences.** Twenty-one flat rows in one bordered list, no
grouping, no section headers. Related settings are adjacent by luck
(`WebSearch`/`SearchBackend`/`SearxUrl` are, and cleverly numbered "1) 2) 3)" in
their hints — nice touch), but Motion/Reveal, Sandbox/Autonomy, and the web/OCR/
codegraph cluster aren't visually chunked. On a short terminal the list is taller
than the screen (`h = ALL.len()+4` clamped to height) so it scrolls with no
scrollbar or "more below" affordance — the user can't tell rows are hidden.

**Fix (minimal).** Insert dim section-header rows ("Agent", "Appearance", "Web",
"Advanced") — they're just non-selectable `Line`s the `sel` index skips. Add a
"▼ more" indicator (or reuse `draw_scrollbar`) when rows exceed the visible area.

---

## 12. Spinner/status glyph vocabulary is inconsistent — P2

**Where:** `theme.rs::Glyphs` (`thinking` vs `spinner` sets), `view.rs::render_tool`
(uses `g.spinner`), `tui.rs::hint_row` (uses `g.thinking`).

**What the user experiences.** Two different animated glyph sets run at once: the
status row's "thinking" sweep (`· ✻ ✽ ✶ ✳ ✢`) and each running tool's braille
`spinner` (`⠋⠙⠹…`). They spin at different cadences (sweep is time-derived; tool
spinner is `tick`-derived) and look unrelated, so a busy screen has two competing
motions. Minor, but it undercuts the "composed" feel.

**Fix (minimal).** Pick one motion language: either use the braille spinner in
the status row too, or the sweep glyph on tools. Drive both from the same
time-based frame (`anim::sweep`) so cadence matches.

---

## End-user role-play

### Beginner ("I just installed this")
- Opens koda → sees the KODA logo and a blinking prompt. **Doesn't know what to
  type.** (Finding #4.) Types "hi", gets a reply, sees a collapsed tool card,
  assumes that's all the output there is — **never discovers `ctrl+r`.** (#5, #8.)
- Hits an error (wrong endpoint). Sees a red line, **doesn't know it's fixable
  with `/setup`.** (#9.)
- Verdict: "Looks slick for two seconds, then I'm lost." First impression is
  strong (banner) but the orientation cliff is steep.

### Power user ("I live in the terminal")
- Immediately wants keybindings and shell escape. **Finds `ctrl+p` and `@` in the
  hint row but has to run `/help` to learn `!cmd`, `/orc`, history, and expand.** (#5.)
- Runs on a tmux pane at ~90 cols → **token gauge and host vanish; right cluster
  gets cramped and run-on.** (#7, #10.) Notices the sticky-expand label says
  "expand" when everything's already expanded → **momentary confusion.** (#8.)
- Verdict: "Powerful once I read the manual, but the UI doesn't teach itself and
  the status bar gets messy when I shrink the pane."

### Skeptic ("another AI CLI, prove it")
- First screenshot is **NEON** — high-saturation neon reads as "toy," and the dim
  blue-on-blue hint text is **hard to read**. (#6.)
- Opens `/settings` → **21-row wall**, some rows scrolled off with no indicator;
  wonders what half of them do. (#11.)
- Opens an approval modal (thick red), then `/setup` (sharp border), then
  `/settings` (rounded) → **"why does every popup look different?"** (#1.)
- Verdict: "Feels unfinished — inconsistent chrome and a garish default palette."
  These are precisely the perceived-quality issues the brief flags.

---

## Recommended sequencing

If only the top items ship next round, do them in this order for maximum
perceived-quality gain:

1. **#1 unified modal chrome** — one helper, touches 6 call sites, instantly makes
   the app feel coherent.
2. **#6 default to DARK** — one-line change in `resolve`, fixes the first-impression
   and the worst contrast problem.
3. **#2 + #3 markdown headings + inline code** — the assistant's own voice is what
   users read most; this is the content, not the chrome.
4. **#4 first-run orientation** — closes the beginner's onboarding cliff.
5. **#8 expand-affordance correctness** — small, but removes a visible "the UI
   lies to me" moment.
6. **#5 / #7 / #9 / #10** — discoverability, narrow-terminal safety, error recovery,
   status clarity.
7. **#11 / #12** — polish.

All findings are grounded in the current code and none overlap the already-fixed
list.
