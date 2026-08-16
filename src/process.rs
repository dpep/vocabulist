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
            for n in [2usize, 3] {
                for gram in ngram::ngrams(run, n) {
                    store.bump_ngram(&gram, n, register, 1)?;
                }
            }
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

/// Fold one body's sentence shape into the running per-register stats.
fn record_prose_shape(
    store: &Store,
    register: Register,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut words = 0i64;
    let mut syllables = 0i64;
    let mut sentences = 0i64;

    for sentence in crate::complexity::split_sentences(body) {
        let normalized = text::normalize_typography(&sentence);
        let masked = text::mask_non_prose(&normalized);
        let length: Vec<String> = text::tokenize(&masked)
            .iter()
            .map(|t| text::normalize(&t.text))
            .filter(|w| w.chars().any(char::is_alphabetic))
            .collect();
        if length.is_empty() {
            continue;
        }
        sentences += 1;
        words += length.len() as i64;
        syllables += length
            .iter()
            .map(|w| crate::complexity::count_syllables(w) as i64)
            .sum::<i64>();
        store.bump_sentence_length(register, length.len() as i64)?;
    }

    store.bump_prose(register, "sentences", sentences)?;
    store.bump_prose(register, "words", words)?;
    store.bump_prose(register, "syllables", syllables)?;
    Ok(())
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
