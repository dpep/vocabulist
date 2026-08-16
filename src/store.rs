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

use crate::types::{Entry, Kind, Provenance, Register, StatsPayload};

/// How many exemplars we keep per register. A voice profile needs real quoted
/// examples — adjectives like "semi-casual" are unfalsifiable — but keeping
/// the whole corpus to get them is exactly what we're avoiding. Bounded by
/// design, not by policy.
pub const MAX_EXEMPLARS_PER_REGISTER: usize = 25;

/// Schema version, stamped into `PRAGMA user_version`. Bump when the schema
/// changes so an old database is recognizable rather than guessed at.
pub const SCHEMA_VERSION: i64 = 5;

/// Sentence lengths above this are counted in the top bucket. Bounds the
/// histogram against a pathological line (a minified file, a wall of text)
/// without distorting the range real prose lives in.
pub const MAX_SENTENCE_BUCKET: i64 = 60;

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
    author TEXT,
    captured_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    processed_at TIMESTAMP
);

-- Which distinct documents a word turned up in.
--
-- Raw occurrence count is a poor test of whether a word is real: ten hits in
-- one message is weaker evidence than three across three days, because typos
-- are bursty -- they repeat inside the one message you typed fast. Real
-- vocabulary recurs across contexts. So validity keys on this table's
-- cardinality, not on lexicon.count.
CREATE TABLE IF NOT EXISTS word_sources (
    word TEXT NOT NULL,
    doc TEXT NOT NULL,
    first_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (word, doc)
);

CREATE INDEX IF NOT EXISTS idx_word_sources_word ON word_sources(word);

-- How common a word is in ordinary English, as distinct from how often *you*
-- use it (which lives in lexicon.count). Seeded from a small embedded core
-- and grown by mining prose already on the machine. Breaks suggestion ties
-- and lets confusion detection work before any personal evidence exists.
-- Keyed by source, which is not incidental. Mined counts are *replaced* on
-- each seed while core counts take a MAX, because seeding recurs — by hand
-- and every 30 days from the Stop hook. Accumulating instead meant a word
-- appearing once in local markdown reached a count of 2 after the second
-- seed, so MIN_CORPUS_EVIDENCE quietly became 1 and every typo in every
-- README graduated to a real word.
CREATE TABLE IF NOT EXISTS frequency (
    word TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'mined',
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (word, source)
);

-- The handles that are *you*, across services. Reading a channel surfaces
-- everyone's messages; without this there's no way to tell which are yours,
-- and capturing the rest would make the voice profile an average of the
-- room.
-- `denied` is why removal is a flag rather than a delete: detection re-runs
-- on every seed and would otherwise resurrect a handle the user rejected.
CREATE TABLE IF NOT EXISTS identities (
    handle TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT 'manual',
    denied INTEGER NOT NULL DEFAULT 0,
    added_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Messages already captured, by stable per-message key.
--
-- Reading the same channel twice is normal and must be idempotent. Without
-- this, a re-read inflates register counts and n-grams — word_sources would
-- dedup, but the voice tables would not, so the same sentence would look like
-- a habit.
CREATE TABLE IF NOT EXISTS captured (
    source_key TEXT PRIMARY KEY,
    captured_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Small key/value corner for facts about the store itself, such as when it
-- was last seeded.
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spool_unprocessed ON spool(processed_at)
    WHERE processed_at IS NULL;

-- Running prose totals per register. Recorded while the text is still here,
-- because processing deletes it -- sentence-level facts cannot be recovered
-- from word counts afterward.
CREATE TABLE IF NOT EXISTS prose_stats (
    register TEXT NOT NULL,
    metric TEXT NOT NULL,
    value INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (register, metric)
);

-- Distribution, not just the mean. Uniform sentence length reads as monotone
-- and high variance reads as conversational, so the shape is the signal --
-- a running average would throw away the interesting half.
CREATE TABLE IF NOT EXISTS sentence_lengths (
    register TEXT NOT NULL,
    length INTEGER NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (register, length)
);
";

/// Bring an existing database up to the current schema.
///
/// `CREATE TABLE IF NOT EXISTS` covers new tables but not new *columns*, so
/// added columns need an explicit ALTER, and re-running one on an
/// already-migrated database is expected.
fn migrate(conn: &Connection) -> Result<()> {
    add_column(conn, "ALTER TABLE spool ADD COLUMN author TEXT")?;
    add_column(
        conn,
        "ALTER TABLE identities ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'",
    )?;
    add_column(
        conn,
        "ALTER TABLE identities ADD COLUMN denied INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

/// Run an idempotent `ADD COLUMN`, tolerating only the duplicate-column error.
///
/// Ignoring *every* error here would be simpler but hides the failures that
/// matter — a typo'd column, a missing table, a locked or corrupt database —
/// and the damage surfaces much later as a query against a column that was
/// never added.
fn add_column(conn: &Connection, sql: &str) -> Result<()> {
    match conn.execute(sql, []) {
        Ok(_) => Ok(()),
        // SQLite reports this only as message text; there's no distinct code.
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e),
    }
}

/// One staged body awaiting processing.
#[derive(Debug, Clone, PartialEq)]
pub struct SpoolRow {
    pub id: i64,
    pub register: Register,
    pub body: String,
    /// `user`, `other`, or `assistant` — who we believe wrote it.
    pub authored_by: String,
    /// Stable per-row document key, for source-diversity counting.
    pub doc: String,
}

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
        migrate(&conn)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))
    }

    /// Run `f` inside one immediate transaction, rolling back on error.
    ///
    /// Two problems, one mechanism. Capture hooks run concurrently, and a
    /// read-then-write upsert would otherwise race; `BEGIN IMMEDIATE` takes
    /// the write lock up front so writers serialize instead. And a run that
    /// dies partway can't leave counts half-applied against a spool row that
    /// still looks unprocessed — which would double-count it on the retry and
    /// quietly corrupt the evidence the real-word thresholds depend on.
    pub fn transaction<T, E>(
        &self,
        f: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f() {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(e) => {
                // Best-effort: the original error is what the caller needs.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
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
        // Kind is derived rather than stored: provenance plus dictionary
        // membership already determine it, and a stored copy would drift.
        let dictionary = crate::dict::load();
        let mut stmt = self.conn.prepare(
            "SELECT l.word, l.provenance, l.count,
                    (SELECT COUNT(*) FROM word_sources s WHERE s.word = l.word)
               FROM lexicon l
              WHERE l.word LIKE ?1
              ORDER BY l.count DESC, l.word ASC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], |r| {
            let word: String = r.get(0)?;
            let prov = Provenance::parse(&r.get::<_, String>(1)?).unwrap_or(Provenance::Observed);
            let count: i64 = r.get(2)?;
            let sources: i64 = r.get(3)?;
            Ok(Entry {
                kind: classify(&word, prov, dictionary.as_ref()),
                word,
                provenance: prov,
                validity: validity(prov, sources),
                count,
                sources,
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
        self.spool_with_author(register, source, body, authored_by, None)
    }

    /// Stage text with an attributed author.
    pub fn spool_with_author(
        &self,
        register: Register,
        source: Option<&str>,
        body: &str,
        authored_by: &str,
        author: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO spool (register, source, body, authored_by, author)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![register.as_str(), source, body, authored_by, author],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Unprocessed spool rows, oldest first.
    ///
    /// The `doc` field identifies the row as a *document* for
    /// source-diversity counting. Each spooled body is its own context — two
    /// Slack messages are independent evidence even though they share a
    /// source label — so the row id is part of the key.
    pub fn pending_spool(&self, limit: usize) -> Result<Vec<SpoolRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, register, body, authored_by, source FROM spool
              WHERE processed_at IS NULL
              ORDER BY id ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            let id: i64 = r.get(0)?;
            let source: Option<String> = r.get(4)?;
            Ok(SpoolRow {
                id,
                register: Register::parse(&r.get::<_, String>(1)?).unwrap_or(Register::Other),
                body: r.get(2)?,
                authored_by: r.get(3)?,
                doc: format!("{}#{id}", source.as_deref().unwrap_or("capture")),
            })
        })?;
        rows.collect()
    }

    /// Delete a spool row once its counts have landed. The counts survive; the
    /// prose does not, and neither does the row — nothing ever reads a
    /// processed row, so keeping a tombstone would be unbounded growth in
    /// exchange for nothing.
    pub fn retire_spool(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM spool WHERE id = ?1", params![id])?;
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

    /// Seconds since the lexicon was last seeded, or `None` if it never was.
    pub fn seconds_since_seed(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT CAST(strftime('%s','now') AS INTEGER) - CAST(value AS INTEGER)
                   FROM meta WHERE key = 'seeded_at'",
                [],
                |r| r.get(0),
            )
            .optional()
    }

    /// Stamp the store as seeded, now.
    pub fn mark_seeded(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('seeded_at', strftime('%s','now'))
             ON CONFLICT(key) DO UPDATE SET value = strftime('%s','now')",
            [],
        )?;
        Ok(())
    }

    /// Record a handle that identifies the user, noting where it came from.
    /// Idempotent, and a manual entry is never overwritten by a detected one.
    pub fn add_identity_from(&self, handle: &str, source: &str) -> Result<bool> {
        // OR IGNORE leaves a denied row denied, which is the point: seeding
        // re-detects everything and must not undo a rejection.
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO identities (handle, source) VALUES (?1, ?2)",
            params![handle.to_lowercase(), source],
        )?;
        Ok(changed > 0)
    }

    /// Record a handle the user named themselves. Naming one explicitly
    /// overrides an earlier rejection.
    pub fn add_identity(&self, handle: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO identities (handle, source, denied) VALUES (?1, 'manual', 0)
             ON CONFLICT(handle) DO UPDATE SET denied = 0, source = 'manual'",
            params![handle.to_lowercase()],
        )?;
        Ok(changed > 0)
    }

    /// Handles with the reason each is believed, for display.
    pub fn identities_with_source(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT handle, source FROM identities WHERE denied = 0 ORDER BY handle")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Reject a handle. Recorded rather than deleted, so the next seed's
    /// detection doesn't bring it back.
    pub fn remove_identity(&self, handle: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT INTO identities (handle, source, denied) VALUES (?1, 'denied', 1)
             ON CONFLICT(handle) DO UPDATE SET denied = 1",
            params![handle.to_lowercase()],
        )? > 0)
    }

    /// Every handle that identifies the user, lowercased.
    pub fn identities(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT handle FROM identities WHERE denied = 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Claim a message key. Returns false if it was already captured, which
    /// is how re-reading a channel stays idempotent.
    pub fn claim_source(&self, key: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO captured (source_key) VALUES (?1)",
            params![key],
        )? > 0)
    }

    /// Replace the mined half of the frequency table.
    ///
    /// A replace rather than an accumulate: the same markdown is re-read on
    /// every seed, and adding to the old counts inflates them without bound.
    pub fn replace_mined_frequencies(
        &self,
        counts: &std::collections::HashMap<String, i64>,
    ) -> Result<()> {
        self.conn
            .execute("DELETE FROM frequency WHERE source = 'mined'", [])?;
        for (word, count) in counts {
            self.conn.execute(
                "INSERT INTO frequency (word, source, count) VALUES (?1, 'mined', ?2)
                 ON CONFLICT(word, source) DO UPDATE SET count = ?2",
                params![word, count],
            )?;
        }
        Ok(())
    }

    /// The whole frequency table, summed across sources.
    pub fn frequencies(&self) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT word, SUM(count) FROM frequency GROUP BY word")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect()
    }

    /// Seed the embedded core list. Idempotent by design — re-seeding takes
    /// the max rather than accumulating, so running `seed` repeatedly doesn't
    /// inflate the prior out of proportion with mined counts.
    pub fn seed_core_frequencies(&self) -> Result<usize> {
        let core = crate::frequency::core_counts();
        for (word, count) in &core {
            self.conn.execute(
                "INSERT INTO frequency (word, source, count) VALUES (?1, 'core', ?2)
                 ON CONFLICT(word, source) DO UPDATE SET count = MAX(count, ?2)",
                params![word, count],
            )?;
        }
        Ok(core.len())
    }

    /// Note that `word` appeared in document `doc`. Idempotent — a word seen
    /// five times in one document still counts as one context.
    pub fn record_word_source(&self, word: &str, doc: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO word_sources (word, doc) VALUES (?1, ?2)",
            params![word, doc],
        )?;
        Ok(())
    }

    /// How many distinct documents a word has appeared in.
    pub fn source_count(&self, word: &str) -> Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM word_sources WHERE word = ?1",
            params![word],
            |r| r.get(0),
        )
    }

    /// Add to a running prose total (`sentences`, `words`, `syllables`).
    pub fn bump_prose(&self, register: Register, metric: &str, by: i64) -> Result<()> {
        if by == 0 {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO prose_stats (register, metric, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(register, metric) DO UPDATE SET value = value + ?3",
            params![register.as_str(), metric, by],
        )?;
        Ok(())
    }

    /// Record one sentence's length, bucketed at [`MAX_SENTENCE_BUCKET`].
    pub fn bump_sentence_length(&self, register: Register, length: i64) -> Result<()> {
        let bucket = length.min(MAX_SENTENCE_BUCKET);
        self.conn.execute(
            "INSERT INTO sentence_lengths (register, length, count) VALUES (?1, ?2, 1)
             ON CONFLICT(register, length) DO UPDATE SET count = count + 1",
            params![register.as_str(), bucket],
        )?;
        Ok(())
    }

    /// Running prose totals, optionally scoped to one register.
    pub fn prose_totals(
        &self,
        register: Option<Register>,
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut out = std::collections::HashMap::new();
        let mut collect = |mut rows: rusqlite::Rows<'_>| -> Result<()> {
            while let Some(row) = rows.next()? {
                let metric: String = row.get(0)?;
                let value: i64 = row.get(1)?;
                *out.entry(metric).or_insert(0) += value;
            }
            Ok(())
        };
        match register {
            Some(r) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT metric, value FROM prose_stats WHERE register = ?1")?;
                collect(stmt.query(params![r.as_str()])?)?;
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT metric, SUM(value) FROM prose_stats GROUP BY metric")?;
                collect(stmt.query([])?)?;
            }
        }
        Ok(out)
    }

    /// Sentence-length histogram as `(length, count)`, optionally scoped.
    pub fn sentence_lengths(&self, register: Option<Register>) -> Result<Vec<(i64, i64)>> {
        let mut out: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
        let mut collect = |mut rows: rusqlite::Rows<'_>| -> Result<()> {
            while let Some(row) = rows.next()? {
                *out.entry(row.get(0)?).or_insert(0) += row.get::<_, i64>(1)?;
            }
            Ok(())
        };
        match register {
            Some(r) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT length, count FROM sentence_lengths WHERE register = ?1")?;
                collect(stmt.query(params![r.as_str()])?)?;
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT length, SUM(count) FROM sentence_lengths GROUP BY length")?;
                collect(stmt.query([])?)?;
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Every n-gram of size `n` with its total count, optionally scoped.
    pub fn ngrams(&self, n: usize, register: Option<Register>) -> Result<Vec<(String, i64)>> {
        let mut out: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut collect = |mut rows: rusqlite::Rows<'_>| -> Result<()> {
            while let Some(row) = rows.next()? {
                *out.entry(row.get(0)?).or_insert(0) += row.get::<_, i64>(1)?;
            }
            Ok(())
        };
        match register {
            Some(r) => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT gram, count FROM ngrams WHERE n = ?1 AND register = ?2")?;
                collect(stmt.query(params![n as i64, r.as_str()])?)?;
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT gram, SUM(count) FROM ngrams WHERE n = ?1 GROUP BY gram")?;
                collect(stmt.query(params![n as i64])?)?;
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Word → occurrence count, optionally scoped to one register.
    ///
    /// Unscoped this reads `lexicon.count`, which includes words seeded from
    /// ground truth at count 0 — those are vocabulary you *have*, not
    /// vocabulary you *used*, so they contribute nothing to frequency and are
    /// dropped here.
    pub fn word_counts(
        &self,
        register: Option<Register>,
    ) -> Result<std::collections::HashMap<String, u64>> {
        let mut out = std::collections::HashMap::new();
        match register {
            Some(r) => {
                let mut stmt = self.conn.prepare(
                    "SELECT word, count FROM word_registers WHERE register = ?1 AND count > 0",
                )?;
                let rows = stmt.query_map(params![r.as_str()], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
                })?;
                for row in rows {
                    let (word, count) = row?;
                    out.insert(word, count);
                }
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT word, count FROM lexicon WHERE count > 0")?;
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
                })?;
                for row in rows {
                    let (word, count) = row?;
                    out.insert(word, count);
                }
            }
        }
        Ok(out)
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
        let by_provenance: std::collections::BTreeMap<String, i64> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        let mut stmt = self.conn.prepare(
            "SELECT register, SUM(count) FROM word_registers GROUP BY register ORDER BY 2 DESC",
        )?;
        let by_register: std::collections::BTreeMap<String, i64> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        // How many bodies each register contributed, which is the question
        // people actually ask — "what has it read?" — and which word counts
        // answer only obliquely.
        let mut stmt = self.conn.prepare(
            "SELECT register, value FROM prose_stats WHERE metric = 'documents' AND value > 0
             ORDER BY value DESC",
        )?;
        let documents: std::collections::BTreeMap<String, i64> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        // Captured message keys carry their origin as a prefix (`slack:`,
        // `gh:`), so the split falls out of the dedup table already kept.
        let mut stmt = self.conn.prepare(
            "SELECT CASE
                 WHEN source_key LIKE 'slack:%' THEN 'slack'
                 WHEN source_key LIKE 'gh:%' THEN 'github'
                 ELSE 'other' END AS origin,
             COUNT(*) FROM captured GROUP BY origin ORDER BY 2 DESC",
        )?;
        let messages: std::collections::BTreeMap<String, i64> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_>>()?;

        Ok(StatsPayload {
            db: self.path.display().to_string(),
            words,
            ngrams,
            spooled,
            by_provenance,
            by_register,
            documents,
            messages,
            integrations: Vec::new(),
        })
    }
}

/// Is this entry ordinary English or a name?
///
/// Derived, not stored. Provenance already answers most of it: anything
/// learned from a repo, tap, binary, or manifest is a name by construction —
/// those sources contain nothing else. What's left is settled by the
/// dictionary, since a word an ordinary dictionary knows is an ordinary word
/// however we happened to learn it.
pub fn classify(
    word: &str,
    provenance: Provenance,
    dictionary: Option<&crate::dict::Dictionary>,
) -> Kind {
    // `rubocop` is a tool *and* a word to its users, but it isn't English.
    // The dictionary is the arbiter, and it outranks provenance because a
    // dependency named `parser` really is the ordinary word.
    if let Some(d) = dictionary
        && crate::dict::contains(d, word)
    {
        return Kind::Word;
    }
    match provenance {
        Provenance::Owned | Provenance::Tap | Provenance::Installed | Provenance::Dependency => {
            Kind::Name
        }
        // Typed by hand or seen in prose: assume ordinary unless the
        // dictionary said otherwise above.
        Provenance::User | Provenance::Observed => Kind::Word,
    }
}

/// Fold provenance and corroboration into one validity score.
///
/// `sources` is the number of *distinct documents* a word appeared in, not
/// how many times it appeared. That's the whole point: a typo hammered five
/// times into one message is one context and stays weak, while a word seen
/// once each in three places is corroborated. Corroboration can lift a
/// merely-observed word but never past a deliberate source, so no amount of
/// repetition outranks an installed binary.
pub fn validity(provenance: Provenance, sources: i64) -> f32 {
    let base = provenance.validity();
    // Saturates faster than a count-based curve would, because independent
    // contexts are much scarcer — and much stronger — than raw occurrences.
    let corroboration = (sources as f32 / (sources as f32 + 2.0)).min(1.0);
    (base + (1.0 - base) * corroboration * 0.5).min(1.0)
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
    fn retiring_spool_removes_the_prose_entirely() {
        let s = store();
        let id = s
            .spool(Register::Slack, Some("test"), "some private text", "user")
            .unwrap();
        assert_eq!(s.pending_spool(10).unwrap().len(), 1);
        s.retire_spool(id).unwrap();
        assert!(s.pending_spool(10).unwrap().is_empty());

        let remaining: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM spool", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn a_rejected_identity_survives_re_detection() {
        let s = store();
        s.add_identity_from("work@example.com", "commits").unwrap();
        assert!(s.identities().unwrap().contains("work@example.com"));

        s.remove_identity("work@example.com").unwrap();
        assert!(!s.identities().unwrap().contains("work@example.com"));

        // Seeding re-detects everything; the rejection has to outlast it.
        s.add_identity_from("work@example.com", "commits").unwrap();
        assert!(
            !s.identities().unwrap().contains("work@example.com"),
            "detection must not resurrect a rejected handle"
        );
    }

    #[test]
    fn naming_a_handle_explicitly_overrides_an_earlier_rejection() {
        let s = store();
        s.remove_identity("dpep").unwrap();
        s.add_identity("dpep").unwrap();
        assert!(s.identities().unwrap().contains("dpep"));
    }

    #[test]
    fn re_mining_the_same_corpus_does_not_inflate_counts() {
        // Seeding recurs — by hand and monthly from the Stop hook. Adding to
        // the previous counts made a word seen once reach the evidence
        // threshold on the second pass, quietly turning every README typo
        // into a real word.
        let s = store();
        let counts: std::collections::HashMap<String, i64> =
            [("zzzunique".to_string(), 1i64)].into_iter().collect();
        s.replace_mined_frequencies(&counts).unwrap();
        s.replace_mined_frequencies(&counts).unwrap();
        s.replace_mined_frequencies(&counts).unwrap();
        assert_eq!(s.frequencies().unwrap()["zzzunique"], 1);
    }

    #[test]
    fn schema_version_is_stamped() {
        assert_eq!(store().schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_failed_transaction_leaves_nothing_behind() {
        let s = store();
        let result: Result<(), rusqlite::Error> = s.transaction(|| {
            s.upsert_word("halfway", "halfway", Provenance::Observed, 1)?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(result.is_err());
        assert!(!s.contains("halfway").unwrap());
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
    fn one_document_counts_once_however_often_the_word_repeats() {
        let s = store();
        for _ in 0..8 {
            s.record_word_source("zblorg", "slack#1").unwrap();
        }
        // Bursty repetition inside one message is one context, not eight.
        assert_eq!(s.source_count("zblorg").unwrap(), 1);

        s.record_word_source("zblorg", "slack#2").unwrap();
        assert_eq!(s.source_count("zblorg").unwrap(), 2);
    }

    #[test]
    fn corroboration_beats_repetition() {
        // A typo hammered into one message versus a word seen in three
        // separate places: the second is the one that should be trusted.
        let bursty = validity(Provenance::Observed, 1);
        let corroborated = validity(Provenance::Observed, 3);
        assert!(corroborated > bursty);
    }

    #[test]
    fn spool_rows_are_distinct_documents_even_from_one_source() {
        let s = store();
        s.spool(Register::Slack, Some("slack"), "first message", "user")
            .unwrap();
        s.spool(Register::Slack, Some("slack"), "second message", "user")
            .unwrap();
        let docs: Vec<String> = s
            .pending_spool(10)
            .unwrap()
            .into_iter()
            .map(|r| r.doc)
            .collect();
        assert_eq!(docs.len(), 2);
        assert_ne!(docs[0], docs[1], "two messages are independent evidence");
    }

    #[test]
    fn ngram_counts_sum_across_registers() {
        let s = store();
        s.bump_ngram("ship it", 2, Register::Slack, 3).unwrap();
        s.bump_ngram("ship it", 2, Register::Commit, 2).unwrap();
        assert_eq!(s.ngram_count("ship it").unwrap(), 5);
    }
}
