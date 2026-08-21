//! Remove what today's filters would never have learned.
//!
//! Capture rules get stricter over time — a masking bug fixed, an envelope
//! recognized — but the store keeps what it was taught under the old ones.
//! Session ids, tool-call ids, and path fragments were ranking as this user's
//! characteristic phrases, and nothing removes them, because the prose they
//! came from was processed into counts and dropped.
//!
//! So the test cannot be "reprocess and compare". It has to be a judgement
//! made on the stored row alone: **is every word of this phrase a word?**
//!
//! By default the test is **shape alone**: a token with a digit, an
//! underscore, or anything else the tokenizer would not produce is not a word,
//! and a phrase containing one is not a phrase. That catches every session id,
//! tool-call id, and `claude-501`, and it cannot take anything else, which is
//! the property that matters when the data it removes is unrecoverable.
//!
//! `--strict` additionally requires something to *vouch* for each word — the
//! bundled dictionary, or your lexicon at better than `observed` provenance.
//! That catches the residue shape cannot reach, `output-file` and
//! `users-dpepper-code-lib-rust`, and it is opt-in because the same rule
//! removes `and the evals` and `apostrophed words`. Those are real, they are
//! yours, and no dictionary has heard of them. Read the dry run before
//! reaching for it.

use crate::store::Store;
use crate::text;
use crate::types::Provenance;
use std::collections::HashSet;

/// What a prune did, or would do.
#[derive(Debug, Default, PartialEq)]
pub struct PruneReport {
    pub ngrams_removed: usize,
    pub words_removed: usize,
    /// A sample of what went, for reading rather than counting.
    pub sample: Vec<String>,
}

const SAMPLE: usize = 15;

/// Is this token something a phrase is allowed to contain?
fn keep(token: &str, vouchers: Option<&HashSet<String>>) -> bool {
    if !text::is_lexical(token) {
        return false;
    }
    match vouchers {
        Some(known) => known.contains(token),
        None => true,
    }
}

/// Remove junk n-grams, and lexicon entries the tokenizer would now reject.
///
/// `dry_run` reports without deleting, because this removes data that cannot
/// be recomputed — the prose it came from is gone.
pub fn run(
    store: &Store,
    dry_run: bool,
    strict: bool,
) -> Result<PruneReport, Box<dyn std::error::Error>> {
    let mut report = PruneReport::default();

    // Everything that can vouch for a word: the bundled dictionary, plus the
    // lexicon at any provenance stronger than `observed`. Built only when
    // asked for, since loading the dictionary is the expensive part.
    let vouchers: Option<HashSet<String>> = strict.then(|| {
        let mut known: HashSet<String> = crate::dict::load()
            .map(|d| d.into_iter().map(|(w, _)| w.to_string()).collect())
            .unwrap_or_default();
        if let Ok(entries) = store.list(None, usize::MAX) {
            for entry in entries {
                if entry.provenance != Provenance::Observed {
                    known.insert(entry.word.clone());
                }
            }
        }
        known
    });

    let doomed: Vec<String> = store
        .all_ngrams()?
        .into_iter()
        .filter(|gram| gram.split(' ').any(|token| !keep(token, vouchers.as_ref())))
        .collect();

    for gram in &doomed {
        if report.sample.len() < SAMPLE {
            report.sample.push(gram.clone());
        }
        if !dry_run {
            store.remove_ngram(gram)?;
        }
    }
    report.ngrams_removed = doomed.len();

    // Words are judged on shape alone. A merely-observed word that is not in
    // any dictionary is the normal case for personal jargon, so the only safe
    // test here is whether the tokenizer would produce it at all.
    let words: Vec<String> = store
        .list(None, usize::MAX)?
        .into_iter()
        .filter(|e| e.provenance == Provenance::Observed && !text::is_lexical(&e.word))
        .map(|e| e.word)
        .collect();
    for word in &words {
        if report.sample.len() < SAMPLE {
            report.sample.push(word.clone());
        }
        if !dry_run {
            store.remove(word)?;
        }
    }
    report.words_removed = words.len();

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Register;

    fn seeded() -> Store {
        let store = Store::open(":memory:").unwrap();
        // Jargon with real provenance — must survive.
        store
            .upsert_word("vocabulist", "vocabulist", Provenance::Owned, 1)
            .unwrap();
        store
            .bump_ngram("the vocabulist", 2, Register::Doc, 1)
            .unwrap();
        // Ordinary prose — must survive.
        store.bump_ngram("the small", 2, Register::Doc, 1).unwrap();
        // Junk of both kinds.
        store
            .bump_ngram("tasks a0d376be-04f5", 2, Register::Doc, 1)
            .unwrap();
        store
            .bump_ngram("claude-501 users-dpepper-code", 2, Register::Doc, 1)
            .unwrap();
        store
    }

    #[test]
    fn removes_ids_by_shape_alone() {
        let store = seeded();
        let report = run(&store, false, false).unwrap();
        assert_eq!(report.ngrams_removed, 2);
        assert_eq!(store.ngram_count("tasks a0d376be-04f5").unwrap(), 0);
        assert_eq!(
            store.ngram_count("claude-501 users-dpepper-code").unwrap(),
            0
        );
    }

    #[test]
    fn keeps_prose_your_jargon_and_your_coinages() {
        let store = seeded();
        store.bump_ngram("the evals", 2, Register::Doc, 1).unwrap();
        run(&store, false, false).unwrap();
        assert_eq!(store.ngram_count("the small").unwrap(), 1);
        assert_eq!(store.ngram_count("the vocabulist").unwrap(), 1);
        // No dictionary has `evals`. The default must not touch it.
        assert_eq!(store.ngram_count("the evals").unwrap(), 1);
    }

    #[test]
    fn strict_reaches_what_shape_cannot() {
        let store = seeded();
        store
            .bump_ngram("output-file private", 2, Register::Doc, 1)
            .unwrap();
        run(&store, false, true).unwrap();
        assert_eq!(store.ngram_count("output-file private").unwrap(), 0);
        // Still keeps what the lexicon vouches for.
        assert_eq!(store.ngram_count("the vocabulist").unwrap(), 1);
    }

    #[test]
    fn a_dry_run_changes_nothing() {
        let store = seeded();
        let report = run(&store, true, false).unwrap();
        assert_eq!(report.ngrams_removed, 2);
        assert_eq!(store.ngram_count("tasks a0d376be-04f5").unwrap(), 1);
    }
}
