# koda UI — the next version: transitions, a status row that answers, a palette that forgives

Status: landed. `src/fx.rs` (new), `src/tui.rs`, `src/view.rs`, `src/panel.rs`.

This builds on `research-tui-delight.md` (why motion must be caused, bounded and
colour-only) and `spec-animation-engine.md` (the clock). It does not relax
either. Where the earlier documents put delight in the cells *already moving*,
this one adds motion to the moments the screen already *changes* — a mode
switch, a finished turn, a step checked off, the context filling — so each
change is seen happening instead of simply being different on the next frame.

---

## 1. The rules every effect obeys

Asserted in `fx::tests`, not just stated:

| Rule | Why | Enforced by |
| --- | --- | --- |
| **Caused, never scheduled.** Starts on an event; never loops. | An idle koda must not wake. Measured after this change: 0.00 s CPU over 10 s idle. | Every effect is a `Pulse` created in an event handler; `wants_frames` lists each one. |
| **Bounded, under 5 s.** | WCAG 2.2.2 requires a pause control for motion past 5 s. | `every_effect_is_shorter_than_wcag_five_seconds` |
| **Colour, not position.** | No text moves under a reader; nothing on WebKit's vestibular-trigger list. | Effects return `Color`s only. |
| **Settles to the static frame.** | Stopping — or motion off — never causes a jump. | `every_effect_settles_on_the_static_colour` |
| **Motion off means off.** | `/motion`, `KODA_REDUCED_MOTION`, `TERM=dumb`, non-tty. | Every effect gates on `motion.animates()`; with it off, the static colour is drawn and no frames are requested. |

On a 16-colour or mono palette `theme::mix` snaps at the halfway point, so each
transition becomes a clean one-step change rather than a flicker
(`a_mode_shift_between_named_colours_still_lands`).

---

## 2. The components

### 2.1 Mode shift — the composer frame (320 ms)

`ctrl+p` used to repaint the frame in the new mode's colour on the next frame and
append a line to the transcript explaining the mode. Now the frame eases from the
old mode's colour into the new one (`ease_out_cubic`), and the mode name in the
top edge arrives lit and settles. The explanation moves to a toast (§2.2): the
frame, its title and the bottom bar already show the mode, so the transcript
keeps only the agent's terse `mode → execute` as the record.

Skipped when a turn is running (the frame is dimmed then) and on screens too
short for a frame (`Metrics::tiny`).

### 2.2 Toasts — the status row answers, then hands back (3.6 s)

The status row is where ephemeral feedback belongs; the transcript is the record.
A toast replaces `● ready` with a glyph and a sentence, arrives lit (first 10%),
holds at its tone colour, fades toward muted (last 20%), and is gone.

| Tone | Glyph | Colour | Used for |
| --- | --- | --- | --- |
| `Done` | `✔` / `+` | success | the turn receipt |
| `Info` | `●` / `o` | accent | a setting changed: mode, `/motion`, `/reveal`, `/think`, `/reason`, `/theme` |
| `Warn` | `⚠` / `!` | warning | a turn cut short |

While a turn runs the row belongs to the turn, so `App::flash` sends the message
to the transcript instead of dropping it. A new turn clears any toast.

**The turn receipt.** Every finished turn leaves one:
`✔ done in 12s · ↓ 1.2k tok · files changed`, in the same vocabulary as the
`turn_meter` that was climbing while it ran. A sub-second turn says `done`, not
`done in 0s`; a cancelled, failed or out-of-steps turn says
`⚠ stopped after 1m15s` — it does not get to say done.

Cost: the quiet middle of a toast requests frames, but ratatui's buffer diff
writes nothing for an unchanged frame, so there is no terminal output (and no
suppressed cursor blink) between the fade-in and the fade-out.

### 2.3 The context gauge eases (450 ms)

The bar in the bottom-right eases from the old reading to the new one, so a jump
from 30% to 70% after a large file read is seen as the context filling. The
percentage beside it is always the true number. A second change mid-ease
continues from where the bar *is*, never snapping back (`Tween::set`).

### 2.4 Plan steps land (700 ms), and a finished plan lingers (1.4 s)

When an update checks a step off, that step's `✔` and text arrive lit and settle
into the done colour, with the strike-through arriving at the same moment. Only
the *same* plan counts: a step is matched by index and text, so a new plan whose
step 2 happens to be done does not flash.

When the last step lands, the panel used to vanish on the frame it went green.
It now stays docked for 1.4 s with its header reading `all 5 done`, then folds.

### 2.5 The command palette forgives

- **Fuzzy after prefix.** Prefix matches keep their order at the top; past two
  letters after the slash, fuzzy matches follow by score — `/cmpt` finds
  `/compact` instead of an empty list. Two letters are too few (`/mo` would drag
  in `/fastmode`).
- **Why a row is there.** Matched characters are drawn in the accent, the fzf
  convention, in both the `/` palette and the `@` file picker
  (`@vwrs` → s**r**c/**v**ie**w**.**rs**).
- **How narrow the filter is.** `5/39` on the top row.
- **The selection is a bar**, on the theme's selection tint, reversed where the
  theme has none — the same rule as the session picker.
- **One match counts.** When the palette shows a single command, Enter takes it.
  It used to send the half-typed `/reas` as "unknown command" even while the row
  above showed `/reason`. An exact match still runs.

### 2.6 "12 new below"

Scrolled up to read while the reply keeps streaming, the hint used to say
`pgdn latest` — and while a turn ran it said only `esc interrupt`, which is
exactly when new lines arrive. It now counts what has landed underneath, beside
the interrupt key when a turn is running:
`esc interrupt · pgdn 42 new below`.

---

## 2.7 The resting screen — what you see before anything moves

The first pass of this work put motion only into moments of change, and the
screen at rest looked exactly as it had. That was the wrong reading of "a new
version": these are the changes visible on every frame.

```
 ▄▀ koda  ·  MiniMax-M2.7                               PLAN  ◖ EXEC ◗  VIBE
 ▌ fix the failing discount test
   ╭─ ✔  READ    test_cart.py  4 lines · 29 tokens
   ╭─ ✎  EDIT    cart.py
 The bug: apply_discount subtracts percent directly▋
 ✳ editing ░░░▒▓█▓▒░░ (4s · ↓ 109 tok)                         esc interrupt
```

- **Header bar.** A gradient wordmark (accent → accent-alt, per character),
  the model, and the three modes as pills with the active one filled in its
  colour. The wordmark shimmers while a turn runs; the active pill eases
  between colours on a mode switch. The model and mode left the bottom bar,
  which no longer repeats them. Hidden below 64 columns, where the bottom bar
  keeps them. ASCII: `<> koda … [PLAN]`.
- **Tool labels.** Every tool header — cards and one-liners alike — leads with
  its name as a bold, upper-case label on a faint tint of its family's colour:
  reads and lookups blue, file changes amber, runs and delegation violet, the
  web in the accent. `[done]` is gone (the ✔ and its colour said it); a failure
  still says `failed`. A summary's leading verb is dropped under a label that
  already says it (`FETCH  https://…`, not `fetched https://…`). Fetch, Browse,
  Debug, Image, Ask got names of their own instead of "Tool".
- **Live tool cards.** Spinner and a live timer while running; on finishing,
  the icon lands lit and settles over 600 ms (the transcript keeps animating
  until it has, so a turn ending mid-flash never freezes a lit icon).
- **Working bar.** A crest sweeping a 10-cell `░▒▓█` track between the current
  step and the meter, hue running accent → accent-alt. The one animation that
  runs for a whole turn — it *is* the working signal — and it stops with it.
- **Typing cursor.** `▋` after the last written line of a streaming reply,
  blinking at ~1 Hz; drawn at window time, so the blink costs no re-render.
  Gone the moment a tool starts or the turn ends.
- **Your messages** carry an accent bar down their left edge.

## 3. Clutter removed (the skill's clutter audit, counted)

| Before | Count | After |
| --- | --- | --- |
| The running activity (`✶ writing the reply`) in the status row **and** the bottom bar | 2 live copies of one fact, one row apart | Status row only. The bottom bar's quiet-stream timer moved into it: `generating · quiet 12s (40s · ↓ 1.2k tok)`. |
| `web` and `ui :7717` as two segments (rendered `web  ui :7717`) | 2 segments, 1 fact | `web :7717` |
| Mode switch explained in the transcript *and* echoed by the agent | 2 transcript lines per `ctrl+p` | 1 (the agent's record); the explanation is a toast |

---

## 4. Defects fixed along the way

- **A finished reply could stay cut off for good.** The agent sends
  `learned N rule candidate(s)` immediately before `TurnEnd` — usually while the
  reveal is still catching up. `push` made the notice the tail, `finish_reveal`
  (which looks only at the tail) then did nothing, and relayout never walked back
  to the reply's cut-short cached lines. Seen live: the last bullet of an answer
  ended at `…without moving t` indefinitely. `push` now settles the tail's reveal
  first. Regression test: `a_block_pushed_mid_reveal_does_not_strand_the_reply`
  (fails without the fix).
- **An edit was not a write.** `wrote_this_turn` was only set for tool
  summaries starting `created`/`wrote`, so `edit_file` — the common case — never
  counted: the receipt left off `files changed`, and the visitor (which fires
  after real work) missed every edit-only turn. Seen live on a turn that fixed
  a file.
- **ASCII mode drew Unicode frames.** With `--icons ascii` (or a non-UTF-8
  locale) the text fell back to ASCII but every border — the composer and all
  eight overlays — still used `╭─╮`. Every frame now goes through
  `panel::frame_set`, which yields `+-|` for the ASCII glyph set.
- **The palette's single match** — Enter sent `/reas` as an unknown command
  while the row above it showed `/reason`. It now completes.
- **`truncate_line` counted chars, not cells**, so a CJK path or an emoji in the
  status row ran past the edge. Test: `truncation_counts_cells_not_chars`.

---

## 5. Floor

| Width | Behaviour |
| --- | --- |
| ≥ 64 | Everything above. |
| 60 (tmux split) | Toasts and receipts fit beside `/help`; the palette keeps its count; the composer has no frame, so the mode shift has nothing to animate and is skipped. |
| < 60 | Unchanged from before: toasts truncate with the row, cell-accurately now. |

---

## 6. Not done, deliberately

- **No toast overlay** over the transcript (a floating box top-right, as web apps
  do): it would cover text someone is reading. The status row is already the
  place the eye goes for "what just happened".
- **No motion in the transcript.** `spec-streaming-motion.md` §1: a line already
  on screen never changes bytes.
- **No new dependency, no new env var.** `fx.rs` is `std` plus `anim` and
  `theme`.
