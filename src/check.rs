//! The checker: decide which words in a piece of text look wrong.
//!
//! The asymmetry drives every decision here. A false "misspelled" is
//! expensive — it trains you to ignore the squiggle, and once you do, the
//! tool is dead. A missed typo costs almost nothing. So the default answer is
//! *accept*, and a word has to work to get flagged.

use std::collections::HashSet;

use crate::ngram;
use crate::text::{self, Token};
use crate::types::{Finding, FindingKind};

/// Maximum edit distance we'll suggest across.
const MAX_EDIT_DISTANCE: usize = 2;
/// Most suggestions to return per finding.
const MAX_SUGGESTIONS: usize = 3;

/// The resolved word sets a check runs against. The lexicon is the authority;
/// the system dictionary is the floor beneath it.
pub struct Checker {
    lexicon: HashSet<String>,
    dictionary: Option<HashSet<String>>,
}

impl Checker {
    pub fn new(lexicon: HashSet<String>, dictionary: Option<HashSet<String>>) -> Self {
        Self {
            lexicon,
            dictionary,
        }
    }

    /// Is this word known to either set?
    pub fn knows(&self, word: &str) -> bool {
        if self.lexicon.contains(word) {
            return true;
        }
        // A word can be in the lexicon only as an identifier part
        // (`rubocop_todo` → `rubocop`, `todo`); those count as known too.
        match &self.dictionary {
            Some(d) => crate::dict::contains(d, word),
            None => false,
        }
    }

    /// Check one line, returning findings tagged with `line`/`col`.
    ///
    /// `evidence` supplies n-gram counts for the real-word pass; pass a
    /// closure returning 0 to skip it entirely.
    pub fn check_line(
        &self,
        line: &str,
        line_no: usize,
        evidence: &mut impl FnMut(&str) -> i64,
    ) -> Vec<Finding> {
        if !text::is_prose_line(line) {
            return Vec::new();
        }
        let masked = text::mask_non_prose(line);
        let tokens = text::tokenize(&masked);

        let normalized: Vec<String> = tokens.iter().map(|t| text::normalize(&t.text)).collect();

        let mut findings = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            if !text::is_checkable(&token.text) {
                continue;
            }
            let word = &normalized[i];

            if !self.knows(word) {
                findings.push(self.unknown_finding(token, word, line_no));
                continue;
            }

            // Known word — but is it the right known word? Only collocation
            // evidence can tell, and only for the confusion sets.
            let prev = i.checked_sub(1).map(|j| normalized[j].as_str());
            let next = normalized.get(i + 1).map(|s| s.as_str());
            if let Some(hit) = ngram::check_real_word(prev, word, next, evidence) {
                findings.push(Finding {
                    kind: FindingKind::RealWord,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    confidence: hit.confidence(),
                    suggestions: vec![hit.suggestion],
                });
            }
        }
        findings
    }

    fn unknown_finding(&self, token: &Token, word: &str, line_no: usize) -> Finding {
        let suggestions = self.suggest(word);
        // A word with a near neighbour is more likely a typo than a coinage;
        // one with no neighbour at all is probably jargon we haven't met.
        let confidence = if suggestions.is_empty() { 0.35 } else { 0.70 };
        Finding {
            kind: FindingKind::Unknown,
            word: token.text.clone(),
            line: line_no,
            col: token.col,
            suggestions,
            confidence,
        }
    }

    /// Ranked replacements within `MAX_EDIT_DISTANCE`. Lexicon words rank
    /// above dictionary words — if you have a word for it, it's your word.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        // (distance, -prefix, -suffix, source rank, word) — every field sorts
        // ascending, so the agreement lengths are negated to put the closest
        // candidate first. Shape outranks source: your lexicon is full of
        // short binary names that sit one edit from everything, and letting
        // provenance win would bury the obvious correction under them.
        // Preferring your own vocabulary is a tie-break, not a trump card.
        let mut scored: Vec<(usize, isize, isize, u8, &String)> = Vec::new();
        let dictionary = self.dictionary.iter().flatten();
        for (candidate, rank) in self
            .lexicon
            .iter()
            .map(|c| (c, 0u8))
            .chain(dictionary.map(|c| (c, 1u8)))
        {
            if let Some(d) = bounded_distance(word, candidate, MAX_EDIT_DISTANCE) {
                let (prefix, suffix) = affinity(word, candidate);
                scored.push((d, -(prefix as isize), -(suffix as isize), rank, candidate));
            }
        }

        scored.sort();
        scored.dedup_by(|a, b| a.4 == b.4);
        scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, _, _, _, w)| w.clone())
            .collect()
    }
}

/// Shared leading and trailing characters, as `(prefix, suffix)`.
///
/// Edit distance alone leaves `small`, `sal`, `mal`, and `ismal` all one edit
/// from `smal`, and an alphabetical tie-break then offers the worst of them
/// first. Leading agreement is the stronger signal — people rarely fumble the
/// start of a word — so callers order on the prefix and fall back to the
/// suffix. Summing the two would be worse than either: a front insertion and a
/// back insertion both preserve the whole word, so the sum ties them.
fn affinity(a: &str, b: &str) -> (usize, usize) {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, suffix)
}

/// Levenshtein distance, abandoned once it exceeds `max`. Returns `None` when
/// the words are further apart than that — the common case, so bailing early
/// is what keeps a full-lexicon scan cheap.
pub fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let (a_len, b_len) = (a.chars().count(), b.chars().count());
    if a_len.abs_diff(b_len) > max {
        return None;
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut current = vec![0usize; b_len + 1];

    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        let mut row_min = current[0];
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
            row_min = row_min.min(current[j + 1]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev, &mut current);
    }
    let d = prev[b_len];
    (d <= max).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(lexicon: &[&str], dictionary: &[&str]) -> Checker {
        Checker::new(
            lexicon.iter().map(|s| s.to_string()).collect(),
            Some(dictionary.iter().map(|s| s.to_string()).collect()),
        )
    }

    fn no_evidence(_: &str) -> i64 {
        0
    }

    #[test]
    fn accepts_lexicon_jargon_the_dictionary_never_heard_of() {
        let c = checker(&["contextdb", "rubocop"], &["and", "are", "fine"]);
        let f = c.check_line("contextdb and rubocop are fine", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn flags_a_genuine_typo_and_suggests() {
        let c = checker(&[], &["ship", "the", "change"]);
        let f = c.check_line("shp the change", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].word, "shp");
        assert_eq!(f[0].kind, FindingKind::Unknown);
        assert!(f[0].suggestions.contains(&"ship".to_string()));
    }

    #[test]
    fn reports_position() {
        let c = checker(&[], &["the", "change"]);
        let f = c.check_line("the zzzqx change", 7, &mut no_evidence);
        assert_eq!((f[0].line, f[0].col), (7, 5));
    }

    #[test]
    fn never_flags_urls_paths_or_code_spans() {
        let c = checker(&[], &["see", "and", "now"]);
        let f = c.check_line(
            "see https://github.com/dpep/ae and `foo_bar` now",
            1,
            &mut no_evidence,
        );
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn skips_code_shaped_lines_entirely() {
        let c = checker(&[], &["let"]);
        assert!(
            c.check_line("    let zzzqx = 1;", 1, &mut no_evidence)
                .is_empty()
        );
        assert!(c.check_line("```rust", 1, &mut no_evidence).is_empty());
    }

    #[test]
    fn unknown_word_without_a_neighbour_is_low_confidence() {
        let c = checker(&[], &["ship"]);
        let f = c.check_line("the zzzqxwv thing", 1, &mut no_evidence);
        assert!(
            f[0].confidence < 0.5,
            "jargon should not be confidently wrong"
        );
    }

    #[test]
    fn prefers_lexicon_words_when_candidates_match_equally_well() {
        // Both are distance 1 from "shix" and agree on the same 3 characters,
        // so provenance is all that's left to separate them.
        let c = checker(&["shiv"], &["shin"]);
        assert_eq!(c.suggest("shix").first().unwrap(), "shiv");
    }

    #[test]
    fn a_closer_dictionary_word_beats_a_scrappier_lexicon_one() {
        // `sh` is a real binary and one edit away, but `ship` keeps more of
        // the word — shape has to win or short command names bury everything.
        let c = checker(&["sh", "scp"], &["ship"]);
        assert_eq!(c.suggest("shp").first().unwrap(), "ship");
    }

    #[test]
    fn catches_a_real_word_error_when_collocations_support_it() {
        let c = checker(&[], &["apart", "form", "from", "the", "rest"]);
        let mut evidence = |gram: &str| match gram {
            "apart from" => 20,
            "from the" => 50,
            _ => 0,
        };
        let f = c.check_line("apart form the rest", 1, &mut evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::RealWord);
        assert_eq!(f[0].suggestions, vec!["from"]);
    }

    #[test]
    fn ranks_the_shape_preserving_candidate_first() {
        // All four are one edit from "smal"; only affinity separates them.
        let c = checker(&[], &["small", "sal", "mal", "ismal"]);
        assert_eq!(c.suggest("smal").first().unwrap(), "small");
    }

    #[test]
    fn affinity_weighs_the_start_of_a_word_most() {
        // A front insertion and a back insertion both preserve every
        // character, so only the leading agreement separates them.
        assert!(affinity("smal", "small").0 > affinity("smal", "ismal").0);
        assert!(affinity("shp", "ship").0 > affinity("shp", "php").0);
    }

    #[test]
    fn affinity_uses_the_suffix_to_break_a_prefix_tie() {
        let (ship_prefix, ship_suffix) = affinity("shp", "ship");
        let (sh_prefix, sh_suffix) = affinity("shp", "sh");
        assert_eq!(ship_prefix, sh_prefix);
        assert!(ship_suffix > sh_suffix);
    }

    #[test]
    fn bounded_distance_bails_past_the_limit() {
        assert_eq!(bounded_distance("ship", "ship", 2), Some(0));
        assert_eq!(bounded_distance("ship", "shp", 2), Some(1));
        assert_eq!(bounded_distance("ship", "elephant", 2), None);
    }
}
