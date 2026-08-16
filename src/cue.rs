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
//! The bar is deliberately high enough that most of a confusion set's context
//! is not covered. Silence is the correct output for everything not listed.

use crate::ngram;

/// Where the cue sits relative to the word it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// The cue immediately precedes it: `apart` in `apart from`.
    Before,
    /// The cue immediately follows it: `or` in `whether or`.
    After,
}

use Position::{After, Before};

/// `(cue, position, the word it selects)`.
///
/// Grouped by confusion set so exclusivity can be checked by reading: within
/// a group, no cue may appear for two different members in the same position.
#[rustfmt::skip]
pub const CUES: &[(&str, Position, &str)] = &[
    // form / from — the most common of these by a wide margin.
    ("apart", Before, "from"), ("aside", Before, "from"), ("away", Before, "from"),
    ("far", Before, "from"), ("differs", Before, "from"), ("differ", Before, "from"),
    ("different", Before, "from"), ("ranging", Before, "from"), ("ranges", Before, "from"),
    ("derived", Before, "from"), ("stems", Before, "from"), ("comes", Before, "from"),
    ("came", Before, "from"), ("suffers", Before, "from"), ("benefit", Before, "from"),
    ("prevent", Before, "from"), ("separate", Before, "from"), ("distinguish", Before, "from"),
    ("migrated", Before, "from"), ("inherited", Before, "from"), ("borrowed", Before, "from"),
    ("scratch", After, "from"),
    ("fill", Before, "form"), ("filled", Before, "form"), ("submit", Before, "form"),
    ("submitted", Before, "form"), ("registration", Before, "form"), ("consent", Before, "form"),
    ("blank", Before, "form"), ("fields", After, "form"), ("submission", After, "form"),

    // then / than — `rather then` is the canonical slip.
    ("rather", Before, "than"), ("more", Before, "than"), ("less", Before, "than"),
    ("better", Before, "than"), ("worse", Before, "than"), ("greater", Before, "than"),
    ("fewer", Before, "than"), ("other", Before, "than"), ("larger", Before, "than"),
    ("smaller", Before, "than"), ("faster", Before, "than"), ("slower", Before, "than"),
    ("higher", Before, "than"), ("lower", Before, "than"), ("cheaper", Before, "than"),
    ("easier", Before, "than"), ("harder", Before, "than"), ("longer", Before, "than"),
    ("shorter", Before, "than"), ("older", Before, "than"), ("newer", Before, "than"),
    ("bigger", Before, "than"), ("stronger", Before, "than"), ("weaker", Before, "than"),
    ("sooner", Before, "than"), ("otherwise", Before, "than"), ("simpler", Before, "than"),
    ("safer", Before, "than"), ("cleaner", Before, "than"),
    ("and", Before, "then"), ("but", Before, "then"), ("back", Before, "then"),
    ("since", Before, "then"), ("until", Before, "then"), ("right", Before, "then"),

    // their / there
    ("own", After, "their"), ("respective", After, "their"),
    ("is", After, "there"), ("are", After, "there"), ("was", After, "there"),
    ("were", After, "there"), ("will", After, "there"), ("would", After, "there"),
    ("might", After, "there"), ("should", After, "there"), ("exists", After, "there"),
    ("remains", After, "there"), ("seems", After, "there"), ("appears", After, "there"),
    ("over", Before, "there"), ("out", Before, "there"), ("up", Before, "there"),

    // weather / whether
    ("or", After, "whether"), ("decide", Before, "whether"), ("decides", Before, "whether"),
    ("decided", Before, "whether"), ("deciding", Before, "whether"), ("determine", Before, "whether"),
    ("determines", Before, "whether"), ("determined", Before, "whether"), ("unclear", Before, "whether"),
    ("unsure", Before, "whether"), ("regardless", Before, "whether"), ("wonder", Before, "whether"),
    ("forecast", After, "weather"), ("conditions", After, "weather"), ("patterns", After, "weather"),

    // affect / effect
    ("side", Before, "effect"), ("net", Before, "effect"), ("adverse", Before, "effect"),
    ("desired", Before, "effect"), ("ripple", Before, "effect"), ("takes", Before, "effect"),
    ("take", Before, "effect"), ("took", Before, "effect"), ("into", Before, "effect"),
    ("will", Before, "affect"), ("may", Before, "affect"), ("can", Before, "affect"),
    ("could", Before, "affect"), ("might", Before, "affect"), ("would", Before, "affect"),
    ("adversely", Before, "affect"), ("negatively", Before, "affect"), ("directly", Before, "affect"),

    // lose / loose
    ("ends", After, "loose"), ("cannon", After, "loose"), ("coupling", After, "loose"),
    ("to", Before, "lose"), ("never", Before, "lose"), ("don't", Before, "lose"),
    ("doesn't", Before, "lose"), ("can't", Before, "lose"), ("won't", Before, "lose"),

    // thorough / through / though
    ("even", Before, "though"), ("as", Before, "though"),
    ("went", Before, "through"), ("goes", Before, "through"), ("going", Before, "through"),
    ("ran", Before, "through"), ("running", Before, "through"), ("passed", Before, "through"),
    ("passes", Before, "through"), ("cut", Before, "through"), ("halfway", Before, "through"),
    ("partway", Before, "through"), ("midway", Before, "through"), ("sifted", Before, "through"),
    ("review", After, "thorough"), ("analysis", After, "thorough"), ("investigation", After, "thorough"),
    ("examination", After, "thorough"),

    // principal / principle
    ("first", Before, "principle"), ("guiding", Before, "principle"), ("fundamental", Before, "principle"),
    ("underlying", Before, "principle"), ("general", Before, "principle"), ("basic", Before, "principle"),
    ("investigator", After, "principal"), ("balance", After, "principal"),

    // discrete / discreet
    ("values", After, "discrete"), ("units", After, "discrete"), ("steps", After, "discrete"),
    ("intervals", After, "discrete"), ("chunks", After, "discrete"), ("packets", After, "discrete"),
    ("samples", After, "discrete"),

    // complement / compliment
    ("pay", Before, "compliment"), ("paid", Before, "compliment"), ("backhanded", Before, "compliment"),
    ("full", Before, "complement"),

    // quiet / quite
    ("not", Before, "quite"), ("a", After, "quite"), ("an", After, "quite"),
    ("possibly", After, "quite"), ("literally", After, "quite"),
    ("keep", Before, "quiet"), ("kept", Before, "quiet"), ("stay", Before, "quiet"),
    ("stayed", Before, "quiet"), ("eerily", Before, "quiet"),

    // trial / trail
    ("clinical", Before, "trial"), ("jury", Before, "trial"), ("error", After, "trial"),
    ("hiking", Before, "trail"), ("paper", Before, "trail"), ("audit", Before, "trail"),

    // casual / causal
    ("link", After, "causal"), ("chain", After, "causal"), ("relationship", After, "causal"),
    ("inference", After, "causal"), ("mechanism", After, "causal"),
    ("observer", After, "casual"), ("conversation", After, "casual"), ("glance", After, "casual"),

    // manger / manager — `manger` is essentially always a slip in this register.
    ("project", Before, "manager"), ("product", Before, "manager"), ("engineering", Before, "manager"),
    ("hiring", Before, "manager"), ("package", Before, "manager"), ("window", Before, "manager"),
    ("session", Before, "manager"), ("connection", Before, "manager"), ("resource", Before, "manager"),
    ("account", Before, "manager"), ("senior", Before, "manager"),

    // defiantly / definitely — `definitely not` is a set phrase; the manner
    // adverb does not take these.
    ("not", After, "definitely"), ("worth", After, "definitely"),

    // pubic / public
    ("api", After, "public"), ("interface", After, "public"), ("method", After, "public"),
    ("key", After, "public"), ("cloud", After, "public"), ("sector", After, "public"),
    ("records", After, "public"), ("domain", After, "public"),

    // untied / united
    ("states", After, "united"), ("kingdom", After, "united"), ("nations", After, "united"),

    // filed / field
    ("text", Before, "field"), ("input", Before, "field"), ("hidden", Before, "field"),
    ("required", Before, "field"), ("name", After, "field"), ("names", After, "field"),
    ("under", After, "filed"), ("against", After, "filed"),

    // angel / angle
    ("right", Before, "angle"), ("acute", Before, "angle"), ("obtuse", Before, "angle"),
    ("brackets", After, "angle"), ("bracket", After, "angle"),
    ("guardian", Before, "angel"),

    // sting / string
    ("empty", Before, "string"), ("format", Before, "string"), ("query", Before, "string"),
    ("connection", Before, "string"), ("literal", After, "string"), ("literals", After, "string"),
    ("interpolation", After, "string"), ("concatenation", After, "string"),

    // county / country
    ("code", After, "country"), ("codes", After, "country"), ("wide", After, "country"),
    ("clerk", After, "county"), ("courthouse", After, "county"),

    // unclear / nuclear
    ("power", After, "nuclear"), ("weapons", After, "nuclear"), ("reactor", After, "nuclear"),
    ("remains", Before, "unclear"), ("still", Before, "unclear"),
];

/// The word a cue selects, if this token is a cue in this position.
fn selects(token: &str, position: Position) -> Option<&'static str> {
    CUES.iter()
        .find(|(cue, p, _)| *p == position && *cue == token)
        .map(|(_, _, word)| *word)
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
        // The exclusivity the table's whole safety rests on, checked rather
        // than trusted to careful reading.
        for (cue, position, word) in CUES {
            for (other_cue, other_position, other_word) in CUES {
                if cue != other_cue || position != other_position || word == other_word {
                    continue;
                }
                assert!(
                    !ngram::confusables(word).contains(other_word),
                    "{cue:?} selects both {word:?} and {other_word:?}"
                );
            }
        }
    }

    #[test]
    fn every_cue_selects_a_word_that_has_confusables() {
        // A cue for a word in no confusion set can never fire, so it is dead
        // weight and probably a mistake.
        for (cue, _, word) in CUES {
            assert!(
                !ngram::confusables(word).is_empty(),
                "cue {cue:?} selects {word:?}, which has no confusables"
            );
        }
    }
}
