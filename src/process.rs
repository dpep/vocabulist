//! Draining the spool: staged text in, counts out, prose gone.
//!
//! This is the one place that decides what a captured body is allowed to
//! teach. Capture (`hook`, `capture`, `ingest`) only stages text and records
//! who wrote it; the authorship rule — vocabulary from anyone, voice only from
//! you — is applied here, once, so a new capture path cannot bypass it by
//! forgetting to.

use crate::ngram;
use crate::store::Store;
use crate::text;
use crate::types::{Provenance, Register};
use crate::watermark;

/// Drain the spool into counts. Words, per-register frequencies, n-grams, and
/// a bounded exemplar sample survive; the prose itself does not.
pub fn process_spool(store: &Store, limit: usize) -> Result<usize, Box<dyn std::error::Error>> {
    let pending = store.pending_spool(limit)?;
    let mut processed = 0;

    for row in pending {
        // One transaction per row: the counts and the retirement land together
        // or not at all, so an interrupted run can't re-apply a row it already
        // half-counted.
        store.transaction(|| -> Result<(), Box<dyn std::error::Error>> {
            process_one(
                store,
                row.id,
                row.register,
                &row.body,
                &row.authored_by,
                &row.doc,
            )
        })?;
        processed += 1;
    }
    Ok(processed)
}

/// Fold one spool row into counts and retire it. Called inside a transaction.
fn process_one(
    store: &Store,
    id: i64,
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
        store.retire_spool(id)?;
        return Ok(());
    }

    if authored_by != "user" {
        for word in body.lines().flat_map(text::prose_words) {
            store.upsert_word(&word, &word, Provenance::Observed, 0)?;
            store.record_word_source(&word, doc)?;
        }
        store.retire_spool(id)?;
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
    store.retire_spool(id)?;
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
    /// Phrase counts taken back, summed over phrases.
    pub phrase_counts: i64,
    /// Phrases whose count reached zero, so they are gone.
    pub phrases_emptied: usize,
    pub word_counts: i64,
    pub exemplars: usize,
    /// The phrases that lost the most, for reading rather than counting.
    pub sample: Vec<(String, i64)>,
}

/// Take back what processing learned from these texts, as if they had been
/// written by someone else.
///
/// For a capture leak found after the fact: the prose is gone, but the text
/// that leaked usually is not — a relayed notification is still in the
/// transcript — so feeding it back here subtracts exactly its counts, where
/// `rm --phrase` deletes a shared phrase like `merged by` outright.
///
/// Counts never go below zero, but nothing records which counts a text
/// added, so text that was never learned — or is fed twice — still takes from
/// the phrases it shares with yours. Feed only what leaked, once. Every
/// phrase length is taken back, though longer ones were counted only once
/// their prefix recurred; the floor absorbs that. Words
/// stay in the lexicon (a word in a leaked line is still a word), as does the
/// record of which documents a word appeared in.
pub fn unlearn(
    store: &Store,
    texts: &[(Register, String)],
    apply: bool,
) -> Result<UnlearnReport, Box<dyn std::error::Error>> {
    use crate::store::Unlearn;
    use std::collections::HashMap;

    let mut grams: HashMap<(String, Register), i64> = HashMap::new();
    let mut words: HashMap<(String, Register), i64> = HashMap::new();
    let mut exemplars = Vec::new();
    let mut lengths: HashMap<(Register, i64), i64> = HashMap::new();
    let mut prose: HashMap<(Register, &str), i64> = HashMap::new();

    for (register, body) in texts {
        let register = *register;
        let body = watermark::strip_trailer(body);
        for line in body.lines() {
            let Some(tokens) = text::prose_tokens(line) else {
                continue;
            };
            for word in tokens.iter().filter(|w| text::is_lexical(w)) {
                *words.entry((word.clone(), register)).or_default() += 1;
            }
            for run in tokens.split(|t| !text::is_lexical(t)) {
                for n in 2..=MAX_PHRASE.min(run.len()) {
                    for gram in ngram::ngrams(run, n) {
                        *grams.entry((gram, register)).or_default() += 1;
                    }
                }
            }
            if tokens.len() >= 6 {
                exemplars.push((
                    register,
                    text::normalize_typography(line).trim().to_string(),
                ));
            }
        }
        let (shape, word_total, syllables) = prose_shape(body);
        for length in &shape {
            *lengths.entry((register, *length)).or_default() += 1;
        }
        *prose.entry((register, "sentences")).or_default() += shape.len() as i64;
        *prose.entry((register, "words")).or_default() += word_total;
        *prose.entry((register, "syllables")).or_default() += syllables;
        *prose.entry((register, "documents")).or_default() += 1;
    }

    let mut report = UnlearnReport {
        texts: texts.len(),
        ..Default::default()
    };
    let mut lost: HashMap<String, i64> = HashMap::new();
    for ((gram, register), by) in &grams {
        let before = store.ngram_count(gram)?;
        let taken = store.unlearn(Unlearn::Gram(gram, *register), *by, apply)?;
        if taken > 0 {
            report.phrase_counts += taken;
            *lost.entry(gram.clone()).or_default() += taken;
            if before == taken {
                report.phrases_emptied += 1;
            }
        }
    }
    for ((word, register), by) in &words {
        report.word_counts += store.unlearn(Unlearn::WordIn(word, *register), *by, apply)?;
        store.unlearn(Unlearn::Word(word), *by, apply)?;
    }
    for (register, line) in &exemplars {
        report.exemplars += store.unlearn_exemplar(*register, line, apply)?;
    }
    for ((register, length), by) in &lengths {
        store.unlearn(Unlearn::SentenceLength(*register, *length), *by, apply)?;
    }
    for ((register, metric), by) in &prose {
        store.unlearn(Unlearn::Prose(*register, metric), *by, apply)?;
    }

    let mut sample: Vec<(String, i64)> = lost.into_iter().collect();
    sample.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sample.truncate(15);
    report.sample = sample;
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
}
