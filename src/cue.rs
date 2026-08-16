//! Collocates that settle a real-word confusion on day one.
//!
//! Real-word errors are the case a dictionary cannot see: every candidate is
//! spelled correctly, so only context distinguishes them. Personal
//! collocations do that well, and a fresh corpus has none — measured, the
//! checker caught **0 of 19** injected real-word errors, because it had
//! nothing to weigh.
//!
//! Frequency was tried as the day-one substitute and does not work; the
//! reasoning is in `docs/PLAN.md` §12b. Briefly: the prior has to beat the
//! typo rate, and it doesn't, so a frequency test fires on every occurrence of
//! the rarer word regardless of the sentence.
//!
//! A *discriminating collocate* carries the context that frequency lacks.
//! `apart from` is idiomatic and `apart form` is always a slip, so the word
//! `apart` decides that confusion by itself, in any sentence, with no corpus
//! at all.
//!
//! ## What earns a place here
//!
//! A cue must be **near-exclusive**: it selects one member of the set, and the
//! others essentially never take it. `the` precedes `form` far more often than
//! `from`, and it is still not a cue — `form the basis` is ordinary English,
//! and a cue that is merely *likely* produces exactly the confident-but-wrong
//! flag that teaches a user to ignore the tool.
//!
//! That judgement is not one a person can hold steady across fifty pairs, and
//! the hand-written first version proved it: it listed `relationship` as
//! selecting `causal`, where the corpus says `casual relationship` outnumbers
//! `causal relationship` three to one. So the table is **derived**, by
//! `script/build-cues.sh`, from Google Books Ngrams — the exclusivity rule
//! stated as arithmetic over real counts. `apart from` beats `apart form` by
//! more than a thousand to one; that is what a cue looks like.
//!
//! The table is deliberately incomplete. Before-cues are only as complete as
//! the corpus shards the build script fetches, and most of each confusion
//! set's context is covered by nothing at all. Silence is the correct output
//! for a context there is no evidence about.

use crate::ngram;
use Position::{After, Before};

/// Where the cue sits relative to the word it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Position {
    /// The cue immediately precedes it: `apart` in `apart from`.
    Before,
    /// The cue immediately follows it: `or` in `whether or`.
    After,
}

/// The generated table: `<position>\t<cue>\t<word it selects>`.
/// Built by `script/build-cues.sh`; see it for provenance and thresholds.
const CUE_DATA: &str = include_str!("../data/cues.txt");

type Table = std::collections::HashMap<(&'static str, Position), &'static str>;

fn table() -> &'static Table {
    static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        CUE_DATA
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let mut field = line.split('\t');
                let position = match field.next()? {
                    "before" => Position::Before,
                    "after" => Position::After,
                    _ => return None,
                };
                Some(((field.next()?, position), field.next()?))
            })
            .collect()
    })
}

/// Every cue in the table, as `(cue, position, selected word)`.
pub fn all() -> impl Iterator<Item = (&'static str, Position, &'static str)> {
    table().iter().map(|((cue, p), word)| (*cue, *p, *word))
}

/// The word a cue selects, if this token is a cue in this position.
fn selects(token: &str, position: Position) -> Option<&'static str> {
    table().get(&(token, position)).copied()
}

/// Judge `word` against its confusables using a bundled discriminating
/// collocate, for the corpus that hasn't learned any of its own yet.
///
/// Returns the word that should have been written, or nothing — which is the
/// answer for everything the table doesn't cover.
///
/// `evidence` is consulted only to **stay quiet**: if the user has actually
/// written this pairing, they meant it, and a general rule doesn't get to
/// overrule the person's own corpus. That inversion is the same one the whole
/// tool rests on.
pub fn check(
    prev: Option<&str>,
    word: &str,
    next: Option<&str>,
    evidence: &mut impl FnMut(&str) -> i64,
) -> Option<String> {
    let alternatives = ngram::confusables(word);
    if alternatives.is_empty() {
        return None;
    }

    for (neighbor, position) in [(prev, Before), (next, After)] {
        let Some(neighbor) = neighbor else { continue };
        let Some(selected) = selects(neighbor, position) else {
            continue;
        };
        // The cue picks the word that was written: this is right, not wrong.
        if selected == word {
            return None;
        }
        if !alternatives.contains(&selected) {
            continue;
        }
        // The user writes it this way. Their corpus outranks the table.
        let written = match position {
            Before => format!("{neighbor} {word}"),
            After => format!("{word} {neighbor}"),
        };
        if evidence(&written) > 0 {
            return None;
        }
        return Some(selected.to_string());
    }
    None
}

/// Confidence in a cue-driven correction.
///
/// Below what corroborated personal collocations earn, and well below
/// certainty: the cue is a strong general rule, not evidence about this
/// writer.
pub const CONFIDENCE: f32 = 0.6;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_evidence(_: &str) -> i64 {
        0
    }

    fn evidence_from(pairs: &[(&str, i64)]) -> impl FnMut(&str) -> i64 + use<> {
        let map: HashMap<String, i64> = pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        move |gram: &str| map.get(gram).copied().unwrap_or(0)
    }

    #[test]
    fn a_cue_before_the_word_decides_it() {
        assert_eq!(
            check(Some("apart"), "form", None, &mut no_evidence),
            Some("from".to_string())
        );
    }

    #[test]
    fn a_cue_after_the_word_decides_it() {
        assert_eq!(
            check(None, "weather", Some("or"), &mut no_evidence),
            Some("whether".to_string())
        );
    }

    #[test]
    fn the_canonical_slip_is_caught() {
        assert_eq!(
            check(Some("rather"), "then", None, &mut no_evidence),
            Some("than".to_string())
        );
    }

    #[test]
    fn a_cue_selecting_the_written_word_stays_silent() {
        assert_eq!(check(Some("apart"), "from", None, &mut no_evidence), None);
        assert_eq!(check(Some("rather"), "than", None, &mut no_evidence), None);
    }

    #[test]
    fn context_the_table_does_not_cover_stays_silent() {
        // `the form` and `the from` are both listed nowhere, deliberately:
        // `form the basis` is ordinary English.
        assert_eq!(check(Some("the"), "form", None, &mut no_evidence), None);
        assert_eq!(check(None, "form", Some("the"), &mut no_evidence), None);
    }

    #[test]
    fn a_word_without_confusables_is_never_judged() {
        assert_eq!(
            check(Some("apart"), "shipped", None, &mut no_evidence),
            None
        );
    }

    #[test]
    fn the_writers_own_corpus_overrules_the_table() {
        let mut seen = evidence_from(&[("apart form", 3)]);
        assert_eq!(check(Some("apart"), "form", None, &mut seen), None);
    }

    #[test]
    fn no_cue_selects_two_members_of_one_set_in_the_same_position() {
        // The exclusivity the table's whole safety rests on. The derivation
        // enforces it by construction — a context word is only emitted when
        // one member takes it overwhelmingly — so this guards the *format*
        // and the parse as much as the data.
        let mut seen: std::collections::HashMap<(&str, Position), &str> =
            std::collections::HashMap::new();
        for (cue, position, word) in all() {
            if let Some(previous) = seen.insert((cue, position), word) {
                assert_eq!(previous, word, "{cue:?} selects two words");
            }
        }
    }

    #[test]
    fn every_cue_selects_a_word_that_has_confusables() {
        // A cue for a word in no confusion set can never fire, so it is dead
        // weight — and a sign the table was built against a stale source.
        for (cue, _, word) in all() {
            assert!(
                !ngram::confusables(word).is_empty(),
                "cue {cue:?} selects {word:?}, which has no confusables"
            );
        }
    }

    #[test]
    fn the_derived_table_rediscovers_the_obvious_cues() {
        // If the corpus cannot find `apart from` and `rather than`, the
        // thresholds in the build script are wrong and everything else the
        // table says is suspect.
        assert_eq!(selects("apart", Before), Some("from"));
        assert_eq!(selects("rather", Before), Some("than"));
        assert_eq!(selects("or", After), Some("whether"));
    }

    #[test]
    fn the_table_is_not_empty() {
        assert!(all().count() > 50, "only {} cues", all().count());
    }
}
