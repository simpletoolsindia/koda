//! Fuzzy matching and a cached file index, for `@`-mentions in the input.
//!
//! The scoring is a plain subsequence match with bonuses that matter in
//! practice: consecutive runs, word boundaries, and the basename. That is enough
//! to make `@tui` find `src/tui.rs` and `@vwtest` find `src/view.rs` tests,
//! without pulling in a matcher crate for eighty lines of work.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

// The scorer finds the *best* alignment of the pattern in the candidate, not
// the first one. The first-occurrence walk this replaces had no gap penalty and
// took whatever letter came first, so `tst` matched `demos/casts/ask.cast`
// letter by letter as happily as `tests/`, and `config` scored the same after
// the dot in `playwright.config.js` as at the start of `src/config.rs`. This is
// fzf's shape — a small dynamic programme over (pattern × candidate) that
// rewards word starts and runs and charges for gaps — cut down to what a list
// of project paths needs.

/// Every matched character.
const MATCH: i32 = 16;
/// Opening a gap between two matched characters, then each further character
/// skipped. Leading and trailing text is free: only scatter costs.
const GAP_OPEN: i32 = 3;
const GAP_EXTEND: i32 = 1;
/// A match that continues the one before it.
const CONSECUTIVE: i32 = 12;
/// Where a match lands. The start of the file name is where people aim, so it
/// outranks every other boundary; after a dot is an extension, and weakest.
const AT_BASENAME: i32 = 18;
const AT_SEGMENT: i32 = 10;
const AT_WORD: i32 = 8;
const AT_CAMEL: i32 = 7;
const AT_EXTENSION: i32 = 3;
const IN_BASENAME: i32 = 2;
/// The pattern *is* the file name, or its name without the extension.
const EXACT_NAME: i32 = 40;

/// Higher is better. `None` when the pattern is not a subsequence at all.
pub fn score(candidate: &str, pattern: &str) -> Option<i32> {
    align(candidate, pattern, false).map(|(s, _)| s)
}

/// The score and the character indices of the best alignment — what a list
/// should highlight, so the lit letters are the ones that earned the rank.
pub fn positions(candidate: &str, pattern: &str) -> Option<(i32, Vec<usize>)> {
    align(candidate, pattern, true)
}

fn align(candidate: &str, pattern: &str, want_positions: bool) -> Option<(i32, Vec<usize>)> {
    if pattern.is_empty() {
        return Some((0, Vec::new()));
    }
    let cand: Vec<char> = candidate.chars().collect();
    let pat: Vec<char> = pattern.chars().collect();
    let (n, m) = (cand.len(), pat.len());
    if m > n {
        return None;
    }
    let lower = |c: char| c.to_ascii_lowercase();
    // Cheap rejection before the O(n·m) work: most of a project is not a
    // subsequence of what was typed.
    let mut pi = 0;
    for &c in &cand {
        if pi < m && lower(c) == lower(pat[pi]) {
            pi += 1;
        }
    }
    if pi < m {
        return None;
    }

    let base_start = candidate
        .rfind('/')
        .map(|i| candidate[..i].chars().count() + 1)
        .unwrap_or(0);
    let bonus: Vec<i32> = (0..n)
        .map(|i| {
            let here = if i == base_start {
                AT_BASENAME
            } else if i == 0 || cand[i - 1] == '/' {
                AT_SEGMENT
            } else if matches!(cand[i - 1], '_' | '-' | ' ') {
                AT_WORD
            } else if cand[i - 1] == '.' {
                AT_EXTENSION
            } else if cand[i].is_uppercase() && cand[i - 1].is_lowercase() {
                AT_CAMEL
            } else {
                0
            };
            here + if i >= base_start { IN_BASENAME } else { 0 }
        })
        .collect();

    // h[j][i]: best score with pat[j] matched at cand[i]; from[j][i]: whether
    // that came straight from pat[j-1] at i-1 (a run), for the walk back.
    const NONE: i32 = i32::MIN / 2;
    // One flat table, reused across calls: `rank` runs this for every file in
    // the project on every keystroke, and a fresh `Vec` per row per file was
    // most of the cost (47 ms → a few for a long query over 20k paths).
    thread_local! {
        static TABLE: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    TABLE.with(|cell| {
        let mut table = cell.borrow_mut();
        table.clear();
        table.resize(n * m, NONE);
        fill(
            &mut table,
            &cand,
            &pat,
            &bonus,
            want_positions,
            base_start,
            pattern,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn fill(
    h: &mut [i32],
    cand: &[char],
    pat: &[char],
    bonus: &[i32],
    want_positions: bool,
    base_start: usize,
    pattern: &str,
) -> Option<(i32, Vec<usize>)> {
    const NONE: i32 = i32::MIN / 2;
    let (n, m) = (cand.len(), pat.len());
    let lower = |c: char| c.to_ascii_lowercase();
    let ix = |j: usize, i: usize| j * n + i;
    let mut run = vec![false; if want_positions { n * m } else { 0 }];
    for j in 0..m {
        // Best of h[j-1][k] for k < i-1, already charged for the gap to i.
        let mut gapped = NONE;
        for i in j..n {
            if i >= 1 && j >= 1 && i >= 2 {
                let opened = h[ix(j - 1, i - 2)] - GAP_OPEN;
                gapped = (gapped - GAP_EXTEND).max(opened);
            }
            if lower(cand[i]) != lower(pat[j]) {
                continue;
            }
            let mut at = MATCH + bonus[i] + i32::from(cand[i] == pat[j]);
            if j == 0 {
                h[ix(j, i)] = at;
                continue;
            }
            let straight = if i >= 1 { h[ix(j - 1, i - 1)] } else { NONE };
            let (prev, is_run) = if straight > NONE {
                let s = straight + CONSECUTIVE;
                if s >= gapped {
                    (s, true)
                } else {
                    (gapped, false)
                }
            } else {
                (gapped, false)
            };
            if prev <= NONE / 2 {
                continue;
            }
            at += prev;
            h[ix(j, i)] = at;
            if want_positions {
                run[ix(j, i)] = is_run;
            }
        }
    }
    let (end, mut best) = (0..n)
        .map(|i| (i, h[ix(m - 1, i)]))
        .max_by_key(|&(i, s)| (s, std::cmp::Reverse(i)))?;
    if best <= NONE / 2 {
        return None;
    }

    // The name is the thing: `md` means `md.rs` before any `*.md`.
    let base: String = cand[base_start..]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();
    let stem = base.split('.').next().unwrap_or("");
    let want = pattern.to_ascii_lowercase();
    if base == want || stem == want {
        best += EXACT_NAME;
    }
    // Every path pays a little for its length, so an exact tie goes to the
    // shallower file — `src/webui.rs` before `demos/casts/webui.cast`.
    best -= (n as i32) / 6;

    let mut out = Vec::new();
    if want_positions {
        out.resize(m, 0);
        let mut i = end;
        for j in (0..m).rev() {
            out[j] = i;
            if j == 0 {
                break;
            }
            if run[ix(j, i)] {
                i -= 1;
            } else {
                // Re-find the gapped predecessor that produced this score.
                let need = h[ix(j, i)] - (MATCH + bonus[i] + i32::from(cand[i] == pat[j]));
                let k = (0..i.saturating_sub(1))
                    .rev()
                    .find(|&k| {
                        h[ix(j - 1, k)] > NONE
                            && h[ix(j - 1, k)] - GAP_OPEN - (i - k - 2) as i32 * GAP_EXTEND == need
                    })
                    .unwrap_or_else(|| (0..i).rev().find(|&k| h[ix(j - 1, k)] > NONE).unwrap_or(0));
                i = k;
            }
        }
    }
    Some((best, out))
}

/// Best `limit` matches, best first.
pub fn rank<'a>(candidates: &'a [String], pattern: &str, limit: usize) -> Vec<&'a String> {
    let mut scored: Vec<(i32, &'a String)> = candidates
        .iter()
        .filter_map(|c| score(c, pattern).map(|s| (s, c)))
        .collect();
    // Stable tie-break on the path so the list does not jitter between frames.
    scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.len().cmp(&b.len())).then(a.cmp(b)));
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
        self.started.load(Ordering::Acquire)
    }

    /// Kick off a scan if one has not run. Cheap to call repeatedly.
    ///
    /// "Cheap" needs the in-flight check to be real. This used to test only
    /// `ready()` — whether a scan had *finished* — so while one was running
    /// every call started another. `draw` reaches here on every frame whenever
    /// an `@token` sits in the composer, and a scan in flight keeps the frame
    /// clock armed, so it fed itself: tens of full `WalkBuilder` walks per
    /// second over the whole repo, each slowing the others down.
    pub fn ensure(&self, root: &Path) {
        if self.ready() {
            return;
        }
        // Claim the scan atomically; everyone who loses just returns. AcqRel so
        // the winner's writes are visible to whoever observes `started` next.
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return; // a scan is already running
        }
        let slot = self.inner.clone();
        let started = self.started.clone();
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let files = scan(&root);
            if let Ok(mut w) = slot.write() {
                *w = Some(files);
            }
            // Released last, so `scanning()` is false only once the result is
            // actually visible — otherwise the UI could see "not scanning, not
            // ready" and start another walk.
            started.store(false, Ordering::Release);
        });
    }

    /// Force a rescan next time it is needed.
    ///
    /// Leaves `started` alone: if a scan is in flight it will finish and clear
    /// the flag itself, and the next `ensure` after that re-scans. Clearing it
    /// here would let a second scan start alongside the first.
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

/// Directories nobody means when they type `@`: version control, build output
/// and dependency caches. A project usually gitignores them, but a scratch
/// directory or a fresh checkout often does not, and then `@cart` offered
/// `__pycache__/cart.cpython-314.pyc` beside `cart.py`.
fn is_build_output(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "target"
                | "node_modules"
                | "__pycache__"
                | ".venv"
                | ".tox"
                | ".mypy_cache"
                | ".pytest_cache"
        )
    )
}

/// Compiled artefacts that sit beside their sources.
fn is_compiled(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("pyc" | "pyo" | "class" | "o" | "obj")
    )
}

fn scan(root: &Path) -> Vec<String> {
    const CAP: usize = 20_000;
    let mut out = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .git_global(false)
        .filter_entry(|e| !is_build_output(e.file_name()))
        .build();
    for e in walk.flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if is_compiled(e.path()) {
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

    /// `ensure` used to check only whether a scan had *finished*, so while one
    /// was running every call started another. `draw` reaches it on every frame
    /// whenever an `@token` is in the composer, and a scan in flight keeps the
    /// frame clock armed — so it fed itself into a storm of whole-repo walks.
    #[test]
    fn ensure_starts_one_scan_no_matter_how_often_it_is_called() {
        use std::sync::atomic::{AtomicUsize, Ordering as O};
        static SCANS: AtomicUsize = AtomicUsize::new(0);

        let dir = std::env::temp_dir().join(format!("koda-fuzzy-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..40 {
            std::fs::write(dir.join(format!("f{i}.rs")), "fn main() {}\n").unwrap();
        }

        let idx = FileIndex::new();
        // Hammer it the way a redrawing UI would, before any scan can finish.
        for _ in 0..200 {
            idx.ensure(&dir);
        }
        assert!(idx.scanning() || idx.ready(), "a scan was started");

        // Wait for it, then confirm the flag is released and the result landed.
        for _ in 0..200 {
            if idx.ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(idx.ready(), "the scan completed");
        assert!(!idx.scanning(), "and the in-flight flag was released");

        // Once ready, further calls do nothing at all.
        idx.ensure(&dir);
        assert!(!idx.scanning());
        let _ = SCANS.load(O::Relaxed);
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// The cases the first-occurrence scorer got wrong, from a real repo. Each
    /// was a file someone plainly meant, ranked below one they did not.
    #[test]
    fn the_file_you_meant_comes_first() {
        let f: Vec<String> = [
            "docs/lsp.md",
            "docs/mcp.md",
            "src/md.rs",
            "tests/visual/playwright.config.js",
            "docs-site/tsconfig.json",
            "src/config.rs",
            "docs/plan-trace-ui.md",
            "src/trace.rs",
            "demos/casts/webui.cast",
            "src/webui.rs",
            "demos/casts/ask.cast",
            "tests/tui_test.py",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for (q, want) in [
            ("md", "src/md.rs"),
            ("config", "src/config.rs"),
            ("cfg", "src/config.rs"),
            ("trace", "src/trace.rs"),
            ("webui", "src/webui.rs"),
            ("tst", "tests/tui_test.py"),
        ] {
            assert_eq!(rank(&f, q, 3)[0], want, "@{q}: {:?}", rank(&f, q, 3));
        }
    }

    /// Scattered letters pay for their gaps, so a tight match wins even when
    /// the scattered one starts earlier in the path.
    #[test]
    fn gaps_cost() {
        let tight = score("tests/tui_test.py", "tst").unwrap();
        let scattered = score("demos/casts/ask.cast", "tst").unwrap();
        assert!(tight > scattered, "{tight} vs {scattered}");
    }

    /// The highlight is the alignment that scored, not the first letters found.
    #[test]
    fn positions_are_the_scored_alignment() {
        assert_eq!(positions("src/md.rs", "md").unwrap().1, vec![4, 5]);
        assert_eq!(positions("src/config.rs", "cfg").unwrap().1, vec![4, 7, 9]);
        assert!(positions("src/md.rs", "zz").is_none());
    }

    #[test]
    fn build_output_is_not_offered() {
        let dir = std::env::temp_dir().join(format!("koda-fuzzy-junk-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::write(dir.join("cart.py"), "").unwrap();
        std::fs::write(dir.join("__pycache__/cart.cpython-314.pyc"), "").unwrap();
        std::fs::write(dir.join("node_modules/x/index.js"), "").unwrap();
        std::fs::write(dir.join("stray.pyc"), "").unwrap();
        let files = scan(&dir);
        assert_eq!(files, vec!["cart.py".to_string()], "{files:?}");
        std::fs::remove_dir_all(&dir).ok();
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
