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
    // Size is the only reason to curate. A short history can still be far too
    // big -- two file reads is two messages and can be the whole window.
    if before <= budget {
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
            let Some(text) = m.content.as_deref() else {
                continue;
            };
            let room = if i == 0 && m.role == Role::User {
                // The opening request is the task and is kept whole -- unless
                // it is itself a pasted log big enough to be the problem, in
                // which case it may still have half the window, head and tail.
                (budget * 2).max(FLOOR)
            } else {
                // The user's own words are worth more room than a tool result,
                // and an assistant's reasoning more than a build log.
                match m.role {
                    Role::User => room.saturating_mul(4),
                    Role::Assistant => room.saturating_mul(2),
                    _ => room,
                }
                .max(FLOOR)
            };
            if let Some(clipped) = clip(text, room) {
                m.content = Some(clipped);
                report.squeezed += 1;
            }
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
            // build log is a coincidence, not relevance. Sliced on a character
            // boundary -- tool results are full of UTF-8 and `&c[..400]` would
            // panic in the middle of one.
            if let Some(c) = &m.content {
                hay.push_str(&c[..floor_char(c, 400)]);
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
/// Two ways a result goes stale: the identical question was asked again later,
/// or the file it described was written since. Both are decided from the calls
/// themselves -- no model, no guessing.
fn stale_calls(history: &[Message]) -> HashMap<String, String> {
    // (id, tool, whole call, path it is about) in order.
    let mut calls: Vec<(String, String, String, String)> = Vec::new();
    for m in history {
        let Some(list) = &m.tool_calls else { continue };
        for c in list {
            let args = c.args();
            calls.push((
                c.id.clone(),
                c.function.name.clone(),
                canon(&args),
                path_of(&args),
            ));
        }
    }

    let mut stale = HashMap::new();
    for (i, (id, tool, canon, path)) in calls.iter().enumerate() {
        for (later_tool, later_canon, later_path) in
            calls[i + 1..].iter().map(|(_, t, c, p)| (t, c, p))
        {
            // The same call, made again. Its answer replaced this one --
            // whether that is a re-read of a file or a second `cargo build`.
            if later_tool == tool && later_canon == canon {
                let what = if path.is_empty() {
                    format!("a later `{tool}` asked this again")
                } else {
                    format!("a later `{tool}` of {path} answered this again")
                };
                stale.insert(id.clone(), what);
                break;
            }
            // Read before the file was written: it describes a file that no
            // longer exists in that form.
            if !path.is_empty() && path == later_path && is_read(tool) && is_write(later_tool) {
                stale.insert(
                    id.clone(),
                    format!("{path} was changed by a later `{later_tool}`"),
                );
                break;
            }
        }
    }
    stale
}

/// A call's arguments, canonicalized, so "the same call again" is a string
/// comparison. Keyed on *all* of them on purpose: two `search`es of the same
/// directory for different patterns are different questions, and matching on
/// the path alone would throw away an answer the model still needs.
fn canon(args: &Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    let mut parts: Vec<String> = obj
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={other}"),
        })
        .collect();
    parts.sort();
    let joined = parts.join("\u{1}");
    joined.chars().take(400).collect()
}

/// The file a call is about, when it names one.
fn path_of(args: &Value) -> String {
    for k in ["path", "file"] {
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
fn clip(text: &str, room: usize) -> Option<String> {
    if text.len() <= room {
        return None;
    }
    let head_end = floor_char(text, room * 3 / 5);
    let tail_start = ceil_char(text, text.len().saturating_sub(room - room * 3 / 5));
    if tail_start <= head_end {
        return None;
    }
    let elided = text[head_end..tail_start].lines().count();
    let out = format!(
        "{}\n… {elided} lines elided to fit the context window …\n{}",
        &text[..head_end],
        &text[tail_start..]
    );
    // The marker has a size of its own. Clipping the last few characters off a
    // message costs more than it saves, and a "saving" that grows the request
    // is the one thing this layer must never do.
    (out.len() < text.len()).then_some(out)
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

    /// A command run twice makes its first log old news -- that is the single
    /// biggest win here, a build log being the largest thing in most windows.
    #[test]
    fn a_repeated_command_supersedes_its_earlier_log() {
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
        assert!(report.superseded >= 4, "{report:?}");
        // The last exchange is the working set: the log the model is reading
        // right now is intact.
        let last = out.last().expect("non-empty");
        assert!(last
            .content
            .as_deref()
            .unwrap_or("")
            .starts_with("log\nxxx"));
        assert!(report.after < report.before, "{report:?}");
        assert_well_formed(&out);
    }

    /// Different questions to the same tool are not each other's answers. Two
    /// searches of the same directory differ only in their pattern, so keying
    /// supersession on the path would throw away a live result.
    #[test]
    fn different_searches_of_one_directory_are_not_conflated() {
        let mut history = vec![Message::user("audit the error paths")];
        for (i, pattern) in ["unwrap", "expect", "panic!"].iter().enumerate() {
            let id = format!("s{i}");
            history.push(Message::assistant_calls(
                None,
                vec![ToolCall::new(
                    id.clone(),
                    "search".into(),
                    format!("{{\"pattern\":\"{pattern}\",\"path\":\"src\"}}"),
                )],
            ));
            history.push(Message::tool(&id, "search", big(pattern)));
        }
        history.push(Message::user("well?"));
        let (_, report) = curate(&history, 3_000);
        assert_eq!(
            report.superseded, 0,
            "distinct patterns are distinct questions: {report:?}"
        );
    }

    /// Tool results are full of UTF-8. Relevance scoring reads the head of one,
    /// and a byte-index slice would panic in the middle of a character.
    #[test]
    fn scoring_a_result_full_of_utf8_does_not_panic() {
        let mut history = vec![Message::user("check src/théme.rs")];
        history.extend(read(
            "a",
            "src/théme.rs",
            &"héllo — wörld ✓\n".repeat(2_000),
        ));
        history.extend(read("b", "src/other.rs", &"→".repeat(9_000)));
        history.push(Message::user("continue"));
        let (out, report) = curate(&history, 1_200);
        assert!(report.changed(), "{report:?}");
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
        let out = clip(&text, 500).expect("a 4k message clips to 500");
        assert!(out.contains("elided"));
        assert!(out.len() < text.len());
        // Round-tripping through str proves every boundary was legal.
        assert!(out.chars().all(|c| c == 'é' || c.is_ascii() || c == '…'));
        // Clipping to nearly the original length is not worth the marker.
        assert_eq!(clip(&text, text.len() - 4), None);
    }

    /// A pasted log as the opening message is the task *and* the problem. It
    /// keeps its head and tail rather than being sent whole or thrown away.
    #[test]
    fn a_huge_opening_request_keeps_its_head_and_tail() {
        let history = vec![
            Message::user(format!("summarise this log\n{}", "LINE\n".repeat(4_000))),
            Message::assistant("reading"),
            Message::user("well?"),
        ];
        let (out, report) = curate(&history, 1_000);
        assert_eq!(report.squeezed, 1, "{report:?}");
        let first = out[0].content.as_deref().unwrap_or("");
        assert!(first.starts_with("summarise this log"), "{first:.60}");
        assert!(first.contains("elided to fit the context window"));
        assert!(
            first.len() < 4_000,
            "clipped to the window: {}",
            first.len()
        );
        assert_eq!(out.len(), history.len(), "nothing was dropped");
    }

    /// Every request the model gets goes through here, so the invariants have
    /// to hold for shapes no hand-written test thought of: legal conversation
    /// out, never bigger than what went in, and no panic on any of it.
    #[test]
    fn any_history_curates_to_something_legal_and_smaller() {
        // A tiny LCG: deterministic, reproducible, no dependency.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..300 {
            let len = (next() % 14) as usize + 1;
            let mut history: Vec<Message> = Vec::new();
            let mut id = 0;
            for _ in 0..len {
                // Bodies of wildly different sizes, some multi-byte.
                let body = match next() % 4 {
                    0 => "ok".to_string(),
                    1 => "é→✓ ".repeat((next() % 900) as usize + 1),
                    2 => "x".repeat((next() % 9_000) as usize + 1),
                    _ => format!("line {}\n", next()).repeat((next() % 200) as usize + 1),
                };
                match next() % 5 {
                    0 => history.push(Message::user(body)),
                    1 => history.push(Message::assistant(body)),
                    _ => {
                        id += 1;
                        let cid = format!("c{id}");
                        let tool = ["read_file", "search", "run_command", "edit_file"]
                            [(next() % 4) as usize];
                        let path = format!("src/f{}.rs", next() % 3);
                        history.push(Message::assistant_calls(
                            None,
                            vec![ToolCall::new(
                                cid.clone(),
                                tool.into(),
                                format!("{{\"path\":\"{path}\"}}"),
                            )],
                        ));
                        history.push(Message::tool(&cid, tool, body));
                    }
                }
            }
            let budget = [0usize, 1, 64, 800, 4_000, 100_000][(next() % 6) as usize];
            let (out, report) = curate(&history, budget);
            assert_well_formed(&out);
            assert!(out.len() <= history.len(), "case {case}: grew a message");
            assert!(
                report.after <= report.before,
                "case {case}: grew: {report:?}"
            );
            // Whatever else goes, the job description stays.
            if history[0].role == Role::User {
                assert_eq!(out[0].role, Role::User, "case {case}: lost the request");
            }
        }
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
