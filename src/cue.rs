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

/// A cue can decide more than one confusion set — `can` selects `field` for
/// {filed, field}, `manager` for {manger, manager}, and `there` for {their,
/// there}, all in the same position. Keying to a single word silently kept
/// whichever happened to land first and dropped the rest.
/// One entry: the word this cue selects, and how far it beat the runner-up in
/// the corpus. The margin is the only evidence we have about how sure to be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub word: &'static str,
    pub margin: f32,
}

type ByCue = std::collections::HashMap<&'static str, Vec<Selection>>;

/// Keyed by position, then by cue — rather than by a `(cue, position)` tuple,
/// so a lookup can borrow the token being checked instead of owning it.
type Table = std::collections::HashMap<Position, ByCue>;

/// What a cue concluded, and how sure it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub word: String,
    pub confidence: f32,
}

fn table() -> &'static Table {
    static TABLE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = Table::new();
        for line in CUE_DATA.lines().filter(|l| !l.starts_with('#')) {
            let mut field = line.split('\t');
            let Some(position) = field.next().and_then(|p| match p {
                "before" => Some(Position::Before),
                "after" => Some(Position::After),
                _ => None,
            }) else {
                continue;
            };
            let (Some(cue), Some(word)) = (field.next(), field.next()) else {
                continue;
            };
            let margin = field.next().and_then(|m| m.parse().ok()).unwrap_or(150.0);
            out.entry(position)
                .or_default()
                .entry(cue)
                .or_default()
                .push(Selection { word, margin });
        }
        out
    })
}

/// Every cue in the table, as `(cue, position, selected word)`.
pub fn all() -> impl Iterator<Item = (&'static str, Position, &'static str)> {
    table().iter().flat_map(|(position, by_cue)| {
        by_cue
            .iter()
            .flat_map(move |(cue, words)| words.iter().map(move |s| (*cue, *position, s.word)))
    })
}

/// The word a cue selects, if this token is a cue in this position.
/// The words this token decides for, in this position — one per confusion set
/// it happens to discriminate.
fn selects(token: &str, position: Position) -> &'static [Selection] {
    table()
        .get(&position)
        .and_then(|by_cue| by_cue.get(token))
        .map(Vec::as_slice)
        .unwrap_or(&[])
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
/// Judge `word` against its confusables using a bundled discriminating
/// collocate, for the corpus that hasn't learned any of its own yet.
///
/// Returns the word that should have been written and how sure that is, or
/// nothing — which is the answer for everything the table doesn't cover.
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
) -> Option<Cue> {
    let alternatives = ngram::confusables(word);
    if alternatives.is_empty() {
        return None;
    }

    // Adjacent only. Reaching across one intervening word was tried and
    // measured — see `docs/PLAN.md` 12h — and it caught nothing extra while
    // costing precision, because the weakest cues are function words that
    // only mean anything immediately beside the word they decide.
    for (neighbor, position) in [(prev, Before), (next, After)] {
        let Some(neighbor) = neighbor else { continue };
        let candidates = selects(neighbor, position);
        // The cue picks the word that was written: this is right, not wrong.
        if candidates.iter().any(|s| s.word == word) {
            return None;
        }
        let Some(selected) = candidates.iter().find(|s| alternatives.contains(&s.word)) else {
            continue;
        };
        // The user writes it this way. Their corpus outranks the table.
        let written = match position {
            Before => format!("{neighbor} {word}"),
            After => format!("{word} {neighbor}"),
        };
        if evidence(&written) > 0 {
            return None;
        }
        return Some(Cue {
            word: selected.word.to_string(),
            confidence: confidence(selected.margin),
        });
    }
    None
}

/// How sure a cue is, from how far it beat the runner-up.
///
/// A flat constant was the first version and it was wrong in the way that
/// matters most for a machine-readable result: `apart from` beats `apart form`
/// by 1444 to 1 while the weakest cues here scrape past 150, and reporting
/// both as the same number presents a guess as a fact.
///
/// Logarithmic, because the margins span three orders of magnitude and the
/// difference between 150 and 300 means far more than between 60,000 and
/// 100,000. The ceiling stays below what corroborated *personal* collocations
/// earn: this is a rule about English, not evidence about this writer.
fn confidence(margin: f32) -> f32 {
    const FLOOR: f32 = 0.50;
    const CEILING: f32 = 0.80;
    const WEAKEST: f32 = 150.0;
    const STRONGEST: f32 = 100_000.0;

    let span = (STRONGEST / WEAKEST).log10();
    let position = (margin.max(WEAKEST) / WEAKEST).log10() / span;
    let raw = (FLOOR + (CEILING - FLOOR) * position.clamp(0.0, 1.0)).clamp(FLOOR, CEILING);
    // Two decimals: this is a coarse judgement from a corpus margin, and
    // `0.574501` would dress it up as a measurement.
    crate::types::round(raw as f64, 2) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_evidence(_: &str) -> i64 {
        0
    }

    /// The words a cue decides, for assertions that only care about which.
    fn decided(token: &str, position: Position) -> Vec<&'static str> {
        selects(token, position).iter().map(|s| s.word).collect()
    }

    fn evidence_from(pairs: &[(&str, i64)]) -> impl FnMut(&str) -> i64 + use<> {
        let map: HashMap<String, i64> = pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        move |gram: &str| map.get(gram).copied().unwrap_or(0)
    }

    #[test]
    fn a_cue_before_the_word_decides_it() {
        assert_eq!(
            check(Some("apart"), "form", None, &mut no_evidence).map(|c| c.word),
            Some("from".to_string())
        );
    }

    #[test]
    fn a_cue_after_the_word_decides_it() {
        // `affect adults` — the cue follows the word it decides. Things get
        // affected; nothing "effects adults".
        assert_eq!(
            check(None, "effect", Some("adults"), &mut no_evidence).map(|c| c.word),
            Some("affect".to_string())
        );
    }

    #[test]
    fn the_canonical_slip_is_caught() {
        assert_eq!(
            check(Some("rather"), "then", None, &mut no_evidence).map(|c| c.word),
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
    fn confidence_tracks_how_far_the_cue_beat_its_runner_up() {
        // A flat constant was the first version, and it reported `apart from`
        // — which wins by 1444 to 1 — identically to a cue that scraped past
        // 150. Surfaced through --json, that presents a guess as a fact.
        assert!(confidence(150.0) < confidence(1_000.0));
        assert!(confidence(1_000.0) < confidence(50_000.0));

        // Never certainty, and never above what corroborated personal
        // collocations earn: this is a rule about English, not evidence about
        // this writer.
        assert!(confidence(f32::MAX) <= 0.80);
        assert!(confidence(0.0) >= 0.50);
    }

    #[test]
    fn a_strong_cue_reports_more_confidence_than_a_weak_one() {
        // `apart` (1444) against `the` (163), both real entries.
        let strong = check(Some("apart"), "form", None, &mut no_evidence).unwrap();
        let weak = check(Some("the"), "from", None, &mut no_evidence).unwrap();
        assert_eq!(strong.word, "from");
        assert_eq!(weak.word, "form");
        assert!(
            strong.confidence > weak.confidence,
            "{} vs {}",
            strong.confidence,
            weak.confidence
        );
    }

    #[test]
    fn one_cue_can_decide_several_confusion_sets() {
        // `can` selects `field`, `manager`, and `there` — one per set. Keying
        // the table to a single word per (cue, position) silently kept
        // whichever landed first and dropped 71 of 891 cues.
        let decided = decided("can", After);
        assert!(decided.len() > 1, "{decided:?}");
    }

    #[test]
    fn no_cue_selects_two_members_of_one_set_in_the_same_position() {
        // The exclusivity the table's whole safety rests on. One cue may
        // decide several *different* sets — `can` picks `field`, `manager`,
        // and `there` — but never two members of one set, which would make
        // the correction a coin flip.
        let mut seen: std::collections::HashMap<(&str, Position), Vec<&str>> =
            std::collections::HashMap::new();
        for (cue, position, word) in all() {
            let decided = seen.entry((cue, position)).or_default();
            for other in decided.iter() {
                assert!(
                    !ngram::confusables(word).contains(other),
                    "{cue:?} selects both {word:?} and {other:?}"
                );
            }
            decided.push(word);
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
        assert!(decided("apart", Before).contains(&"from"));
        assert!(decided("rather", Before).contains(&"than"));
    }

    #[test]
    fn a_cue_whose_runner_up_is_real_english_is_left_out() {
        // These were both in the hand-written table and both were mistakes.
        // `even though` outnumbers `even through` 90 to 1, which sounds
        // decisive until you notice that `even through the night` is ordinary
        // English — so the cue would fire on correct text. Same for `weather
        // or the traffic` against `whether or not`.
        //
        // A ratio high enough to exclude them is the difference between "the
        // runner-up is noise" and "the runner-up is rarer but real", which is
        // the whole judgement this table exists to encode.
        assert!(!decided("even", Before).contains(&"though"));
        assert!(!decided("or", After).contains(&"whether"));
    }

    #[test]
    fn the_table_is_not_empty() {
        assert!(all().count() > 50, "only {} cues", all().count());
    }
}
