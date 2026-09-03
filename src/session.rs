//! Session persistence.
//!
//! Append-only JSONL under `<project>/.koda/sessions/`. One header line, then
//! one line per message as the turn completes.
//!
//! JSONL rather than a database on purpose: appending is a single write with no
//! schema migration and no dependency, a truncated file from a crash still reads
//! back to the last complete line, and you can inspect a session with `tail`.
//! At single-user local scale nothing here needs an index.

use crate::llm::{Message, Role};
use crate::{tel_debug, tel_warn};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub id: String,
    /// Unix seconds.
    pub started: u64,
    pub model: String,
    pub endpoint: String,
    pub cwd: String,
}

/// One line of the file: either the header or a message.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Record {
    Header(Header),
    Msg(Message),
}

/// A session as shown in the picker.
#[derive(Debug, Clone)]
pub struct Summary {
    pub header: Header,
    pub path: PathBuf,
    pub messages: usize,
    /// First user message, for recognising the session.
    pub title: String,
    pub modified: u64,
}

pub fn dir(root: &Path) -> PathBuf {
    root.join(".koda").join("sessions")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Sortable, readable, and unique on one machine. The seconds keep it sortable;
/// a process-wide atomic counter makes two ids created in the same second
/// distinct (the old subsec-nanos mod 10000 could collide ~1/10000).
fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let secs = now();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{secs:010}-{:04}-{:04}", nanos % 10_000, seq % 10_000)
}

pub struct Store {
    path: PathBuf,
    /// How many history messages are already on disk.
    written: usize,
    header: Header,
    enabled: bool,
}

impl Store {
    /// Start a new session file. Failure to open is not fatal: losing history is
    /// better than refusing to run.
    pub fn create(root: &Path, model: &str, endpoint: &str) -> Self {
        let header = Header {
            id: new_id(),
            started: now(),
            model: model.to_string(),
            endpoint: endpoint.to_string(),
            cwd: root.display().to_string(),
        };
        let path = dir(root).join(format!("{}.jsonl", header.id));
        let mut store = Self {
            path,
            written: 0,
            header: header.clone(),
            enabled: true,
        };
        if let Err(e) = store.write_header() {
            tel_warn!("session", format!("cannot start session file: {e}"));
            store.enabled = false;
        }
        store
    }

    /// Continue writing to an existing file.
    pub fn reopen(path: PathBuf, header: Header, written: usize) -> Self {
        Self {
            path,
            written,
            header,
            enabled: true,
        }
    }

    pub fn id(&self) -> &str {
        &self.header.id
    }

    fn write_header(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(&Record::Header(self.header.clone()))?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Append whatever part of `history` is not on disk yet.
    pub fn append(&mut self, history: &[Message]) {
        if !self.enabled || history.len() <= self.written {
            return;
        }
        let result = (|| -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            for m in &history[self.written..] {
                let line = serde_json::to_string(&Record::Msg(m.clone()))?;
                writeln!(f, "{line}")?;
            }
            f.flush()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                tel_debug!(
                    "session",
                    "appended",
                    "messages" => history.len() - self.written,
                    "id" => self.header.id,
                );
                self.written = history.len();
            }
            Err(e) => {
                tel_warn!("session", format!("append failed: {e}"));
                self.enabled = false;
            }
        }
    }

    /// Called after the history is rewritten wholesale (compaction, /clear).
    pub fn rewrite(&mut self, history: &[Message]) {
        if !self.enabled {
            return;
        }
        self.written = 0;
        let _ = std::fs::remove_file(&self.path);
        if self.write_header().is_err() {
            self.enabled = false;
            return;
        }
        self.append(history);
    }
}

/// Read a session file back. Malformed trailing lines are skipped, because a
/// crash mid-write is exactly when you most want the rest.
pub fn read(path: &Path) -> Result<(Header, Vec<Message>)> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut header = None;
    let mut messages = Vec::new();
    for line in BufReader::new(f).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(Record::Header(h)) => header = Some(h),
            Ok(Record::Msg(m)) => messages.push(m),
            Err(_) => continue,
        }
    }
    let header = header.context("session file has no header")?;
    Ok((header, messages))
}

/// Newest first.
pub fn list(root: &Path) -> Vec<Summary> {
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().map(|x| x != "jsonl").unwrap_or(true) {
            continue;
        }
        let Ok((header, messages)) = read(&path) else {
            continue;
        };
        // A session with no exchange is noise in the picker.
        if messages.is_empty() {
            continue;
        }
        let title = messages
            .iter()
            .find(|m| m.role == Role::User)
            .and_then(|m| m.content.clone())
            .map(|c| {
                let one_line = c.split('\n').next().unwrap_or("").trim().to_string();
                one_line.chars().take(70).collect::<String>()
            })
            .unwrap_or_else(|| "(no prompt)".into());
        let modified = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(header.started);
        out.push(Summary {
            header,
            path,
            messages: messages.len(),
            title,
            modified,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

pub fn latest(root: &Path) -> Option<Summary> {
    list(root).into_iter().next()
}

/// Copy an existing session into a new file with a fresh id, so the user can
/// branch a conversation without disturbing the original. Returns the new
/// session's path.
///
/// A fork is a byte copy of the JSONL with the header's `id` rewritten — the
/// history is identical, only the identity differs, so resuming the fork
/// continues from the same point while the original stays put.
pub fn fork(src: &Path, root: &Path) -> Result<PathBuf> {
    let (mut header, messages) = read(src)?;
    let new = new_id();
    header.id = new.clone();
    let dest = dir(root).join(format!("{new}.jsonl"));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut out = String::new();
    out.push_str(&serde_json::to_string(&Record::Header(header))?);
    out.push('\n');
    for m in &messages {
        out.push_str(&serde_json::to_string(&Record::Msg(m.clone()))?);
        out.push('\n');
    }
    std::fs::write(&dest, out).with_context(|| format!("writing {}", dest.display()))?;
    tel_debug!("session", "forked", "from" => src.display(), "to" => dest.display());
    Ok(dest)
}

/// Full-text search across every saved session in this project.
///
/// Scans message content case-insensitively and returns the matching sessions
/// newest-first, each paired with the number of messages that matched — enough
/// for a picker to show "3 hits" beside the title. At single-user local scale a
/// linear scan is instant and needs no index (the same reasoning that makes the
/// store plain JSONL).
pub fn search(root: &Path, query: &str) -> Vec<(Summary, usize)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(Summary, usize)> = Vec::new();
    for summary in list(root) {
        let Ok((_, messages)) = read(&summary.path) else {
            continue;
        };
        let hits = messages
            .iter()
            .filter(|m| {
                m.content
                    .as_deref()
                    .map(|c| c.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
            .count();
        if hits > 0 {
            out.push((summary, hits));
        }
    }
    out
}

/// "just now", "12m ago", "3h ago", "5d ago" — relative is what you actually
/// want when picking a session to continue.
pub fn ago(then: u64) -> String {
    let now = now();
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("koda-session-{tag}"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trips_a_conversation() {
        let root = tmp("roundtrip");
        let mut store = Store::create(&root, "m1", "http://x/v1");
        let history = vec![Message::user("fix the bug"), Message::assistant("done")];
        store.append(&history);

        let found = list(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].messages, 2);
        assert_eq!(found[0].title, "fix the bug");

        let (header, messages) = read(&found[0].path).unwrap();
        assert_eq!(header.model, "m1");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_deref(), Some("fix the bug"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn append_only_writes_the_new_tail() {
        let root = tmp("tail");
        let mut store = Store::create(&root, "m", "e");
        let mut history = vec![Message::user("one")];
        store.append(&history);
        store.append(&history); // no-op
        history.push(Message::assistant("two"));
        store.append(&history);

        let (_, messages) = read(&store.path).unwrap();
        assert_eq!(messages.len(), 2, "a message was duplicated or lost");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_truncated_line_does_not_lose_the_rest() {
        let root = tmp("truncated");
        let mut store = Store::create(&root, "m", "e");
        store.append(&[Message::user("kept")]);
        // Simulate a crash mid-write.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&store.path)
            .unwrap();
        f.write_all(b"{\"t\":\"msg\",\"role\":\"assi").unwrap();
        drop(f);

        let (_, messages) = read(&store.path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("kept"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rewrite_replaces_history_after_compaction() {
        let root = tmp("rewrite");
        let mut store = Store::create(&root, "m", "e");
        store.append(&[Message::user("a"), Message::assistant("b")]);
        store.rewrite(&[Message::user("summary")]);
        let (_, messages) = read(&store.path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("summary"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn empty_sessions_are_hidden_and_newest_is_first() {
        let root = tmp("order");
        let _empty = Store::create(&root, "m", "e"); // never appended
        assert!(list(&root).is_empty(), "empty session should not be listed");

        let mut a = Store::create(&root, "m", "e");
        a.append(&[Message::user("older")]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut b = Store::create(&root, "m", "e");
        b.append(&[Message::user("newer")]);

        let found = list(&root);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].title, "newer", "newest must sort first");
        assert_eq!(latest(&root).unwrap().title, "newer");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(now()), "just now");
        assert_eq!(ago(now().saturating_sub(120)), "2m ago");
        assert_eq!(ago(now().saturating_sub(7200)), "2h ago");
        assert_eq!(ago(now().saturating_sub(200_000)), "2d ago");
    }

    #[test]
    fn fork_copies_history_under_a_new_id() {
        let root = tmp("fork");
        let mut store = Store::create(&root, "m1", "http://x/v1");
        store.append(&[
            Message::user("original question"),
            Message::assistant("answer"),
        ]);
        let src = store.path.clone();

        let dest = fork(&src, &root).unwrap();
        assert_ne!(dest, src, "fork must get a new file");

        let (orig_h, orig_msgs) = read(&src).unwrap();
        let (fork_h, fork_msgs) = read(&dest).unwrap();
        assert_ne!(fork_h.id, orig_h.id, "fork must have a distinct id");
        assert_eq!(
            fork_msgs.len(),
            orig_msgs.len(),
            "history must be identical"
        );
        assert_eq!(fork_msgs[0].content, orig_msgs[0].content);
        // Both sessions are now listed.
        assert_eq!(list(&root).len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn search_finds_sessions_by_content() {
        let root = tmp("search");
        let mut a = Store::create(&root, "m", "e");
        a.append(&[
            Message::user("fix the DISCOUNT bug"),
            Message::assistant("done"),
        ]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut b = Store::create(&root, "m", "e");
        b.append(&[Message::user("add pagination")]);

        // Case-insensitive substring match on content.
        let hits = search(&root, "discount");
        assert_eq!(hits.len(), 1, "only one session mentions discount");
        assert_eq!(hits[0].0.title, "fix the DISCOUNT bug");
        assert_eq!(hits[0].1, 1, "one message matched");

        // A term in neither session finds nothing; empty query is inert.
        assert!(search(&root, "kubernetes").is_empty());
        assert!(search(&root, "   ").is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
