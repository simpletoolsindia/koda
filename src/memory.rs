//! Project memory: what the agent learned here, carried into later sessions.
//!
//! Scope is deliberately narrow, because "the agent learns" is easy to say and
//! easy to get wrong. Two things are recorded, both cheap and both verifiable:
//!
//! 1. **Facts the agent chose to remember** — written explicitly through the
//!    `remember` tool. Nothing is inferred behind the user's back.
//! 2. **Command outcomes** — which commands succeeded here and which failed, so
//!    the next session runs `just test` rather than guessing `npm test`.
//!
//! Everything lives in one readable file at `<project>/.koda/memory.md`, which
//! the user can edit or delete. There is no hidden state and no model in the
//! loop: a summary the user cannot inspect is a liability, not a feature.

use crate::{tel_debug, tel_info};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const MAX_NOTES: usize = 60;
const MAX_COMMANDS: usize = 40;

#[derive(Debug, Default, Clone)]
pub struct Memory {
    /// Free-form facts, newest last.
    pub notes: Vec<String>,
    /// command -> (successes, failures)
    pub commands: BTreeMap<String, (u32, u32)>,
    dirty: bool,
}

pub fn path(root: &Path) -> PathBuf {
    root.join(".koda").join("memory.md")
}

impl Memory {
    pub fn load(root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path(root)) else {
            return Self::default();
        };
        let mut m = Self::default();
        let mut section = "";
        for line in text.lines() {
            let t = line.trim();
            if let Some(h) = t.strip_prefix("## ") {
                section = match h.trim().to_ascii_lowercase().as_str() {
                    "notes" => "notes",
                    "commands" => "commands",
                    _ => "",
                };
                continue;
            }
            let Some(item) = t.strip_prefix("- ") else {
                continue;
            };
            match section {
                "notes" => m.notes.push(item.to_string()),
                "commands" => {
                    // `just test` — 4 ok, 1 failed
                    if let Some((cmd, stats)) = item.rsplit_once(" — ") {
                        let cmd = cmd.trim().trim_matches('`').to_string();
                        let mut ok = 0;
                        let mut fail = 0;
                        for part in stats.split(',') {
                            let part = part.trim();
                            let n: u32 = part
                                .split_whitespace()
                                .next()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            if part.contains("ok") {
                                ok = n;
                            } else if part.contains("fail") {
                                fail = n;
                            }
                        }
                        if !cmd.is_empty() {
                            m.commands.insert(cmd, (ok, fail));
                        }
                    }
                }
                _ => {}
            }
        }
        tel_debug!(
            "memory",
            "loaded",
            "notes" => m.notes.len(),
            "commands" => m.commands.len(),
        );
        m
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty() && self.commands.is_empty()
    }

    /// Record a fact. Returns false if it was already known.
    pub fn remember(&mut self, note: &str) -> bool {
        let note = note.trim();
        if note.is_empty() {
            return false;
        }
        let normalized = note.to_ascii_lowercase();
        if self
            .notes
            .iter()
            .any(|n| n.to_ascii_lowercase() == normalized)
        {
            return false;
        }
        self.notes.push(note.to_string());
        if self.notes.len() > MAX_NOTES {
            self.notes.remove(0);
        }
        self.dirty = true;
        true
    }

    pub fn forget(&mut self, needle: &str) -> usize {
        let needle = needle.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return 0;
        }
        let before = self.notes.len();
        self.notes
            .retain(|n| !n.to_ascii_lowercase().contains(&needle));
        let removed = before - self.notes.len();
        if removed > 0 {
            self.dirty = true;
        }
        removed
    }

    /// Note how a command turned out here.
    pub fn record_command(&mut self, command: &str, ok: bool) {
        // Only the head of a command line is worth keeping; arguments vary.
        let key: String = command
            .trim()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        if key.is_empty() {
            return;
        }
        let entry = self.commands.entry(key).or_insert((0, 0));
        if ok {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
        if self.commands.len() > MAX_COMMANDS {
            // Drop whatever has the least evidence behind it.
            if let Some(worst) = self
                .commands
                .iter()
                .min_by_key(|(_, (o, f))| o + f)
                .map(|(k, _)| k.clone())
            {
                self.commands.remove(&worst);
            }
        }
        self.dirty = true;
    }

    /// The part worth putting in a system prompt: facts, and the commands that
    /// actually work here.
    pub fn brief(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\nWhat you learned in this project before:\n");
        for n in self.notes.iter().rev().take(20).collect::<Vec<_>>().iter().rev() {
            let _ = writeln!(out, "- {n}");
        }
        let mut working: Vec<(&String, u32)> = self
            .commands
            .iter()
            .filter(|(_, (ok, fail))| *ok > 0 && ok >= fail)
            .map(|(k, (ok, _))| (k, *ok))
            .collect();
        working.sort_by_key(|(_, ok)| std::cmp::Reverse(*ok));
        if !working.is_empty() {
            out.push_str("Commands that work here: ");
            let list: Vec<String> = working
                .iter()
                .take(8)
                .map(|(c, _)| format!("`{c}`"))
                .collect();
            let _ = writeln!(out, "{}", list.join(", "));
        }
        let broken: Vec<&String> = self
            .commands
            .iter()
            .filter(|(_, (ok, fail))| *fail > 0 && *ok == 0)
            .map(|(k, _)| k)
            .take(5)
            .collect();
        if !broken.is_empty() {
            let list: Vec<String> = broken.iter().map(|c| format!("`{c}`")).collect();
            let _ = writeln!(out, "Commands that failed here: {}", list.join(", "));
        }
        out
    }

    /// Persist if anything changed. The file is markdown so the user can read
    /// and edit exactly what the agent believes.
    pub fn save(&mut self, root: &Path) -> std::io::Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        let file = path(root);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::from(
            "# koda memory\n\nWritten by koda as it works in this project. Edit or delete \
             freely — it is read at startup and nothing else depends on it.\n",
        );
        if !self.notes.is_empty() {
            text.push_str("\n## Notes\n");
            for n in &self.notes {
                let _ = writeln!(text, "- {n}");
            }
        }
        if !self.commands.is_empty() {
            text.push_str("\n## Commands\n");
            for (cmd, (ok, fail)) in &self.commands {
                let _ = writeln!(text, "- `{cmd}` — {ok} ok, {fail} failed");
            }
        }
        std::fs::write(&file, text)?;
        self.dirty = false;
        tel_info!("memory", "saved", "file" => file.display());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("koda-mem-{tag}"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn remembers_and_deduplicates() {
        let mut m = Memory::default();
        assert!(m.remember("tests run with just test"));
        assert!(!m.remember("Tests run with just test"), "case-insensitive dedupe");
        assert!(!m.remember("   "));
        assert_eq!(m.notes.len(), 1);
    }

    #[test]
    fn round_trips_through_the_file() {
        let d = dir("roundtrip");
        let mut m = Memory::default();
        m.remember("migrations live in db/migrate");
        m.record_command("just test", true);
        m.record_command("just test", true);
        m.record_command("npm test", false);
        assert!(m.save(&d).unwrap());
        // A second save with no changes is a no-op.
        assert!(!m.save(&d).unwrap());

        let loaded = Memory::load(&d);
        assert_eq!(loaded.notes, vec!["migrations live in db/migrate"]);
        assert_eq!(loaded.commands.get("just test"), Some(&(2, 0)));
        assert_eq!(loaded.commands.get("npm test"), Some(&(0, 1)));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn brief_separates_working_from_broken_commands() {
        let mut m = Memory::default();
        m.record_command("just test", true);
        m.record_command("npm test", false);
        m.remember("the API lives in src/api");
        let b = m.brief();
        assert!(b.contains("the API lives in src/api"), "{b}");
        assert!(b.contains("Commands that work here: `just test`"), "{b}");
        assert!(b.contains("failed here: `npm test`"), "{b}");
    }

    #[test]
    fn empty_memory_contributes_nothing_to_the_prompt() {
        assert!(Memory::default().brief().is_empty());
    }

    #[test]
    fn forget_removes_matching_notes() {
        let mut m = Memory::default();
        m.remember("uses yarn");
        m.remember("uses postgres");
        assert_eq!(m.forget("yarn"), 1);
        assert_eq!(m.notes, vec!["uses postgres"]);
        assert_eq!(m.forget("nothing"), 0);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let d = dir("missing");
        assert!(Memory::load(&d).is_empty());
        std::fs::remove_dir_all(&d).ok();
    }
}
