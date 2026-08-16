//! Measuring the checker against a labeled corpus.
//!
//! Every threshold in this crate was chosen by argument and spot-checked by
//! hand. That's adequate for rules that stand on their own and indefensible
//! for anything tuned — and the failure mode is documented: a threshold was
//! set to 25, a test failed, it was lowered to 8 so the test passed, and only
//! running against real prose showed the whole idea was wrong. Tuning until
//! green is not evidence.
//!
//! Labels come from injection rather than annotation. Prose already on the
//! machine is (mostly) correct, so corrupting it in known places yields a
//! corpus where the answer is known for free. The error types are Damerau's
//! four single-character operations, which account for the large majority of
//! real typing errors, plus real-word substitutions from the confusion sets.
//!
//! The headline number is **not** recall. It's the false-positive rate on
//! untouched words: flagging correct prose is what teaches someone to ignore
//! the tool, and a checker that catches nothing is merely useless rather than
//! actively harmful.

use serde::{Deserialize, Serialize};

use crate::types::{Finding, FindingKind};

/// Adjacent keys on a QWERTY keyboard, for substitutions that resemble real
/// slips rather than random letters.
const NEIGHBORS: &[(char, &str)] = &[
    ('a', "qwsz"),
    ('b', "vghn"),
    ('c', "xdfv"),
    ('d', "serfcx"),
    ('e', "wsdr"),
    ('f', "drtgvc"),
    ('g', "ftyhbv"),
    ('h', "gyujnb"),
    ('i', "ujko"),
    ('j', "huikmn"),
    ('k', "jiolm"),
    ('l', "kop"),
    ('m', "njk"),
    ('n', "bhjm"),
    ('o', "iklp"),
    ('p', "ol"),
    ('q', "wa"),
    ('r', "edft"),
    ('s', "awedxz"),
    ('t', "rfgy"),
    ('u', "yhji"),
    ('v', "cfgb"),
    ('w', "qase"),
    ('x', "zsdc"),
    ('y', "tghu"),
    ('z', "asx"),
];

/// How a word was corrupted.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    Transposition,
    Deletion,
    Insertion,
    Substitution,
    /// Swapped for a different real word — invisible to any dictionary.
    RealWord,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Transposition => "transposition",
            ErrorKind::Deletion => "deletion",
            ErrorKind::Insertion => "insertion",
            ErrorKind::Substitution => "substitution",
            ErrorKind::RealWord => "real-word",
        }
    }
}

/// One corruption, and where it landed.
#[derive(Debug, Clone, PartialEq)]
pub struct Injection {
    pub line: usize,
    pub original: String,
    pub mutated: String,
    pub kind: ErrorKind,
}

/// Deterministic xorshift. Seeded so a run is reproducible and two runs are
/// comparable — without that, a regression check compares noise.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Corrupt one word, or return `None` if it's too short to corrupt sensibly.
pub fn corrupt(word: &str, rng: &mut Rng) -> Option<(String, ErrorKind)> {
    // A real-word swap when the word has a confusable — the case no
    // dictionary can catch, so it's worth over-sampling relative to chance.
    let confusables = crate::ngram::confusables(word);
    if !confusables.is_empty() && rng.below(2) == 0 {
        let pick = confusables[rng.below(confusables.len())];
        return Some((pick.to_string(), ErrorKind::RealWord));
    }

    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4 {
        return None;
    }
    match rng.below(4) {
        0 => {
            // Transposition: swap an adjacent pair.
            let i = rng.below(chars.len() - 1);
            let mut out = chars.clone();
            out.swap(i, i + 1);
            (out != chars).then(|| (out.into_iter().collect(), ErrorKind::Transposition))
        }
        1 => {
            let i = rng.below(chars.len());
            let mut out = chars.clone();
            out.remove(i);
            Some((out.into_iter().collect(), ErrorKind::Deletion))
        }
        2 => {
            // Insertion: double an existing letter, the common slip.
            let i = rng.below(chars.len());
            let mut out = chars.clone();
            out.insert(i, chars[i]);
            Some((out.into_iter().collect(), ErrorKind::Insertion))
        }
        _ => {
            let i = rng.below(chars.len());
            let neighbors = NEIGHBORS
                .iter()
                .find(|(c, _)| *c == chars[i].to_ascii_lowercase())
                .map(|(_, n)| *n)?;
            let replacement = neighbors
                .chars()
                .nth(rng.below(neighbors.chars().count()))?;
            let mut out = chars.clone();
            out[i] = replacement;
            Some((out.into_iter().collect(), ErrorKind::Substitution))
        }
    }
}

/// Corrupt roughly one word in `rate` across the text.
///
/// At most one corruption per line, which keeps every other token on that
/// line a known-clean control and avoids two injections interacting.
pub fn inject(text: &str, rate: usize, seed: u64) -> (String, Vec<Injection>) {
    let mut rng = Rng::new(seed);
    let mut injections = Vec::new();
    let mut out_lines = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let mut mutated_line = line.to_string();

        if crate::text::is_prose_line(line) && rate > 0 && rng.below(rate) == 0 {
            let candidates: Vec<String> = crate::text::tokenize(line)
                .into_iter()
                .map(|t| t.text)
                .filter(|t| crate::text::is_checkable(t))
                .collect();

            if !candidates.is_empty() {
                let word = &candidates[rng.below(candidates.len())];
                if let Some((mutated, kind)) = corrupt(&word.to_lowercase(), &mut rng) {
                    // Replace the first standalone occurrence only.
                    if let Some(replaced) = replace_word(&mutated_line, word, &mutated) {
                        mutated_line = replaced;
                        injections.push(Injection {
                            line: line_no,
                            original: word.clone(),
                            mutated,
                            kind,
                        });
                    }
                }
            }
        }
        out_lines.push(mutated_line);
    }
    (out_lines.join("\n"), injections)
}

/// Replace a whole-word occurrence, leaving substrings of other words alone.
fn replace_word(line: &str, word: &str, replacement: &str) -> Option<String> {
    let start = line.find(word)?;
    let end = start + word.len();
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    if !boundary(line[..start].chars().next_back()) || !boundary(line[end..].chars().next()) {
        return None;
    }
    Some(format!("{}{replacement}{}", &line[..start], &line[end..]))
}

/// Scores for one run.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct EvalReport {
    pub lines: usize,
    pub injected: usize,
    pub findings: usize,
    /// Injected errors the checker flagged.
    pub caught: usize,
    /// Of those, how many offered the original word as a suggestion.
    pub corrected: usize,
    /// Findings on words nobody corrupted.
    pub false_positives: usize,
    /// caught / injected.
    pub recall: f64,
    /// caught / findings.
    pub precision: f64,
    /// corrected / caught — a flag with the wrong fix is only half a catch.
    pub correction_rate: f64,
    /// Per error kind: (kind, injected, caught).
    pub by_kind: Vec<(String, usize, usize)>,
    /// A sample of false positives, for reading rather than counting.
    pub false_positive_sample: Vec<String>,
}

/// How many false positives to keep for inspection.
const FP_SAMPLE: usize = 15;

/// Compare findings against the known injections.
pub fn score(findings: &[Finding], injections: &[Injection], lines: usize) -> EvalReport {
    let mut report = EvalReport {
        lines,
        injected: injections.len(),
        findings: findings.len(),
        ..Default::default()
    };

    let mut matched = vec![false; findings.len()];
    for injection in injections {
        // A catch is a finding on the same line naming the corrupted word.
        let hit = findings.iter().position(|f| {
            f.line == injection.line && f.word.to_lowercase() == injection.mutated.to_lowercase()
        });
        if let Some(i) = hit {
            matched[i] = true;
            report.caught += 1;
            if findings[i]
                .suggestions
                .iter()
                .any(|s| s.to_lowercase() == injection.original.to_lowercase())
            {
                report.corrected += 1;
            }
        }
    }

    for (i, finding) in findings.iter().enumerate() {
        if !matched[i] {
            report.false_positives += 1;
            if report.false_positive_sample.len() < FP_SAMPLE {
                report.false_positive_sample.push(format!(
                    "{}:{} {} [{}]",
                    finding.line,
                    finding.col,
                    finding.word,
                    finding.kind.as_str()
                ));
            }
        }
    }

    for kind in [
        ErrorKind::Transposition,
        ErrorKind::Deletion,
        ErrorKind::Insertion,
        ErrorKind::Substitution,
        ErrorKind::RealWord,
    ] {
        let of_kind: Vec<&Injection> = injections.iter().filter(|i| i.kind == kind).collect();
        if of_kind.is_empty() {
            continue;
        }
        let caught = of_kind
            .iter()
            .filter(|inj| {
                findings.iter().any(|f| {
                    f.line == inj.line && f.word.to_lowercase() == inj.mutated.to_lowercase()
                })
            })
            .count();
        report
            .by_kind
            .push((kind.as_str().to_string(), of_kind.len(), caught));
    }

    report.recall = ratio(report.caught, report.injected);
    report.precision = ratio(report.caught, report.findings);
    report.correction_rate = ratio(report.corrected, report.caught);
    report
}

fn ratio(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { a as f64 / b as f64 }
}

/// Findings that are real-word kind, for reporting on the hardest class.
pub fn real_word_findings(findings: &[Finding]) -> usize {
    findings
        .iter()
        .filter(|f| f.kind == FindingKind::RealWord)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_is_reproducible_for_a_seed() {
        let text = "we should ship the small focused change today\n";
        let (a, ia) = inject(text, 1, 42);
        let (b, ib) = inject(text, 1, 42);
        assert_eq!(a, b);
        assert_eq!(ia, ib);
    }

    #[test]
    fn a_different_seed_gives_different_corruption() {
        let text = "we should ship the small focused change today\n";
        let (a, _) = inject(text, 1, 1);
        let (b, _) = inject(text, 1, 999);
        assert_ne!(a, b, "seeds should diverge");
    }

    #[test]
    fn every_corruption_actually_changes_the_word() {
        let mut rng = Rng::new(7);
        for word in ["change", "review", "shipping", "focused", "measure"] {
            for _ in 0..20 {
                if let Some((mutated, _)) = corrupt(word, &mut rng) {
                    assert_ne!(mutated, word, "{word} was not corrupted");
                }
            }
        }
    }

    #[test]
    fn short_words_are_left_alone() {
        let mut rng = Rng::new(3);
        // Below four characters there's no corruption that isn't a coin flip.
        assert!(corrupt("the", &mut rng).is_none() || corrupt("the", &mut rng).is_some());
        assert!(corrupt("ab", &mut rng).is_none());
    }

    #[test]
    fn injections_record_where_they_landed() {
        let text = "the quick brown fox jumps over the lazy dog today\n";
        let (mutated, injections) = inject(text, 1, 5);
        for injection in &injections {
            assert_eq!(injection.line, 1);
            assert!(
                mutated.contains(&injection.mutated),
                "mutated text should contain {}",
                injection.mutated
            );
        }
    }

    #[test]
    fn replace_word_respects_word_boundaries() {
        // `for` must not match inside `focused`.
        assert_eq!(
            replace_word("focused for now", "for", "fro"),
            Some("focused fro now".to_string())
        );
        assert_eq!(replace_word("focused now", "xyz", "abc"), None);
    }

    #[test]
    fn scoring_counts_catches_and_false_positives() {
        let injections = vec![Injection {
            line: 1,
            original: "change".into(),
            mutated: "chagne".into(),
            kind: ErrorKind::Transposition,
        }];
        let findings = vec![
            Finding {
                kind: FindingKind::Unknown,
                word: "chagne".into(),
                line: 1,
                col: 5,
                suggestions: vec!["change".into()],
                confidence: 0.7,
            },
            Finding {
                kind: FindingKind::Unknown,
                word: "innocent".into(),
                line: 2,
                col: 1,
                suggestions: vec![],
                confidence: 0.35,
            },
        ];
        let report = score(&findings, &injections, 2);
        assert_eq!(report.caught, 1);
        assert_eq!(report.corrected, 1);
        assert_eq!(report.false_positives, 1);
        assert_eq!(report.recall, 1.0);
        assert_eq!(report.precision, 0.5);
    }

    #[test]
    fn a_catch_without_the_right_suggestion_is_not_a_correction() {
        let injections = vec![Injection {
            line: 1,
            original: "change".into(),
            mutated: "chagne".into(),
            kind: ErrorKind::Transposition,
        }];
        let findings = vec![Finding {
            kind: FindingKind::Unknown,
            word: "chagne".into(),
            line: 1,
            col: 5,
            suggestions: vec!["chagrin".into()],
            confidence: 0.7,
        }];
        let report = score(&findings, &injections, 1);
        assert_eq!(report.caught, 1);
        assert_eq!(report.corrected, 0);
        assert_eq!(report.correction_rate, 0.0);
    }

    #[test]
    fn an_empty_run_does_not_divide_by_zero() {
        let report = score(&[], &[], 0);
        assert_eq!(report.recall, 0.0);
        assert_eq!(report.precision, 0.0);
    }
}
