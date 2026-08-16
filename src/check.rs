//! The checker: decide which words in a piece of text look wrong.
//!
//! The asymmetry drives every decision here. A false "misspelled" is
//! expensive — it trains you to ignore the squiggle, and once you do, the
//! tool is dead. A missed typo costs almost nothing. So the default answer is
//! *accept*, and a word has to work to get flagged.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::ngram;
use crate::profile::Profile;
use crate::text::{self, Token};
use crate::types::{Finding, FindingKind, Suggestion};

/// Maximum edit distance we'll suggest across.
const MAX_EDIT_DISTANCE: usize = 2;
/// Most suggestions to return per finding.
const MAX_SUGGESTIONS: usize = 3;
/// How often a word must appear in mined prose before it counts as real.
///
/// Measured on a held-out corpus, precision rises as this falls — 86% at 3,
/// 89% at 2, 91% at 1 — with recall flat. Two rather than one anyway: the
/// injections can't measure the risk that actually matters here, because a
/// synthetic typo never appears in local prose while a real one in someone's
/// README does. Requiring corroboration is the same principle the rest of the
/// store uses, and the two-point precision difference doesn't buy out of it.
const MIN_CORPUS_EVIDENCE: i64 = 2;

/// How much less likely each additional edit is. Rough, and rough is enough:
/// the point is that a two-edit correction should lose badly to a one-edit
/// correction of comparable frequency.
const EDIT_PENALTY: f64 = 0.05;

/// Bonus when the typo's letters all appear, in order, inside the candidate —
/// meaning the correction is pure insertion.
///
/// Uniform edit cost is the weakest part of this model, and this is the
/// cheapest useful correction to it. `plese` → `please` inserts a letter;
/// `plese` → `these` substitutes `p` for `t`, keys nowhere near each other.
/// Both are one edit, but dropping a letter is a far commoner slip than
/// striking a key across the keyboard — and without this, the more frequent
/// word wins regardless of how implausible the edit was.
const SUBSEQUENCE_BONUS: f64 = 25.0;

/// Below this share of the belief, a suggestion is noise rather than an
/// option. The top candidate always survives regardless.
const MIN_SUGGESTION_SCORE: f32 = 0.02;

/// The resolved word sets a check runs against. The lexicon is the authority;
/// the system dictionary is the floor beneath it.
pub struct Checker {
    lexicon: HashSet<String>,
    /// General-English frequency, for breaking ties the dictionary can't.
    frequency: std::collections::HashMap<String, i64>,
    /// Loaded on first miss, not up front. Reading ~236k words costs tens of
    /// milliseconds, and text whose words are all in the lexicon never needs
    /// it — which is the common case once the lexicon is seeded.
    dictionary: OnceCell<Option<HashSet<String>>>,
    profile: Rc<Profile>,
}

impl Checker {
    /// A checker over an explicit backstop — used by tests, which supply a
    /// fixture dictionary rather than reading the system one.
    pub fn new(lexicon: HashSet<String>, dictionary: Option<HashSet<String>>) -> Self {
        let cell = OnceCell::new();
        let _ = cell.set(dictionary);
        Self {
            lexicon,
            frequency: std::collections::HashMap::new(),
            dictionary: cell,
            profile: Rc::new(Profile::disabled()),
        }
    }

    /// A checker that reads the system word list on demand.
    pub fn with_profile(lexicon: HashSet<String>, profile: Rc<Profile>) -> Self {
        Self {
            lexicon,
            frequency: std::collections::HashMap::new(),
            dictionary: OnceCell::new(),
            profile,
        }
    }

    /// Attach general-English frequencies, used to rank suggestions and to
    /// judge confusions before personal evidence exists.
    pub fn with_frequency(mut self, frequency: std::collections::HashMap<String, i64>) -> Self {
        self.frequency = frequency;
        self
    }

    fn frequency_of(&self, word: &str) -> i64 {
        self.frequency.get(word).copied().unwrap_or(0)
    }

    /// The backstop, loading it on first use.
    fn dictionary(&self) -> Option<&HashSet<String>> {
        self.dictionary
            .get_or_init(|| {
                let loaded = self.profile.time("dictionary_load", crate::dict::load);
                self.profile.count(
                    "dictionary_words",
                    loaded.as_ref().map_or(0, |d| d.len()) as u64,
                );
                loaded
            })
            .as_ref()
    }

    /// Is this word known to either set?
    ///
    /// A hyphenated compound counts as known when all of its parts are.
    /// English forms these freely and no word list can enumerate them — the
    /// system dictionary carries two hyphenated entries in 236k — so without
    /// this, `well-known`, `long-term`, and `local-first` all read as typos.
    /// That's the exact false-positive class this tool exists to remove.
    pub fn knows(&self, word: &str) -> bool {
        if self.knows_atom(word) {
            return true;
        }
        if !word.contains('-') {
            return false;
        }
        let mut parts = word.split('-').filter(|p| !p.is_empty()).peekable();
        let mut any = false;
        for part in &mut parts {
            any = true;
            // Short fragments (`e-mail`, `x-ray`) carry no signal, and the
            // checker already declines to judge words this short on their own.
            if part.chars().count() < 3 {
                continue;
            }
            if !self.knows_atom(part) {
                return false;
            }
        }
        any
    }

    /// A single word, against the lexicon, then mined prose, then the
    /// backstop dictionary.
    ///
    /// Mined prose sits above the dictionary because the dictionary is
    /// `web2` — Webster's Second International, published 1934. It has no
    /// `inline`, `download`, `roadmap`, or `pre`, so a word list alone flags
    /// ordinary modern English as misspelled. Words seen repeatedly in real
    /// prose on this machine are real words, and the repetition threshold is
    /// what keeps a typo in someone's README from qualifying.
    fn knows_atom(&self, word: &str) -> bool {
        if self.lexicon.contains(word) {
            return true;
        }
        if self.frequency_of(word) >= MIN_CORPUS_EVIDENCE {
            return true;
        }
        match self.dictionary() {
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
        self.profile.count("lines_seen", 1);
        if !text::is_prose_line(line) {
            return Vec::new();
        }
        self.profile.count("lines_checked", 1);
        let line = text::normalize_typography(line);
        let masked = text::mask_non_prose(&line);
        let tokens = text::tokenize(&masked);

        let normalized: Vec<String> = tokens.iter().map(|t| text::normalize(&t.text)).collect();
        self.profile.count("tokens", tokens.len() as u64);

        let mut findings = Vec::new();
        let mut sentence_initial = true;
        for (i, token) in tokens.iter().enumerate() {
            let starts_sentence = sentence_initial;
            // The next token begins a sentence only if this one ended one.
            sentence_initial = ends_sentence(&masked, token, &tokens.get(i + 1).map(|t| t.col));

            if !text::is_checkable(&token.text) {
                continue;
            }
            if text::is_proper_noun(&token.text, starts_sentence) {
                self.profile.count("proper_nouns_skipped", 1);
                continue;
            }
            self.profile.count("tokens_checked", 1);
            let word = &normalized[i];

            // Before the known-word gate, not after: `dont`, `didnt`, and
            // `thats` are all *in* the system word list, so gating on
            // "unknown" made this unreachable for the words it targets.
            if let Some(fixed) = crate::contraction::expand(word) {
                findings.push(Finding {
                    kind: FindingKind::Contraction,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    suggestions: vec![Suggestion {
                        word: fixed.to_string(),
                        score: 1.0,
                    }],
                    confidence: crate::contraction::CONFIDENCE,
                });
                continue;
            }

            if !self.knows(word) {
                findings.push(self.unknown_finding(token, word, line_no));
                continue;
            }

            // Known word — but is it the right known word? Collocation
            // evidence answers this best, and frequency answers it at all
            // when the corpus is still empty.
            let prev = i.checked_sub(1).map(|j| normalized[j].as_str());
            let next = normalized.get(i + 1).map(|s| s.as_str());
            if let Some(hit) = ngram::check_real_word(prev, word, next, evidence) {
                findings.push(Finding {
                    kind: FindingKind::RealWord,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    confidence: hit.confidence(),
                    suggestions: vec![Suggestion {
                        word: hit.suggestion,
                        score: 1.0,
                    }],
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
    pub fn suggest(&self, word: &str) -> Vec<Suggestion> {
        // (distance, -frequency, -prefix, -suffix, source rank, word) — every
        // field sorts ascending, so the values that should win are negated.
        //
        // Frequency sits above shape because it's the stronger evidence when
        // it exists: `aviod` shares three leading letters with `avid` and only
        // two with `avoid`, so prefix agreement alone picks the wrong one.
        // Shape then decides among candidates no frequency list knows, and
        // outranks source — your lexicon is full of short binary names that
        // sit one edit from everything, and letting provenance win would bury
        // the obvious correction under them.
        self.profile.count("suggest_calls", 1);
        let mut scored: Vec<(usize, i64, isize, isize, u8, &String)> = Vec::new();
        let dictionary = self.dictionary().into_iter().flatten();
        for (candidate, rank) in self
            .lexicon
            .iter()
            .map(|c| (c, 0u8))
            .chain(dictionary.map(|c| (c, 1u8)))
        {
            // Every known word is measured against every unknown one. This
            // counter is what makes that cost visible under --profile.
            self.profile.count("candidates_scanned", 1);
            if let Some(d) = bounded_distance(word, candidate, MAX_EDIT_DISTANCE) {
                let (prefix, suffix) = affinity(word, candidate);
                scored.push((
                    d,
                    -self.frequency_of(candidate),
                    -(prefix as isize),
                    -(suffix as isize),
                    rank,
                    candidate,
                ));
            }
        }
        self.profile.count("candidates_kept", scored.len() as u64);

        scored.sort();
        scored.dedup_by(|a, b| a.5 == b.5);
        let kept: Vec<(usize, &String)> = scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(d, _, _, _, _, w)| (d, w))
            .collect();

        // Noisy channel, in miniature: weight each candidate by how likely the
        // word is at all, times how likely this typo is given that word.
        // Frequency supplies the first term; edit distance stands in for the
        // second, since a second edit is far rarer than a first.
        let mut weighted: Vec<(f64, &String)> = kept
            .iter()
            .map(|(distance, candidate)| {
                let prior = (self.frequency_of(candidate) + 1) as f64;
                let mut weight = prior * EDIT_PENALTY.powi(*distance as i32);
                if is_subsequence(word, candidate) {
                    weight *= SUBSEQUENCE_BONUS;
                }
                (weight, *candidate)
            })
            .collect();

        // Rank by the score, not by the candidate-generation order. They
        // disagree — generation orders by edit distance then shape, while the
        // score weighs how likely the word is against how likely the slip is —
        // and a list whose order contradicts its own numbers is worse than
        // either alone.
        weighted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let total: f64 = weighted.iter().map(|(w, _)| w).sum();
        let mut out: Vec<Suggestion> = weighted
            .into_iter()
            .map(|(weight, candidate)| Suggestion {
                word: candidate.clone(),
                // Normalized, so the scores read as a distribution over the
                // candidates offered rather than as unrelated magnitudes.
                // Rounded: these are estimates from a rough error model, and
                // printing 0.8823529 implies a precision that isn't there.
                score: if total > 0.0 {
                    round_to(weight / total, 3)
                } else {
                    0.0
                },
            })
            .collect();

        // Drop the also-rans. Offering `help 1.00, hep 0.00, heal 0.00` asks
        // the reader to weigh two options the model has already dismissed;
        // the first is always kept so a finding is never left with no fix.
        let mut index = 0;
        out.retain(|s| {
            index += 1;
            index == 1 || s.score >= MIN_SUGGESTION_SCORE
        });
        out
    }
}

/// Does a sentence end between this token and the next?
///
/// Looks at the characters separating them rather than the token itself, so
/// `e.g.` mid-sentence doesn't reset the state for every abbreviation.
fn ends_sentence(line: &str, token: &Token, next_col: &Option<usize>) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let start = token.col - 1 + token.text.chars().count();
    let end = next_col.map_or(chars.len(), |c| (c - 1).min(chars.len()));
    if start >= end {
        return false;
    }
    chars[start..end]
        .iter()
        .any(|c| matches!(c, '.' | '!' | '?' | ':'))
}

/// A stateful pass over a whole document.
///
/// Some things that shouldn't be spell-checked can't be recognized one line
/// at a time. A fenced code block is the clear case: every line inside it is
/// code, but a line reading `# returns the users nam` looks like prose in
/// isolation. Deciding that requires remembering the fence opened above.
///
/// The same reasoning covers YAML front matter, which is configuration
/// wearing a colon.
pub struct Scanner<'a> {
    checker: &'a Checker,
    fence: Option<String>,
    in_front_matter: bool,
    line_no: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(checker: &'a Checker) -> Self {
        Self {
            checker,
            fence: None,
            in_front_matter: false,
            line_no: 0,
        }
    }

    /// Feed the next line. Returns findings for it, or nothing if the line
    /// sits in a region where spelling doesn't apply.
    pub fn feed(&mut self, line: &str, evidence: &mut impl FnMut(&str) -> i64) -> Vec<Finding> {
        self.line_no += 1;
        let trimmed = line.trim();

        // Front matter: `---` on the very first line opens it.
        if self.line_no == 1 && trimmed == "---" {
            self.in_front_matter = true;
            return Vec::new();
        }
        if self.in_front_matter {
            if trimmed == "---" || trimmed == "..." {
                self.in_front_matter = false;
            }
            return Vec::new();
        }

        // Fences: ``` or ~~~, closed by the same marker. Tracking which one
        // opened the block keeps a ``` inside a ~~~ block from closing it.
        if let Some(marker) = &self.fence {
            if trimmed.starts_with(marker.as_str()) {
                self.fence = None;
            }
            return Vec::new();
        }
        if let Some(marker) = fence_marker(trimmed) {
            self.fence = Some(marker);
            return Vec::new();
        }

        self.checker.check_line(line, self.line_no, evidence)
    }

    /// True when the scanner is inside a region it's skipping — useful for
    /// callers that want to report why nothing came back.
    pub fn skipping(&self) -> bool {
        self.fence.is_some() || self.in_front_matter
    }
}

/// The fence marker a line opens, if it opens one.
fn fence_marker(trimmed: &str) -> Option<String> {
    for marker in ["```", "~~~"] {
        if trimmed.starts_with(marker) {
            return Some(marker.to_string());
        }
    }
    None
}

/// Round to `places` decimals. The scores come from a deliberately rough
/// error model; carrying seventeen significant figures through the JSON
/// claims a precision the model doesn't have.
fn round_to(value: f64, places: u32) -> f32 {
    let factor = 10f64.powi(places as i32);
    ((value * factor).round() / factor) as f32
}

/// Do all of `needle`'s characters appear in `haystack`, in order?
///
/// True exactly when the correction is pure insertion — the typo dropped
/// letters rather than mistyping them.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
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

/// Edit distance counting a transposition as **one** edit, abandoned once it
/// exceeds `max`. Returns `None` when the words are further apart than that —
/// the common case, so bailing early is what keeps a full-lexicon scan cheap.
///
/// This is Damerau-Levenshtein (optimal string alignment) rather than plain
/// Levenshtein, because swapping two adjacent letters is one of the most
/// common ways to mistype a word and plain Levenshtein charges it as two
/// substitutions. That difference is not academic: it puts `aviod` two edits
/// from `avoid` but only one from `avid`, so the obvious correction loses to
/// a worse one.
pub fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (a_len, b_len) = (a_chars.len(), b_chars.len());
    if a_len.abs_diff(b_len) > max {
        return None;
    }

    // Three rows, because a transposition looks back two positions.
    let mut prev_prev: Vec<usize> = vec![0; b_len + 1];
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut current = vec![0usize; b_len + 1];

    for i in 0..a_len {
        current[0] = i + 1;
        let mut row_min = current[0];
        for j in 0..b_len {
            let cost = usize::from(a_chars[i] != b_chars[j]);
            let mut best = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
            if i > 0 && j > 0 && a_chars[i] == b_chars[j - 1] && a_chars[i - 1] == b_chars[j] {
                best = best.min(prev_prev[j - 1] + 1);
            }
            current[j + 1] = best;
            row_min = row_min.min(best);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev_prev, &mut prev);
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

    /// Just the words, for assertions that don't care about the scores.
    fn words(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.word.as_str()).collect()
    }

    #[test]
    fn accepts_lexicon_jargon_the_dictionary_never_heard_of() {
        let c = checker(&["contextdb", "rubocop"], &["and", "are", "fine"]);
        let f = c.check_line("contextdb and rubocop are fine", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn accepts_hyphenated_compounds_built_from_known_parts() {
        // No word list enumerates these; English builds them on demand.
        let c = checker(
            &["local", "first"],
            &["well", "known", "long", "term", "design"],
        );
        let f = c.check_line(
            "a well-known long-term local-first design",
            1,
            &mut no_evidence,
        );
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn still_flags_a_compound_whose_part_is_misspelled() {
        let c = checker(&[], &["well", "known", "result"]);
        let f = c.check_line("a well-knwon result", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].word, "well-knwon");
    }

    #[test]
    fn fixes_contractions_typed_without_an_apostrophe() {
        let c = checker(&[], &["we", "ship", "that"]);
        let f = c.check_line("we dont ship that", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Contraction);
        assert_eq!(words(&f[0].suggestions), vec!["don't"]);
        assert!(f[0].confidence > 0.8);
    }

    #[test]
    fn contractions_are_caught_even_when_the_word_list_contains_them() {
        // `dont`, `didnt` and `thats` are all in /usr/share/dict/words, so a
        // check gated on "unknown word" would never reach them.
        let c = checker(&[], &["we", "dont", "ship", "that"]);
        let f = c.check_line("we dont ship that", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Contraction);
        assert_eq!(words(&f[0].suggestions), vec!["don't"]);
    }

    #[test]
    fn a_curly_apostrophe_reads_as_the_same_word() {
        // What macOS, Slack, and Gmail actually emit.
        let c = checker(&[], &["we", "do", "ship", "that"]);
        assert!(
            c.check_line("we don\u{2019}t ship that", 1, &mut no_evidence)
                .is_empty()
        );
    }

    #[test]
    fn columns_are_character_positions() {
        let c = checker(&[], &["caf\u{e9}", "the"]);
        // "café the zzzqx" — byte offsets would drift by the accent.
        let f = c.check_line("caf\u{e9} the zzzqx", 1, &mut no_evidence);
        assert_eq!(f[0].word, "zzzqx");
        assert_eq!(f[0].col, 10);
    }

    #[test]
    fn handles_non_ascii_without_panicking() {
        let c = checker(&[], &["notes", "from", "the", "trip"]);
        let f = c.check_line("İstanbul notes from the trip", 1, &mut no_evidence);
        // 'İstanbul' is a proper noun we don't know; the point is it survives.
        assert!(f.len() <= 1);
    }

    #[test]
    fn flags_a_genuine_typo_and_suggests() {
        let c = checker(&[], &["ship", "the", "change"]);
        let f = c.check_line("shp the change", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].word, "shp");
        assert_eq!(f[0].kind, FindingKind::Unknown);
        assert!(words(&f[0].suggestions).contains(&"ship"));
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
        assert_eq!(c.suggest("shix").first().unwrap().word, "shiv");
    }

    #[test]
    fn a_closer_dictionary_word_beats_a_scrappier_lexicon_one() {
        // `sh` is a real binary and one edit away, but `ship` keeps more of
        // the word — shape has to win or short command names bury everything.
        let c = checker(&["sh", "scp"], &["ship"]);
        assert_eq!(c.suggest("shp").first().unwrap().word, "ship");
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
        assert_eq!(words(&f[0].suggestions), vec!["from"]);
    }

    #[test]
    fn ranks_the_shape_preserving_candidate_first() {
        // All four are one edit from "smal"; only affinity separates them.
        let c = checker(&[], &["small", "sal", "mal", "ismal"]);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
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
    fn suggestion_scores_form_a_distribution() {
        let frequency: std::collections::HashMap<String, i64> =
            [("help".to_string(), 5_000i64), ("hep".to_string(), 2i64)]
                .into_iter()
                .collect();
        let c = Checker::new(
            HashSet::new(),
            Some(
                ["help", "hep", "heal"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        )
        .with_frequency(frequency);

        let suggestions = c.suggest("hepl");
        assert_eq!(suggestions[0].word, "help");
        let total: f32 = suggestions.iter().map(|s| s.score).sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "scores should sum to 1: {total}"
        );
    }

    #[test]
    fn an_insertion_beats_a_substitution_by_a_commoner_word() {
        // Both are one edit from `plese`, and `these` is the more frequent
        // word — but dropping a letter is a far commoner slip than striking
        // a key across the keyboard.
        let frequency: std::collections::HashMap<String, i64> = [
            ("these".to_string(), 50_000i64),
            ("please".to_string(), 3_000i64),
        ]
        .into_iter()
        .collect();
        let c = Checker::new(
            HashSet::new(),
            Some(["please", "these"].iter().map(|s| s.to_string()).collect()),
        )
        .with_frequency(frequency);

        assert_eq!(c.suggest("plese").first().unwrap().word, "please");
    }

    #[test]
    fn suggestions_are_ordered_by_their_own_scores() {
        let c = checker(&[], &["ship", "shop", "chip"]);
        let suggestions = c.suggest("shp");
        for pair in suggestions.windows(2) {
            assert!(
                pair[0].score >= pair[1].score,
                "order must agree with the scores: {suggestions:?}"
            );
        }
    }

    #[test]
    fn is_subsequence_detects_pure_insertions() {
        assert!(is_subsequence("plese", "please"));
        assert!(!is_subsequence("plese", "these"));
        assert!(is_subsequence("teh", "teach"));
        assert!(!is_subsequence("teh", "the"));
    }

    #[test]
    fn a_transposition_costs_one_edit_not_two() {
        // The reason `aviod` used to suggest `avid` over `avoid`.
        assert_eq!(bounded_distance("aviod", "avoid", 2), Some(1));
        assert_eq!(bounded_distance("teh", "the", 2), Some(1));
        assert_eq!(bounded_distance("recieve", "receive", 2), Some(1));
    }

    #[test]
    fn transposition_plus_frequency_ranks_the_intended_word_first() {
        // Both are one edit away once transposition is free, and `avid`
        // actually shares the longer prefix — so frequency is what decides.
        let frequency: std::collections::HashMap<String, i64> =
            [("avoid".to_string(), 20_000i64)].into_iter().collect();
        let c = Checker::new(
            HashSet::new(),
            Some(
                ["avoid", "avid", "avian"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        )
        .with_frequency(frequency);
        assert_eq!(c.suggest("aviod").first().unwrap().word, "avoid");
    }

    #[test]
    fn shape_still_decides_when_no_frequency_is_known() {
        let c = checker(&[], &["small", "sal", "mal"]);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
    }

    #[test]
    fn frequency_breaks_ties_the_dictionary_cannot() {
        // All three are one edit from "smal" and agree on the same prefix;
        // only how common they are separates them.
        let frequency: std::collections::HashMap<String, i64> =
            [("small".to_string(), 50_000i64)].into_iter().collect();
        let c = Checker::new(
            HashSet::new(),
            Some(
                ["small", "smalm", "smalt"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        )
        .with_frequency(frequency);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
    }

    #[test]
    fn skips_mid_sentence_capitals_as_proper_nouns() {
        let c = checker(&[], &["we", "use", "and", "for", "this"]);
        let f = c.check_line("we use Guiraud and Zblorgian for this", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn still_checks_a_capital_that_opens_a_sentence() {
        let c = checker(&[], &["the", "word"]);
        let f = c.check_line("Zzzqxwv the word", 1, &mut no_evidence);
        assert_eq!(f.len(), 1, "sentence-initial caps carry no name signal");
    }

    #[test]
    fn resumes_checking_capitals_after_a_full_stop() {
        let c = checker(&[], &["done", "the", "word"]);
        let f = c.check_line("done. Zzzqxwv the word", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn skips_pluralized_acronyms() {
        let c = checker(&[], &["and", "the"]);
        let f = c.check_line("the URLs and PRs and IDs", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn mined_prose_covers_words_the_1934_dictionary_lacks() {
        // web2 has no "inline", "download", or "roadmap".
        let frequency: std::collections::HashMap<String, i64> =
            [("inline".to_string(), 9i64), ("roadmap".to_string(), 5i64)]
                .into_iter()
                .collect();
        let c = Checker::new(HashSet::new(), Some(HashSet::new())).with_frequency(frequency);
        assert!(c.knows("inline"));
        assert!(c.knows("roadmap"));
        // But a word seen once is not yet evidence of anything.
        assert!(!c.knows("zzzqxwv"));
    }

    #[test]
    fn skips_everything_inside_a_fenced_block() {
        let c = checker(&[], &["real", "prose", "here"]);
        let doc = "real prose here\n```\nzzzqx zzzqxwv qqxjjv\n```\nreal prose here";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn a_different_fence_marker_does_not_close_the_block() {
        let c = checker(&[], &[]);
        let doc = "~~~\nzzzqx\n```\nzzzqxwv\n~~~";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn resumes_checking_after_the_block_closes() {
        let c = checker(&[], &["and", "then"]);
        let doc = "```\nzzzqx\n```\nand then zzzqxwv";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].word, "zzzqxwv");
        assert_eq!(findings[0].line, 4, "line numbers survive skipped regions");
    }

    #[test]
    fn skips_yaml_front_matter() {
        let c = checker(&[], &["real", "prose"]);
        let doc = "---\nname: zzzqx\ndescription: zzzqxwv\n---\nreal prose";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn a_horizontal_rule_midway_is_not_front_matter() {
        // `---` only opens front matter on line 1.
        let c = checker(&[], &["some", "prose"]);
        let doc = "some prose\n---\nzzzqxwv";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn bounded_distance_bails_past_the_limit() {
        assert_eq!(bounded_distance("ship", "ship", 2), Some(0));
        assert_eq!(bounded_distance("ship", "shp", 2), Some(1));
        assert_eq!(bounded_distance("ship", "elephant", 2), None);
    }
}
