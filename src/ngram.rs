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

/// A collocation and how strongly its words attract each other.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Collocation {
    pub gram: String,
    pub count: i64,
    /// Dunning's G². Higher means the pairing is less explicable by how
    /// common each word is on its own.
    pub log_likelihood: f64,
}

/// Rank n-grams by association strength.
///
/// Raw frequency would just return "of the" and "in the" — pairs that are
/// common because their words are common. G² asks a better question: given
/// how often each part appears alone, is appearing *together* surprising?
/// That's what separates a phrase you actually use from ordinary words that
/// happened to be adjacent.
///
/// Longer phrases work by splitting at the **last** space rather than the
/// only one, so "the small focused" is tested as "the small" followed by
/// "focused". The contingency table stays 2x2 — which is what G² needs — and
/// the question becomes "does this phrase extend in a surprising way", which
/// is the right question for a phrase rather than a pair.
pub fn rank_collocations(bigrams: &[(String, i64)], min_count: i64) -> Vec<Collocation> {
    use std::collections::HashMap;

    let total: f64 = bigrams.iter().map(|(_, c)| *c as f64).sum();
    if total == 0.0 {
        return Vec::new();
    }

    // Marginals: how often each word leads, and how often each word follows.
    let mut leads: HashMap<&str, f64> = HashMap::new();
    let mut follows: HashMap<&str, f64> = HashMap::new();
    for (gram, count) in bigrams {
        let Some((first, second)) = gram.rsplit_once(' ') else {
            continue;
        };
        *leads.entry(first).or_insert(0.0) += *count as f64;
        *follows.entry(second).or_insert(0.0) += *count as f64;
    }

    let mut out: Vec<Collocation> = bigrams
        .iter()
        .filter(|(_, count)| *count >= min_count)
        .filter_map(|(gram, count)| {
            let (first, second) = gram.rsplit_once(' ')?;
            let k11 = *count as f64;
            // The 2x2 table: this pair, this word with any other, any other
            // word with this one, and everything else.
            let k12 = leads.get(first).copied().unwrap_or(0.0) - k11;
            let k21 = follows.get(second).copied().unwrap_or(0.0) - k11;
            let k22 = total - k11 - k12 - k21;
            Some(Collocation {
                gram: gram.clone(),
                count: *count,
                log_likelihood: log_likelihood(k11, k12, k21, k22),
            })
        })
        .collect();

    out.sort_by(|a, b| {
        b.log_likelihood
            .partial_cmp(&a.log_likelihood)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
            .then(a.gram.cmp(&b.gram))
    });
    out
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
    fn ranks_a_real_phrase_above_two_common_words() {
        // "of the" is frequent but unremarkable — both words are everywhere.
        // "high signal" is rarer yet nearly always co-occurs, which is what
        // makes it a phrase rather than an accident.
        let bigrams = vec![
            ("of the".to_string(), 30),
            ("of a".to_string(), 25),
            ("in the".to_string(), 25),
            ("on the".to_string(), 20),
            ("high signal".to_string(), 8),
        ];
        let ranked = rank_collocations(&bigrams, 2);
        assert_eq!(ranked[0].gram, "high signal");
    }

    #[test]
    fn min_count_filters_one_off_pairings() {
        let bigrams = vec![("seen once".to_string(), 1), ("seen often".to_string(), 9)];
        let ranked = rank_collocations(&bigrams, 2);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].gram, "seen often");
    }

    #[test]
    fn ranking_an_empty_corpus_yields_nothing() {
        assert!(rank_collocations(&[], 1).is_empty());
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
