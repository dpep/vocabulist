//! Embedded SQLite storage: the lexicon, per-register counts, n-grams, the
//! bounded exemplar set, and the capture spool.
//!
//! The spool is deliberately *not* an archive. Capture stages text there,
//! processing derives counts from it, and the raw text is dropped. Personal
//! prose is the liability; the aggregates are nearly lossless for our purpose
//! and carry none of the risk. That's the one place this diverges hard from a
//! general-purpose firehose, which retains blobs so it can re-extract later.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::types::{Entry, Provenance, Register, StatsPayload};

/// How many exemplars we keep per register. A voice profile needs real quoted
/// examples — adjectives like "semi-casual" are unfalsifiable — but keeping
/// the whole corpus to get them is exactly what we're avoiding. Bounded by
/// design, not by policy.
pub const MAX_EXEMPLARS_PER_REGISTER: usize = 25;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

-- The lexicon proper. One row per normalized word. `provenance` holds the
-- strongest evidence seen so far and only ever ratchets upward, so a word
-- first noticed in prose is upgraded (never downgraded) when it later turns
-- up as an installed binary.
CREATE TABLE IF NOT EXISTS lexicon (
    word TEXT PRIMARY KEY,
    display TEXT NOT NULL,
    provenance TEXT NOT NULL DEFAULT 'observed',
    count INTEGER NOT NULL DEFAULT 0,
    first_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_lexicon_provenance ON lexicon(provenance);

-- How often a word appears in each register. The same word can be ordinary in
-- one voice and jarring in another, so the counts stay split rather than
-- summed into a single frequency.
CREATE TABLE IF NOT EXISTS word_registers (
    word TEXT NOT NULL REFERENCES lexicon(word) ON DELETE CASCADE,
    register TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (word, register)
);

-- Collocations. Unigrams live in `lexicon`; this holds n>=2. Real-word errors
-- (`form` for `from`) are invisible to a dictionary because both spellings are
-- words -- only the company they keep gives them away.
CREATE TABLE IF NOT EXISTS ngrams (
    gram TEXT NOT NULL,
    n INTEGER NOT NULL,
    register TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (gram, register)
);

CREATE INDEX IF NOT EXISTS idx_ngrams_n ON ngrams(n, count DESC);

-- A bounded, ranked sample of real sentences per register -- the evidence a
-- voice profile quotes from. Capped at MAX_EXEMPLARS_PER_REGISTER; the
-- weakest row is evicted when a better one arrives.
CREATE TABLE IF NOT EXISTS exemplars (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    register TEXT NOT NULL,
    text TEXT NOT NULL,
    score REAL NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_exemplars_register ON exemplars(register, score);

-- Transient capture staging. Rows land here, get processed into counts, and
-- are deleted. `authored_by` records whether we believe you wrote it --
-- assistant-authored text is captured for provenance but never learned from.
CREATE TABLE IF NOT EXISTS spool (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    register TEXT NOT NULL,
    source TEXT,
    body TEXT NOT NULL,
    authored_by TEXT NOT NULL DEFAULT 'user',
    captured_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    processed_at TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_spool_unprocessed ON spool(processed_at)
    WHERE processed_at IS NULL;
";

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(&path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Insert or upgrade one word. Provenance ratchets: a stronger source
    /// overwrites a weaker one, never the reverse. Returns whether the row was
    /// newly created and whether an existing row's provenance improved.
    pub fn upsert_word(
        &self,
        word: &str,
        display: &str,
        provenance: Provenance,
        increment: i64,
    ) -> Result<(bool, bool)> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT provenance FROM lexicon WHERE word = ?1",
                params![word],
                |r| r.get(0),
            )
            .optional()?;

        match existing {
            None => {
                self.conn.execute(
                    "INSERT INTO lexicon (word, display, provenance, count)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![word, display, provenance.as_str(), increment],
                )?;
                Ok((true, false))
            }
            Some(current) => {
                let current = Provenance::parse(&current).unwrap_or(Provenance::Observed);
                let upgraded = provenance > current;
                let winner = if upgraded { provenance } else { current };
                self.conn.execute(
                    "UPDATE lexicon
                        SET provenance = ?2,
                            count = count + ?3,
                            last_seen = CURRENT_TIMESTAMP
                      WHERE word = ?1",
                    params![word, winner.as_str(), increment],
                )?;
                Ok((false, upgraded))
            }
        }
    }

    /// Bump a word's count within one register. The word must already exist.
    pub fn bump_register(&self, word: &str, register: Register, increment: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO word_registers (word, register, count) VALUES (?1, ?2, ?3)
             ON CONFLICT(word, register) DO UPDATE SET count = count + ?3",
            params![word, register.as_str(), increment],
        )?;
        Ok(())
    }

    pub fn contains(&self, word: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM lexicon WHERE word = ?1",
                params![word],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Every word in the lexicon, for building the in-memory check set.
    pub fn words(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT word FROM lexicon")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Lexicon entries matching an optional substring filter, strongest first.
    pub fn list(&self, filter: Option<&str>, limit: usize) -> Result<Vec<Entry>> {
        let pattern = format!("%{}%", filter.unwrap_or(""));
        let mut stmt = self.conn.prepare(
            "SELECT word, provenance, count FROM lexicon
              WHERE word LIKE ?1
              ORDER BY count DESC, word ASC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |r| {
            let word: String = r.get(0)?;
            let prov = Provenance::parse(&r.get::<_, String>(1)?).unwrap_or(Provenance::Observed);
            let count: i64 = r.get(2)?;
            Ok(Entry {
                word,
                provenance: prov,
                validity: validity(prov, count),
                count,
            })
        })?;
        rows.collect()
    }

    pub fn remove(&self, word: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM lexicon WHERE word = ?1", params![word])?
            > 0)
    }

    /// Bump one n-gram's count in a register.
    pub fn bump_ngram(&self, gram: &str, n: usize, register: Register, inc: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ngrams (gram, n, register, count) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(gram, register) DO UPDATE SET count = count + ?4",
            params![gram, n as i64, register.as_str(), inc],
        )?;
        Ok(())
    }

    /// Total count for an n-gram across all registers.
    pub fn ngram_count(&self, gram: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM ngrams WHERE gram = ?1",
                params![gram],
                |r| r.get(0),
            )
            .unwrap_or(0))
    }

    /// Stage captured text. Returns the new row id.
    pub fn spool(
        &self,
        register: Register,
        source: Option<&str>,
        body: &str,
        authored_by: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO spool (register, source, body, authored_by)
             VALUES (?1, ?2, ?3, ?4)",
            params![register.as_str(), source, body, authored_by],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Unprocessed spool rows, oldest first.
    pub fn pending_spool(&self, limit: usize) -> Result<Vec<(i64, Register, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, register, body, authored_by FROM spool
              WHERE processed_at IS NULL
              ORDER BY id ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let register = Register::parse(&r.get::<_, String>(1)?).unwrap_or(Register::Other);
            Ok((r.get(0)?, register, r.get(2)?, r.get(3)?))
        })?;
        rows.collect()
    }

    /// Mark a spool row processed and drop its body. The counts survive; the
    /// prose does not.
    pub fn retire_spool(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE spool SET processed_at = CURRENT_TIMESTAMP, body = '' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Record an exemplar, evicting the weakest if the register is at cap.
    pub fn add_exemplar(&self, register: Register, text: &str, score: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO exemplars (register, text, score) VALUES (?1, ?2, ?3)",
            params![register.as_str(), text, score],
        )?;
        self.conn.execute(
            "DELETE FROM exemplars WHERE id IN (
                 SELECT id FROM exemplars WHERE register = ?1
                  ORDER BY score DESC, id DESC
                  LIMIT -1 OFFSET ?2
             )",
            params![register.as_str(), MAX_EXEMPLARS_PER_REGISTER as i64],
        )?;
        Ok(())
    }

    pub fn exemplars(&self, register: Register) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT text FROM exemplars WHERE register = ?1 ORDER BY score DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![register.as_str()], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    pub fn stats(&self) -> Result<StatsPayload> {
        let words = self
            .conn
            .query_row("SELECT COUNT(*) FROM lexicon", [], |r| r.get(0))?;
        let ngrams = self
            .conn
            .query_row("SELECT COUNT(*) FROM ngrams", [], |r| r.get(0))?;
        let spooled = self.conn.query_row(
            "SELECT COUNT(*) FROM spool WHERE processed_at IS NULL",
            [],
            |r| r.get(0),
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT provenance, COUNT(*) FROM lexicon GROUP BY provenance ORDER BY 2 DESC",
        )?;
        let by_provenance: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        let mut stmt = self.conn.prepare(
            "SELECT register, SUM(count) FROM word_registers GROUP BY register ORDER BY 2 DESC",
        )?;
        let by_register: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        Ok(StatsPayload {
            db: self.path.display().to_string(),
            words,
            ngrams,
            spooled,
            by_provenance,
            by_register,
        })
    }
}

/// Fold provenance and recurrence into one validity score. Recurrence can lift
/// a merely-observed word but never past a deliberate source, so seeing a typo
/// twice doesn't outrank an installed binary.
pub fn validity(provenance: Provenance, count: i64) -> f32 {
    let base = provenance.validity();
    let recurrence = (count as f32 / (count as f32 + 5.0)).min(1.0);
    (base + (1.0 - base) * recurrence * 0.5).min(1.0)
}

/// Default database location: `$XDG_DATA_HOME/vocabulist/lexicon.db`, else
/// `~/.local/share/vocabulist/lexicon.db`.
pub fn default_db_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("vocabulist").join("lexicon.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn upsert_reports_new_then_existing() {
        let s = store();
        assert_eq!(
            s.upsert_word("rubocop", "rubocop", Provenance::Installed, 1)
                .unwrap(),
            (true, false)
        );
        assert_eq!(
            s.upsert_word("rubocop", "rubocop", Provenance::Installed, 1)
                .unwrap(),
            (false, false)
        );
    }

    #[test]
    fn provenance_only_ratchets_upward() {
        let s = store();
        s.upsert_word("contextdb", "contextdb", Provenance::Observed, 1)
            .unwrap();
        let (_, upgraded) = s
            .upsert_word("contextdb", "contextdb", Provenance::Owned, 1)
            .unwrap();
        assert!(upgraded);

        // A weaker sighting must not undo it.
        let (_, upgraded) = s
            .upsert_word("contextdb", "contextdb", Provenance::Observed, 1)
            .unwrap();
        assert!(!upgraded);
        let entries = s.list(Some("contextdb"), 10).unwrap();
        assert_eq!(entries[0].provenance, Provenance::Owned);
    }

    #[test]
    fn retiring_spool_drops_the_prose_but_keeps_the_row() {
        let s = store();
        let id = s
            .spool(Register::Slack, Some("test"), "some private text", "user")
            .unwrap();
        assert_eq!(s.pending_spool(10).unwrap().len(), 1);
        s.retire_spool(id).unwrap();
        assert!(s.pending_spool(10).unwrap().is_empty());

        let body: String = s
            .conn
            .query_row("SELECT body FROM spool WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body, "");
    }

    #[test]
    fn exemplars_stay_capped() {
        let s = store();
        for i in 0..MAX_EXEMPLARS_PER_REGISTER + 10 {
            s.add_exemplar(Register::Prompt, &format!("line {i}"), i as f64)
                .unwrap();
        }
        assert_eq!(
            s.exemplars(Register::Prompt).unwrap().len(),
            MAX_EXEMPLARS_PER_REGISTER
        );
    }

    #[test]
    fn exemplar_eviction_keeps_the_best() {
        let s = store();
        s.add_exemplar(Register::Doc, "weak", 0.1).unwrap();
        for i in 0..MAX_EXEMPLARS_PER_REGISTER {
            s.add_exemplar(Register::Doc, &format!("strong {i}"), 9.0)
                .unwrap();
        }
        assert!(
            !s.exemplars(Register::Doc)
                .unwrap()
                .contains(&"weak".to_string())
        );
    }

    #[test]
    fn validity_never_exceeds_a_stronger_provenance() {
        let observed = validity(Provenance::Observed, 1000);
        assert!(observed < Provenance::Installed.validity());
    }

    #[test]
    fn ngram_counts_sum_across_registers() {
        let s = store();
        s.bump_ngram("ship it", 2, Register::Slack, 3).unwrap();
        s.bump_ngram("ship it", 2, Register::Commit, 2).unwrap();
        assert_eq!(s.ngram_count("ship it").unwrap(), 5);
    }
}
