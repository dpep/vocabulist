//! Vocabulary and linguistic complexity, for a text or for a person.
//!
//! Two modes, because they answer different questions. Pointed at a text it
//! describes *that writing*. Pointed at the lexicon it describes *the writer*,
//! aggregated across everything captured.
//!
//! The corpus mode is deliberately narrower, and the reason is structural:
//! processing drops the prose, so anything below the word — sentence length,
//! clause depth, readability — cannot be recovered later. Those metrics are
//! available for a text and absent for the corpus until `process` starts
//! recording them as it goes. That's a real limitation of spool-not-archive,
//! not an oversight.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Word-level metrics, available from either a text or accumulated counts.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Vocabulary {
    /// Total words counted.
    pub tokens: u64,
    /// Distinct words.
    pub types: u64,
    /// types / tokens. Intuitive, but falls as a text gets longer, so it only
    /// compares like-sized samples.
    pub type_token_ratio: f64,
    /// types / sqrt(tokens) — Guiraud's R. Length-normalized, so this is the
    /// one to compare across samples of different sizes.
    pub guiraud_r: f64,
    /// Share of distinct words appearing exactly once. High means a wide,
    /// thinly-used vocabulary.
    pub hapax_ratio: f64,
    pub mean_word_length: f64,
    /// Share of tokens 7+ characters — a cheap, robust complexity proxy.
    pub long_word_ratio: f64,
}

/// Sentence-level metrics. Only computable from prose, never from counts.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Readability {
    pub sentences: u64,
    pub mean_sentence_length: f64,
    /// Spread of sentence length. Two writers can share a mean and read
    /// nothing alike — this is the half that tells them apart.
    #[serde(default)]
    pub sentence_length_stddev: f64,
    pub mean_syllables_per_word: f64,
    /// Flesch Reading Ease. Roughly: 90+ very easy, 60-70 plain English,
    /// 30 and below heavy going. Approximate — the syllable count is a
    /// heuristic, so treat it as a trend line rather than a grade.
    pub flesch_reading_ease: f64,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct Report {
    pub scope: String,
    pub vocabulary: Vocabulary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readability: Option<Readability>,
}

/// Metrics from a word-frequency table — the corpus mode.
pub fn from_counts(scope: &str, counts: &HashMap<String, u64>) -> Report {
    let tokens: u64 = counts.values().sum();
    let types = counts.len() as u64;
    let hapax = counts.values().filter(|c| **c == 1).count() as u64;
    let char_total: u64 = counts
        .iter()
        .map(|(w, c)| w.chars().count() as u64 * c)
        .sum();
    let long: u64 = counts
        .iter()
        .filter(|(w, _)| w.chars().count() >= 7)
        .map(|(_, c)| *c)
        .sum();

    Report {
        scope: scope.to_string(),
        vocabulary: Vocabulary {
            tokens,
            types,
            type_token_ratio: ratio(types, tokens),
            guiraud_r: if tokens == 0 {
                0.0
            } else {
                types as f64 / (tokens as f64).sqrt()
            },
            hapax_ratio: ratio(hapax, types),
            mean_word_length: ratio(char_total, tokens),
            long_word_ratio: ratio(long, tokens),
        },
        readability: None,
    }
}

/// Metrics from prose — the text mode, which adds the sentence level.
pub fn from_text(scope: &str, text: &str) -> Report {
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut syllables = 0u64;

    // The same pipeline the corpus path uses. It used to skip masking and the
    // prose-line test, so a text's URL fragments counted as vocabulary while
    // the corpus never saw them — two modes claiming to be comparable while
    // measuring different token populations.
    for line in text.lines() {
        for word in crate::text::prose_words(line) {
            syllables += count_syllables(&word);
            *counts.entry(word).or_insert(0) += 1;
        }
    }

    let mut report = from_counts(scope, &counts);

    // Per-sentence lengths, so the text path reports the same spread the
    // corpus path does.
    let mut histogram: HashMap<i64, i64> = HashMap::new();
    for sentence in split_sentences(text) {
        let words = crate::text::tokenize(&crate::text::normalize_typography(&sentence))
            .iter()
            .filter(|t| t.text.chars().any(char::is_alphabetic))
            .count() as i64;
        if words > 0 {
            *histogram.entry(words).or_insert(0) += 1;
        }
    }
    let histogram: Vec<(i64, i64)> = histogram.into_iter().collect();
    let sentences: u64 = histogram.iter().map(|(_, c)| *c as u64).sum();

    let mut readability = readability_from_totals(sentences, report.vocabulary.tokens, syllables);
    readability.sentence_length_stddev = sentence_length_stddev(&histogram);
    report.readability = Some(readability);
    report
}

/// Split prose into sentences on terminal punctuation. Used by `process` to
/// record sentence shape before the text is dropped.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?') && current.chars().any(char::is_alphanumeric) {
            out.push(std::mem::take(&mut current));
        }
    }
    if current.chars().any(char::is_alphanumeric) {
        out.push(current);
    }
    out
}

/// Readability from running totals rather than from text — the corpus path,
/// where the prose is long gone and only these sums remain.
pub fn readability_from_totals(sentences: u64, words: u64, syllables: u64) -> Readability {
    let words_per_sentence = ratio(words, sentences);
    let syllables_per_word = ratio(syllables, words);
    Readability {
        sentences,
        mean_sentence_length: words_per_sentence,
        sentence_length_stddev: 0.0,
        mean_syllables_per_word: syllables_per_word,
        flesch_reading_ease: 206.835 - 1.015 * words_per_sentence - 84.6 * syllables_per_word,
    }
}

/// Standard deviation of sentence length from a `(length, count)` histogram.
///
/// The distribution is the point, not the mean: uniform sentence length reads
/// as monotone, high variance as conversational. A running average would have
/// discarded exactly the half that carries the style signal.
pub fn sentence_length_stddev(histogram: &[(i64, i64)]) -> f64 {
    let n: i64 = histogram.iter().map(|(_, c)| *c).sum();
    if n < 2 {
        return 0.0;
    }
    let mean = histogram
        .iter()
        .map(|(len, c)| (*len * *c) as f64)
        .sum::<f64>()
        / n as f64;
    let variance = histogram
        .iter()
        .map(|(len, c)| {
            let d = *len as f64 - mean;
            d * d * *c as f64
        })
        .sum::<f64>()
        / n as f64;
    variance.sqrt()
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Vowel-group syllable count with a silent-`e` correction. A heuristic, and
/// the weakest input to Flesch — good enough to trend, not to grade.
pub fn count_syllables(word: &str) -> u64 {
    let chars: Vec<char> = word.chars().collect();
    let is_vowel = |c: char| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y');

    let mut count = 0u64;
    let mut prev_vowel = false;
    for &c in &chars {
        let vowel = is_vowel(c);
        if vowel && !prev_vowel {
            count += 1;
        }
        prev_vowel = vowel;
    }
    // Trailing silent 'e', but never down to zero: "the" keeps its syllable.
    // Consonant + "le" is the standard exception — that 'e' is pronounced, so
    // "simple" and "table" are two syllables, not one.
    let consonant_le = chars.len() >= 3
        && chars[chars.len() - 2..] == ['l', 'e']
        && !is_vowel(chars[chars.len() - 3]);
    if count > 1 && chars.last() == Some(&'e') && !consonant_le {
        count -= 1;
    }
    count.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(w, c)| (w.to_string(), *c)).collect()
    }

    #[test]
    fn counts_types_and_tokens() {
        let r = from_counts("test", &counts(&[("ship", 3), ("small", 1)]));
        assert_eq!(r.vocabulary.tokens, 4);
        assert_eq!(r.vocabulary.types, 2);
        assert_eq!(r.vocabulary.type_token_ratio, 0.5);
    }

    #[test]
    fn hapax_ratio_counts_words_used_once() {
        let r = from_counts("test", &counts(&[("ship", 5), ("rare", 1), ("scarce", 1)]));
        // Two of three distinct words appear exactly once.
        assert!((r.vocabulary.hapax_ratio - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn guiraud_is_stable_where_ttr_is_not() {
        // Same vocabulary richness, ten times the text: TTR collapses,
        // Guiraud stays comparable. That's why it's the headline number.
        let small = from_counts("s", &counts(&[("a", 1), ("b", 1), ("c", 1), ("d", 1)]));
        let large = from_counts("l", &counts(&[("a", 10), ("b", 10), ("c", 10), ("d", 10)]));
        assert!(large.vocabulary.type_token_ratio < small.vocabulary.type_token_ratio / 5.0);
        assert!(large.vocabulary.guiraud_r > small.vocabulary.type_token_ratio / 5.0);
    }

    #[test]
    fn empty_input_does_not_divide_by_zero() {
        let r = from_counts("empty", &HashMap::new());
        assert_eq!(r.vocabulary.tokens, 0);
        assert_eq!(r.vocabulary.guiraud_r, 0.0);
        assert_eq!(r.vocabulary.type_token_ratio, 0.0);
    }

    #[test]
    fn text_mode_adds_the_sentence_level() {
        let r = from_text("text", "We ship small changes. They are easier to review.");
        let readability = r.readability.unwrap();
        assert_eq!(readability.sentences, 2);
        assert!(readability.mean_sentence_length > 3.0);
    }

    #[test]
    fn syllable_heuristic_is_roughly_right() {
        assert_eq!(count_syllables("ship"), 1);
        assert_eq!(count_syllables("the"), 1);
        assert_eq!(count_syllables("simple"), 2);
        assert_eq!(count_syllables("table"), 2);
        assert_eq!(count_syllables("vocabulary"), 5);
        // Silent 'e' still drops where it genuinely is silent.
        assert_eq!(count_syllables("shape"), 1);
        assert_eq!(count_syllables("ship"), 1);
    }

    #[test]
    fn stddev_separates_uniform_prose_from_varied_prose() {
        let uniform = [(10, 4)];
        let varied = [(2, 2), (10, 1), (24, 1)];
        assert_eq!(sentence_length_stddev(&uniform), 0.0);
        assert!(sentence_length_stddev(&varied) > 5.0);
    }

    #[test]
    fn stddev_needs_more_than_one_sentence() {
        assert_eq!(sentence_length_stddev(&[(12, 1)]), 0.0);
        assert_eq!(sentence_length_stddev(&[]), 0.0);
    }

    #[test]
    fn splits_prose_into_sentences() {
        let parts = split_sentences("One thing. Then another! And a third?");
        assert_eq!(parts.len(), 3);
        // A trailing fragment still counts.
        assert_eq!(split_sentences("no punctuation").len(), 1);
        assert!(split_sentences("   ").is_empty());
    }

    #[test]
    fn readability_from_totals_matches_the_text_path() {
        let text = "We ship small changes. They are easier to review.";
        let from_prose = from_text("t", text).readability.unwrap();
        let from_sums = readability_from_totals(
            from_prose.sentences,
            from_text("t", text).vocabulary.tokens,
            (from_prose.mean_syllables_per_word * from_text("t", text).vocabulary.tokens as f64)
                .round() as u64,
        );
        assert!((from_prose.flesch_reading_ease - from_sums.flesch_reading_ease).abs() < 1.0);
    }

    #[test]
    fn corpus_mode_reports_no_readability() {
        // The prose is gone by then — the limitation is explicit, not silent.
        assert!(
            from_counts("lexicon", &counts(&[("ship", 1)]))
                .readability
                .is_none()
        );
    }
}
