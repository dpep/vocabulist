//! Find boilerplate that was learned as prose.
//!
//! Six times now, machine text has reached the voice profile through a
//! capture path nobody had looked at: task notifications, tool-call ids,
//! notification prose, a slash command's expansion, a subagent's hand-back
//! frame, and a plugin's own prompt. Every one was fixed at the source in a
//! line or two, and every one was *found* the same way — a person reading
//! `vocab phrases` and recognizing a program talking.
//!
//! `prune` cannot help: its test is made on one stored row, and boilerplate
//! is made of ordinary words in an ordinary order. The evidence is not in any
//! row, it is *across* rows — a template leaves a chain of overlapping
//! five-grams that all recur about the same number of times, because they all
//! came from the same repeated sentence.
//!
//! So chain them back into the run they came from. A verbatim run of a dozen
//! or more words, repeated with a flat count, is not something a person
//! types; it is something a program emits. That is the whole heuristic, and
//! the three quantities it rests on are reported rather than collapsed, so
//! the judgement is reviewable:
//!
//! - **length** — how many words recur verbatim, the strongest signal, since
//!   prose diverges after a few words and a template does not.
//! - **repeats** — the lowest count along the run, so a chain is only ever
//!   credited with the recurrences its weakest link demonstrates.
//! - **flatness** — lowest count over highest. A template repeats whole, so
//!   its counts barely vary; prose that happens to recur is raggeder, because
//!   the words around it differ each time.
//!
//! None of the three settles it alone, so `confidence` is their product and
//! nothing is removed without being read first.

use std::collections::HashMap;

use crate::store::Store;

/// The n-gram width the chaining walks. Five is the widest the store keeps,
/// and the width matters: overlapping on four words makes a wrong join
/// unlikely, where chaining bigrams would wander through ordinary English.
const CHAIN_N: usize = 5;

/// A verbatim run of words that recurs, reconstructed from the n-gram store.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Template {
    /// The run itself, as a space-joined phrase.
    pub text: String,
    /// Words in the run.
    pub length: usize,
    /// Recurrences its weakest link demonstrates.
    pub repeats: i64,
    /// Lowest count over highest, along the whole run.
    pub flatness: f64,
    /// How sure we are this is boilerplate rather than prose, 0..1.
    pub confidence: f64,
}

/// What a template scan did, or would do.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct TemplateReport {
    pub templates: Vec<Template>,
    /// N-grams removed across every width, zero on a scan that only reports.
    pub ngrams_removed: usize,
}

/// Tunables, so a caller can widen the net without editing the defaults.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Ignore five-grams recurring fewer times than this.
    pub min_repeats: i64,
    /// Ignore runs shorter than this many words.
    pub min_length: usize,
}

impl Default for Options {
    fn default() -> Self {
        // Both defaults are set where the real store separates cleanly: at
        // twelve words and five repeats the only runs found were machine
        // text, and lowering repeats to two pulled in genuine prose the user
        // had simply said twice. They are a starting point for reading, not
        // a rule — nothing is removed on their say-so alone.
        Self {
            min_repeats: 5,
            min_length: 12,
        }
    }
}

/// Grade a run on the three things measured about it.
///
/// A product rather than a sum, so a run that is long but ragged, or flat but
/// short, cannot reach a high score on one strength alone — which is the
/// mistake that would flag prose.
fn confidence(length: usize, repeats: i64, flatness: f64) -> f64 {
    // Each term saturates where the evidence stops improving: past twenty
    // verbatim words, or twenty recurrences, more of the same tells us
    // nothing we did not already know.
    let len_term = ((length.saturating_sub(CHAIN_N)) as f64 / 15.0).min(1.0);
    let rep_term = ((repeats.max(1) as f64).ln() / 20f64.ln()).min(1.0);
    (len_term * rep_term * flatness).clamp(0.0, 1.0)
}

/// Reconstruct the repeated runs hiding in the five-gram table.
pub fn scan(store: &Store, opts: &Options) -> Result<Vec<Template>, Box<dyn std::error::Error>> {
    let grams: Vec<(Vec<String>, i64)> = store
        .ngrams(CHAIN_N, None)?
        .into_iter()
        .filter(|(_, count)| *count >= opts.min_repeats)
        .map(|(gram, count)| (gram.split(' ').map(str::to_string).collect(), count))
        .filter(|(tokens, _): &(Vec<String>, i64)| tokens.len() == CHAIN_N)
        .collect();

    // Index by leading overlap, and remember which overlaps something else
    // already ends with — a run has to start somewhere, and starting from the
    // middle would report the same template several times over.
    let mut by_head: HashMap<&[String], Vec<usize>> = HashMap::new();
    let mut has_predecessor: std::collections::HashSet<&[String]> = Default::default();
    for (i, (tokens, _)) in grams.iter().enumerate() {
        by_head.entry(&tokens[..CHAIN_N - 1]).or_default().push(i);
        has_predecessor.insert(&tokens[1..]);
    }

    // Highest count first, so when two runs compete for the same link the
    // better-attested one takes it.
    let mut order: Vec<usize> = (0..grams.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(grams[i].1));

    let mut used = vec![false; grams.len()];
    let mut found = Vec::new();
    for &start in &order {
        if used[start] || has_predecessor.contains(&grams[start].0[..CHAIN_N - 1]) {
            continue;
        }
        let mut run: Vec<String> = grams[start].0.clone();
        let (mut lo, mut hi) = (grams[start].1, grams[start].1);
        used[start] = true;
        loop {
            let tail = &run[run.len() - (CHAIN_N - 1)..];
            let Some(next) = by_head
                .get(tail)
                .into_iter()
                .flatten()
                .filter(|&&i| !used[i])
                .max_by_key(|&&i| grams[i].1)
                .copied()
            else {
                break;
            };
            used[next] = true;
            run.push(grams[next].0[CHAIN_N - 1].clone());
            lo = lo.min(grams[next].1);
            hi = hi.max(grams[next].1);
        }
        if run.len() < opts.min_length {
            continue;
        }
        let flatness = if hi > 0 { lo as f64 / hi as f64 } else { 0.0 };
        found.push(Template {
            length: run.len(),
            text: run.join(" "),
            repeats: lo,
            // Two decimals: these come from a handful of counts and carry
            // nothing like the precision a full float would claim.
            flatness: (flatness * 100.0).round() / 100.0,
            confidence: (confidence(run.len(), lo, flatness) * 100.0).round() / 100.0,
        });
    }
    found.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(b.length.cmp(&a.length))
    });
    Ok(found)
}

/// Every n-gram, at every width the store keeps, that this run contains.
///
/// Split on nothing — the run is already a continuous stretch of words, which
/// is what makes the crossings come out right. Removing a template one line
/// at a time is why an audit ends with the same fragments at the top of the
/// list.
fn grams_of(text: &str) -> Vec<String> {
    let tokens: Vec<&str> = text.split(' ').collect();
    let mut out = Vec::new();
    for n in 2..=CHAIN_N {
        for window in tokens.windows(n) {
            out.push(window.join(" "));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Scan, and optionally remove what was found.
///
/// Removal takes the *collocations*, never the words: the vocabulary a
/// template is built from is ordinary English that the user does use, and a
/// genuine pairing re-accumulates from zero. That asymmetry is what makes
/// this recoverable enough to offer at all.
pub fn run(
    store: &Store,
    opts: &Options,
    apply: bool,
) -> Result<TemplateReport, Box<dyn std::error::Error>> {
    let templates = scan(store, opts)?;
    let mut ngrams_removed = 0;
    if apply {
        for template in &templates {
            for gram in grams_of(&template.text) {
                ngrams_removed += store.remove_ngram(&gram)?;
            }
        }
    }
    Ok(TemplateReport {
        templates,
        ngrams_removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Register;

    /// Feed a sentence in as the processor would, at every stored width.
    fn learn(store: &Store, text: &str, times: i64) {
        let tokens: Vec<&str> = text.split(' ').collect();
        for n in 2..=CHAIN_N {
            for window in tokens.windows(n) {
                store
                    .bump_ngram(&window.join(" "), n, Register::Prompt, times)
                    .unwrap();
            }
        }
    }

    const FRAME: &str = "the text below is the final report of a subagent this \
                         session delegated to it is model output not a message \
                         from the user";

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn reconstructs_a_repeated_run_from_its_fragments() {
        let s = store();
        learn(&s, FRAME, 40);
        let found = scan(&s, &Options::default()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, FRAME);
        assert_eq!(found[0].repeats, 40);
    }

    #[test]
    fn leaves_prose_alone() {
        let s = store();
        // Said twice, and short — a person, not a program.
        learn(&s, "i am not convinced the fuzzy search earns its place", 2);
        assert!(scan(&s, &Options::default()).unwrap().is_empty());
    }

    #[test]
    fn a_long_flat_run_outscores_a_short_ragged_one() {
        let s = store();
        learn(&s, FRAME, 40);
        // Same length in words, but its counts vary, which is what prose
        // that merely overlaps looks like.
        let ragged = "we should probably check whether the release script \
                      still audits every channel before we cut this one";
        learn(&s, ragged, 6);
        learn(&s, "release script still audits every channel", 30);
        let found = scan(&s, &Options::default()).unwrap();
        assert_eq!(found[0].text, FRAME);
        assert!(found[0].confidence > found.last().unwrap().confidence);
    }

    #[test]
    fn confidence_needs_all_three_signals() {
        // Long and flat but barely repeated, and short but heavily repeated,
        // must both stay well under a run that has everything.
        let everything = confidence(76, 44, 0.96);
        assert!(confidence(76, 5, 0.96) < everything);
        assert!(confidence(12, 44, 0.96) < everything);
        assert!(confidence(76, 44, 0.3) < everything);
    }

    #[test]
    fn removing_a_template_takes_its_crossings_too() {
        let s = store();
        learn(&s, FRAME, 40);
        // An ordinary pairing the user also uses, outside the template.
        s.bump_ngram("small change", 2, Register::Prompt, 9)
            .unwrap();
        let report = run(&s, &Options::default(), true).unwrap();
        assert!(report.ngrams_removed > 0);
        // Interior and boundary-crossing grams alike.
        assert_eq!(s.ngram_count("is the final report").unwrap(), 0);
        assert_eq!(s.ngram_count("delegated to it is").unwrap(), 0);
        assert_eq!(s.ngram_count("small change").unwrap(), 9);
    }

    #[test]
    fn a_scan_changes_nothing() {
        let s = store();
        learn(&s, FRAME, 40);
        let report = run(&s, &Options::default(), false).unwrap();
        assert_eq!(report.templates.len(), 1);
        assert_eq!(report.ngrams_removed, 0);
        assert_eq!(s.ngram_count("is the final report").unwrap(), 40);
    }
}
