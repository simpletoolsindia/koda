//! Transcript model and rendering.
//!
//! Each block caches its rendered lines, so streaming a token re-lays-out only
//! the block that changed. Visual rules follow one idea: spend signal on the
//! exception, not on every row. Assistant prose carries no marker at all — it
//! is the default voice — while the things you scan for (a failed tool, your
//! own message) get exactly one accent each.

use crate::md;
use crate::panel::{self};
use crate::theme::{Glyphs, Theme};
use crate::tools::{Todo, TodoStatus};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Item {
    User(String),
    Assistant(String),
    Reasoning {
        text: String,
        started: Instant,
        elapsed: Option<Duration>,
        expanded: bool,
    },
    Tool {
        id: String,
        name: String,
        label: String,
        /// None while running.
        ok: Option<bool>,
        summary: String,
        detail: String,
        /// Structured result; drives the per-tool layout.
        view: crate::tools::ToolView,
        expanded: bool,
        started: Instant,
        elapsed: Option<Duration>,
        /// 0 = main agent, 1 = inside a delegated subagent.
        depth: u8,
        /// True when another tool card follows immediately: suppresses the pad.
        grouped: bool,
    },
    Notice(String),
    Error(String),
    Todos(Vec<Todo>),
    /// Already-laid-out lines, e.g. a framed panel. Rendered verbatim so exact
    /// alignment survives; re-wrapping a frame would tear it apart.
    Raw(Vec<Line<'static>>),
}

struct Block {
    item: Item,
    cache: Option<(u16, u64, Vec<Line<'static>>)>,
    /// Line index where this block starts. Maintained by `relayout` so that
    /// `window` can binary-search to the first visible block instead of walking
    /// the whole transcript on every frame.
    offset: usize,
}

/// How much of the streaming reply is already laid out, so the next frame only
/// has to render what arrived since. Reset whenever anything it assumes could
/// have changed (a different block, a new width, or text that did not simply
/// grow).
struct StreamRender {
    /// Which block this describes.
    block: usize,
    width: u16,
    /// Bytes of the shown text folded into the kept lines.
    stable_end: usize,
    /// How many leading cached lines came from that prefix.
    stable_lines: usize,
}

pub struct Transcript {
    blocks: Vec<Block>,
    pub show_reasoning: bool,
    /// Sticky global preference: expand every tool block's output. Toggled with
    /// ctrl+r. Unlike a per-block toggle, this persists across new responses so
    /// the user doesn't have to re-expand after every turn.
    pub expand_tools: bool,
    /// Sticky global preference: expand every reasoning block's body. Toggled
    /// with ctrl+t. Persists across responses (bug fix: it used to reset).
    pub expand_reasoning: bool,
    pub theme: Theme,
    pub glyphs: Glyphs,
    /// Index of a tool block that is currently animating, if any.
    animating: Option<usize>,
    /// Animation frame, advanced by the UI tick. Only running tools use it.
    /// Wall-clock time of the current frame. Animated blocks derive their own
    /// phase from this, so no frame counter has to be threaded through.
    pub now: Instant,
    /// How many characters of the streaming tail block are shown.
    ///
    /// A local model can emit a whole paragraph in one chunk, which lands as a
    /// wall of text. Revealing at a steady rate makes the response read as
    /// arriving rather than as appearing, and costs nothing once it catches up.
    reveal: usize,
    /// When the reveal cursor last advanced, so its rate is wall-clock based.
    reveal_at: Option<Instant>,
    /// Whether to reveal gradually at all. Only the TUI sets this, and only
    /// when a frame clock exists to advance the cursor.
    pub animate_reveal: bool,
    total: usize,
    /// Width the offsets were computed at.
    laid_out_at: u16,
    /// Incremental render state for the streaming reply (see `relayout`).
    ///
    /// A reply arrives token by token, and each token invalidates the block. Re-
    /// rendering the whole block every frame is O(reply) per frame, so streaming
    /// a long answer costs O(reply²) — measured at 21µs/frame for 2KB rising to
    /// 880µs/frame at 60KB. Markdown here is a line-wise pass, so the part of
    /// the reply before the last blank line renders the same no matter what
    /// arrives later: keep those lines and re-render only the tail.
    stream: Option<StreamRender>,
    /// Index of the earliest block whose cached offset may be stale.
    dirty_from: usize,
}

impl Transcript {
    pub fn new(theme: Theme, glyphs: Glyphs) -> Self {
        Self {
            blocks: Vec::new(),
            show_reasoning: true,
            expand_tools: false,
            expand_reasoning: false,
            theme,
            glyphs,
            animating: None,
            now: Instant::now(),
            reveal: 0,
            reveal_at: None,
            // Off unless something is driving a frame clock. A transcript with
            // nobody advancing the cursor must show all of its text, not none
            // of it — that is the difference between an animation and a bug.
            animate_reveal: false,
            total: 0,
            laid_out_at: 0,
            stream: None,
            dirty_from: 0,
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.total = 0;
        self.dirty_from = 0;
    }

    /// Force a full re-render, e.g. after a theme change.
    pub fn invalidate(&mut self) {
        for b in self.blocks.iter_mut() {
            b.cache = None;
        }
        self.dirty_from = 0;
    }

    /// Drop the oldest blocks so a long session cannot grow without limit,
    /// returning how many lines went so the caller can hold the view still.
    ///
    /// Nothing ever removed a block before: `blocks` grew for the life of the
    /// session, and each one keeps its rendered lines as well as its text —
    /// measured at about twenty times the raw size. That was roughly 4-5 MB an
    /// hour of ordinary use, and a single large file read added half a megabyte
    /// on its own.
    ///
    /// Evicting in one chunk rather than one block at a time keeps the full
    /// re-layout this forces to once every `CHUNK` blocks instead of every turn.
    pub fn trim_blocks(&mut self) -> usize {
        const MAX_BLOCKS: usize = 4_000;
        const CHUNK: usize = 500;
        if self.blocks.len() <= MAX_BLOCKS {
            return 0;
        }
        let drop = CHUNK.min(self.blocks.len());
        // Offsets are still valid here, so the line count of the dropped run is
        // where the first surviving block starts.
        let lines = self.blocks[drop].offset;
        self.blocks.drain(..drop);
        // Every offset shifts, so everything must be re-offset. The cached
        // renders survive — only the positions changed.
        self.dirty_from = 0;
        self.total = self.total.saturating_sub(lines);
        lines
    }

    fn push(&mut self, item: Item) {
        self.blocks.push(Block {
            item,
            cache: None,
            offset: 0,
        });
        self.dirty_from = self.dirty_from.min(self.blocks.len() - 1);
    }

    pub fn user(&mut self, text: String) {
        self.push(Item::User(text));
    }

    /// Replace the existing task list in place, so repeated updates do not
    /// stack up copies of the same plan.
    pub fn todos(&mut self, items: Vec<Todo>) {
        for (i, b) in self.blocks.iter_mut().enumerate().rev() {
            if let Item::Todos(existing) = &mut b.item {
                *existing = items;
                b.cache = None;
                self.dirty_from = self.dirty_from.min(i);
                return;
            }
        }
        self.push(Item::Todos(items));
    }

    /// (done, total) for the status bar.
    pub fn todo_progress(&self) -> Option<(usize, usize)> {
        self.blocks.iter().rev().find_map(|b| match &b.item {
            Item::Todos(items) if !items.is_empty() => {
                let done = items
                    .iter()
                    .filter(|i| i.status == TodoStatus::Done)
                    .count();
                // Once every step is done the task is finished — drop the live
                // counter so a stale "N/N steps" doesn't linger in the status row.
                // The completed plan still shows in the transcript as a record.
                if done == items.len() {
                    None
                } else {
                    Some((done, items.len()))
                }
            }
            _ => None,
        })
    }

    /// The current task list, for the sticky plan panel above the input. Returns
    /// the most recent non-empty list so the plan stays visible even after it
    /// has scrolled out of the transcript.
    pub fn current_todos(&self) -> Option<Vec<Todo>> {
        self.blocks.iter().rev().find_map(|b| match &b.item {
            Item::Todos(items) if !items.is_empty() => Some(items.clone()),
            _ => None,
        })
    }

    /// Mark every step of the current plan done. Called when a turn ends with no
    /// further work queued: small models often finish the task but forget the
    /// final `todo` update that flips the last step to done, which would leave
    /// the sticky plan and step counter lingering forever. This retires the plan
    /// cleanly once the agent has actually stopped working.
    pub fn complete_current_plan(&mut self) {
        for (i, b) in self.blocks.iter_mut().enumerate().rev() {
            if let Item::Todos(items) = &mut b.item {
                if items.is_empty() {
                    continue;
                }
                let changed = items.iter().any(|it| it.status != TodoStatus::Done);
                if changed {
                    for it in items.iter_mut() {
                        it.status = TodoStatus::Done;
                    }
                    b.cache = None;
                    self.dirty_from = self.dirty_from.min(i);
                }
                return;
            }
        }
    }

    /// Append pre-rendered lines. The caller owns their width.
    pub fn raw(&mut self, mut lines: Vec<Line<'static>>) {
        lines.push(Line::default());
        self.push(Item::Raw(lines));
    }

    pub fn notice(&mut self, text: String) {
        self.push(Item::Notice(text));
    }

    pub fn error(&mut self, text: String) {
        self.push(Item::Error(text));
    }

    pub fn assistant_delta(&mut self, chunk: &str) {
        self.close_reasoning();
        let last = self.blocks.len().saturating_sub(1);
        if let Some(Block {
            item: Item::Assistant(s),
            ..
        }) = self.blocks.last_mut()
        {
            s.push_str(chunk);
            // Deliberately *not* clearing the cache. The signature already
            // includes the text length, so `relayout` sees this block as stale
            // and re-renders it — while the previous lines survive for the
            // incremental path to extend instead of re-rendering the whole
            // reply. Dropping them here is what made streaming a long answer
            // cost O(reply) per frame.
            self.dirty_from = self.dirty_from.min(last);
            return;
        }
        self.reveal = 0;
        self.reveal_at = None;
        self.push(Item::Assistant(chunk.to_string()));
    }

    /// Characters in the streaming tail block, or None if the tail is not one.
    fn tail_len(&self) -> Option<usize> {
        match self.blocks.last() {
            Some(Block {
                item: Item::Assistant(s),
                ..
            }) => Some(s.chars().count()),
            _ => None,
        }
    }

    /// Advance the reveal toward the text that has arrived. Returns whether
    /// anything changed, so the caller knows if a repaint is warranted.
    pub fn advance_reveal(&mut self) -> bool {
        if !self.animate_reveal {
            return false;
        }
        let Some(total) = self.tail_len() else {
            return false;
        };
        if self.reveal >= total {
            return false;
        }
        let now = Instant::now();
        let dt = self
            .reveal_at
            .map(|t| now.duration_since(t))
            .unwrap_or(Duration::from_millis(75));
        self.reveal = crate::anim::reveal_step(dt, self.reveal, total);
        self.reveal_at = Some(now);
        let last = self.blocks.len() - 1;
        if let Some(b) = self.blocks.last_mut() {
            b.cache = None;
        }
        self.dirty_from = self.dirty_from.min(last);
        true
    }

    /// Whether the reveal is still behind the text that has arrived.
    pub fn revealing(&self) -> bool {
        self.animate_reveal && self.tail_len().is_some_and(|n| self.reveal < n)
    }

    /// Show everything immediately. Called when a turn ends, when a tool starts,
    /// and whenever motion is off: a partially revealed block must never be left
    /// stranded behind a completed turn.
    pub fn finish_reveal(&mut self) {
        if let Some(total) = self.tail_len() {
            if self.reveal < total {
                self.reveal = total;
                let last = self.blocks.len() - 1;
                if let Some(b) = self.blocks.last_mut() {
                    b.cache = None;
                }
                self.dirty_from = self.dirty_from.min(last);
            }
        }
        self.reveal_at = None;
    }

    pub fn reasoning_delta(&mut self, chunk: &str) {
        let last = self.blocks.len().saturating_sub(1);
        if let Some(Block {
            item: Item::Reasoning { text, elapsed, .. },
            cache,
            ..
        }) = self.blocks.last_mut()
        {
            if elapsed.is_none() {
                text.push_str(chunk);
                *cache = None;
                self.dirty_from = self.dirty_from.min(last);
                return;
            }
        }
        self.push(Item::Reasoning {
            text: chunk.to_string(),
            started: Instant::now(),
            elapsed: None,
            expanded: false,
        });
    }

    /// Stamp the duration on a trailing reasoning block once real output starts.
    fn close_reasoning(&mut self) {
        let last = self.blocks.len().saturating_sub(1);
        if let Some(Block {
            item: Item::Reasoning {
                started, elapsed, ..
            },
            cache,
            ..
        }) = self.blocks.last_mut()
        {
            if elapsed.is_none() {
                *elapsed = Some(started.elapsed());
                *cache = None;
                self.dirty_from = self.dirty_from.min(last);
            }
        }
    }

    pub fn tool_start(&mut self, id: String, name: String, label: String, depth: u8) {
        self.close_reasoning();
        // A run of tool cards is one group; drop the trailing pad of the
        // previous card so they read as a block rather than a list of islands.
        let last = self.blocks.len().saturating_sub(1);
        if let Some(b) = self.blocks.last_mut() {
            if let Item::Tool { grouped, .. } = &mut b.item {
                *grouped = true;
                b.cache = None;
                self.dirty_from = self.dirty_from.min(last);
            }
        }
        self.push(Item::Tool {
            id,
            name,
            label,
            ok: None,
            summary: String::new(),
            detail: String::new(),
            view: crate::tools::ToolView::Plain,
            expanded: false,
            started: Instant::now(),
            elapsed: None,
            depth,
            grouped: false,
        });
    }

    pub fn tool_end(
        &mut self,
        id: &str,
        ok_v: bool,
        summary_v: String,
        detail_v: String,
        view_v: crate::tools::ToolView,
    ) {
        for (i, b) in self.blocks.iter_mut().enumerate().rev() {
            if let Item::Tool {
                id: bid,
                name: bid_name,
                ok,
                summary,
                detail,
                view,
                expanded,
                started,
                elapsed,
                ..
            } = &mut b.item
            {
                if bid == id {
                    *ok = Some(ok_v);
                    *summary = summary_v;
                    *detail = detail_v;
                    *view = view_v;
                    *elapsed = Some(started.elapsed());
                    // Show failures, and always show a diff of your own files:
                    // those are the results you must not have to ask for.
                    let writes_files = matches!(bid_name.as_str(), "write_file" | "edit_file");
                    *expanded = !ok_v || writes_files;
                    b.cache = None;
                    self.dirty_from = self.dirty_from.min(i);
                    return;
                }
            }
        }
    }

    /// Expand or collapse the most recent tool block. Superseded in the UI by
    /// the sticky `toggle_tools_pref`, but retained for tests and completeness.
    #[allow(dead_code)]
    pub fn toggle_last_tool(&mut self) -> bool {
        for (i, b) in self.blocks.iter_mut().enumerate().rev() {
            if let Item::Tool { expanded, .. } = &mut b.item {
                *expanded = !*expanded;
                b.cache = None;
                self.dirty_from = self.dirty_from.min(i);
                return true;
            }
        }
        false
    }

    /// Expand or collapse the most recent reasoning block. Superseded in the UI
    /// by the sticky `toggle_reasoning_pref`, but retained for tests.
    #[allow(dead_code)]
    pub fn toggle_last_reasoning(&mut self) -> bool {
        for (i, b) in self.blocks.iter_mut().enumerate().rev() {
            if let Item::Reasoning { expanded, .. } = &mut b.item {
                *expanded = !*expanded;
                b.cache = None;
                self.dirty_from = self.dirty_from.min(i);
                return true;
            }
        }
        false
    }

    /// Flip the sticky global tool-expand preference (ctrl+r). Applies to every
    /// tool block and persists across new responses, so the user sets it once.
    pub fn toggle_tools_pref(&mut self) -> bool {
        self.expand_tools = !self.expand_tools;
        self.invalidate();
        true
    }

    /// Flip the sticky global reasoning-expand preference (ctrl+t). Applies to
    /// every reasoning block and persists across responses.
    pub fn toggle_reasoning_pref(&mut self) -> bool {
        self.expand_reasoning = !self.expand_reasoning;
        self.invalidate();
        true
    }

    /// Rebuild a readable transcript from a saved conversation. Tool calls come
    /// back collapsed with their results, so a resumed session looks like the
    /// one you left rather than a wall of JSON.
    pub fn restore(&mut self, messages: &[crate::llm::Message]) {
        use crate::llm::Role;
        self.clear();
        let mut pending: Vec<(String, String)> = Vec::new(); // (id, label)
        for m in messages {
            match m.role {
                Role::User => {
                    // Tool results in text-protocol mode arrive as user turns.
                    let text = m.content.clone().unwrap_or_default();
                    if text.starts_with("Tool result (") {
                        continue;
                    }
                    self.user(text);
                }
                Role::Assistant => {
                    if let Some(text) = &m.content {
                        if !text.trim().is_empty() {
                            self.assistant_delta(text);
                        }
                    }
                    for c in m.tool_calls.iter().flatten() {
                        let label = crate::agent::label_for(&c.function.name, &c.args());
                        pending.push((c.id.clone(), label));
                    }
                }
                Role::Tool => {
                    let id = m.tool_call_id.clone().unwrap_or_default();
                    let name = m.name.clone().unwrap_or_default();
                    let label = pending
                        .iter()
                        .position(|(pid, _)| *pid == id)
                        .map(|i| pending.remove(i).1)
                        .unwrap_or_else(|| name.clone());
                    let content = m.content.clone().unwrap_or_default();
                    let ok = !content.starts_with("ERROR:");
                    self.tool_start(id.clone(), name, label.clone(), 0);
                    // A restored session only has the model-facing text.
                    self.tool_end(&id, ok, label, content, crate::tools::ToolView::Plain);
                }
                Role::System => {}
            }
        }
        // Anything still pending never got a result: show it as unfinished.
        for (id, label) in pending {
            self.tool_start(id.clone(), String::new(), label.clone(), 0);
            self.tool_end(
                &id,
                false,
                label,
                "(no result recorded)".into(),
                crate::tools::ToolView::Plain,
            );
        }
    }

    pub fn last_assistant(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|b| match &b.item {
            Item::Assistant(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn total_lines(&self) -> usize {
        self.total
    }

    /// Re-render any block whose content, width or flags changed.
    /// Re-render whatever changed and refresh line offsets.
    ///
    /// The naive version re-signed and re-summed every block each frame, which
    /// is O(transcript) per frame for a transcript that is almost entirely
    /// static. This walks only from the earliest block that could have changed.
    pub fn relayout(&mut self, width: u16) -> usize {
        // A running block re-renders about ten times a second; quantising the
        // clock into the cache signature is what lets an otherwise-cached
        // transcript animate without a full relayout.
        let tick = (self.now.elapsed().as_millis() / 100) as usize;
        let show = self.show_reasoning;
        let expand_tools = self.expand_tools;
        let expand_reasoning = self.expand_reasoning;
        let theme = self.theme;
        let glyphs = self.glyphs;

        // A width change invalidates every wrap, so everything is dirty.
        let mut from = if width != self.laid_out_at {
            0
        } else {
            self.dirty_from.min(self.blocks.len())
        };

        // A running tool animates, so it and everything after it must re-offset.
        if let Some(i) = self.animating {
            from = from.min(i);
        }

        if from >= self.blocks.len() && width == self.laid_out_at {
            return self.total;
        }

        // Resume from the end of the last clean block. A freshly pushed block
        // has offset 0, so trusting its own offset would restart the count.
        let mut cursor = if from == 0 {
            0
        } else {
            let prev = &self.blocks[from - 1];
            prev.offset + prev.cache.as_ref().map(|c| c.2.len()).unwrap_or(0)
        };
        let mut animating = None;
        let last_i = self.blocks.len().saturating_sub(1);
        // Held outside the loop because the loop borrows `self.blocks` mutably.
        let mut stream = self.stream.take();
        // Only the tail can be mid-reveal; everything before it is settled.
        let reveal = self.reveal;
        let revealing = self.animate_reveal;
        for (i, b) in self.blocks.iter_mut().enumerate().skip(from) {
            let cut = if revealing && i == last_i {
                Some(reveal)
            } else {
                None
            };
            let mut sig = signature(&b.item, show, tick);
            // Folding the global tool-expand preference into the cache key means
            // flipping it (ctrl+r) re-renders every tool block, not just the last.
            sig ^= u64::from(expand_tools) << 5;
            sig ^= u64::from(expand_reasoning) << 6;
            // The reveal position is part of what is on screen, so it has to be
            // part of the cache key or the block renders once and freezes.
            if let Some(c) = cut {
                sig ^= (c as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            let stale = match &b.cache {
                Some((w, s, _)) => *w != width || *s != sig,
                None => true,
            };
            if stale {
                // The streaming reply is the one block that changes every frame,
                // so it gets the incremental path: keep the lines already laid
                // out for its settled prefix and render only the tail.
                let reused = (i == last_i)
                    .then(|| stream_render(&mut stream, i, b, width, cut, &theme))
                    .flatten();
                let lines = match reused {
                    Some(lines) => lines,
                    None => {
                        if i == last_i {
                            stream = None;
                        }
                        render_item(
                            &b.item,
                            width as usize,
                            show,
                            expand_tools,
                            expand_reasoning,
                            &theme,
                            &glyphs,
                            tick,
                            cut,
                        )
                    }
                };
                b.cache = Some((width, sig, lines));
            }
            if animating.is_none() && is_running(&b.item) {
                animating = Some(i);
            }
            b.offset = cursor;
            cursor += b.cache.as_ref().map(|c| c.2.len()).unwrap_or(0);
        }
        self.animating = animating;
        self.laid_out_at = width;
        self.dirty_from = self.blocks.len();
        self.total = cursor;
        self.stream = stream;
        cursor
    }

    /// Clone the visible window of lines.
    ///
    /// Offsets are sorted, so the first visible block is a binary search rather
    /// than a walk from the top — the difference between O(blocks) and
    /// O(log blocks) on every single frame.
    pub fn window(&self, from: usize, count: usize) -> Vec<Line<'static>> {
        let mut out = Vec::with_capacity(count);
        if self.blocks.is_empty() || count == 0 {
            return out;
        }
        let start_block = match self.blocks.binary_search_by(|b| b.offset.cmp(&from)) {
            Ok(i) => i,
            // `from` lands inside the preceding block.
            Err(i) => i.saturating_sub(1),
        };
        for b in &self.blocks[start_block..] {
            let Some((_, _, lines)) = &b.cache else {
                continue;
            };
            let skip = from.saturating_sub(b.offset);
            if skip >= lines.len() {
                continue;
            }
            for l in &lines[skip..] {
                out.push(l.clone());
                if out.len() == count {
                    return out;
                }
            }
        }
        out
    }
}

/// Cheap content hash for a pre-rendered block. FNV-1a over the text: enough to
/// distinguish two panels, and far cheaper than comparing lines.
fn raw_hash(lines: &[Line<'static>]) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for l in lines {
        for s in &l.spans {
            for b in s.content.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        h ^= b'\n' as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    (h >> 16) as usize
}

fn is_running(item: &Item) -> bool {
    matches!(item, Item::Tool { ok: None, .. })
}

fn signature(item: &Item, show_reasoning: bool, tick: usize) -> u64 {
    let (len, flags) = match item {
        Item::User(s) | Item::Assistant(s) | Item::Notice(s) | Item::Error(s) => (s.len(), 0u8),
        // Two different panels can easily have the same number of lines, so the
        // key has to describe the content or one will render as the other.
        Item::Raw(lines) => (raw_hash(lines), 64),
        Item::Todos(items) => (
            items.iter().map(|i| i.text.len() + 1).sum::<usize>(),
            32 | items
                .iter()
                .filter(|i| i.status == TodoStatus::Done)
                .count()
                .min(31) as u8,
        ),
        Item::Reasoning {
            text,
            elapsed,
            expanded,
            ..
        } => (
            text.len(),
            1 | (u8::from(elapsed.is_some()) << 1)
                | (u8::from(*expanded) << 2)
                | (u8::from(show_reasoning) << 3),
        ),
        Item::Tool {
            label,
            summary,
            detail,
            ok,
            expanded,
            depth,
            elapsed,
            grouped,
            ..
        } => (
            // A running tool animates, so its frame is part of the signature;
            // a finished one is static and stays cached.
            label.len()
                + summary.len()
                + detail.len()
                + usize::from(*grouped)
                + if ok.is_none() { tick % 10 * 4096 } else { 0 },
            16 | (match ok {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            }) | (u8::from(*expanded) << 2)
                | ((*depth).min(3) << 3)
                | (u8::from(elapsed.is_some()) << 5),
        ),
    };
    (len as u64) << 8 | flags as u64
}

/// Human-readable duration: short enough to sit inside a one-line tool card.
fn human_ms(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// Present-tense verb for a tool in flight, so the card says what is happening
/// rather than just spinning.
fn running_verb(name: &str) -> &'static str {
    match name {
        "read_file" => "reading",
        "write_file" => "writing",
        "edit_file" => "editing",
        "list_dir" => "listing",
        "find_files" => "finding",
        "search" => "searching",
        "run_command" => "running",
        "delegate" => "delegating",
        "web_search" => "searching the web",
        "skill" => "reading skill",
        "todo" => "planning",
        "codegraph" => "consulting the code graph",
        "remember" => "noting",
        _ => "working",
    }
}

#[allow(clippy::too_many_arguments)]
/// Lay out the streaming reply by keeping the lines already rendered for its
/// settled prefix and rendering only what arrived since.
///
/// Returns `None` when the incremental path does not apply — a block that is not
/// a plain reply, a width change, text that did not simply grow, or nothing
/// settled yet — and the caller falls back to a full render. Correctness rests on
/// `md::stable_prefix_end`: see the invariant proved in md's tests.
fn stream_render(
    state: &mut Option<StreamRender>,
    index: usize,
    block: &mut Block,
    width: u16,
    cut: Option<usize>,
    theme: &Theme,
) -> Option<Vec<Line<'static>>> {
    let Item::Assistant(text) = &block.item else {
        return None;
    };
    // Exactly the slice the full renderer would show, so the two paths agree.
    let shown = shown_prefix(text, cut);
    let split = md::stable_prefix_end(shown);
    if split == 0 {
        return None; // nothing has settled yet: one short render is cheaper
    }

    let fresh = || StreamRender {
        block: index,
        width,
        stable_end: 0,
        stable_lines: 0,
    };
    let st = match state {
        // Same block, same width, and the settled prefix only grew: reusable.
        Some(s) if s.block == index && s.width == width && s.stable_end <= split => s,
        _ => {
            *state = Some(fresh());
            state.as_mut()?
        }
    };

    // Take the previous lines; without them there is nothing to extend.
    let mut lines = match block.cache.take() {
        Some((w, _, lines)) if w == width && lines.len() >= st.stable_lines => lines,
        _ => {
            st.stable_end = 0;
            st.stable_lines = 0;
            Vec::new()
        }
    };
    lines.truncate(st.stable_lines);

    // Fold the newly settled text into the kept lines. The dropped byte is the
    // `\n` that separates the halves, which `split` on the whole would consume.
    if split > st.stable_end {
        let seg = &shown[st.stable_end..split - 1];
        lines.extend(md::render(seg, width as usize, theme));
        st.stable_end = split;
        st.stable_lines = lines.len();
    }
    // Only the unsettled tail is re-rendered every frame.
    lines.extend(md::render(&shown[split..], width as usize, theme));
    lines.push(Line::default());
    Some(lines)
}

/// The prefix of a reply that is visible mid-reveal. Slicing on a char boundary
/// matters: cutting a multi-byte character in half would panic.
fn shown_prefix(text: &str, cut: Option<usize>) -> &str {
    match cut {
        Some(n) if n < text.chars().count() => {
            let end = text
                .char_indices()
                .nth(n)
                .map(|(i, _)| i)
                .unwrap_or(text.len());
            &text[..end]
        }
        _ => text,
    }
}

// Nine inputs, each of them genuinely needed to draw one block, and all of them
// per-call rather than per-session state. Bundling them into a struct would move
// the argument list rather than shorten it, on the hottest path in the renderer.
#[allow(clippy::too_many_arguments)]
fn render_item(
    item: &Item,
    width: usize,
    show_reasoning: bool,
    expand_tools: bool,
    expand_reasoning: bool,
    t: &Theme,
    g: &Glyphs,
    tick: usize,
    // How many characters of this block to show, when it is mid-reveal.
    cut: Option<usize>,
) -> Vec<Line<'static>> {
    let width = width.max(12);
    match item {
        // Your own words, on a warm fill. The tint does the work a marker used
        // to do, so the text itself is left plain.
        Item::User(text) => {
            let body = md::wrap_spans(
                vec![Span::styled(text.clone(), t.body())],
                width.saturating_sub(4),
                0,
            );
            let mut lines = panel::fill(body, width, t.bg_user, 2);
            lines.push(Line::default());
            lines
        }

        // The assistant is the default voice, so it gets no marker at all.
        Item::Assistant(text) => {
            // Mid-reveal, render only the prefix that has been "typed" so far.
            let shown = shown_prefix(text, cut);
            let mut lines = md::render(shown, width, t);
            lines.push(Line::default());
            lines
        }

        Item::Reasoning {
            text,
            started,
            elapsed,
            expanded,
        } => {
            if !show_reasoning {
                return Vec::new();
            }
            let open = *expanded || expand_reasoning;
            // Roughly 4 characters per token — the same convention koda uses to
            // budget context — so the user sees how much thinking a model spent,
            // not just how long it took.
            let toks = text.chars().count() / 4;
            let label = match elapsed {
                Some(d) if toks > 0 => format!("thought for {} · ~{} tokens", human_ms(*d), toks),
                Some(d) => format!("thought for {}", human_ms(*d)),
                None if toks > 0 => format!(
                    "thinking {} · ~{} tokens",
                    human_ms(started.elapsed()),
                    toks
                ),
                None => format!("thinking {}", human_ms(started.elapsed())),
            };
            let mut lines = vec![Line::from({
                let mut row = vec![
                    Span::styled(format!("{} ", g.pending), t.dim()),
                    Span::styled(label, t.dim().add_modifier(Modifier::ITALIC)),
                ];
                // Only advertise the toggle when it would do something new:
                // show "ctrl+t expand" while collapsed, nothing once expanded.
                if !open {
                    row.push(Span::styled("  ctrl+t expand".to_string(), t.dim()));
                }
                row
            })];
            if open {
                for para in text.split('\n') {
                    for l in md::wrap_spans(
                        vec![Span::styled(para.to_string(), t.dim())],
                        width.saturating_sub(4),
                        2,
                    ) {
                        lines.push(indent(l, 3, t, g, 0));
                    }
                }
            }
            lines.push(Line::default());
            lines
        }

        Item::Raw(lines) => lines.clone(),

        Item::Notice(text) => md::wrap_spans(
            vec![
                Span::styled(format!("{} ", g.sep), t.dim()),
                Span::styled(text.clone(), t.dim()),
            ],
            width,
            2,
        ),

        Item::Error(text) => {
            let body = md::wrap_spans(
                vec![
                    Span::styled(format!("{} ", g.fail), t.emphasis(t.error)),
                    Span::styled(text.clone(), t.emphasis(t.error)),
                ],
                width.saturating_sub(4),
                2,
            );
            let mut lines = panel::fill(body, width, t.bg_tool_err, 2);
            lines.push(Line::default());
            lines
        }

        // The plan is the one thing worth framing: it is a standing summary of
        // the work, not a line in the log, so it gets a card with progress.
        Item::Todos(items) => {
            let done = items
                .iter()
                .filter(|i| i.status == TodoStatus::Done)
                .count();
            let total = items.len();
            let any_active = items.iter().any(|i| i.status == TodoStatus::Active);
            let complete = done == total && total > 0;
            let avail = width.clamp(30, 100);

            // Header: a status glyph, "Tasks", and the count. The glyph reflects
            // the whole plan — done when finished, active while a step runs,
            // pending otherwise.
            let head_glyph = if complete {
                g.ok
            } else if any_active {
                g.running
            } else {
                g.pending
            };
            let head_style = if complete {
                t.emphasis(t.success)
            } else if any_active {
                t.emphasis(t.warning)
            } else {
                t.fg(t.accent)
            };
            let mut lines: Vec<Line<'static>> = Vec::new();
            lines.push(Line::from(vec![
                Span::styled(format!("{head_glyph} "), head_style),
                Span::styled("Tasks".to_string(), t.emphasis(t.heading)),
                Span::styled(format!(" ({total})"), t.dim()),
            ]));

            // One tree row per task: ├── for all but the last, └── for the last.
            for (i, it) in items.iter().enumerate() {
                let last = i + 1 == items.len();
                let branch = if last { g.last } else { g.branch };
                let (glyph, glyph_style, text_style, tag) = match it.status {
                    TodoStatus::Done => (
                        g.ok,
                        t.emphasis(t.success),
                        t.dim().add_modifier(Modifier::CROSSED_OUT),
                        " [done]",
                    ),
                    TodoStatus::Active => (
                        g.running,
                        t.emphasis(t.warning),
                        t.body().add_modifier(Modifier::BOLD),
                        "",
                    ),
                    TodoStatus::Pending => (g.pending, t.dim(), t.dim(), ""),
                };
                let text: String = it.text.chars().take(avail.saturating_sub(14)).collect();
                let mut row = vec![
                    Span::styled(format!(" {branch} "), t.dim()),
                    Span::styled(format!("{glyph} "), glyph_style),
                    Span::styled(format!("{}. ", i + 1), t.dim()),
                    Span::styled(text, text_style),
                ];
                if !tag.is_empty() {
                    row.push(Span::styled(tag.to_string(), t.emphasis(t.success)));
                }
                lines.push(Line::from(row));
            }
            lines.push(Line::default());
            lines
        }

        Item::Tool {
            name,
            label,
            ok,
            summary,
            detail,
            view,
            expanded,
            started,
            elapsed,
            depth,
            grouped,
            ..
        } => render_tool(
            name,
            label,
            ok,
            summary,
            detail,
            view,
            *expanded || expand_tools,
            started,
            elapsed,
            *depth,
            *grouped,
            width,
            t,
            g,
            tick,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
/// The verb and identity glyph a tool shows once it has settled.
fn tool_identity(name: &str, g: &Glyphs) -> (&'static str, String) {
    match name {
        "read_file" => ("Read", g.ok.to_string()),
        "write_file" => ("Write", g.pencil.to_string()),
        "edit_file" => ("Edit", g.pencil.to_string()),
        "run_command" => ("Run", g.prompt.to_string()),
        "search" => ("Grep", g.magnify.to_string()),
        "find_files" => ("Glob", g.magnify.to_string()),
        "list_dir" => ("List", g.ok.to_string()),
        "delegate" => ("Task", g.branch_arrow.to_string()),
        "web_search" => ("Search", g.magnify.to_string()),
        "codegraph" => ("Graph", g.magnify.to_string()),
        "remember" => ("Memory", g.ok.to_string()),
        "skill" => ("Skill", g.ok.to_string()),
        "todo" => ("Plan", g.check_on.to_string()),
        _ => ("Tool", g.ok.to_string()),
    }
}

/// Render one tool call. Each tool gets the presentation its result deserves:
/// a diff is not a directory listing, and a grep hit is not a shell transcript.
#[allow(clippy::too_many_arguments)]
fn render_tool(
    name: &str,
    label: &str,
    ok: &Option<bool>,
    summary: &str,
    detail: &str,
    view: &crate::tools::ToolView,
    expanded: bool,
    started: &Instant,
    elapsed: &Option<Duration>,
    depth: u8,
    grouped: bool,
    width: usize,
    t: &Theme,
    g: &Glyphs,
    tick: usize,
) -> Vec<Line<'static>> {
    use crate::tools::ToolView as V;
    let _ = grouped;
    let (title, settled_glyph) = tool_identity(name, g);
    let indent_w = 2 + depth as usize * 2;
    let avail = width.saturating_sub(indent_w).max(20);

    // Icon: spinner while running, tool identity once done, cross on failure.
    let icon = match ok {
        None => (g.spinner[tick % g.spinner.len()].to_string(), t.warning),
        Some(true) => (settled_glyph, t.success),
        Some(false) => (g.fail.to_string(), t.error),
    };
    let timing = match (ok, elapsed) {
        (Some(_), Some(d)) if d.as_millis() >= 10 => vec![human_ms(*d)],
        (None, _) if started.elapsed().as_millis() > 400 => vec![human_ms(started.elapsed())],
        _ => Vec::new(),
    };

    // Failures are the same shape for every tool: the header plus the message.
    if *ok == Some(false) {
        let head = panel::status_line(
            Some(icon),
            title,
            Some((first_word_target(label), t.text)),
            &timing,
            t,
            g,
        );
        let body: Vec<Line<'static>> = detail
            .lines()
            .take(if expanded { 40 } else { 6 })
            .flat_map(|l| {
                md::hard_wrap(
                    l.trim_end().trim_start_matches("ERROR: "),
                    avail.saturating_sub(4),
                )
            })
            .map(|s| Line::from(Span::styled(s, t.fg(t.error))))
            .collect();
        return indent_all(
            panel::railed(head, body, None, avail, panel::Frame::Failed, t, g),
            indent_w,
            t,
            g,
            depth,
        );
    }

    let lines = match view {
        // ---- a diff is the whole point of a write or an edit ----------------
        V::Diff {
            path,
            diff,
            added,
            removed,
            created,
        } => {
            let verb = if *created { "Create" } else { title };
            let head = panel::status_line(
                Some(icon),
                verb,
                Some((path.clone(), t.info)),
                &timing,
                t,
                g,
            );
            let stats = vec![
                Span::styled(format!("+{added}"), t.fg(t.diff_add)),
                Span::styled("/".to_string(), t.dim()),
                Span::styled(format!("-{removed}"), t.fg(t.diff_del)),
            ];
            let from = diff.find("@@ ").unwrap_or(0);
            let mut body = md::render_diff(&diff[from..], avail.saturating_sub(4), t);
            let clipped = clip_body(&mut body, expanded, 12, t, g);
            let tail = if clipped {
                Some(vec![panel::expand_hint(t)])
            } else {
                Some(stats)
            };
            panel::railed(head, body, tail, avail, panel::Frame::Done, t, g)
        }

        // ---- a shell command: the command, then its output ------------------
        V::Run {
            command,
            stdout,
            stderr,
            code,
        } => {
            let mut meta = timing.clone();
            if *code != 0 {
                meta.push(format!("exit {code}"));
            }
            let head = panel::status_line(Some(icon), title, None, &meta, t, g);
            let mut body = vec![Line::from(vec![
                Span::styled("$ ".to_string(), t.dim()),
                Span::styled(command.clone(), t.emphasis(t.text)),
            ])];
            let out = if stdout.is_empty() { stderr } else { stdout };
            let stream_style = if stdout.is_empty() && !stderr.is_empty() {
                t.fg(t.warning)
            } else {
                t.body()
            };
            for l in out.lines() {
                for piece in md::hard_wrap(l.trim_end(), avail.saturating_sub(4)) {
                    body.push(Line::from(Span::styled(piece, stream_style)));
                }
            }
            let clipped = clip_body(&mut body, expanded, 11, t, g);
            let state = if *code == 0 {
                panel::Frame::Done
            } else {
                panel::Frame::Failed
            };
            panel::railed(
                head,
                body,
                clipped.then(|| vec![panel::expand_hint(t)]),
                avail,
                state,
                t,
                g,
            )
        }

        // ---- file contents with a real line-number gutter -------------------
        V::Read {
            path,
            lang,
            lines: src,
            start,
            total,
            truncated,
        } => {
            let mut meta = vec![format!("{total} lines")];
            meta.extend(timing.clone());
            if *truncated {
                meta.push("truncated".into());
            }
            let head =
                panel::status_line(Some(icon), title, Some((path.clone(), t.info)), &meta, t, g);
            let gw = (start + src.len()).to_string().len().max(2);
            let mut body: Vec<Line<'static>> = src
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let mut spans = vec![Span::styled(
                        format!("{:>gw$} ", start + i, gw = gw),
                        t.fg(t.diff_hunk),
                    )];
                    spans.extend(md::highlight(
                        &md::hard_wrap(l.trim_end(), avail.saturating_sub(gw + 5))
                            .first()
                            .cloned()
                            .unwrap_or_default(),
                        lang,
                        t,
                    ));
                    Line::from(spans)
                })
                .collect();
            let clipped = clip_body(&mut body, expanded, 14, t, g);
            panel::railed(
                head,
                body,
                clipped.then(|| vec![panel::expand_hint(t)]),
                avail,
                panel::Frame::Done,
                t,
                g,
            )
        }

        // ---- grep hits, grouped under the file they are in ------------------
        V::Matches {
            pattern,
            groups,
            hits,
            truncated,
        } => {
            let mut meta = vec![
                plural(*hits, "match", "matches"),
                plural(groups.len(), "file", "files"),
            ];
            meta.extend(timing.clone());
            if *truncated {
                meta.push("truncated".into());
            }
            let mut out = vec![Line::from(panel::status_line(
                Some(icon),
                title,
                Some((pattern.clone(), t.accent)),
                &meta,
                t,
                g,
            ))];
            let cap = if expanded { groups.len() } else { 4 };
            for grp in groups.iter().take(cap) {
                out.push(Line::from(vec![
                    Span::styled(format!("  {} ", g.branch), t.dim()),
                    Span::styled(grp.file.clone(), t.fg(t.info)),
                    Span::styled(
                        format!("  {}", plural(grp.lines.len(), "hit", "hits")),
                        t.dim(),
                    ),
                ]));
                let shown = if expanded { grp.lines.len() } else { 3 };
                for (n, (ln, text)) in grp.lines.iter().take(shown).enumerate() {
                    let last = n + 1 == grp.lines.len().min(shown);
                    out.push(Line::from(vec![
                        Span::styled(
                            format!("  {}  {} ", g.vline, if last { g.last } else { g.branch }),
                            t.dim(),
                        ),
                        Span::styled(format!("{ln:>4} "), t.fg(t.diff_hunk)),
                        Span::styled(
                            md::hard_wrap(text.trim(), avail.saturating_sub(16))
                                .first()
                                .cloned()
                                .unwrap_or_default(),
                            t.body(),
                        ),
                    ]));
                }
                if !expanded && grp.lines.len() > shown {
                    out.push(Line::from(Span::styled(
                        format!("  {}  {} {} more", g.vline, g.last, grp.lines.len() - shown),
                        t.dim(),
                    )));
                }
            }
            if !expanded && groups.len() > cap {
                out.push(Line::from(vec![
                    Span::styled(
                        format!("  {} {} more files", g.last, groups.len() - cap),
                        t.dim(),
                    ),
                    panel::expand_hint(t),
                ]));
            }
            out
        }

        // ---- a flat file list, as a tree ------------------------------------
        V::Files {
            pattern,
            files,
            truncated,
        } => {
            let mut meta = vec![plural(files.len(), "file", "files")];
            meta.extend(timing.clone());
            if *truncated {
                meta.push("truncated".into());
            }
            let mut out = vec![Line::from(panel::status_line(
                Some(icon),
                title,
                Some((pattern.clone(), t.accent)),
                &meta,
                t,
                g,
            ))];
            let cap = if expanded { files.len() } else { 8 };
            for (i, f) in files.iter().take(cap).enumerate() {
                let last = i + 1 == files.len().min(cap);
                out.push(Line::from(vec![
                    Span::styled(
                        format!("  {} ", if last { g.last } else { g.branch }),
                        t.dim(),
                    ),
                    Span::styled(f.clone(), t.body()),
                ]));
            }
            if !expanded && files.len() > cap {
                out.push(Line::from(vec![
                    Span::styled(format!("  {} {} more", g.last, files.len() - cap), t.dim()),
                    panel::expand_hint(t),
                ]));
            }
            out
        }

        // ---- a directory, dirs first, as a tree ----------------------------
        V::Listing { path, entries, .. } => {
            let mut meta = vec![plural(entries.len(), "entry", "entries")];
            meta.extend(timing.clone());
            let mut out = vec![Line::from(panel::status_line(
                Some(icon),
                title,
                Some((path.clone(), t.info)),
                &meta,
                t,
                g,
            ))];
            let cap = if expanded { entries.len() } else { 10 };
            for (i, e) in entries.iter().take(cap).enumerate() {
                let last = i + 1 == entries.len().min(cap);
                let (nm, st) = if e.is_dir {
                    (format!("{}/", e.name), t.emphasis(t.info))
                } else {
                    (e.name.clone(), t.body())
                };
                let mut row = vec![
                    Span::styled(
                        format!("  {} ", if last { g.last } else { g.branch }),
                        t.dim(),
                    ),
                    Span::styled(nm, st),
                ];
                if !e.is_dir {
                    row.push(Span::styled(
                        format!("  {}", crate::tools::human_size(e.size)),
                        t.dim(),
                    ));
                }
                out.push(Line::from(row));
            }
            if !expanded && entries.len() > cap {
                out.push(Line::from(vec![
                    Span::styled(
                        format!("  {} {} more", g.last, entries.len() - cap),
                        t.dim(),
                    ),
                    panel::expand_hint(t),
                ]));
            }
            out
        }

        // ---- anything without a bespoke view -------------------------------
        V::Plain => {
            let head = match ok {
                None => format!("{} {label}", running_verb(name)),
                Some(_) if summary.is_empty() => label.to_string(),
                Some(_) => summary.to_string(),
            };
            let mut spans = vec![Span::styled(format!("{} ", icon.0), t.emphasis(icon.1))];
            spans.push(Span::styled(head, t.fg(t.tool_title)));
            if !timing.is_empty() {
                spans.push(Span::styled(format!("  {}", timing.join(g.sep)), t.dim()));
            }
            if !expanded && !detail.is_empty() && ok.is_some() {
                spans.push(panel::expand_hint(t));
            }
            let mut out = md::wrap_spans(spans, avail, 0);
            if expanded && !detail.is_empty() {
                let lang = match name {
                    "run_command" => "shell",
                    _ => "text",
                };
                let mut body: Vec<Line<'static>> = detail
                    .lines()
                    .flat_map(|l| md::hard_wrap(l.trim_end(), avail.saturating_sub(4)))
                    .map(|s| Line::from(md::highlight(&s, lang, t)))
                    .collect();
                clip_body(&mut body, expanded, 40, t, g);
                for l in body {
                    let mut row = vec![Span::styled(format!(" {} ", g.rail), t.dim())];
                    row.extend(l.spans);
                    out.push(Line::from(row));
                }
            }
            out
        }
    };
    // Running tool: show a clear indeterminate progress bar under the header so
    // a long call (a build, a test run) visibly moves rather than looking hung.
    // A bright block of fixed width sweeps back and forth across a track — a
    // distinct "marquee" style that reads unmistakably as in-progress, derived
    // from elapsed time so it animates on the frame clock.
    let mut lines = lines;
    if ok.is_none() && started.elapsed().as_millis() > 250 {
        let track = avail.saturating_sub(4).clamp(10, 36);
        let block = (track / 5).max(3);
        let span = track.saturating_sub(block).max(1);
        let period = 1600u128;
        let phase = (started.elapsed().as_millis() % period) as f32 / period as f32;
        let tri = if phase < 0.5 {
            phase * 2.0
        } else {
            (1.0 - phase) * 2.0
        };
        let pos = (tri * span as f32).round() as usize;
        let mut spans = vec![Span::styled(" ".to_string(), t.dim())];
        for i in 0..track {
            let on = i >= pos && i < pos + block;
            if on {
                spans.push(Span::styled("━".to_string(), t.fg(t.accent)));
            } else {
                spans.push(Span::styled("─".to_string(), t.dim()));
            }
        }
        lines.push(Line::from(spans));
    }
    // A blank line after every tool block gives the transcript breathing room,
    // so tool calls, diffs and prose don't run together into a dense wall.
    lines.push(Line::default());
    indent_all(lines, indent_w, t, g, depth)
}

/// Trim a body to `cap` rows, leaving a dim marker. Returns whether it clipped.
fn clip_body(
    body: &mut Vec<Line<'static>>,
    expanded: bool,
    cap: usize,
    t: &Theme,
    g: &Glyphs,
) -> bool {
    let hard = if expanded { 200 } else { cap };
    if body.len() <= hard {
        return false;
    }
    let extra = body.len() - hard + 1;
    body.truncate(hard.saturating_sub(1));
    body.push(Line::from(Span::styled(
        format!("{} {extra} more lines", g.ellipsis),
        t.dim(),
    )));
    true
}

/// "1 match" / "5 matches" — getting this wrong is the kind of small wrongness
/// that makes a UI feel unfinished.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The path or subject a label is about, for a failure header.
fn first_word_target(label: &str) -> String {
    label.split_whitespace().nth(1).unwrap_or(label).to_string()
}

/// Push a rendered block right, drawing a rail when it belongs to a subagent.
fn indent_all(
    lines: Vec<Line<'static>>,
    indent_w: usize,
    t: &Theme,
    g: &Glyphs,
    depth: u8,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|l| {
            let mut spans = Vec::with_capacity(l.spans.len() + 2);
            if depth > 0 {
                // A subagent's output is railed, because that nesting is the one
                // thing the reader cannot infer from the content.
                spans.push(Span::raw(" ".repeat(indent_w.saturating_sub(2))));
                spans.push(Span::styled(format!("{} ", g.rail), t.fg(t.accent_alt)));
            } else {
                spans.push(Span::raw(" ".repeat(indent_w)));
            }
            spans.extend(l.spans);
            Line::from(spans)
        })
        .collect()
}

/// Indent a rendered line under its tool card, drawing a rail so the output
/// reads as attached rather than as an indented island. A subagent's rail takes
/// the subagent accent, so nesting is visible at a glance.
fn indent(line: Line<'static>, width: usize, t: &Theme, g: &Glyphs, depth: u8) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    let (rail_colour, lead) = if depth > 0 {
        (t.accent_alt, 2)
    } else {
        (t.border, 1)
    };
    spans.push(Span::raw(" ".repeat(lead)));
    spans.push(Span::styled(format!("{} ", g.rail), t.fg(rail_colour)));
    let used = lead + 2;
    if width > used {
        spans.push(Span::raw(" ".repeat(width - used)));
    }
    spans.extend(line.spans);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing ever removed a transcript block, and each keeps its rendered
    /// lines as well as its text — about twenty times the raw size. A long
    /// session grew for ever. Eviction has to keep the view still: scroll is in
    /// lines, so the caller shifts it by however many were dropped.
    #[test]
    fn a_long_transcript_is_bounded_and_keeps_its_offsets_straight() {
        let mut tr = Transcript::new(crate::theme::resolve("auto"), crate::theme::UNICODE);
        for i in 0..4_600 {
            tr.user(format!("message {i}"));
        }
        let w = 100u16;
        tr.relayout(w);
        let before_blocks = tr.blocks.len();
        let before_lines = tr.total_lines();

        let dropped = tr.trim_blocks();
        assert!(dropped > 0, "an over-long transcript is trimmed");
        assert!(tr.blocks.len() < before_blocks, "blocks actually went");
        tr.relayout(w);

        // The offsets must still be a correct running total, or `window`'s
        // binary search lands on the wrong block.
        let mut expect = 0usize;
        for b in &tr.blocks {
            assert_eq!(b.offset, expect, "offsets are contiguous after eviction");
            expect += b.cache.as_ref().map(|c| c.2.len()).unwrap_or(0);
        }
        assert_eq!(tr.total_lines(), expect, "the total matches the blocks");
        assert_eq!(
            before_lines - dropped,
            tr.total_lines(),
            "the reported drop matches the lines actually removed"
        );

        // The newest content survives; the oldest is what went.
        // The tail, not one line of it: a block renders with padding, so the
        // final line is often blank.
        let tail: String = tr
            .window(tr.total_lines().saturating_sub(6), 6)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            tail.contains("4599"),
            "the newest message survives: {tail:?}"
        );
        let head: String = tr
            .window(0, 6)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            !head.contains("message 0"),
            "the oldest is what went: {head:?}"
        );

        // A transcript under the cap is left completely alone.
        let mut small = Transcript::new(crate::theme::resolve("auto"), crate::theme::UNICODE);
        small.user("hello".into());
        small.relayout(w);
        assert_eq!(small.trim_blocks(), 0, "a short transcript is not touched");
    }

    /// The freeze: `draw` used to lay the transcript out at one width, then
    /// again one column narrower when it decided a scrollbar was needed.
    /// `relayout` treats a width change as total invalidation, so the two calls
    /// invalidated each other every frame for the rest of the session — every
    /// block re-rendered, markdown and all, on every keystroke. It cost 33 ms a
    /// frame by 4800 blocks and got worse the longer someone worked.
    ///
    /// This pins the property that matters: laying out repeatedly at one width
    /// must leave nothing to do. If a future change reintroduces a second width
    /// per frame, the second assertion fails.
    #[test]
    fn relaying_out_at_a_stable_width_is_free() {
        let mut tr = Transcript::new(crate::theme::resolve("auto"), crate::theme::UNICODE);
        for i in 0..300 {
            tr.user(format!("question {i}"));
            tr.assistant_delta(&format!("reply **{i}** with a list:\n- a\n- b\n"));
        }
        let w = 100u16;
        tr.relayout(w);
        // Nothing is dirty once it has been laid out at this width.
        assert_eq!(
            tr.dirty_from,
            tr.blocks.len(),
            "a settled transcript has no dirty blocks"
        );
        let before: Vec<u64> = tr
            .blocks
            .iter()
            .filter_map(|b| b.cache.as_ref().map(|c| c.1))
            .collect();
        tr.relayout(w);
        let after: Vec<u64> = tr
            .blocks
            .iter()
            .filter_map(|b| b.cache.as_ref().map(|c| c.1))
            .collect();
        assert_eq!(
            before, after,
            "a second frame at the same width re-renders nothing"
        );

        // And the thing that used to happen: one column narrower invalidates
        // everything. That is why the width must not change between frames.
        assert_eq!(tr.laid_out_at, w);
        tr.relayout(w - 1);
        assert_eq!(
            tr.laid_out_at,
            w - 1,
            "a different width does re-lay everything out"
        );
    }

    /// `relayout` quantises the transcript clock into each block's cache
    /// signature so a running tool re-renders about ten times a second — that is
    /// what animates the spinner and its elapsed timer.
    ///
    /// It only works while `now` is a fixed epoch. It was being reset on every
    /// frame, which made `elapsed()` a few microseconds and the quantum
    /// permanently 0, so a running block never invalidated itself. That went
    /// unnoticed because the width flap was re-rendering everything anyway;
    /// fixing the flap without this would have left the spinner frozen.
    #[test]
    fn a_running_tool_re_renders_as_the_clock_moves() {
        let mut tr = Transcript::new(crate::theme::resolve("auto"), crate::theme::UNICODE);
        tr.user("go".to_string());
        tr.tool_start("1".into(), "run_command".into(), "$ sleep 5".into(), 0);
        let w = 100u16;
        tr.relayout(w);
        let sig = |t: &Transcript| t.blocks.last().and_then(|b| b.cache.as_ref().map(|c| c.1));
        let before = sig(&tr);
        assert!(before.is_some(), "the running block is cached");

        // Same width, so nothing but the clock has changed.
        std::thread::sleep(std::time::Duration::from_millis(160));
        tr.relayout(w);
        assert_ne!(
            sig(&tr),
            before,
            "a running tool must re-render as time passes, or the spinner and \
             the elapsed timer freeze"
        );

        // And the specific mistake: the UI used to assign `now = Instant::now()`
        // on every frame. With the epoch reset, elapsed() is microseconds, the
        // quantum is always 0, and the running block stops invalidating — which
        // is precisely the frozen spinner. Simulated here so a future change
        // that reintroduces the reset is caught by this test rather than by a
        // user watching a spinner sit still.
        // The real loop was: reset the epoch, then render — every frame. Mirror
        // that exactly. elapsed() is then always a few microseconds, the quantum
        // is always 0, and the signature never moves however long you wait.
        tr.now = std::time::Instant::now();
        tr.relayout(w);
        let anchored = sig(&tr);
        std::thread::sleep(std::time::Duration::from_millis(160));
        tr.now = std::time::Instant::now(); // the per-frame reset
        tr.relayout(w);
        assert_eq!(
            sig(&tr),
            anchored,
            "resetting the epoch each frame freezes the animation — this is the \
             behaviour that must not come back"
        );
        // Put the clock back to a sane epoch for the rest of the test.
        tr.now = std::time::Instant::now();

        // A finished block, by contrast, stays put.
        tr.tool_end(
            "1",
            true,
            "done".into(),
            String::new(),
            crate::tools::ToolView::Plain,
        );
        tr.relayout(w);
        let settled = sig(&tr);
        std::thread::sleep(std::time::Duration::from_millis(160));
        tr.relayout(w);
        assert_eq!(
            sig(&tr),
            settled,
            "a finished block does not re-render on the clock"
        );
    }

    fn shot(t: &Transcript) -> String {
        t.window(0, t.total_lines().max(1))
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| format!("{}\u{1}{:?}", s.content, s.style))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The streaming reply is rendered incrementally: its settled prefix is kept
    /// and only the tail is re-rendered. That is only safe if *every* frame looks
    /// exactly like a transcript rendered in one shot — so compare them at every
    /// single delta, not just at the end.
    #[test]
    fn streaming_renders_frame_for_frame_like_one_shot() {
        let doc = "# Title\n\nfirst paragraph of prose that wraps across the width.\n\n\
                   ```rust\nfn f() {\n\n    let x = 1;\n}\n```\n\n\
                   - a list item\n- another one\n\n\
                   | a | b |\n|---|---|\n| 1 | 2 |\n\n\
                   > a quote\n\nlast paragraph, ünïcödé and 日本語 included.";
        let mut inc = tr();
        inc.user("question".into());
        let mut acc = String::new();
        for piece in doc.split_inclusive(' ') {
            inc.assistant_delta(piece);
            acc.push_str(piece);
            inc.relayout(48);

            let mut one = tr();
            one.user("question".into());
            one.assistant_delta(&acc);
            one.relayout(48);
            assert_eq!(
                shot(&inc),
                shot(&one),
                "incremental render diverged after {} bytes",
                acc.len()
            );
        }
    }

    /// Same guarantee while the reveal animation is cutting the text short: the
    /// shown prefix grows, so the incremental path must track it.
    #[test]
    fn streaming_matches_one_shot_while_revealing() {
        let doc = "para one here\n\npara two here\n\npara three ends it";
        let mut inc = tr();
        inc.animate_reveal = true;
        inc.user("q".into());
        inc.assistant_delta(doc);
        // Walk the reveal cursor forward the way the frame clock does.
        for chars in 1..=doc.chars().count() {
            inc.reveal = chars;
            inc.dirty_from = 0;
            inc.relayout(40);

            let mut one = tr();
            one.animate_reveal = true;
            one.user("q".into());
            one.assistant_delta(doc);
            one.reveal = chars;
            one.dirty_from = 0;
            one.relayout(40);
            assert_eq!(shot(&inc), shot(&one), "diverged at reveal {chars}");
        }
    }

    /// A width change throws away every wrap, including the kept prefix.
    #[test]
    fn a_width_change_rerenders_the_streaming_reply() {
        let doc = "alpha beta gamma delta epsilon\n\nzeta eta theta iota kappa lambda mu nu";
        let mut inc = tr();
        inc.user("q".into());
        for piece in doc.split_inclusive(' ') {
            inc.assistant_delta(piece);
            inc.relayout(30);
        }
        inc.relayout(90);
        let mut one = tr();
        one.user("q".into());
        one.assistant_delta(doc);
        one.relayout(90);
        assert_eq!(
            shot(&inc),
            shot(&one),
            "re-wrap after a width change diverged"
        );
    }

    /// A reply with no blank line has no settled prefix, so it must still render
    /// correctly through the full path.
    #[test]
    fn a_single_paragraph_reply_still_renders_correctly() {
        let doc = "one very long paragraph with no blank lines at all so nothing ever settles \
                   and the whole thing is re-rendered every frame";
        let mut inc = tr();
        inc.user("q".into());
        let mut acc = String::new();
        for piece in doc.split_inclusive(' ') {
            inc.assistant_delta(piece);
            acc.push_str(piece);
            inc.relayout(36);
        }
        let mut one = tr();
        one.user("q".into());
        one.assistant_delta(&acc);
        one.relayout(36);
        assert_eq!(shot(&inc), shot(&one));
    }
    use crate::theme::{ANSI, UNICODE};

    fn tr() -> Transcript {
        Transcript::new(ANSI, UNICODE)
    }

    fn flat(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect()
    }

    #[test]
    fn two_panels_of_equal_length_do_not_collide() {
        let a = vec![Line::from(Span::raw("alpha".to_string()))];
        let b = vec![Line::from(Span::raw("bravo".to_string()))];
        assert_ne!(
            raw_hash(&a),
            raw_hash(&b),
            "same-length blocks must hash differently"
        );

        let mut t = tr();
        t.raw(a);
        t.raw(b);
        t.relayout(60);
        let text = flat(&t.window(0, 20));
        assert!(text.contains("alpha") && text.contains("bravo"), "{text}");
    }

    #[test]
    fn caches_until_content_changes() {
        let mut t = tr();
        t.assistant_delta("hello");
        let a = t.relayout(40);
        assert_eq!(a, t.relayout(40));
        t.assistant_delta(" world");
        assert!(t.relayout(40) >= a);
    }

    #[test]
    fn window_slices_lines() {
        let mut t = tr();
        t.user("one".into());
        t.assistant_delta("two");
        assert!(t.relayout(40) >= 3);
        assert_eq!(t.window(1, 2).len(), 2);
    }

    /// Render one tool view and return the plain text, for eyeballing layout.
    fn render_view(name: &str, label: &str, v: crate::tools::ToolView, w: u16) -> String {
        let mut t = tr();
        t.tool_start("1".into(), name.into(), label.into(), 0);
        t.tool_end("1", true, label.into(), String::new(), v);
        t.relayout(w);
        flat(&t.window(0, 40))
    }

    #[test]
    fn run_view_shows_command_and_exit_code() {
        use crate::tools::ToolView;
        let out = render_view(
            "run_command",
            "$ pytest",
            ToolView::Run {
                command: "python3 -m pytest -q".into(),
                stdout: "3 passed in 0.04s".into(),
                stderr: String::new(),
                code: 0,
            },
            72,
        );
        assert!(out.contains("Run"), "{out}");
        assert!(out.contains("$ python3 -m pytest -q"), "{out}");
        assert!(out.contains("3 passed"), "{out}");
        println!("\n--- run ---\n{out}");
    }

    #[test]
    fn failing_command_reports_its_exit_code() {
        use crate::tools::ToolView;
        let out = render_view(
            "run_command",
            "$ false",
            ToolView::Run {
                command: "false".into(),
                stdout: String::new(),
                stderr: "boom".into(),
                code: 1,
            },
            72,
        );
        assert!(out.contains("exit 1"), "{out}");
    }

    #[test]
    fn matches_view_groups_hits_under_each_file() {
        use crate::tools::{MatchGroup, ToolView};
        let out = render_view(
            "search",
            "search TODO",
            ToolView::Matches {
                pattern: "TODO".into(),
                groups: vec![
                    MatchGroup {
                        file: "src/a.rs".into(),
                        lines: vec![(3, "// TODO fix".into()), (9, "// TODO drop".into())],
                    },
                    MatchGroup {
                        file: "src/b.rs".into(),
                        lines: vec![(1, "// TODO later".into())],
                    },
                ],
                hits: 3,
                truncated: false,
            },
            72,
        );
        assert!(out.contains("Grep"), "{out}");
        assert!(out.contains("3 matches"), "{out}");
        assert!(out.contains("2 files"), "{out}");
        assert!(
            out.contains("src/a.rs") && out.contains("src/b.rs"),
            "{out}"
        );
        assert!(
            out.contains("2 hits") && out.contains("1 hit"),
            "pluralised: {out}"
        );
        println!("\n--- grep ---\n{out}");
    }

    #[test]
    fn listing_view_marks_directories_and_sizes() {
        use crate::tools::{DirEntry, ToolView};
        let out = render_view(
            "list_dir",
            "list .",
            ToolView::Listing {
                path: ".".into(),
                entries: vec![
                    DirEntry {
                        name: "src".into(),
                        is_dir: true,
                        size: 0,
                    },
                    DirEntry {
                        name: "README.md".into(),
                        is_dir: false,
                        size: 2048,
                    },
                ],
                truncated: false,
            },
            72,
        );
        assert!(out.contains("src/"), "dirs get a slash: {out}");
        assert!(out.contains("README.md"), "{out}");
        assert!(
            out.contains("2.0K") || out.contains("2K"),
            "size shown: {out}"
        );
        println!("\n--- list ---\n{out}");
    }

    #[test]
    fn read_view_numbers_its_lines() {
        use crate::tools::ToolView;
        let out = render_view(
            "read_file",
            "read a.py",
            ToolView::Read {
                path: "a.py".into(),
                lang: "py".into(),
                lines: vec!["import os".into(), "print(os.name)".into()],
                start: 10,
                total: 40,
                truncated: true,
            },
            72,
        );
        assert!(
            out.contains("10 import os"),
            "gutter starts at offset: {out}"
        );
        assert!(out.contains("11 print"), "{out}");
        assert!(
            out.contains("40 lines") && out.contains("truncated"),
            "{out}"
        );
        println!("\n--- read ---\n{out}");
    }

    #[test]
    fn diff_view_carries_its_stats() {
        use crate::tools::ToolView;
        let out = render_view(
            "edit_file",
            "edit a.py",
            ToolView::Diff {
                path: "a.py".into(),
                diff: "@@ -1,2 +1,2 @@\n-old\n+new\n".into(),
                added: 1,
                removed: 1,
                created: false,
            },
            72,
        );
        assert!(out.contains("Edit"), "{out}");
        assert!(
            out.contains("+1") && out.contains("-1"),
            "stats in footer: {out}"
        );
        println!("\n--- diff ---\n{out}");
    }

    #[test]
    fn failed_tool_expands_with_detail() {
        let mut t = tr();
        t.tool_start("1".into(), "read_file".into(), "read a".into(), 0);
        t.tool_end(
            "1",
            false,
            "boom".into(),
            "ERROR: boom".into(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        // The header already carries the failure glyph, so the body shows the
        // message without repeating "ERROR:".
        let text = flat(&t.window(0, 20));
        assert!(text.contains("boom"), "{text}");
        assert!(!text.contains("ERROR:"), "redundant prefix kept: {text}");
    }

    #[test]
    fn successful_tool_stays_collapsed_and_shows_summary() {
        let mut t = tr();
        t.tool_start("1".into(), "read_file".into(), "read a.rs".into(), 0);
        t.tool_end(
            "1",
            true,
            "read a.rs (12 lines)".into(),
            "1| x".into(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        let text = flat(&t.window(0, 20));
        assert!(text.contains("read a.rs (12 lines)"), "{text}");
        assert!(
            !text.contains("1| x"),
            "detail should stay collapsed: {text}"
        );
        assert!(t.toggle_tools_pref());
        t.relayout(60);
        assert!(flat(&t.window(0, 20)).contains("1| x"));
    }

    #[test]
    fn sticky_expand_persists_across_a_new_tool_block() {
        // The reported bug: after ctrl+r expands output, the NEXT tool call's
        // output would come back collapsed. With the global preference it stays
        // expanded for every block, old and new, until toggled off.
        let mut t = tr();
        t.expand_tools = true; // as if the user pressed ctrl+r
        t.tool_start("1".into(), "read_file".into(), "read a.rs".into(), 0);
        t.tool_end(
            "1",
            true,
            "read a.rs".into(),
            "AAA-body".into(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        assert!(
            flat(&t.window(0, 40)).contains("AAA-body"),
            "first block expanded"
        );
        // A brand-new tool arrives in a later response.
        t.tool_start("2".into(), "read_file".into(), "read b.rs".into(), 0);
        t.tool_end(
            "2",
            true,
            "read b.rs".into(),
            "BBB-body".into(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        let text = flat(&t.window(0, 40));
        assert!(
            text.contains("AAA-body"),
            "old block still expanded: {text}"
        );
        assert!(
            text.contains("BBB-body"),
            "NEW block also expanded (no reset): {text}"
        );
    }

    #[test]
    fn reasoning_collapses_to_a_duration() {
        let mut t = tr();
        t.reasoning_delta("let me think about this carefully");
        t.assistant_delta("answer"); // closing the reasoning stamps elapsed
        t.relayout(60);
        let text = flat(&t.window(0, 20));
        assert!(text.contains("thought for"), "{text}");
        assert!(
            !text.contains("carefully"),
            "reasoning body should be hidden: {text}"
        );
        // Expanding the most-recent reasoning block reveals its body.
        assert!(t.toggle_last_reasoning());
        t.relayout(60);
        assert!(flat(&t.window(0, 20)).contains("carefully"));
    }

    #[test]
    fn reasoning_shows_a_token_estimate() {
        let mut t = tr();
        // ~120 chars of reasoning -> ~30 tokens at 4 chars/token.
        t.reasoning_delta(&"analyze the failing case and pick the fix. ".repeat(3));
        t.assistant_delta("done");
        t.relayout(80);
        let text = flat(&t.window(0, 20));
        assert!(text.contains("thought for"), "{text}");
        assert!(
            text.contains("tokens"),
            "reasoning should show token estimate: {text}"
        );
    }

    #[test]
    fn running_tool_shows_a_progress_bar() {
        let mut t = tr();
        t.tool_start("1".into(), "run_command".into(), "cargo build".into(), 0);
        // Backdate the start so it's past the 250ms reveal threshold.
        if let Item::Tool { started, .. } = &mut t.blocks.last_mut().unwrap().item {
            *started = Instant::now() - Duration::from_millis(600);
        }
        t.relayout(80);
        let text = flat(&t.window(0, 20));
        assert!(
            text.contains('━') || text.contains('─'),
            "running tool should show a progress track: {text}"
        );
    }

    #[test]
    fn hiding_reasoning_removes_it_entirely() {
        let mut t = tr();
        t.reasoning_delta("hidden thoughts");
        t.show_reasoning = false;
        t.relayout(60);
        assert!(!flat(&t.window(0, 20)).contains("thought"));
    }

    #[test]
    fn expanded_detail_gets_a_rail() {
        let mut t = tr();
        t.tool_start("1".into(), "run_command".into(), "$ ls".into(), 0);
        t.tool_end(
            "1",
            false,
            "failed".into(),
            "boom".into(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        let text = flat(&t.window(0, 20));
        assert!(
            text.contains(UNICODE.rail),
            "detail should sit under a rail: {text}"
        );
    }

    #[test]
    fn nested_tools_get_a_rail() {
        let mut t = tr();
        t.tool_start("1".into(), "search".into(), "search /x/".into(), 1);
        t.tool_end(
            "1",
            true,
            "search /x/ (1 hit)".into(),
            String::new(),
            crate::tools::ToolView::Plain,
        );
        t.relayout(60);
        assert!(flat(&t.window(0, 20)).contains(UNICODE.vline));
    }

    #[test]
    fn assistant_prose_has_no_marker() {
        let mut t = tr();
        t.assistant_delta("plain answer");
        t.relayout(60);
        let first = t.window(0, 1);
        let text = flat(&first);
        assert!(text.starts_with("plain"), "unexpected marker: {text:?}");
    }

    /// The whole point of the offset cache: a big, static transcript must not
    /// cost anything per frame.
    #[test]
    fn idle_relayout_of_a_large_transcript_does_no_work() {
        let mut t = tr();
        for i in 0..3000 {
            t.user(format!("message number {i}"));
            t.assistant_delta(&format!("reply number {i}\n"));
        }
        let total = t.relayout(80);
        assert!(total > 6000, "expected a big transcript, got {total}");

        // A second pass at the same width must re-render nothing.
        let before: Vec<usize> = t.blocks.iter().map(|b| b.offset).collect();
        assert_eq!(t.relayout(80), total);
        let after: Vec<usize> = t.blocks.iter().map(|b| b.offset).collect();
        assert_eq!(before, after);
        assert_eq!(t.dirty_from, t.blocks.len(), "nothing should be left dirty");
    }

    /// Timing, not just behaviour: an idle frame on a large transcript should be
    /// microseconds. Generous bound so this is not flaky on a loaded machine.
    #[test]
    fn idle_frames_are_cheap_at_scale() {
        let mut t = tr();
        for i in 0..4000 {
            t.user(format!(
                "message number {i} with enough text to wrap once or twice"
            ));
        }
        t.relayout(80);

        let started = std::time::Instant::now();
        for _ in 0..500 {
            t.relayout(80);
            let _ = t.window(t.total_lines().saturating_sub(40), 40);
        }
        let per_frame = started.elapsed() / 500;
        println!("idle frame on a 4000-block transcript: {per_frame:?}");
        assert!(
            per_frame < std::time::Duration::from_millis(2),
            "idle frame took {per_frame:?} on a 4000-block transcript"
        );
    }

    #[test]
    fn window_seeks_instead_of_walking() {
        let mut t = tr();
        for i in 0..2000 {
            t.user(format!("m{i}"));
        }
        let total = t.relayout(80);
        // A window near the end must return the tail, not the head.
        let tail = t.window(total - 3, 3);
        let text: String = tail
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("m1999"),
            "expected the last message, got {text:?}"
        );
        // Offsets are monotonic, which is what makes the binary search valid.
        let offsets: Vec<usize> = t.blocks.iter().map(|b| b.offset).collect();
        assert!(offsets.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn appending_only_relayouts_the_new_block() {
        let mut t = tr();
        for i in 0..500 {
            t.user(format!("m{i}"));
        }
        t.relayout(80);
        // Appending marks only the new tail dirty.
        t.user("fresh".into());
        assert_eq!(t.dirty_from, t.blocks.len() - 1);
        let total = t.relayout(80);
        assert!(total > 500);
        assert_eq!(t.dirty_from, t.blocks.len());
    }

    #[test]
    fn width_change_invalidates_everything() {
        let mut t = tr();
        t.user("a fairly long message that will wrap differently at other widths".into());
        let wide = t.relayout(100);
        let narrow = t.relayout(30);
        assert!(narrow > wide, "narrower should wrap to more lines");
    }

    #[test]
    fn human_ms_is_compact() {
        assert_eq!(human_ms(Duration::from_millis(12)), "12ms");
        assert_eq!(human_ms(Duration::from_millis(1500)), "1.5s");
        assert_eq!(human_ms(Duration::from_secs(75)), "1m15s");
    }
}
