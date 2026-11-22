//! Draining the spool: staged text in, counts out, prose gone.
//!
//! This is the one place that decides what a captured body is allowed to
//! teach. Capture (`hook`, `capture`, `ingest`) only stages text and records
//! who wrote it; the authorship rule — vocabulary from anyone, voice only from
//! you — is applied here, once, so a new capture path cannot bypass it by
//! forgetting to.

use std::collections::{HashMap, HashSet};

use crate::ngram;
use crate::store::Store;
use crate::text;
use crate::types::{Provenance, Register};
use crate::watermark;

/// Drain the spool into counts. Words, per-register frequencies, n-grams, and
/// a bounded exemplar sample survive; the prose itself does not.
pub fn process_spool(store: &Store, limit: usize) -> Result<usize, Box<dyn std::error::Error>> {
    process_rows(store, store.pending_spool(limit)?)
}

fn process_rows(
    store: &Store,
    pending: Vec<crate::store::SpoolRow>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut processed = 0;
    for row in pending {
        // One transaction per row: the counts and the retirement land together
        // or not at all, so an interrupted run can't re-apply a row it already
        // half-counted. The row is claimed inside it, because another drainer
        // may have taken it, or re-attributed it, since the snapshot.
        let counted = store.transaction(|| -> Result<bool, Box<dyn std::error::Error>> {
            let Some(authored_by) = store.claim_spool(row.id)? else {
                return Ok(false);
            };
            process_one(store, row.register, &row.body, &authored_by, &row.doc)?;
            Ok(true)
        })?;
        processed += usize::from(counted);
    }
    Ok(processed)
}

/// Fold one claimed spool row into counts. Called inside a transaction.
fn process_one(
    store: &Store,
    register: Register,
    body: &str,
    authored_by: &str,
    doc: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // One rule for everything you didn't write, whether a colleague or an
    // assistant wrote it: it corroborates that a word is *real* without
    // saying anything about how *you* write. It reaches the lexicon and the
    // source-diversity table and stops there — no register counts, no
    // collocations, no exemplars, no prose stats.
    //
    // Assistant drafts are worth keeping for the same reason a colleague's
    // are: they're about your work and carry your project's jargon. Knowing
    // `utilize` is a word doesn't make you write it, because vocabulary and
    // voice are separate axes here. What must not happen is the diction
    // being fed back as yours, and excluding it from the voice tables is
    // what prevents that.
    //
    // Note this uses the **whole** body, not a trailer-stripped one. Trailer
    // stripping salvages a human body carrying an appended attribution;
    // there's nothing to salvage here, and a marker that prefixes the message
    // rather than following it — `claudomatic:` — would strip everything.
    // Counted for everything processed, not just your own writing: the
    // question this answers is "what has it read", and a colleague's message
    // was still read.
    store.bump_prose(register, "documents", 1)?;

    // A transcript is the user's words in a recognizer's spelling, the
    // mirror image of a colleague's text: evidence of what they say, none of
    // how a word is spelled. So it reaches the speech register's counts and
    // stops there. Not `lexicon.count`, which is the written total the veto
    // weighs. No `word_sources`, which is what trust counts, or a
    // recognizer's recurring mishearing would earn trust, and soundly would
    // then correct toward it. No n-grams either: they are read across registers as
    // real-word evidence, and homophones are exactly where a recognizer
    // picks the spelling. Sentence shape and exemplars are its punctuation,
    // not the user's (PLAN §6a).
    if register == Register::Speech {
        if authored_by == "user" {
            for word in body.lines().flat_map(text::prose_words) {
                store.upsert_word(&word, &word, Provenance::Observed, 0)?;
                store.bump_register(&word, register, 1)?;
            }
        }
        return Ok(());
    }

    if authored_by != "user" {
        // The one thing an assistant's text is allowed to shape: the
        // baseline a draft written as you is scored against (voice.rs). It is
        // stored under its own author and never read as yours.
        if authored_by == "assistant" {
            let writing = watermark::without_markers(body);
            store.bump_voice(register, "assistant", &crate::voice::measure(&writing))?;
        }
        for word in body.lines().flat_map(text::prose_words) {
            store.upsert_word(&word, &word, Provenance::Observed, 0)?;
            store.record_word_source(&word, doc)?;
        }
        return Ok(());
    }

    // Your own text: drop any appended attribution, then learn voice from it.
    let body = watermark::strip_trailer(body);

    for line in body.lines() {
        let Some(tokens) = text::prose_tokens(line) else {
            continue;
        };

        for word in tokens.iter().filter(|w| text::is_lexical(w)) {
            store.upsert_word(word, word, Provenance::Observed, 1)?;
            store.bump_register(word, register, 1)?;
            store.record_word_source(word, doc)?;
        }

        // N-grams are built over *runs* of ordinary words, so a token that
        // isn't one both stays out of the phrase and breaks the sequence.
        //
        // Filtering the junk out first would have invented adjacencies that
        // were never written — "ship 42 widgets" would yield "ship widgets".
        // Keeping the junk in was worse: a session id and a path fragment
        // became a top-ranked phrase, because a UUID appearing twice is a
        // wildly surprising collocation by any association measure.
        for run in tokens.split(|t| !text::is_lexical(t)) {
            record_phrases(store, register, run)?;
        }
        if tokens.len() >= 6 {
            store.add_exemplar(
                register,
                text::normalize_typography(line).trim(),
                tokens.len() as f64,
            )?;
        }
    }

    // Sentence-level facts, recorded now because the prose is about to be
    // deleted and none of this can be derived from word counts later.
    record_prose_shape(store, register, body)?;
    store.bump_voice(register, "user", &crate::voice::measure(body))?;
    Ok(())
}

/// The longest phrase worth following. Five words is where an English
/// sentence's characteristic constructions live — "as far as I can", "at the
/// end of the" — and beyond it repetition is rare enough to be quotation.
const MAX_PHRASE: usize = 5;

/// How often a phrase must have been seen before its extensions are tracked.
const RECURRENCE: i64 = 2;

/// Count the phrases in one run of ordinary words.
///
/// Pairs are always counted. Longer phrases are counted only when the phrase
/// one word shorter has already recurred — the *apriori* property, which holds
/// because a phrase can never appear more often than its own prefix. Anything
/// whose prefix was seen once cannot itself be seen twice, so following it
/// costs a row and can never yield a collocation.
///
/// That distinction is the whole reason longer phrases are affordable.
/// Measured on ordinary prose, 13.9% of pairs recur and only 1.3% of
/// five-word runs do; storing them all would be four times the rows to surface
/// an eighth as many phrases. Gating on the prefix keeps the rows that can
/// still become evidence and drops the rest before they are written.
///
/// The cost is a one-occurrence lag, and it is worth stating precisely: a
/// phrase is counted only from the point its prefix recurred, so for n >= 3
/// the stored count is *at least one lower* than the true one. A phrase said
/// twice is stored once. `phrases --min-count 2` on a long phrase therefore
/// means "said at least three times", which is the right bar anyway for a
/// claim about habitual phrasing.
fn record_phrases(
    store: &Store,
    register: Register,
    run: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    for gram in ngram::ngrams(run, 2) {
        store.bump_ngram(&gram, 2, register, 1)?;
    }

    for n in 3..=MAX_PHRASE {
        if run.len() < n {
            break;
        }
        let mut extended = false;
        for gram in ngram::ngrams(run, n) {
            // The prefix is this phrase without its last word. Counted across
            // registers on purpose: this decides whether a phrase is worth
            // following at all, which is a question about the language rather
            // than about one voice.
            let Some((prefix, _)) = gram.rsplit_once(' ') else {
                continue;
            };
            if store.ngram_count(prefix)? < RECURRENCE {
                continue;
            }
            store.bump_ngram(&gram, n, register, 1)?;
            extended = true;
        }
        // Nothing at this length qualified, so nothing longer can either —
        // every longer phrase has one of these as its prefix.
        if !extended {
            break;
        }
    }
    Ok(())
}

/// One body's sentence lengths, and its word and syllable totals.
fn prose_shape(body: &str) -> (Vec<i64>, i64, i64) {
    let mut lengths = Vec::new();
    let mut syllables = 0i64;
    for sentence in crate::complexity::split_sentences(body) {
        let normalized = text::normalize_typography(&sentence);
        let masked = text::mask_non_prose(&normalized);
        let words: Vec<String> = text::tokenize(&masked)
            .iter()
            .map(|t| text::normalize(&t.text))
            .filter(|w| w.chars().any(char::is_alphabetic))
            .collect();
        if words.is_empty() {
            continue;
        }
        lengths.push(words.len() as i64);
        syllables += words
            .iter()
            .map(|w| crate::complexity::count_syllables(w) as i64)
            .sum::<i64>();
    }
    let words = lengths.iter().sum();
    (lengths, words, syllables)
}

/// Fold one body's sentence shape into the running per-register stats.
fn record_prose_shape(
    store: &Store,
    register: Register,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (lengths, words, syllables) = prose_shape(body);
    for &length in &lengths {
        store.bump_sentence_length(register, length)?;
    }
    store.bump_prose(register, "sentences", lengths.len() as i64)?;
    store.bump_prose(register, "words", words)?;
    store.bump_prose(register, "syllables", syllables)?;
    Ok(())
}

/// What [`unlearn`] took back, or would.
#[derive(Debug, Default, PartialEq)]
pub struct UnlearnReport {
    pub texts: usize,
    /// Texts that matched nothing learned, so took nothing: never learned,
    /// already taken back, or fed under the wrong register.
    pub unmatched: usize,
    /// Phrase counts taken back, summed over phrases.
    pub phrase_counts: i64,
    /// Phrases whose count reached zero, so they are gone.
    pub phrases_emptied: usize,
    pub word_counts: i64,
    /// Documents taken off the words they appeared in, which is what trust
    /// counts.
    pub word_sources: i64,
    /// Style marker counts taken back, including their denominators.
    pub voice_counts: i64,
    pub exemplars: usize,
    /// The phrases that lost the most, for reading rather than counting.
    pub sample: Vec<(String, i64)>,
}

/// One text to take back, attributed as processing attributed it.
pub struct Learned {
    pub register: Register,
    /// `user`, `assistant` or `other`, as [`crate::ingest::Record::authored_by`].
    pub authored_by: &'static str,
    pub body: String,
}

/// What processing added for some texts, keyed as the store keys it.
#[derive(Default)]
struct Tally {
    grams: HashMap<(String, Register), i64>,
    /// `word_registers`.
    registers: HashMap<(String, Register), i64>,
    /// `lexicon.count`, the written total.
    counts: HashMap<String, i64>,
    /// `word_sources` rows: one per document a word appeared in.
    sources: HashMap<String, i64>,
    exemplars: Vec<(Register, String)>,
    lengths: HashMap<(Register, i64), i64>,
    prose: HashMap<(Register, &'static str), i64>,
    voice: HashMap<(Register, &'static str), crate::voice::Counts>,
}

impl Tally {
    /// Mirrors [`process_one`], branch for branch.
    fn add(&mut self, text: &Learned) {
        let register = text.register;
        let body = text.body.as_str();
        *self.prose.entry((register, "documents")).or_default() += 1;
        let distinct = |words: Vec<String>| words.into_iter().collect::<HashSet<_>>();

        if register == Register::Speech {
            if text.authored_by == "user" {
                for word in body.lines().flat_map(text::prose_words) {
                    *self.registers.entry((word, register)).or_default() += 1;
                }
            }
            return;
        }

        if text.authored_by != "user" {
            if text.authored_by == "assistant" {
                let writing = watermark::without_markers(body);
                crate::voice::add(
                    self.voice.entry((register, "assistant")).or_default(),
                    &crate::voice::measure(&writing),
                );
            }
            for word in distinct(body.lines().flat_map(text::prose_words).collect()) {
                *self.sources.entry(word).or_default() += 1;
            }
            return;
        }

        let body = watermark::strip_trailer(body);
        let mut seen = HashSet::new();
        for line in body.lines() {
            let Some(tokens) = text::prose_tokens(line) else {
                continue;
            };
            for word in tokens.iter().filter(|w| text::is_lexical(w)) {
                *self.registers.entry((word.clone(), register)).or_default() += 1;
                *self.counts.entry(word.clone()).or_default() += 1;
                seen.insert(word.clone());
            }
            for run in tokens.split(|t| !text::is_lexical(t)) {
                for n in 2..=MAX_PHRASE.min(run.len()) {
                    for gram in ngram::ngrams(run, n) {
                        *self.grams.entry((gram, register)).or_default() += 1;
                    }
                }
            }
            if tokens.len() >= 6 {
                self.exemplars.push((
                    register,
                    text::normalize_typography(line).trim().to_string(),
                ));
            }
        }
        for word in seen {
            *self.sources.entry(word).or_default() += 1;
        }
        let (shape, word_total, syllables) = prose_shape(body);
        for length in &shape {
            *self.lengths.entry((register, *length)).or_default() += 1;
        }
        *self.prose.entry((register, "sentences")).or_default() += shape.len() as i64;
        *self.prose.entry((register, "words")).or_default() += word_total;
        *self.prose.entry((register, "syllables")).or_default() += syllables;
        crate::voice::add(
            self.voice.entry((register, "user")).or_default(),
            &crate::voice::measure(body),
        );
    }

    /// Take these counts back from the store, or only measure what would
    /// go. Document counts are taken but not reported: every text has one.
    fn take(
        &self,
        store: &Store,
        apply: bool,
    ) -> Result<UnlearnReport, Box<dyn std::error::Error>> {
        use crate::store::Unlearn;

        let mut report = UnlearnReport::default();
        let mut lost: HashMap<String, i64> = HashMap::new();
        for ((gram, register), by) in &self.grams {
            let (taken, emptied) = store.unlearn(Unlearn::Gram(gram, *register), *by, apply)?;
            if taken > 0 {
                report.phrase_counts += taken;
                *lost.entry(gram.clone()).or_default() += taken;
                report.phrases_emptied += usize::from(emptied);
            }
        }
        for ((word, register), by) in &self.registers {
            report.word_counts += store
                .unlearn(Unlearn::WordIn(word, *register), *by, apply)?
                .0;
        }
        for (word, by) in &self.counts {
            store.unlearn(Unlearn::Word(word), *by, apply)?;
        }
        for (word, by) in &self.sources {
            report.word_sources += store.unlearn_sources(word, *by, apply)?;
        }
        for (register, line) in &self.exemplars {
            report.exemplars += store.unlearn_exemplar(*register, line, apply)?;
        }
        for ((register, length), by) in &self.lengths {
            store.unlearn(Unlearn::SentenceLength(*register, *length), *by, apply)?;
        }
        for ((register, metric), by) in &self.prose {
            store.unlearn(Unlearn::Prose(*register, metric), *by, apply)?;
        }
        for ((register, author), counts) in &self.voice {
            for (marker, by) in counts {
                report.voice_counts += store
                    .unlearn(Unlearn::Voice(*register, author, marker), *by, apply)?
                    .0;
            }
        }

        let mut sample: Vec<(String, i64)> = lost.into_iter().collect();
        sample.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sample.truncate(15);
        report.sample = sample;
        Ok(report)
    }
}

/// Take back what processing learned from these texts.
///
/// For a capture leak found after the fact: the prose is gone, but the text
/// that leaked usually is not — a relayed notification is still in the
/// transcript — so feeding it back here subtracts exactly its counts, where
/// `rm --phrase` deletes a shared phrase like `merged by` outright. Each text
/// takes back what its authorship let it teach: your own text its voice and
/// phrases, someone else's only the documents its words appeared in.
///
/// Counts never go below zero, but nothing records which counts a text
/// added, so text that was never learned — or is fed twice — still takes from
/// the phrases it shares with yours. Feed only what leaked, once. Every
/// phrase length is taken back, though longer ones were counted only once
/// their prefix recurred; the floor absorbs that. Words stay in the lexicon
/// (a word in a leaked line is still a word), but lose the documents the
/// text gave them, so a word only the leak vouched for is no longer trusted.
pub fn unlearn(
    store: &Store,
    texts: &[Learned],
    apply: bool,
) -> Result<UnlearnReport, Box<dyn std::error::Error>> {
    let mut all = Tally::default();
    let mut unmatched = 0;
    for text in texts {
        let mut one = Tally::default();
        one.add(text);
        let would = one.take(store, false)?;
        // Judged by the counts kept per register, since the usual miss is
        // the wrong one. Someone else's text kept none to judge by.
        let matched = text.authored_by == "other"
            || would.phrase_counts + would.word_counts + would.voice_counts > 0;
        // A text that matched nothing takes nothing, not even the counts
        // kept across registers, so feeding it again under the right
        // register is safe.
        if matched {
            all.add(text);
        } else {
            unmatched += 1;
        }
    }
    let mut report = all.take(store, apply)?;
    report.texts = texts.len();
    report.unmatched = unmatched;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_text_corroborates_vocabulary_but_not_voice() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Pr,
                Some("pr"),
                "Some prose here about widgets\n\nCo-Authored-By: Claude <x>",
                "assistant",
            )
            .unwrap();
        assert_eq!(process_spool(&store, 10).unwrap(), 1);

        // The word is evidence — it's about your work and carries your jargon.
        assert!(store.contains("widgets").unwrap());
        // The phrasing is not yours and must not shape the voice tables.
        assert_eq!(store.ngram_count("about widgets").unwrap(), 0);
        assert_eq!(
            store
                .prose_totals(None)
                .unwrap()
                .get("sentences")
                .copied()
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn a_row_another_drainer_took_is_not_counted_twice() {
        // Two drainers (a Stop hook and `vocab process`) snapshot the same
        // pending rows; only the first to reach a row may count it, and it
        // counts it as attributed *now*, not as the stale snapshot says.
        let path = std::env::temp_dir().join(format!("vocab-drain-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (a, b) = (Store::open(&path).unwrap(), Store::open(&path).unwrap());
        a.spool(
            Register::Slack,
            None,
            "ship the small focused change",
            "user",
        )
        .unwrap();
        let peer = a
            .spool(Register::Prompt, Some("s"), "the zblorg widget", "user")
            .unwrap();
        let stale = a.pending_spool(10).unwrap();

        b.set_spool_author(peer, "other").unwrap();
        assert_eq!(process_spool(&b, 1).unwrap(), 1);
        assert_eq!(process_rows(&a, stale).unwrap(), 1);

        assert_eq!(a.ngram_count("small focused").unwrap(), 1);
        assert_eq!(a.ngram_count("zblorg widget").unwrap(), 0);
        assert!(a.pending_spool(10).unwrap().is_empty());
        drop((a, b));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn processing_learns_words_and_ngrams_from_your_own_text() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Slack,
                None,
                "ship the small focused change",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert!(store.contains("focused").unwrap());
        assert_eq!(store.ngram_count("small focused").unwrap(), 1);
    }

    #[test]
    fn text_from_others_corroborates_without_shaping_voice() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool_with_author(
                Register::Pr,
                Some("pr"),
                "the zblorg handles retries",
                "other",
                Some("colleague"),
            )
            .unwrap();
        process_spool(&store, 10).unwrap();

        // Their word counts as evidence the word is real...
        assert!(store.contains("zblorg").unwrap());
        assert_eq!(store.source_count("zblorg").unwrap(), 1);
        // ...but says nothing about how *you* write.
        assert_eq!(store.ngram_count("the zblorg").unwrap(), 0);
        let totals = store.prose_totals(None).unwrap();
        assert_eq!(totals.get("sentences").copied().unwrap_or(0), 0);
    }

    #[test]
    fn your_own_text_feeds_both_evidence_and_voice() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Slack,
                Some("slack"),
                "the zblorg ships today",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();

        assert_eq!(store.source_count("zblorg").unwrap(), 1);
        assert_eq!(store.ngram_count("the zblorg").unwrap(), 1);
        assert!(store.prose_totals(None).unwrap()["sentences"] > 0);
    }

    #[test]
    fn speech_counts_what_you_say_but_never_how_it_is_spelled() {
        let store = Store::open(":memory:").unwrap();
        let line = "the zblorg ships today and the zblorg ships tomorrow too";
        store
            .spool(Register::Speech, Some("soundly:a"), line, "user")
            .unwrap();
        process_spool(&store, 10).unwrap();

        assert_eq!(
            store.word_counts(Some(Register::Speech)).unwrap()["zblorg"],
            2
        );
        // Never summed into the written total.
        assert!(!store.word_counts(None).unwrap().contains_key("zblorg"));
        // Trust counts documents, and a transcript is not one.
        assert_eq!(store.source_count("zblorg").unwrap(), 0);
        assert_eq!(store.ngram_count("the zblorg").unwrap(), 0);
        assert!(store.exemplars(Register::Speech).unwrap().is_empty());
        let totals = store.prose_totals(Some(Register::Speech)).unwrap();
        assert_eq!(totals.get("sentences").copied().unwrap_or(0), 0);
        assert_eq!(totals["documents"], 1);
    }

    #[test]
    fn someone_elses_speech_teaches_nothing() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool_with_author(Register::Speech, None, "the zblorg", "other", Some("x"))
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert!(!store.contains("zblorg").unwrap());
    }

    #[test]
    fn a_human_message_keeps_its_prose_despite_a_trailer() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Commit,
                None,
                "Fix the flaky widget spec\n\nCo-Authored-By: Claude <x>",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert!(store.contains("widget").unwrap());
        assert!(!store.contains("claude").unwrap());
    }

    #[test]
    fn a_phrase_is_followed_only_once_its_prefix_recurs() {
        let store = Store::open(":memory:").unwrap();
        let say = |text: &str| {
            store.spool(Register::Doc, None, text, "user").unwrap();
            process_spool(&store, 10).unwrap();
        };

        // First sighting: the pair is counted, the triple is not, because
        // nothing yet suggests this pair is anything but an accident.
        say("we should ship the small focused change");
        assert_eq!(store.ngram_count("small focused").unwrap(), 1);
        assert_eq!(store.ngram_count("small focused change").unwrap(), 0);

        // Second: the pair has now recurred, so the triple starts being
        // followed from here on.
        say("we should ship the small focused change");
        assert_eq!(store.ngram_count("small focused").unwrap(), 2);
        assert_eq!(store.ngram_count("small focused change").unwrap(), 1);

        // And a fourth word follows once the triple has itself recurred.
        say("we should ship the small focused change today");
        say("we should ship the small focused change today");
        assert!(store.ngram_count("small focused change today").unwrap() >= 1);
    }

    #[test]
    fn a_phrase_said_once_costs_nothing_beyond_its_pairs() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Doc,
                None,
                "an entirely unrepeated sequence of words",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        // Every triple in it would be a row that can never become evidence.
        assert_eq!(
            store.ngram_count("entirely unrepeated sequence").unwrap(),
            0
        );
    }

    #[test]
    fn junk_tokens_stay_out_of_phrases() {
        // A session id appearing twice is a wildly surprising collocation by
        // any association measure, so it ranked above real phrases.
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Doc,
                None,
                "the run a0d376be-04f5 finished cleanly",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert_eq!(store.ngram_count("run a0d376be-04f5").unwrap(), 0);
        assert_eq!(store.ngram_count("a0d376be-04f5 finished").unwrap(), 0);
        // ...and the words on either side are still learned.
        assert_eq!(store.ngram_count("the run").unwrap(), 1);
        assert_eq!(store.ngram_count("finished cleanly").unwrap(), 1);
    }

    #[test]
    fn a_dropped_token_never_joins_its_neighbors() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(Register::Slack, None, "ship 42 widgets", "user")
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert_eq!(store.ngram_count("ship widgets").unwrap(), 0);
    }

    #[test]
    fn an_assistant_trailer_is_not_part_of_the_baseline() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Pr,
                Some("pr"),
                "Ship the widget.\n\nCo-Authored-By: Claude <noreply@example.com>",
                "assistant",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        let baseline = store.voice(Register::Pr, "assistant").unwrap();
        assert_eq!(baseline.get("sentences"), Some(&1));
        assert_eq!(baseline.get("colon"), None);
        assert_eq!(baseline.get("unterminated"), None);
    }

    #[test]
    fn unlearning_speech_leaves_the_written_total_alone() {
        let store = Store::open(":memory:").unwrap();
        store
            .upsert_word("widget", "widget", Provenance::Observed, 3)
            .unwrap();
        store
            .spool(Register::Speech, None, "the widget works", "user")
            .unwrap();
        process_spool(&store, 10).unwrap();
        unlearn(
            &store,
            &[learned(Register::Speech, "user", "the widget works")],
            true,
        )
        .unwrap();
        let count: i64 = store
            .list(None, usize::MAX)
            .unwrap()
            .into_iter()
            .find(|e| e.word == "widget")
            .map(|e| e.count)
            .unwrap();
        assert_eq!(count, 3);
    }

    fn learned(register: Register, authored_by: &'static str, body: &str) -> Learned {
        Learned {
            register,
            authored_by,
            body: body.into(),
        }
    }

    fn trusted(store: &Store, word: &str) -> bool {
        store.trust().unwrap().trusts(word, |_| false)
    }

    #[test]
    fn unlearning_someone_elses_text_takes_back_only_what_it_corroborated() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(Register::Prompt, None, "run the zblorg widget", "user")
            .unwrap();
        for _ in 0..2 {
            store
                .spool(Register::Prompt, None, "run the zblorg widget", "other")
                .unwrap();
        }
        process_spool(&store, 10).unwrap();
        assert!(trusted(&store, "zblorg"));
        let before = store.voice(Register::Prompt, "user").unwrap();

        let report = unlearn(
            &store,
            &[
                learned(Register::Prompt, "other", "run the zblorg widget"),
                learned(Register::Prompt, "other", "run the zblorg widget"),
            ],
            true,
        )
        .unwrap();
        assert_eq!(report.phrase_counts, 0);
        assert_eq!(report.word_counts, 0);
        assert_eq!(report.voice_counts, 0);
        assert_eq!(store.voice(Register::Prompt, "user").unwrap(), before);
        assert_eq!(store.ngram_count("zblorg widget").unwrap(), 1);
        // Your one document is all that vouches for it now.
        assert_eq!(store.source_count("zblorg").unwrap(), 1);
        assert!(!trusted(&store, "zblorg"));
    }

    #[test]
    fn a_word_only_a_leak_taught_is_no_longer_trusted() {
        let store = Store::open(":memory:").unwrap();
        for body in ["widgetbot says zblorg", "widgetbot says quux"] {
            store.spool(Register::Prompt, None, body, "user").unwrap();
        }
        process_spool(&store, 10).unwrap();
        assert!(trusted(&store, "widgetbot"));

        let texts = [
            learned(Register::Prompt, "user", "widgetbot says zblorg"),
            learned(Register::Prompt, "user", "widgetbot says quux"),
        ];
        let report = unlearn(&store, &texts, true).unwrap();
        assert_eq!(report.unmatched, 0);
        assert!(!trusted(&store, "widgetbot"));
        // Taken back already, so a second pass matches nothing.
        assert_eq!(unlearn(&store, &texts, false).unwrap().unmatched, 2);
    }
}
