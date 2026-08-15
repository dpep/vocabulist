//! Collocations: what words you put next to what other words.
//!
//! Two jobs, one mechanism. Ranked by log-likelihood this is the phrase
//! extractor — the thing that knows you write "high-signal" and not
//! "informative". Queried pointwise it catches **real-word errors**, the
//! `form`/`from` class that a dictionary is structurally blind to because
//! both spellings are perfectly good words.
//!
//! Counting, not embeddings. Association measures over n-gram counts are
//! deterministic, explainable, and need no model — and for "which words go
//! together" they're not a compromise, they're the right tool. Semantics only
//! becomes necessary for the *generative* direction (offering your word for
//! someone else's), which is a separate problem.

/// Minimum combined evidence before a real-word substitution is even
/// considered. Below this the corpus simply hasn't seen enough to have an
/// opinion, and guessing would burn the user's trust.
pub const MIN_REALWORD_EVIDENCE: i64 = 3;

/// How much more evidence the alternative needs than the written word. High
/// on purpose — flagging a correct word is far more costly than missing a
/// typo, because it teaches you to ignore the tool.
pub const REALWORD_RATIO: f64 = 4.0;

/// Real-word confusion sets: groups of legitimate words that get typed for
/// one another. Every member is a valid English word, which is exactly why a
/// dictionary can't help.
pub const CONFUSION_SETS: &[&[&str]] = &[
    &["form", "from"],
    &["casual", "causal"],
    &["quiet", "quite"],
    &["trial", "trail"],
    &["manger", "manager"],
    &["defiantly", "definitely"],
    &["pubic", "public"],
    &["untied", "united"],
    &["filed", "field"],
    &["angel", "angle"],
    &["lose", "loose"],
    &["then", "than"],
    &["their", "there"],
    &["weather", "whether"],
    &["discrete", "discreet"],
    &["complement", "compliment"],
    &["principal", "principle"],
    &["affect", "effect"],
    &["sting", "string"],
    &["thorough", "through", "though"],
    &["county", "country"],
    &["unclear", "nuclear"],
    &["statistic", "statistics"],
];

/// The other members of `word`'s confusion set, if it belongs to one.
pub fn confusables(word: &str) -> Vec<&'static str> {
    CONFUSION_SETS
        .iter()
        .filter(|set| set.contains(&word))
        .flat_map(|set| set.iter().copied())
        .filter(|w| *w != word)
        .collect()
}

/// Consecutive n-grams over a token slice, space-joined.
pub fn ngrams(tokens: &[String], n: usize) -> Vec<String> {
    if n == 0 || tokens.len() < n {
        return Vec::new();
    }
    tokens.windows(n).map(|w| w.join(" ")).collect()
}

/// Dunning's log-likelihood ratio (G²) for a 2x2 contingency table. The
/// corpus-linguistics standard for collocation strength: unlike raw PMI it
/// stays well-behaved when counts are small, which is the regime a personal
/// corpus lives in permanently.
pub fn log_likelihood(k11: f64, k12: f64, k21: f64, k22: f64) -> f64 {
    let n = k11 + k12 + k21 + k22;
    if n <= 0.0 {
        return 0.0;
    }
    let row1 = k11 + k12;
    let row2 = k21 + k22;
    let col1 = k11 + k21;
    let col2 = k12 + k22;

    let term = |observed: f64, expected: f64| {
        if observed <= 0.0 || expected <= 0.0 {
            0.0
        } else {
            observed * (observed / expected).ln()
        }
    };

    2.0 * (term(k11, row1 * col1 / n)
        + term(k12, row1 * col2 / n)
        + term(k21, row2 * col1 / n)
        + term(k22, row2 * col2 / n))
}

/// One candidate real-word correction, with the evidence behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct RealWordHit {
    pub suggestion: String,
    pub written_evidence: i64,
    pub suggested_evidence: i64,
}

impl RealWordHit {
    /// How sure we are the written word is wrong. Saturates well below 1.0 —
    /// collocation evidence is suggestive, never proof.
    pub fn confidence(&self) -> f32 {
        let ratio = self.suggested_evidence as f64 / (self.written_evidence as f64 + 1.0);
        let scaled = (ratio / (ratio + 8.0)) as f32;
        (0.35 + scaled).min(0.85)
    }
}

/// Judge one word against its confusables using the company it keeps.
///
/// `evidence` reports how often a bigram has been seen; the caller supplies it
/// from the store. Returns nothing unless one alternative clears both the
/// absolute floor and the ratio — silence is the correct output for a corpus
/// that hasn't learned enough yet.
pub fn check_real_word(
    prev: Option<&str>,
    word: &str,
    next: Option<&str>,
    evidence: &mut impl FnMut(&str) -> i64,
) -> Option<RealWordHit> {
    let alternatives = confusables(word);
    if alternatives.is_empty() {
        return None;
    }

    let score = |candidate: &str, evidence: &mut dyn FnMut(&str) -> i64| -> i64 {
        let mut total = 0;
        if let Some(p) = prev {
            total += evidence(&format!("{p} {candidate}"));
        }
        if let Some(nx) = next {
            total += evidence(&format!("{candidate} {nx}"));
        }
        total
    };

    let written = score(word, evidence);
    let best = alternatives
        .iter()
        .map(|alt| (*alt, score(alt, evidence)))
        .max_by_key(|(_, s)| *s)?;

    if best.1 < MIN_REALWORD_EVIDENCE {
        return None;
    }
    if (best.1 as f64) < REALWORD_RATIO * (written as f64 + 1.0) {
        return None;
    }
    Some(RealWordHit {
        suggestion: best.0.to_string(),
        written_evidence: written,
        suggested_evidence: best.1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn evidence_from(pairs: &[(&str, i64)]) -> impl FnMut(&str) -> i64 + use<> {
        let map: HashMap<String, i64> = pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        move |gram: &str| map.get(gram).copied().unwrap_or(0)
    }

    #[test]
    fn builds_consecutive_ngrams() {
        let toks: Vec<String> = ["ship", "the", "small", "change"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            ngrams(&toks, 2),
            vec!["ship the", "the small", "small change"]
        );
        assert_eq!(ngrams(&toks, 3), vec!["ship the small", "the small change"]);
        assert!(ngrams(&toks, 9).is_empty());
    }

    #[test]
    fn confusables_are_symmetric_and_exclude_self() {
        assert_eq!(confusables("form"), vec!["from"]);
        assert_eq!(confusables("from"), vec!["form"]);
        assert!(confusables("ship").is_empty());
    }

    #[test]
    fn flags_a_real_word_error_with_enough_evidence() {
        let mut ev = evidence_from(&[("apart from", 12), ("from the", 40)]);
        let hit = check_real_word(Some("apart"), "form", Some("the"), &mut ev);
        assert_eq!(hit.unwrap().suggestion, "from");
    }

    #[test]
    fn stays_silent_without_evidence() {
        let mut ev = evidence_from(&[]);
        assert!(check_real_word(Some("apart"), "form", Some("the"), &mut ev).is_none());
    }

    #[test]
    fn stays_silent_when_the_written_word_is_well_attested() {
        // "fill in the form" is ordinary; the alternative must not win.
        let mut ev = evidence_from(&[("the form", 30), ("form and", 12), ("from and", 2)]);
        assert!(check_real_word(Some("the"), "form", Some("and"), &mut ev).is_none());
    }

    #[test]
    fn confidence_never_reaches_certainty() {
        let hit = RealWordHit {
            suggestion: "from".into(),
            written_evidence: 0,
            suggested_evidence: 10_000,
        };
        assert!(hit.confidence() <= 0.85);
    }

    #[test]
    fn log_likelihood_is_zero_for_independent_counts() {
        // Perfectly proportional table — no association at all.
        let g2 = log_likelihood(10.0, 10.0, 10.0, 10.0);
        assert!(g2.abs() < 1e-9, "expected ~0, got {g2}");
    }

    #[test]
    fn log_likelihood_grows_with_association() {
        let weak = log_likelihood(5.0, 100.0, 100.0, 10_000.0);
        let strong = log_likelihood(80.0, 25.0, 25.0, 10_000.0);
        assert!(strong > weak);
    }

    #[test]
    fn log_likelihood_handles_empty_input() {
        assert_eq!(log_likelihood(0.0, 0.0, 0.0, 0.0), 0.0);
    }
}
