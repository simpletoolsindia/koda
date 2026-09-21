//! What koda remembers about a project, and how it finds the right part of it.
//!
//! `memory.rs` keeps the human view — `.koda/memory.md`, readable and editable.
//! This keeps the working copy: one SQLite file per project, so memory can be
//! *recalled* for a request instead of dumped into every prompt. The design and
//! the survey behind it are in `docs/research-agent-memory.md`; the short form:
//!
//! - **Typed.** A `decision` keeps its *why* — the survey's "remember the
//!   decision, not the description" — alongside `fact`, `preference` and
//!   `procedure`.
//! - **Recalled, not dumped.** FTS5 (BM25) finds the memories that share words
//!   with the request; when an embedding model is configured, cosine
//!   similarity finds the ones that share meaning. The two lists are fused by
//!   reciprocal rank, so either alone still works.
//! - **Superseded, not piled up.** A memory given the `subject` of an older
//!   one of the same kind replaces it. The old row is kept, marked, so what was
//!   true then stays answerable; only what is true now is recalled.
//! - **Traceable.** Each memory records where it came from (the session and
//!   turn), so a wrong one can be found and forgotten.
//!
//! No model runs on write or on recall — the embedding, when there is one, is
//! computed by the caller through the endpoint koda already talks to.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// What a memory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fact,
    Decision,
    Preference,
    Procedure,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Decision => "decision",
            Kind::Preference => "preference",
            Kind::Procedure => "procedure",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s.trim().to_ascii_lowercase().as_str() {
            "decision" => Kind::Decision,
            "preference" | "pref" => Kind::Preference,
            "procedure" | "how-to" | "howto" => Kind::Procedure,
            _ => Kind::Fact,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    pub text: String,
    /// For a decision, why it was made.
    pub why: String,
    /// What it is about, for superseding: "test command", "db engine".
    pub subject: String,
    /// Where it came from: "session 1789973446 turn 4".
    pub source: String,
    /// Unix seconds.
    pub created: i64,
    pub uses: i64,
}

impl Entry {
    /// One line, as the prompt and memory.md show it.
    pub fn line(&self) -> String {
        let mut s = match self.kind {
            Kind::Fact => self.text.clone(),
            k => format!("[{}] {}", k.as_str(), self.text),
        };
        if !self.why.is_empty() {
            s.push_str(&format!(" — because {}", self.why));
        }
        s
    }
}

/// What `add` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub id: i64,
    /// False when the same memory was already there.
    pub new: bool,
    /// The lines of memories this one replaced.
    pub replaced: Vec<String>,
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memories (
    id            INTEGER PRIMARY KEY,
    kind          TEXT    NOT NULL,
    text          TEXT    NOT NULL,
    why           TEXT    NOT NULL DEFAULT '',
    subject       TEXT    NOT NULL DEFAULT '',
    source        TEXT    NOT NULL DEFAULT '',
    created       INTEGER NOT NULL,
    superseded_by INTEGER,
    uses          INTEGER NOT NULL DEFAULT 0,
    last_used     INTEGER,
    embedding     BLOB,
    embed_model   TEXT
);
CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    text, why, subject, content='memories', content_rowid='id',
    tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(rowid, text, why, subject)
    VALUES (new.id, new.text, new.why, new.subject);
END;
CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
    INSERT INTO memories_fts(memories_fts, rowid, text, why, subject)
    VALUES ('delete', old.id, old.text, old.why, old.subject);
END;
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// The text index's layout; a different one is rebuilt on open.
const FTS_VERSION: &str = "2";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Words worth searching for: three letters or more, not filler.
fn terms(q: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "what", "where", "when",
        "how", "does", "please", "can", "you", "are", "was", "not", "all", "any", "should",
        "would", "could", "need", "want", "use", "make", "fix", "add",
        // Stemmed, these collide with everything: Porter turns "one" into
        // "on", so "…in one line" recalled every memory mentioning "on".
        "one", "two", "line", "lines", "way", "thing", "things", "get", "got", "just", "also",
        "now", "then", "there", "here", "its", "very", "some", "more", "much", "only", "say",
        "tell", "give", "show", "let", "sure", "yes", "yeah", "okay",
    ];
    q.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !STOP.contains(&w.as_str()))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

const COLUMNS: &str = "id, kind, text, why, subject, source, created, uses";

fn entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get(0)?,
        kind: Kind::parse(&r.get::<_, String>(1)?),
        text: r.get(2)?,
        why: r.get(3)?,
        subject: r.get(4)?,
        source: r.get(5)?,
        created: r.get(6)?,
        uses: r.get(7)?,
    })
}

impl Store {
    /// Open (creating if need be) the store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // The TUI lists memories while the agent writes them.
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        // Version 2 of the text index stems words (`print` finds `printed`).
        // An index built before that is rebuilt once, from the rows.
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'fts_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if v.as_deref() != Some(FTS_VERSION) {
            conn.execute_batch(&format!(
                "DROP TABLE IF EXISTS memories_fts;
                 CREATE VIRTUAL TABLE memories_fts USING fts5(
                     text, why, subject, content='memories', content_rowid='id',
                     tokenize='porter unicode61');
                 INSERT INTO memories_fts(memories_fts) VALUES('rebuild');
                 INSERT OR REPLACE INTO meta(key, value) VALUES ('fts_version', '{FTS_VERSION}');"
            ))?;
        }
        Ok(Self { conn })
    }

    /// Remember something. A memory whose text is already remembered is not
    /// added twice; one with the `subject` of an active memory of the same
    /// kind replaces it.
    pub fn add(
        &self,
        kind: Kind,
        text: &str,
        why: &str,
        subject: &str,
        source: &str,
    ) -> Result<Added> {
        let text = text.trim();
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE superseded_by IS NULL AND lower(text) = ?1",
                params![norm(text)],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(Added {
                id,
                new: false,
                replaced: Vec::new(),
            });
        }
        let subject = norm(subject);
        self.conn.execute(
            "INSERT INTO memories(kind, text, why, subject, source, created) VALUES (?1,?2,?3,?4,?5,?6)",
            params![kind.as_str(), text, why.trim(), subject, source, now()],
        )?;
        let id = self.conn.last_insert_rowid();
        let mut replaced = Vec::new();
        if !subject.is_empty() {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM memories
                 WHERE superseded_by IS NULL AND subject = ?1 AND kind = ?2 AND id != ?3"
            ))?;
            let old: Vec<Entry> = stmt
                .query_map(params![subject, kind.as_str(), id], entry)?
                .collect::<rusqlite::Result<_>>()?;
            for o in old {
                self.conn.execute(
                    "UPDATE memories SET superseded_by = ?1 WHERE id = ?2",
                    params![id, o.id],
                )?;
                replaced.push(o.line());
            }
        }
        Ok(Added {
            id,
            new: true,
            replaced,
        })
    }

    /// Delete memories whose text contains `needle`, returning their lines.
    pub fn forget(&self, needle: &str) -> Result<Vec<String>> {
        let n = norm(needle);
        if n.is_empty() {
            return Ok(Vec::new());
        }
        let gone: Vec<Entry> = self
            .active()?
            .into_iter()
            .filter(|e| norm(&e.text).contains(&n) || norm(&e.line()).contains(&n))
            .collect();
        for e in &gone {
            self.conn
                .execute("DELETE FROM memories WHERE id = ?1", params![e.id])?;
        }
        Ok(gone.iter().map(Entry::line).collect())
    }

    pub fn forget_id(&self, id: i64) -> Result<Option<String>> {
        let e = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM memories WHERE id = ?1"),
                params![id],
                entry,
            )
            .optional()?;
        self.conn
            .execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        Ok(e.map(|e| e.line()))
    }

    /// Everything currently true, newest first.
    pub fn active(&self) -> Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM memories WHERE superseded_by IS NULL ORDER BY created DESC, id DESC"
        ))?;
        let v = stmt
            .query_map([], entry)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// The memories for a request, best first, at most `k`. `query_vec` is the
    /// request's embedding when there is an embedding model, and adds the
    /// memories that share its meaning to the ones that share its words.
    pub fn recall(
        &self,
        query: &str,
        k: usize,
        query_vec: Option<(&str, &[f32])>,
    ) -> Result<Vec<Entry>> {
        const RRF: f64 = 60.0;
        let mut score: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();

        let t = terms(query);
        if !t.is_empty() {
            // Each term quoted, so punctuation in a request is never FTS syntax.
            let q = t
                .iter()
                .map(|w| format!("\"{}\"", w.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" OR ");
            let mut stmt = self.conn.prepare(
                "SELECT m.id FROM memories_fts f JOIN memories m ON m.id = f.rowid
                 WHERE memories_fts MATCH ?1 AND m.superseded_by IS NULL
                 ORDER BY bm25(memories_fts) LIMIT 30",
            )?;
            let ids: Vec<i64> = stmt
                .query_map(params![q], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for (rank, id) in ids.into_iter().enumerate() {
                *score.entry(id).or_default() += 1.0 / (RRF + rank as f64 + 1.0);
            }
        }

        if let Some((model, qv)) = query_vec {
            let mut stmt = self.conn.prepare(
                "SELECT id, embedding FROM memories
                 WHERE superseded_by IS NULL AND embedding IS NOT NULL AND embed_model = ?1",
            )?;
            let mut sims: Vec<(i64, f32)> = stmt
                .query_map(params![model], |r| {
                    Ok((r.get::<_, i64>(0)?, from_blob(&r.get::<_, Vec<u8>>(1)?)))
                })?
                .filter_map(|r| r.ok())
                .map(|(id, v)| (id, cosine(qv, &v)))
                .filter(|(_, s)| *s > 0.3)
                .collect();
            sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (rank, (id, _)) in sims.into_iter().take(30).enumerate() {
                *score.entry(id).or_default() += 1.0 / (RRF + rank as f64 + 1.0);
            }
        }

        let mut ranked: Vec<(i64, f64)> = score.into_iter().collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.cmp(&a.0))
        });
        let mut out = Vec::new();
        for (id, _) in ranked.into_iter().take(k) {
            if let Some(e) = self
                .conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM memories WHERE id = ?1"),
                    params![id],
                    entry,
                )
                .optional()?
            {
                self.conn.execute(
                    "UPDATE memories SET uses = uses + 1, last_used = ?1 WHERE id = ?2",
                    params![now(), id],
                )?;
                out.push(e);
            }
        }
        Ok(out)
    }

    pub fn set_embedding(&self, id: i64, model: &str, v: &[f32]) -> Result<()> {
        self.conn.execute(
            "UPDATE memories SET embedding = ?1, embed_model = ?2 WHERE id = ?3",
            params![to_blob(v), model, id],
        )?;
        Ok(())
    }

    /// Active memories without an embedding from `model`, to fill in.
    pub fn unembedded(&self, model: &str, limit: usize) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, why FROM memories WHERE superseded_by IS NULL
             AND (embedding IS NULL OR embed_model IS NOT ?1) LIMIT ?2",
        )?;
        let v = stmt
            .query_map(params![model, limit as i64], |r| {
                let text: String = r.get(1)?;
                let why: String = r.get(2)?;
                Ok((
                    r.get(0)?,
                    if why.is_empty() {
                        text
                    } else {
                        format!("{text} — {why}")
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// Bring in the notes of the old `memory.md`, once.
    pub fn import_legacy(&self, notes: &[String]) -> Result<usize> {
        let done: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'imported_legacy'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if done.is_some() {
            return Ok(0);
        }
        let mut n = 0;
        for note in notes {
            let (kind, text) = match note.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                Some((k, t)) => (Kind::parse(k), t.to_string()),
                None => (Kind::Fact, note.clone()),
            };
            let (text, why) = match text.split_once(" — because ") {
                Some((t, w)) => (t.to_string(), w.to_string()),
                None => (text, String::new()),
            };
            if self
                .add(kind, &text, &why, "", "imported from memory.md")?
                .new
            {
                n += 1;
            }
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('imported_legacy', '1')",
            [],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    #[test]
    fn recall_finds_the_memories_that_share_the_requests_words() {
        let s = store();
        s.add(
            Kind::Fact,
            "Tests run with `just test`, not cargo test",
            "",
            "test command",
            "",
        )
        .unwrap();
        s.add(
            Kind::Decision,
            "Use SQLite for the cache",
            "no server to run on laptops",
            "cache engine",
            "",
        )
        .unwrap();
        s.add(
            Kind::Preference,
            "Keep commit subjects under 60 characters",
            "",
            "",
            "",
        )
        .unwrap();
        let got = s.recall("how do I run the tests?", 3, None).unwrap();
        assert_eq!(
            got[0].text, "Tests run with `just test`, not cargo test",
            "{got:?}"
        );
        let got = s.recall("which database for the cache", 3, None).unwrap();
        assert_eq!(got[0].kind, Kind::Decision);
        assert_eq!(
            got[0].line(),
            "[decision] Use SQLite for the cache — because no server to run on laptops"
        );
        assert!(s.recall("zebra", 3, None).unwrap().is_empty());
        // Filler in a request is not a search term: stemmed, "one" is "on".
        s.add(Kind::Fact, "The staging server is on port 2222", "", "", "")
            .unwrap();
        let got = s.recall("answer in one line", 5, None).unwrap();
        assert!(got.iter().all(|e| !e.text.contains("staging")), "{got:?}");
        // Word forms: the request says "print", the memory says "printed".
        s.add(
            Kind::Preference,
            "Currency values are printed with two decimals",
            "",
            "",
            "",
        )
        .unwrap();
        let got = s.recall("how should I print the price", 3, None).unwrap();
        assert!(
            got.iter().any(|e| e.text.starts_with("Currency")),
            "{got:?}"
        );
        // Punctuation in a request is never FTS syntax.
        assert!(s.recall("\"quoted\" AND (paren*) -minus", 3, None).is_ok());
    }

    #[test]
    fn a_new_memory_on_the_same_subject_replaces_the_old() {
        let s = store();
        s.add(
            Kind::Fact,
            "Tests run with npm test",
            "",
            "test command",
            "",
        )
        .unwrap();
        let a = s
            .add(
                Kind::Fact,
                "Tests run with pnpm test",
                "",
                "Test  Command",
                "",
            )
            .unwrap();
        assert_eq!(a.replaced, vec!["Tests run with npm test"]);
        let active: Vec<String> = s.active().unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(active, vec!["Tests run with pnpm test"]);
        assert_eq!(
            s.recall("tests", 5, None).unwrap().len(),
            1,
            "only what is true now"
        );
        // The same text twice is one memory.
        let again = s
            .add(Kind::Fact, "tests run with  PNPM test", "", "", "")
            .unwrap();
        assert!(!again.new);
    }

    #[test]
    fn meaning_finds_what_words_miss() {
        let s = store();
        let a = s
            .add(
                Kind::Fact,
                "Deploys go out through the release workflow",
                "",
                "",
                "",
            )
            .unwrap();
        let b = s
            .add(Kind::Fact, "The logo is in assets", "", "", "")
            .unwrap();
        s.set_embedding(a.id, "m", &[1.0, 0.0]).unwrap();
        s.set_embedding(b.id, "m", &[0.0, 1.0]).unwrap();
        // No shared word with "shipping"; the vector finds it anyway.
        let got = s
            .recall("shipping to production", 1, Some(("m", &[0.9, 0.1])))
            .unwrap();
        assert_eq!(got[0].id, a.id);
        // Another model's vectors are not compared.
        assert!(s
            .recall("shipping to production", 1, Some(("other", &[0.9, 0.1])))
            .unwrap()
            .is_empty());
        assert_eq!(s.unembedded("m", 10).unwrap().len(), 0);
        assert_eq!(s.unembedded("other", 10).unwrap().len(), 2);
    }

    #[test]
    fn forget_and_import() {
        let s = store();
        s.add(Kind::Fact, "The API key lives in .env", "", "", "")
            .unwrap();
        assert_eq!(s.forget("api key").unwrap().len(), 1);
        assert!(s.active().unwrap().is_empty());
        let n = s
            .import_legacy(&[
                "Build with make".into(),
                "[decision] Use tabs — because the linter says so".into(),
            ])
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(s.import_legacy(&["again".into()]).unwrap(), 0, "only once");
        let d = s.recall("tabs linter", 1, None).unwrap();
        assert_eq!(
            (d[0].kind, d[0].why.as_str()),
            (Kind::Decision, "the linter says so")
        );
    }

    #[test]
    fn it_persists() {
        let dir = std::env::temp_dir().join(format!("koda-memstore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("memory.db");
        Store::open(&p)
            .unwrap()
            .add(Kind::Fact, "Port is 8080", "", "", "")
            .unwrap();
        assert_eq!(
            Store::open(&p).unwrap().active().unwrap()[0].text,
            "Port is 8080"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn kinds_parse_by_name_and_alias_and_default_to_fact() {
        for k in [
            Kind::Fact,
            Kind::Decision,
            Kind::Preference,
            Kind::Procedure,
        ] {
            assert_eq!(Kind::parse(k.as_str()), k);
            assert_eq!(
                Kind::parse(&format!("  {}  ", k.as_str().to_uppercase())),
                k
            );
        }
        assert_eq!(Kind::parse("pref"), Kind::Preference);
        assert_eq!(Kind::parse("how-to"), Kind::Procedure);
        assert_eq!(Kind::parse("howto"), Kind::Procedure);
        assert_eq!(Kind::parse("rumour"), Kind::Fact);
        assert_eq!(Kind::parse(""), Kind::Fact);
    }

    #[test]
    fn a_line_shows_kind_and_reason() {
        let e = |kind, why: &str| Entry {
            id: 1,
            kind,
            text: "Use tabs".into(),
            why: why.into(),
            subject: String::new(),
            source: String::new(),
            created: 0,
            uses: 0,
        };
        assert_eq!(e(Kind::Fact, "").line(), "Use tabs");
        assert_eq!(e(Kind::Fact, "gofmt").line(), "Use tabs — because gofmt");
        assert_eq!(e(Kind::Procedure, "").line(), "[procedure] Use tabs");
    }

    /// A subject is shared across kinds, but a decision about the database
    /// does not make a fact about it untrue.
    #[test]
    fn superseding_stays_within_a_kind() {
        let s = store();
        s.add(
            Kind::Fact,
            "The database is Postgres 15",
            "",
            "database",
            "",
        )
        .unwrap();
        let d = s
            .add(
                Kind::Decision,
                "Move to Postgres 16",
                "security fixes",
                "database",
                "",
            )
            .unwrap();
        assert!(d.replaced.is_empty(), "{:?}", d.replaced);
        assert_eq!(s.active().unwrap().len(), 2);
        // An empty subject never supersedes anything.
        s.add(Kind::Fact, "Unrelated one", "", "", "").unwrap();
        s.add(Kind::Fact, "Unrelated two", "", "", "").unwrap();
        assert_eq!(s.active().unwrap().len(), 4);
    }

    #[test]
    fn active_is_newest_first_and_text_is_trimmed() {
        let s = store();
        s.add(Kind::Fact, "  first memory  ", "", "", "").unwrap();
        s.add(Kind::Fact, "second memory", "", "", "").unwrap();
        let active: Vec<String> = s.active().unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(active, vec!["second memory", "first memory"]);
    }

    #[test]
    fn recall_honours_k_and_counts_uses() {
        let s = store();
        for i in 0..5 {
            s.add(
                Kind::Fact,
                &format!("Deploy target number {i} is staging"),
                "",
                "",
                "",
            )
            .unwrap();
        }
        let got = s.recall("deploy staging", 2, None).unwrap();
        assert_eq!(got.len(), 2);
        let used: i64 = s
            .active()
            .unwrap()
            .iter()
            .filter(|e| got.iter().any(|g| g.id == e.id))
            .map(|e| e.uses)
            .sum();
        assert_eq!(used, 2, "each recalled memory counts one use");
        assert!(s.recall("deploy", 0, None).unwrap().is_empty());
    }

    #[test]
    fn a_request_of_only_filler_recalls_nothing() {
        let s = store();
        s.add(Kind::Fact, "Show the price with two decimals", "", "", "")
            .unwrap();
        assert!(s.recall("", 5, None).unwrap().is_empty());
        assert!(s
            .recall("can you show me one more", 5, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_superseded_memory_is_not_recalled_by_meaning_either() {
        let s = store();
        let old = s.add(Kind::Fact, "Port is 8080", "", "port", "").unwrap();
        s.set_embedding(old.id, "m", &[1.0, 0.0]).unwrap();
        let new = s.add(Kind::Fact, "Port is 9090", "", "port", "").unwrap();
        s.set_embedding(new.id, "m", &[0.0, 1.0]).unwrap();
        let got = s.recall("xyzzy", 5, Some(("m", &[1.0, 0.0]))).unwrap();
        assert!(got.iter().all(|e| e.id != old.id), "{got:?}");
    }

    #[test]
    fn weak_or_mismatched_vectors_do_not_count() {
        let s = store();
        let a = s.add(Kind::Fact, "Alpha memory", "", "", "").unwrap();
        s.set_embedding(a.id, "m", &[1.0, 0.0, 0.0]).unwrap();
        // Orthogonal: similarity 0, under the threshold.
        assert!(s
            .recall("zzz", 5, Some(("m", &[0.0, 1.0, 0.0])))
            .unwrap()
            .is_empty());
        // A different dimension (the embedding model changed size).
        assert!(s
            .recall("zzz", 5, Some(("m", &[1.0, 0.0])))
            .unwrap()
            .is_empty());
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert!((cosine(&[2.0, 0.0], &[5.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn vectors_survive_the_blob_round_trip() {
        let v = vec![0.0, -1.5, 3.25, f32::MIN_POSITIVE, 1e9];
        assert_eq!(from_blob(&to_blob(&v)), v);
        assert!(
            from_blob(&[1, 2, 3]).is_empty(),
            "a torn blob is not a vector"
        );
    }

    #[test]
    fn forget_by_id_and_by_nothing() {
        let s = store();
        let a = s
            .add(
                Kind::Decision,
                "Use rustls",
                "no OpenSSL on Windows",
                "",
                "",
            )
            .unwrap();
        assert_eq!(
            s.forget_id(a.id).unwrap().as_deref(),
            Some("[decision] Use rustls — because no OpenSSL on Windows")
        );
        assert_eq!(s.forget_id(a.id).unwrap(), None, "already gone");
        s.add(Kind::Fact, "Keep me", "", "", "").unwrap();
        assert!(s.forget("   ").unwrap().is_empty(), "blank forgets nothing");
        assert_eq!(s.active().unwrap().len(), 1);
    }

    #[test]
    fn a_forgotten_memory_leaves_the_text_index_too() {
        let s = store();
        s.add(Kind::Fact, "The kiosk password rotates weekly", "", "", "")
            .unwrap();
        s.forget("kiosk").unwrap();
        assert!(s.recall("kiosk password", 5, None).unwrap().is_empty());
    }

    #[test]
    fn filler_words_are_not_terms() {
        let t = terms("How do I run the tests in one line?");
        assert!(
            t.contains(&"run".to_string()) && t.contains(&"tests".to_string()),
            "{t:?}"
        );
        for filler in ["how", "the", "one", "line"] {
            assert!(!t.contains(&filler.to_string()), "{filler} in {t:?}");
        }
    }
}
