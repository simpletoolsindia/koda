//! Turn tracing: what actually happened, end to end.
//!
//! `log.rs` records *events*; this records *structure*. One `Turn` per
//! top-level user message, holding the ordered steps that turn took — every
//! model call (with the exact request we sent and the raw SSE we got back),
//! every tool call (args, outcome, approval, diff), and every compaction. That
//! is enough for the web UI to lay a turn out as a waterfall and for a person
//! to answer "why did it do that?" without re-running anything.
//!
//! Like `debug.rs` this is a process-global sink behind a switch, so no call
//! site has to thread a handle through. It is bounded on both axes: a ring of
//! the last `MAX_TURNS` turns, and per-field payload caps, so a long session
//! cannot grow memory without limit.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Turns kept in memory. Enough to look back over a working session.
const MAX_TURNS: usize = 50;
/// The request body is the biggest payload (it contains the whole history), so
/// it gets a larger budget than the rest.
const CAP_REQUEST: usize = 128 * 1024;
const CAP_FIELD: usize = 32 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);
static VERSION: AtomicU64 = AtomicU64::new(0);

fn started() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn ring() -> &'static Mutex<VecDeque<Turn>> {
    static RING: OnceLock<Mutex<VecDeque<Turn>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_TURNS)))
}

/// Seconds since the process started — the same clock `log.rs` uses, so a trace
/// step and a log line can be lined up.
fn now() -> f64 {
    started().elapsed().as_secs_f64()
}

fn bump() {
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Model,
    Tool,
    Compaction,
}

/// Whether the user was asked before a tool ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    /// Not gated: a read-only tool, or autonomy covered it.
    Auto,
    Approved,
    Denied,
}

/// One request/response round trip against the model.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelCall {
    /// The exact JSON body sent (pretty-printed, truncated).
    pub request: String,
    /// Raw SSE bytes as received, verbatim until the cap.
    pub response: String,
    pub reasoning: String,
    pub text: String,
    pub finish_reason: Option<String>,
    pub retries: u32,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    /// Names of the tools this call asked for, in order.
    pub tool_calls: Vec<String>,
    pub error: Option<String>,
}

/// One tool invocation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolStep {
    pub name: String,
    /// Pretty-printed arguments.
    pub args: String,
    pub ok: bool,
    pub summary: String,
    pub detail: String,
    pub approval: Option<Approval>,
    /// The diff/preview shown for a write, when there is one.
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub seq: usize,
    pub kind: StepKind,
    /// Short label for the waterfall row (tool name, model id, "compaction").
    pub label: String,
    pub started: f64,
    pub ms: u64,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolStep>,
    /// Compaction outcome, e.g. "18400 → 4200 tokens".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub id: u64,
    pub started: f64,
    pub ended: Option<f64>,
    pub mode: String,
    pub model: String,
    pub endpoint: String,
    pub input: String,
    pub status: Status,
    pub steps: Vec<Step>,
    pub reply: String,
    pub tokens: usize,
}

impl Turn {
    fn ms(&self) -> u64 {
        let end = self.ended.unwrap_or_else(now);
        ((end - self.started).max(0.0) * 1000.0) as u64
    }
}

/// A turn without its payloads — what the turn rail lists.
#[derive(Debug, Clone, Serialize)]
pub struct TurnSummary {
    pub id: u64,
    pub started: f64,
    pub ms: u64,
    pub mode: String,
    pub model: String,
    pub input: String,
    pub status: Status,
    pub steps: usize,
    pub model_calls: usize,
    pub tool_calls: usize,
    pub tokens: usize,
    pub reply: String,
    pub running: bool,
}

/// A handle to an open step. Copy and payload-free, so it can be handed to the
/// HTTP layer (which streams raw response bytes into it) without borrowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepRef {
    pub turn: u64,
    pub seq: usize,
}

/// Turn tracing on or off. Driven by the web UI being enabled: without a viewer
/// there is no reason to hold payloads in memory.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether capture is active. `KODA_TRACE=1` forces it on.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
        || matches!(std::env::var("KODA_TRACE").ok().as_deref(), Some("1") | Some("true"))
}

pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// Open a turn. `None` when tracing is off, which makes every other call here
/// a no-op — callers pass the `Option` straight through.
pub fn begin_turn(mode: &str, model: &str, endpoint: &str, input: &str) -> Option<u64> {
    if !enabled() {
        return None;
    }
    let id = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let mut input = input.to_string();
    cap(&mut input, CAP_FIELD);
    let turn = Turn {
        id,
        started: now(),
        ended: None,
        mode: mode.to_string(),
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        input,
        status: Status::Running,
        steps: Vec::new(),
        reply: String::new(),
        tokens: 0,
    };
    let mut ring = ring().lock().ok()?;
    if ring.len() >= MAX_TURNS {
        ring.pop_front();
    }
    ring.push_back(turn);
    bump();
    Some(id)
}

pub fn end_turn(id: Option<u64>, status: Status, reply: &str, tokens: usize) {
    let Some(id) = id else { return };
    with_turn(id, |t| {
        t.ended = Some(now());
        t.status = status;
        t.reply = reply.to_string();
        cap(&mut t.reply, CAP_FIELD);
        t.tokens = tokens;
        // A turn that ends while a step is still open (cancel, hard error) must
        // not leave that step spinning forever in the UI.
        for s in t.steps.iter_mut().filter(|s| s.running) {
            s.running = false;
            s.ms = ((now() - s.started).max(0.0) * 1000.0) as u64;
        }
    });
}

/// Open a step inside a turn. The step is visible (and marked running) at once,
/// so a live turn streams into the UI rather than appearing when it finishes.
pub fn open_step(turn: Option<u64>, kind: StepKind, label: &str) -> Option<StepRef> {
    let id = turn?;
    let mut out = None;
    with_turn(id, |t| {
        let seq = t.steps.len();
        t.steps.push(Step {
            seq,
            kind,
            label: label.to_string(),
            started: now(),
            ms: 0,
            running: true,
            model: None,
            tool: None,
            note: None,
        });
        out = Some(StepRef { turn: id, seq });
    });
    out
}

pub fn finish_model(step: Option<StepRef>, mut call: ModelCall) {
    let Some(step) = step else { return };
    cap(&mut call.request, CAP_REQUEST);
    cap(&mut call.reasoning, CAP_FIELD);
    cap(&mut call.text, CAP_FIELD);
    with_step(step, move |s| {
        // The raw SSE and the retry count were streamed in while the call was
        // open; the closing payload must not wipe them.
        if let Some(open) = s.model.take() {
            call.response = open.response;
            if call.retries == 0 {
                call.retries = open.retries;
            }
        }
        s.model = Some(call);
        s.running = false;
        s.ms = ((now() - s.started).max(0.0) * 1000.0) as u64;
    });
}

pub fn finish_tool(step: Option<StepRef>, mut call: ToolStep) {
    let Some(step) = step else { return };
    cap(&mut call.args, CAP_FIELD);
    cap(&mut call.detail, CAP_FIELD);
    if let Some(d) = call.diff.as_mut() {
        cap(d, CAP_FIELD);
    }
    with_step(step, |s| {
        s.tool = Some(call.clone());
        s.running = false;
        s.ms = ((now() - s.started).max(0.0) * 1000.0) as u64;
    });
}

/// Close a compaction step with the token counts, so context loss is visible in
/// the waterfall instead of being an invisible gap.
pub fn finish_compaction(step: Option<StepRef>, before: usize, after: usize) {
    let Some(step) = step else { return };
    with_step(step, |s| {
        s.note = Some(format!("{before} → {after} tokens"));
        s.running = false;
        s.ms = ((now() - s.started).max(0.0) * 1000.0) as u64;
    });
}

/// Appended once when a stream outgrows its budget, so nobody mistakes a capped
/// capture for the whole response.
const SSE_CAPPED: &str = "\n…[truncated — response longer than the trace cap]";

/// Append raw response bytes to an open model step, verbatim, until the cap.
pub fn append_sse(step: Option<StepRef>, bytes: &[u8]) {
    let Some(step) = step else { return };
    with_step(step, |s| {
        let m = s.model.get_or_insert_with(ModelCall::default);
        if m.response.len() >= CAP_FIELD {
            if !m.response.ends_with(SSE_CAPPED) {
                m.response.push_str(SSE_CAPPED);
            }
            return;
        }
        m.response.push_str(&String::from_utf8_lossy(bytes));
        cap(&mut m.response, CAP_FIELD);
    });
}

/// How many times the HTTP layer had to retry this call.
pub fn set_retries(step: Option<StepRef>, retries: u32) {
    let Some(step) = step else { return };
    with_step(step, |s| {
        s.model.get_or_insert_with(ModelCall::default).retries = retries;
    });
}

fn with_turn(id: u64, f: impl FnOnce(&mut Turn)) {
    let Ok(mut ring) = ring().lock() else { return };
    if let Some(t) = ring.iter_mut().find(|t| t.id == id) {
        f(t);
        bump();
    }
}

fn with_step(step: StepRef, f: impl FnOnce(&mut Step)) {
    with_turn(step.turn, |t| {
        if let Some(s) = t.steps.get_mut(step.seq) {
            f(s);
        }
    });
}

/// Newest first, so the rail can render straight from this.
pub fn summaries() -> Vec<TurnSummary> {
    let Ok(ring) = ring().lock() else { return Vec::new() };
    ring.iter()
        .rev()
        .map(|t| TurnSummary {
            id: t.id,
            started: t.started,
            ms: t.ms(),
            mode: t.mode.clone(),
            model: t.model.clone(),
            input: first_line(&t.input, 160),
            status: t.status,
            steps: t.steps.len(),
            model_calls: t.steps.iter().filter(|s| s.kind == StepKind::Model).count(),
            tool_calls: t.steps.iter().filter(|s| s.kind == StepKind::Tool).count(),
            tokens: t.tokens,
            reply: first_line(&t.reply, 160),
            running: t.status == Status::Running,
        })
        .collect()
}

/// One turn with every payload.
pub fn turn(id: u64) -> Option<Turn> {
    let ring = ring().lock().ok()?;
    ring.iter().find(|t| t.id == id).cloned()
}

/// The turn currently running, if any — what the UI pins to the top and follows.
pub fn live() -> Option<Turn> {
    let ring = ring().lock().ok()?;
    ring.iter().rev().find(|t| t.status == Status::Running).cloned()
}

pub fn clear() {
    if let Ok(mut ring) = ring().lock() {
        ring.clear();
    }
    bump();
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let cut: String = line.chars().take(max).collect();
    format!("{cut}…")
}

/// Truncate on a char boundary and say how much was dropped, so a reader is
/// never misled into thinking they are looking at the whole payload.
fn cap(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut keep = max;
    while keep > 0 && !s.is_char_boundary(keep) {
        keep -= 1;
    }
    let dropped = s.len() - keep;
    s.truncate(keep);
    s.push_str(&format!("\n…[truncated {dropped} bytes]"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// The ring is process-global, so these tests must not interleave.
    fn guard() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fresh() {
        std::env::remove_var("KODA_TRACE");
        set_enabled(true);
        clear();
    }

    #[test]
    fn disabled_capture_is_a_no_op() {
        let _g = guard();
        clear();
        std::env::remove_var("KODA_TRACE");
        set_enabled(false);
        assert!(begin_turn("execute", "m", "e", "hi").is_none());
        // Every downstream call tolerates the None handle.
        let step = open_step(None, StepKind::Model, "m");
        assert!(step.is_none());
        finish_model(step, ModelCall::default());
        end_turn(None, Status::Ok, "x", 0);
        assert!(summaries().is_empty());
    }

    #[test]
    fn a_turn_with_model_and_tool_steps_reconstructs_in_order() {
        let _g = guard();
        fresh();
        let t = begin_turn("execute", "granite", "http://x/v1", "fix the bug");
        assert!(t.is_some());

        // Model call 1 asks for two tools.
        let s1 = open_step(t, StepKind::Model, "granite");
        append_sse(s1, b"data: {\"choices\":[]}\n");
        set_retries(s1, 1);
        finish_model(
            s1,
            ModelCall {
                request: "{\"model\":\"granite\"}".into(),
                text: "let me look".into(),
                tool_calls: vec!["read_file".into(), "search".into()],
                prompt_tokens: 100,
                completion_tokens: 12,
                ..Default::default()
            },
        );
        // Two tools, then a second model call, then a third tool.
        for name in ["read_file", "search"] {
            let s = open_step(t, StepKind::Tool, name);
            finish_tool(
                s,
                ToolStep {
                    name: name.into(),
                    args: "{}".into(),
                    ok: true,
                    summary: format!("{name} ok"),
                    approval: Some(Approval::Auto),
                    ..Default::default()
                },
            );
        }
        let s2 = open_step(t, StepKind::Model, "granite");
        finish_model(s2, ModelCall { text: "writing".into(), ..Default::default() });
        let s3 = open_step(t, StepKind::Tool, "write_file");
        finish_tool(
            s3,
            ToolStep {
                name: "write_file".into(),
                ok: true,
                approval: Some(Approval::Approved),
                diff: Some("- old\n+ new".into()),
                ..Default::default()
            },
        );
        end_turn(t, Status::Ok, "done", 1234);

        let full = turn(t.unwrap()).expect("turn is retrievable");
        assert_eq!(full.status, Status::Ok);
        assert_eq!(full.tokens, 1234);
        assert_eq!(full.steps.len(), 5);
        let kinds: Vec<StepKind> = full.steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                StepKind::Model,
                StepKind::Tool,
                StepKind::Tool,
                StepKind::Model,
                StepKind::Tool
            ]
        );
        assert_eq!(full.steps[0].seq, 0);
        assert_eq!(full.steps[4].seq, 4);
        // Payloads survive on the right steps.
        let m = full.steps[0].model.as_ref().unwrap();
        assert!(m.response.contains("data:"), "raw SSE kept: {:?}", m.response);
        assert_eq!(m.retries, 1);
        assert_eq!(m.tool_calls, vec!["read_file", "search"]);
        assert_eq!(full.steps[1].tool.as_ref().unwrap().name, "read_file");
        assert_eq!(
            full.steps[4].tool.as_ref().unwrap().approval,
            Some(Approval::Approved)
        );
        assert!(full.steps.iter().all(|s| !s.running));

        // The summary reports the shape without the payloads.
        let sums = summaries();
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].model_calls, 2);
        assert_eq!(sums[0].tool_calls, 3);
        assert_eq!(sums[0].input, "fix the bug");
        assert!(!sums[0].running);
    }

    #[test]
    fn a_running_turn_is_live_and_newest_first() {
        let _g = guard();
        fresh();
        let a = begin_turn("execute", "m", "e", "first");
        end_turn(a, Status::Ok, "ok", 1);
        let b = begin_turn("plan", "m", "e", "second");
        let open = open_step(b, StepKind::Model, "m");
        assert!(open.is_some());

        let running = live().expect("the open turn is live");
        assert_eq!(running.input, "second");
        assert!(running.steps[0].running);
        // Newest first in the rail.
        let sums = summaries();
        assert_eq!(sums[0].input, "second");
        assert_eq!(sums[1].input, "first");

        // Ending the turn closes any step left open.
        end_turn(b, Status::Cancelled, "", 0);
        assert!(live().is_none());
        let full = turn(b.unwrap()).unwrap();
        assert!(!full.steps[0].running);
        assert_eq!(full.status, Status::Cancelled);
    }

    #[test]
    fn compaction_is_a_visible_step() {
        let _g = guard();
        fresh();
        let t = begin_turn("execute", "m", "e", "long task");
        let s = open_step(t, StepKind::Compaction, "compaction");
        finish_compaction(s, 18_400, 4_200);
        end_turn(t, Status::Ok, "", 0);
        let full = turn(t.unwrap()).unwrap();
        assert_eq!(full.steps[0].kind, StepKind::Compaction);
        assert_eq!(full.steps[0].note.as_deref(), Some("18400 → 4200 tokens"));
    }

    #[test]
    fn payloads_are_truncated_not_unbounded() {
        let _g = guard();
        fresh();
        let t = begin_turn("execute", "m", "e", "x");
        let s = open_step(t, StepKind::Model, "m");
        // Stream far more than the cap; the step must stay bounded.
        for _ in 0..40 {
            append_sse(s, "y".repeat(4096).as_bytes());
        }
        finish_model(
            s,
            ModelCall {
                request: "r".repeat(CAP_REQUEST * 2),
                text: "t".repeat(CAP_FIELD * 2),
                ..Default::default()
            },
        );
        end_turn(t, Status::Ok, "", 0);
        let full = turn(t.unwrap()).unwrap();
        let m = full.steps[0].model.as_ref().unwrap();
        assert!(m.response.len() < CAP_FIELD + 64, "sse len {}", m.response.len());
        assert!(m.response.contains("truncated"));
        assert!(m.request.len() < CAP_REQUEST + 64);
        assert!(m.text.len() < CAP_FIELD + 64);
    }

    #[test]
    fn the_ring_drops_the_oldest_turns() {
        let _g = guard();
        fresh();
        for i in 0..MAX_TURNS + 10 {
            let t = begin_turn("execute", "m", "e", &format!("turn {i}"));
            end_turn(t, Status::Ok, "", 0);
        }
        let sums = summaries();
        assert_eq!(sums.len(), MAX_TURNS);
        // Newest first, oldest dropped.
        assert_eq!(sums[0].input, format!("turn {}", MAX_TURNS + 9));
        assert!(!sums.iter().any(|s| s.input == "turn 0"));
    }

    #[test]
    fn version_advances_so_the_ui_can_poll() {
        let _g = guard();
        fresh();
        let before = version();
        let t = begin_turn("execute", "m", "e", "x");
        assert!(version() > before);
        let mid = version();
        end_turn(t, Status::Ok, "", 0);
        assert!(version() > mid);
    }

    #[test]
    fn env_override_forces_tracing_on() {
        let _g = guard();
        set_enabled(false);
        std::env::set_var("KODA_TRACE", "1");
        assert!(enabled());
        std::env::remove_var("KODA_TRACE");
        assert!(!enabled());
    }
}
