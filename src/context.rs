//! Context curation: deciding what the model still needs to see.
//!
//! A local model with an 8k window spends most of it on tool results it can no
//! longer use — the first read of a file that has since been edited twice, the
//! directory listing from six steps back, the 400-line build log whose only
//! payload was "ok". The old rule (drop the oldest message when the total gets
//! big) frees that space by throwing away the *task statement*, because that is
//! what happens to be at the front.
//!
//! This layer decides instead of dropping, in escalating passes, and stops as
//! soon as the history fits:
//!
//! 1. **Supersede** — a `read_file` answered again later, or a file edited
//!    since it was read, is stale. Its body becomes one line saying so.
//! 2. **Squeeze** — what survives is truncated on a ladder: recent steps keep
//!    their detail, older ones keep their head and tail, and anything naming
//!    what the user just asked about keeps twice as much as anything that
//!    doesn't.
//! 3. **Drop** — only now are whole exchanges removed, oldest first, and never
//!    the opening request: the one message a coding agent cannot work without
//!    is the description of the job.
//!
//! It runs on a copy at send time. The real history, the session file and
//! `/compact` are untouched, so nothing here is unrecoverable: turn the
//! context up and the next request carries the detail again.

use crate::llm::{Message, Role};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Messages at the end that are never touched. This is the working set — the
/// current request, the call in flight, the result it is reading right now —
/// and curating it would break the step rather than shrink it.
const KEEP_RECENT: usize = 6;

/// Characters a tool result may keep in the newest curated generation. Halves
/// with each generation further back, down to `FLOOR`.
const ALLOWANCE: usize = 3_000;
const FLOOR: usize = 240;

/// Ordinary prose that carries no signal about which file the user means.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "you", "your", "can", "not",
    "are", "was", "were", "have", "has", "had", "but", "all", "any", "how", "why", "what", "when",
    "then", "than", "them", "they", "our", "out", "get", "please", "make", "add", "fix", "now",
    "code", "file", "files", "use", "using", "need", "want", "should", "would", "could", "there",
    "here", "one", "two", "also", "just", "like", "some", "more", "does", "did", "will", "its",
];

/// What curation did, for the log and the status line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Results made obsolete by a later call, reduced to a stub.
    pub superseded: usize,
    /// Results truncated to their allowance.
    pub squeezed: usize,
    /// Whole exchanges removed because the first two passes were not enough.
    pub dropped: usize,
    pub before: usize,
    pub after: usize,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.superseded + self.squeezed + self.dropped > 0
    }
}

/// The view of `history` to send this step, fitted to `budget` tokens.
///
/// Under budget the history is returned as it stands: curation costs nothing
/// and elides nothing until the window is actually short.
pub fn curate(history: &[Message], budget: usize) -> (Vec<Message>, Report) {
    let before = total(history);
    let mut report = Report {
        before,
        after: before,
        ..Report::default()
    };
    if before <= budget || history.len() <= KEEP_RECENT {
        return (history.to_vec(), report);
    }

    let mut blocks = blocks(history);
    // The tail is the working set; `pinned` is the opening request. Everything
    // between them is what curation is allowed to touch.
    let tail_start = tail_start(&blocks, budget);
    let focus = focus_terms(history);
    let stale = stale_calls(history);

    // Pass 1: results a later call already answered.
    for b in blocks.iter_mut().take(tail_start) {
        for m in b.msgs.iter_mut() {
            if m.role != Role::Tool {
                continue;
            }
            let Some(id) = m.tool_call_id.as_deref() else {
                continue;
            };
            let Some(reason) = stale.get(id) else {
                continue;
            };
            let len = m.content.as_deref().map(str::len).unwrap_or(0);
            // A stub that is longer than what it replaces is not a saving.
            if len <= FLOOR {
                continue;
            }
            m.content = Some(format!("[elided: {reason}]"));
            report.superseded += 1;
        }
    }
    report.after = total_blocks(&blocks);
    if report.after <= budget {
        return (flatten(blocks), report);
    }

    // Pass 2: the ladder. Generation 0 is the block just before the working
    // set; each step back gets half the room of the one after it.
    for (i, b) in blocks.iter_mut().take(tail_start).enumerate() {
        let generation = tail_start - 1 - i;
        let mut room = ALLOWANCE >> generation.min(8);
        if b.mentions(&focus) {
            room = room.saturating_mul(2);
        }
        for m in b.msgs.iter_mut() {
            // The user's own words are the task; they get far more room than a
            // tool result, and the opening request keeps all of it.
            let room = match m.role {
                Role::User => room.saturating_mul(4),
                Role::Assistant => room.saturating_mul(2),
                _ => room,
            };
            let room = room.max(FLOOR);
            let Some(text) = m.content.as_deref() else {
                continue;
            };
            if i == 0 && m.role == Role::User {
                continue;
            }
            if text.len() <= room {
                continue;
            }
            m.content = Some(clip(text, room));
            report.squeezed += 1;
        }
    }
    report.after = total_blocks(&blocks);
    if report.after <= budget {
        return (flatten(blocks), report);
    }

    // Pass 3: remove whole exchanges, oldest first, keeping the opening
    // request. Whole exchanges, because a tool result without the call that
    // asked for it is a protocol error, not a saving.
    let mut keep: Vec<bool> = vec![true; blocks.len()];
    let mut running = report.after;
    for i in 1..tail_start {
        if running <= budget {
            break;
        }
        running = running.saturating_sub(blocks[i].tokens());
        keep[i] = false;
        report.dropped += 1;
    }
    let kept: Vec<Block> = blocks
        .into_iter()
        .zip(keep)
        .filter_map(|(b, k)| k.then_some(b))
        .collect();
    report.after = total_blocks(&kept);
    (flatten(kept), report)
}

/// One exchange: a user turn, or an assistant turn with the tool results that
/// answered it. Curation moves whole blocks so a call and its results are never
/// separated.
struct Block {
    msgs: Vec<Message>,
}

impl Block {
    fn tokens(&self) -> usize {
        self.msgs.iter().map(|m| m.approx_tokens()).sum()
    }

    /// Does this exchange name anything the user just asked about?
    fn mentions(&self, focus: &HashSet<String>) -> bool {
        if focus.is_empty() {
            return false;
        }
        self.msgs.iter().any(|m| {
            let mut hay = String::new();
            if let Some(calls) = &m.tool_calls {
                for c in calls {
                    hay.push_str(&c.function.arguments);
                }
            }
            // Only the opening of a result: a term buried in line 900 of a
            // build log is a coincidence, not relevance.
            if let Some(c) = &m.content {
                hay.push_str(&c[..c.len().min(400)]);
            }
            let hay = hay.to_ascii_lowercase();
            focus.iter().any(|t| hay.contains(t.as_str()))
        })
    }
}

fn blocks(history: &[Message]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for m in history {
        // Tool results belong to the assistant turn that called them; anything
        // else opens a new exchange.
        if m.role == Role::Tool && !out.is_empty() {
            out.last_mut().expect("checked").msgs.push(m.clone());
        } else {
            out.push(Block {
                msgs: vec![m.clone()],
            });
        }
    }
    out
}

/// Index of the first block of the protected working set.
///
/// Bounded by tokens as well as by count: on a small window six recent
/// messages can be the whole budget, and protecting them would leave curation
/// nothing to work with and the request still too big. The step in flight is
/// protected whatever it costs -- squeezing the result the model is reading
/// right now breaks the step instead of shrinking it.
fn tail_start(blocks: &[Block], budget: usize) -> usize {
    let cap = (budget / 2).max(1);
    let mut msgs = 0;
    let mut tokens = 0;
    let mut i = blocks.len();
    while i > 0 {
        let b = &blocks[i - 1];
        if i < blocks.len() && (msgs >= KEEP_RECENT || tokens + b.tokens() > cap) {
            break;
        }
        msgs += b.msgs.len();
        tokens += b.tokens();
        i -= 1;
    }
    // Leave at least the opening request outside the tail, or there is nothing
    // curation is allowed to touch.
    i.max(1).min(blocks.len())
}

fn flatten(blocks: Vec<Block>) -> Vec<Message> {
    blocks.into_iter().flat_map(|b| b.msgs).collect()
}

fn total(msgs: &[Message]) -> usize {
    msgs.iter().map(|m| m.approx_tokens()).sum()
}

fn total_blocks(blocks: &[Block]) -> usize {
    blocks.iter().map(|b| b.tokens()).sum()
}

/// Tool calls whose results no longer describe the world, mapped to the reason.
///
/// Two ways a result goes stale: the same question was asked again later, or
/// the file it described was written since. Both are decided from the calls
/// themselves — no model, no guessing.
fn stale_calls(history: &[Message]) -> HashMap<String, String> {
    // (id, tool, subject) in order.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for m in history {
        let Some(list) = &m.tool_calls else { continue };
        for c in list {
            let args = c.args();
            let subject = subject(&args);
            calls.push((c.id.clone(), c.function.name.clone(), subject));
        }
    }

    let mut stale = HashMap::new();
    for (i, (id, tool, subject)) in calls.iter().enumerate() {
        if subject.is_empty() {
            continue;
        }
        for (later_tool, later_subject) in calls[i + 1..].iter().map(|(_, t, s)| (t, s)) {
            if later_subject != subject {
                continue;
            }
            if later_tool == tool {
                stale.insert(
                    id.clone(),
                    format!("a later `{tool}` of {subject} answered this again"),
                );
                break;
            }
            if is_read(tool) && is_write(later_tool) {
                stale.insert(
                    id.clone(),
                    format!("{subject} was changed by a later `{later_tool}`"),
                );
                break;
            }
        }
    }
    stale
}

/// What a call is *about* — the path it reads, the pattern it searches, the
/// symbol it looks up. Calls with no subject are never treated as superseded:
/// two `run_command`s are not the same command.
fn subject(args: &Value) -> String {
    for k in ["path", "file", "pattern", "query", "name", "dir"] {
        if let Some(v) = args.get(k).and_then(Value::as_str) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

fn is_read(tool: &str) -> bool {
    matches!(
        tool,
        "read_file" | "list_dir" | "search" | "find_files" | "codegraph"
    )
}

fn is_write(tool: &str) -> bool {
    matches!(tool, "edit_file" | "write_file" | "apply_patch")
}

/// The words worth keeping context for: identifiers and paths from the request
/// the agent is working on right now.
fn focus_terms(history: &[Message]) -> HashSet<String> {
    let Some(last) = history
        .iter()
        .rev()
        .find(|m| m.role == Role::User && m.content.is_some())
    else {
        return HashSet::new();
    };
    let text = last.content.as_deref().unwrap_or("").to_ascii_lowercase();
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '/' || c == '-'))
        .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
        .take(40)
        .map(str::to_string)
        .collect()
}

/// Keep the head and the tail, say how much went. A tool result's head names
/// what it is and its tail is usually the answer (the error, the last lines of
/// a build); the middle is what can go.
fn clip(text: &str, room: usize) -> String {
    let head_end = floor_char(text, room * 3 / 5);
    let tail_start = ceil_char(text, text.len().saturating_sub(room - room * 3 / 5));
    if tail_start <= head_end {
        return text.to_string();
    }
    let elided = text[head_end..tail_start].lines().count();
    format!(
        "{}\n… {elided} lines elided to fit the context window …\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// Largest char boundary at or below `n`, so slicing never splits a character.
fn floor_char(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// Smallest char boundary at or above `n`.
fn ceil_char(s: &str, n: usize) -> usize {
    let mut n = n.min(s.len());
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ToolCall;

    /// A read of `path` and the big result that came back.
    fn read(id: &str, path: &str, body: &str) -> [Message; 2] {
        [
            Message::assistant_calls(
                None,
                vec![ToolCall::new(
                    id.into(),
                    "read_file".into(),
                    format!("{{\"path\":\"{path}\"}}"),
                )],
            ),
            Message::tool(id, "read_file", body),
        ]
    }

    fn edit(id: &str, path: &str) -> [Message; 2] {
        [
            Message::assistant_calls(
                None,
                vec![ToolCall::new(
                    id.into(),
                    "edit_file".into(),
                    format!("{{\"path\":\"{path}\"}}"),
                )],
            ),
            Message::tool(id, "edit_file", "edited"),
        ]
    }

    fn big(tag: &str) -> String {
        format!("{tag}\n{}", "x".repeat(8_000))
    }

    /// Everything the model is given must still be a legal conversation: every
    /// tool result answers a call that is still there.
    fn assert_well_formed(msgs: &[Message]) {
        let mut ids: HashSet<&str> = HashSet::new();
        for m in msgs {
            if let Some(calls) = &m.tool_calls {
                for c in calls {
                    ids.insert(c.id.as_str());
                }
            }
            if m.role == Role::Tool {
                let id = m.tool_call_id.as_deref().unwrap_or("");
                assert!(ids.contains(id), "orphaned tool result {id}");
            }
        }
    }

    #[test]
    fn under_budget_nothing_is_touched() {
        let history = vec![
            Message::user("add a flag"),
            Message::assistant("done"),
            Message::user("thanks"),
        ];
        let (out, report) = curate(&history, 100_000);
        assert_eq!(out.len(), history.len());
        assert!(!report.changed());
        assert_eq!(out[1].content.as_deref(), Some("done"));
    }

    /// The point of the layer: a stale read costs one line instead of 2k
    /// tokens, and the task survives.
    #[test]
    fn a_reread_file_keeps_only_its_latest_result() {
        let mut history = vec![Message::user("refactor src/agent.rs")];
        history.extend(read("a", "src/agent.rs", &big("first")));
        history.extend(read("b", "src/other.rs", &big("other")));
        history.extend(read("c", "src/agent.rs", &big("second")));
        history.push(Message::user("now what"));
        history.push(Message::assistant("working"));

        let (out, report) = curate(&history, 3_000);
        assert!(report.superseded >= 1, "{report:?}");
        assert_eq!(report.dropped, 0, "elision was enough: {report:?}");
        let stub = out
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("a"))
            .expect("the superseded result is still in place");
        assert!(
            stub.content.as_deref().unwrap_or("").contains("elided"),
            "{stub:?}"
        );
        assert!(stub
            .content
            .as_deref()
            .unwrap_or("")
            .contains("src/agent.rs"));
        // The later read of the same file is the live one and keeps its body.
        let live = out
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c"))
            .expect("kept");
        assert!(live.content.as_deref().unwrap_or("").starts_with("second"));
        assert_eq!(out[0].content.as_deref(), Some("refactor src/agent.rs"));
        assert_well_formed(&out);
    }

    /// A file read before it was edited describes a file that no longer exists
    /// in that form. Keeping it verbatim is worse than saying so.
    #[test]
    fn a_read_is_stale_once_the_file_is_edited() {
        let mut history = vec![Message::user("fix the parser")];
        history.extend(read("a", "src/parse.rs", &big("before")));
        history.extend(edit("b", "src/parse.rs"));
        history.push(Message::user("keep going"));
        history.push(Message::assistant("ok"));
        history.push(Message::user("and now"));
        history.push(Message::assistant("ok"));

        let (out, report) = curate(&history, 2_000);
        assert!(report.superseded >= 1, "{report:?}");
        let stub = out
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("a"))
            .expect("kept");
        assert!(
            stub.content
                .as_deref()
                .unwrap_or("")
                .contains("changed by a later `edit_file`"),
            "{stub:?}"
        );
        assert_well_formed(&out);
    }

    /// Two `run_command`s are not the same command, and a result the agent is
    /// reading right now is not old news.
    #[test]
    fn unrelated_results_and_the_working_set_survive() {
        let mut history = vec![Message::user("build it")];
        for i in 0..6 {
            let id = format!("r{i}");
            history.push(Message::assistant_calls(
                None,
                vec![ToolCall::new(
                    id.clone(),
                    "run_command".into(),
                    "{\"command\":\"cargo build\"}".into(),
                )],
            ));
            history.push(Message::tool(&id, "run_command", big("log")));
        }
        let (out, report) = curate(&history, 2_000);
        assert_eq!(report.superseded, 0, "no subject, no supersession");
        // The last exchange is the working set and must be intact.
        let last = out.last().expect("non-empty");
        assert!(last
            .content
            .as_deref()
            .unwrap_or("")
            .starts_with("log\nxxx"));
        assert!(report.after < report.before, "{report:?}");
        assert_well_formed(&out);
    }

    /// When elision is not enough, exchanges go — but never the request that
    /// says what the session is for.
    #[test]
    fn dropping_starts_after_the_opening_request() {
        let mut history = vec![Message::user("port the CLI to clap 4")];
        for i in 0..12 {
            history.extend(read(
                &format!("f{i}"),
                &format!("src/f{i}.rs"),
                &big("body"),
            ));
        }
        history.push(Message::user("continue"));
        let (out, report) = curate(&history, 1_500);
        assert!(report.dropped > 0, "{report:?}");
        assert_eq!(out[0].content.as_deref(), Some("port the CLI to clap 4"));
        assert_eq!(out.last().unwrap().content.as_deref(), Some("continue"));
        assert!(report.after <= report.before);
        assert_well_formed(&out);
    }

    #[test]
    fn clipping_never_splits_a_character() {
        let text = "é".repeat(4_000);
        let out = clip(&text, 500);
        assert!(out.contains("elided"));
        assert!(out.len() < text.len());
        // Round-tripping through str proves every boundary was legal.
        assert!(out.chars().all(|c| c == 'é' || c.is_ascii() || c == '…'));
    }

    #[test]
    fn focus_terms_come_from_the_live_request_not_the_first_one() {
        let history = vec![
            Message::user("set up the repo"),
            Message::assistant("done"),
            Message::user("now fix src/theme.rs and the palette"),
        ];
        let terms = focus_terms(&history);
        assert!(terms.contains("src/theme.rs"), "{terms:?}");
        assert!(terms.contains("palette"));
        assert!(!terms.contains("the"), "stopwords are noise: {terms:?}");
    }
}
