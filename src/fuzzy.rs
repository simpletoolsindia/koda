//! Fuzzy matching and a cached file index, for `@`-mentions in the input.
//!
//! The scoring is a plain subsequence match with bonuses that matter in
//! practice: consecutive runs, word boundaries, and the basename. That is enough
//! to make `@tui` find `src/tui.rs` and `@vwtest` find `src/view.rs` tests,
//! without pulling in a matcher crate for eighty lines of work.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// Higher is better. `None` when the pattern is not a subsequence at all.
pub fn score(candidate: &str, pattern: &str) -> Option<i32> {
    if pattern.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let pat: Vec<char> = pattern.chars().collect();
    if pat.len() > cand.len() {
        return None;
    }

    // Where the basename starts, so matches in the filename outrank matches in
    // a directory that happens to share letters.
    let base_start = candidate
        .rfind('/')
        .map(|i| candidate[..i].chars().count() + 1)
        .unwrap_or(0);

    let mut total = 0i32;
    let mut ci = 0usize;
    let mut streak = 0i32;

    for (pi, p) in pat.iter().enumerate() {
        let lower = p.to_ascii_lowercase();
        let mut found = None;
        while ci < cand.len() {
            let c = cand[ci];
            if c.to_ascii_lowercase() == lower {
                found = Some(ci);
                break;
            }
            ci += 1;
        }
        let at = found?;

        let mut points = 1;
        // Exact case match is a weak signal, but a real one.
        if cand[at] == *p {
            points += 1;
        }
        // A run of consecutive matches is the strongest signal there is.
        streak = if pi > 0 && at > 0 && cand[at - 1].eq_ignore_ascii_case(&pat[pi - 1]) {
            streak + 1
        } else {
            0
        };
        points += streak * 4;
        // Start of a path segment or a word.
        let boundary = at == 0
            || matches!(cand[at - 1], '/' | '_' | '-' | '.' | ' ')
            || (cand[at].is_uppercase() && !cand[at - 1].is_uppercase());
        if boundary {
            points += 6;
        }
        if at >= base_start {
            points += 3;
        }
        total += points;
        ci = at + 1;
    }

    // Prefer shorter candidates when scores are otherwise close.
    total -= (cand.len() as i32) / 12;
    Some(total)
}

/// Best `limit` matches, best first.
pub fn rank<'a>(candidates: &'a [String], pattern: &str, limit: usize) -> Vec<&'a String> {
    let mut scored: Vec<(i32, &'a String)> = candidates
        .iter()
        .filter_map(|c| score(c, pattern).map(|s| (s, c)))
        .collect();
    // Stable tie-break on the path so the list does not jitter between frames.
    scored.sort_by_key(|(s, c)| (std::cmp::Reverse(*s), (*c).clone()));
    scored.into_iter().take(limit).map(|(_, c)| c).collect()
}

/// Project files, gathered once off-thread and reused for every keystroke.
#[derive(Clone, Default)]
pub struct FileIndex {
    inner: Arc<RwLock<Option<Vec<String>>>>,
    /// Whether a scan has been kicked off. Needed to distinguish "no scan has
    /// been asked for" from "a scan is running": the UI only needs to repaint
    /// for the second, and treating them alike keeps the frame clock armed
    /// forever.
    started: Arc<AtomicBool>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ready(&self) -> bool {
        self.inner.read().map(|g| g.is_some()).unwrap_or(false)
    }

    /// True while a scan is in flight, and only then.
    pub fn scanning(&self) -> bool {
        self.started.load(Ordering::Relaxed) && !self.ready()
    }

    /// Kick off a scan if one has not run. Cheap to call repeatedly.
    pub fn ensure(&self, root: &Path) {
        if self.ready() {
            return;
        }
        self.started.store(true, Ordering::Relaxed);
        let slot = self.inner.clone();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let files = scan(&root);
            if let Ok(mut w) = slot.write() {
                *w = Some(files);
            }
        });
    }

    /// Force a rescan next time it is needed.
    pub fn invalidate(&self) {
        if let Ok(mut w) = self.inner.write() {
            *w = None;
        }
    }

    pub fn matches(&self, pattern: &str, limit: usize) -> Vec<String> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };
        let Some(files) = guard.as_ref() else {
            return Vec::new();
        };
        rank(files, pattern, limit).into_iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|f| f.len()))
            .unwrap_or(0)
    }
}

fn scan(root: &Path) -> Vec<String> {
    const CAP: usize = 20_000;
    let mut out = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .git_global(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "target")
        .build();
    for e in walk.flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = e.path().strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
            if out.len() >= CAP {
                break;
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        [
            "src/tui.rs",
            "src/view.rs",
            "src/theme.rs",
            "src/agent.rs",
            "tests/tui_test.py",
            "README.md",
            "docs/architecture/overview.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn non_matches_are_rejected() {
        assert!(score("src/tui.rs", "zzz").is_none());
        assert!(score("a", "abc").is_none());
        assert_eq!(score("anything", ""), Some(0));
    }

    #[test]
    fn subsequences_match_in_order() {
        assert!(score("src/view.rs", "svr").is_some());
        // Reversed order cannot match: `w` appears before `s` nowhere after it.
        assert!(score("src/view.rs", "wsrc").is_none());
        assert!(score("abc", "cba").is_none());
    }

    #[test]
    fn basename_beats_directory() {
        let f = files();
        let ranked = rank(&f, "tui", 5);
        assert_eq!(ranked[0], "src/tui.rs", "got {ranked:?}");
    }

    #[test]
    fn consecutive_runs_outrank_scattered_letters() {
        let scattered = score("s-r-c-t-h-e-m-e", "theme").unwrap_or(i32::MIN);
        let consecutive = score("src/theme.rs", "theme").unwrap();
        assert!(
            consecutive > scattered,
            "consecutive {consecutive} should beat scattered {scattered}"
        );
    }

    #[test]
    fn shorter_paths_win_ties() {
        let f = vec![
            "src/tui.rs".to_string(),
            "docs/deep/nested/place/src/tui.rs".to_string(),
        ];
        assert_eq!(rank(&f, "tui.rs", 2)[0], "src/tui.rs");
    }

    #[test]
    fn ranking_is_stable_for_equal_scores() {
        let f = vec!["b.rs".to_string(), "a.rs".to_string()];
        let first = rank(&f, "rs", 2);
        let second = rank(&f, "rs", 2);
        assert_eq!(first, second);
    }

    #[test]
    fn index_scans_a_project_and_respects_gitignore() {
        let dir = std::env::temp_dir().join("koda-fuzzy-index");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join(".gitignore"), "secret/\n").unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::write(dir.join("secret/keys.txt"), "x").unwrap();

        let files = scan(&dir);
        assert!(files.iter().any(|f| f.ends_with("main.rs")), "{files:?}");
        assert!(
            !files.iter().any(|f| f.contains("keys.txt")),
            "gitignored file leaked: {files:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_index_returns_nothing_rather_than_panicking() {
        let idx = FileIndex::new();
        assert!(!idx.ready());
        assert!(idx.matches("anything", 5).is_empty());
        assert_eq!(idx.len(), 0);
    }
}
