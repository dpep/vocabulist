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

    for line in text.lines() {
        let normalized = crate::text::normalize_typography(line);
        for token in crate::text::tokenize(&normalized) {
            let word = crate::text::normalize(&token.text);
            if word.chars().count() < 2 || !word.chars().any(|c| c.is_alphabetic()) {
                continue;
            }
            syllables += count_syllables(&word);
            *counts.entry(word).or_insert(0) += 1;
        }
    }

    let mut report = from_counts(scope, &counts);
    let sentences = count_sentences(text);
    let tokens = report.vocabulary.tokens;

    let words_per_sentence = ratio(tokens, sentences);
    let syllables_per_word = ratio(syllables, tokens);
    report.readability = Some(Readability {
        sentences,
        mean_sentence_length: words_per_sentence,
        mean_syllables_per_word: syllables_per_word,
        flesch_reading_ease: 206.835 - 1.015 * words_per_sentence - 84.6 * syllables_per_word,
    });
    report
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Sentences, by terminal punctuation. A trailing fragment counts as one, so
/// a single unpunctuated line isn't reported as zero sentences.
fn count_sentences(text: &str) -> u64 {
    let mut sentences = 0u64;
    let mut pending = false;
    for ch in text.chars() {
        if matches!(ch, '.' | '!' | '?') {
            if pending {
                sentences += 1;
                pending = false;
            }
        } else if ch.is_alphanumeric() {
            pending = true;
        }
    }
    sentences + u64::from(pending)
}

/// Vowel-group syllable count with a silent-`e` correction. A heuristic, and
/// the weakest input to Flesch — good enough to trend, not to grade.
fn count_syllables(word: &str) -> u64 {
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
    fn counts_a_trailing_fragment_as_a_sentence() {
        assert_eq!(count_sentences("no terminal punctuation here"), 1);
        assert_eq!(count_sentences("One. Two! Three?"), 3);
        assert_eq!(count_sentences(""), 0);
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
    fn corpus_mode_reports_no_readability() {
        // The prose is gone by then — the limitation is explicit, not silent.
        assert!(
            from_counts("lexicon", &counts(&[("ship", 1)]))
                .readability
                .is_none()
        );
    }
}
