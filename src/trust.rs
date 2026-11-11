//! Whether a word has earned trust: the one rule the checker, `list
//! --trusted` and `sync` all read.
//!
//! A deliberate source is trusted outright. An observed word needs two
//! distinct documents, and must not be a habitual typo — a near miss of a word
//! the user writes far more often. Recurrence alone cannot tell those apart:
//! `teh` typed for `the` every week recurs as reliably as real jargon does.
//! What separates them is the neighbour (PLAN §12l).

use std::collections::HashMap;

use crate::types::{Edit, Provenance, TypoPair};

/// Distinct documents an observed word needs. One sighting is equally
/// evidence of a typo, and typos are bursty within a document.
pub const MIN_DOCS: i64 = 2;

/// How many times as often the user must write the neighbour before a near
/// miss of it reads as a typo rather than a word of its own.
pub const DOMINANCE: f64 = 5.0;

/// Shortest word a single-letter change can convict. Four-letter words sit
/// one interior edit from too many real ones (`lang`, `aest`).
const MIN_EDIT_LENGTH: usize = 5;

/// Shortest word a transposition can convict — `teh`, `hte`.
const MIN_SWAP_LENGTH: usize = 3;

/// What the lexicon knows about one word, as far as trust is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Standing {
    pub provenance: Provenance,
    pub count: i64,
    /// Distinct documents it appeared in.
    pub docs: i64,
}

/// Every word's standing, so a judgement about one can consult its
/// neighbours.
#[derive(Default)]
pub struct Trust {
    words: HashMap<String, Standing>,
    /// Typo to intended word, as confirmed by hand.
    confirmed: HashMap<String, String>,
}

impl FromIterator<(String, Standing)> for Trust {
    fn from_iter<I: IntoIterator<Item = (String, Standing)>>(iter: I) -> Self {
        Self {
            words: iter.into_iter().collect(),
            confirmed: HashMap::new(),
        }
    }
}

impl Trust {
    /// Attach the pairs the user confirmed. They hold whatever the counts
    /// say, as a deliberate source does.
    pub fn with_confirmed(mut self, confirmed: HashMap<String, String>) -> Self {
        self.confirmed = confirmed;
        self
    }

    pub fn standing(&self, word: &str) -> Option<&Standing> {
        self.words.get(word)
    }

    /// Words from a deliberate source, which need no corroboration.
    pub fn deliberate(&self) -> impl Iterator<Item = (&str, Provenance)> {
        self.words
            .iter()
            .filter(|(_, s)| s.provenance > Provenance::Observed)
            .map(|(w, s)| (w.as_str(), s.provenance))
    }

    /// Has `word` earned trust?
    ///
    /// `in_dictionary` is asked only when a veto is in play, so a caller that
    /// loads its dictionary lazily keeps that laziness.
    pub fn trusts(&self, word: &str, in_dictionary: impl Fn(&str) -> bool) -> bool {
        let Some(standing) = self.words.get(word) else {
            return false;
        };
        if standing.provenance > Provenance::Observed {
            return true;
        }
        standing.docs >= MIN_DOCS && self.typo_of(word, in_dictionary).is_none()
    }

    /// The word `word` is a habitual typo of, if it is one: confirmed by
    /// hand, or derived from the counts.
    ///
    /// Never for a deliberate source — `trekr` is the user's even though they
    /// write `trek`. A derived pair is never a dictionary word either, or
    /// `form` would be a typo of `from`.
    pub fn typo_of(&self, word: &str, in_dictionary: impl Fn(&str) -> bool) -> Option<TypoPair> {
        let standing = self.words.get(word);
        if standing.is_some_and(|s| s.provenance > Provenance::Observed) {
            return None;
        }
        let (count, docs) = standing.map_or((0, 0), |s| (s.count, s.docs));
        let count_of = |w: &str| self.words.get(w).map_or(0, |s| s.count);
        let ratio = |intended: &str| count_of(intended) as f64 / count.max(1) as f64;
        let pair = |intended: String, source, edit| {
            // Two significant figures is what a ratio of small counts holds.
            let ratio = significant(ratio(&intended), 2);
            TypoPair {
                score: belief(source, ratio),
                intended_count: count_of(&intended),
                typo: word.to_string(),
                intended,
                source,
                edit,
                count,
                docs,
                ratio,
            }
        };

        if let Some(intended) = self.confirmed.get(word) {
            let edit = typo_shaped_neighbours(word)
                .into_iter()
                .find(|(c, _)| c == intended)
                .map(|(_, e)| e);
            return Some(pair(intended.clone(), Provenance::User, edit));
        }
        standing?;
        let (intended, edit) = typo_shaped_neighbours(word)
            .into_iter()
            .map(|(c, e)| (ratio(&c), c, e))
            .filter(|(r, _, _)| *r >= DOMINANCE)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, c, e)| (c, e))?;
        // Last, because it may load a dictionary and is rarely reached.
        if in_dictionary(word) {
            return None;
        }
        Some(pair(intended, Provenance::Observed, Some(edit)))
    }

    /// Every typo pair, confirmed first, then by how often the typo recurs.
    pub fn typos(&self, in_dictionary: impl Fn(&str) -> bool) -> Vec<TypoPair> {
        let observed = self
            .words
            .iter()
            .filter(|(w, s)| {
                s.provenance == Provenance::Observed && !self.confirmed.contains_key(*w)
            })
            .map(|(w, _)| w);
        let mut pairs: Vec<TypoPair> = self
            .confirmed
            .keys()
            .chain(observed)
            .filter_map(|w| self.typo_of(w, &in_dictionary))
            .collect();
        pairs.sort_by(|a, b| {
            b.source
                .cmp(&a.source)
                .then(b.docs.cmp(&a.docs))
                .then(b.count.cmp(&a.count))
                .then(a.typo.cmp(&b.typo))
        });
        pairs
    }
}

/// How sure a typo pair is.
///
/// Confirmed by hand is certain. A derived pair is `ratio / (ratio +
/// DOMINANCE)`: a coin flip at the veto's own bar, 0.91 at ten times it.
/// Recurrence enters only through the ratio, whose denominator is the typo's
/// own count. It is not a separate bonus, because among the labelled pairs it
/// did not separate slips from words: `behaviour` and `repros` recur in six
/// documents each.
fn belief(source: Provenance, ratio: f64) -> f32 {
    let p = if source == Provenance::User {
        1.0
    } else {
        ratio / (ratio + DOMINANCE)
    };
    crate::types::round(p, 2) as f32
}

/// `value` to `digits` significant figures.
fn significant(value: f64, digits: i32) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    let scale = 10f64.powi(digits - 1 - value.log10().floor() as i32);
    (value * scale).round() / scale
}

/// The edit-distance-1 neighbours of `word` whose difference has the shape a
/// slip makes, with the edit that would turn each into `word`.
///
/// A transposition anywhere. A single-letter change only strictly inside the
/// word: real near misses mostly change the first or last letter (`jsonl`,
/// `readmes`), and slips mostly don't. Hyphenation is spelling variance, not
/// a slip, so a hyphenated word has no neighbours here.
fn typo_shaped_neighbours(word: &str) -> Vec<(String, Edit)> {
    const ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz'";
    let mut out = Vec::new();
    if word.contains('-') {
        return out;
    }
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    let collect = |v: &[char]| v.iter().collect::<String>();

    if n >= MIN_SWAP_LENGTH {
        for i in 0..n - 1 {
            if chars[i] != chars[i + 1] {
                let mut c = chars.clone();
                c.swap(i, i + 1);
                out.push((collect(&c), Edit::Transposition));
            }
        }
    }
    if n < MIN_EDIT_LENGTH {
        return out;
    }
    for i in 1..n - 1 {
        // The typo carries an extra letter at `i`.
        let mut c = chars.clone();
        c.remove(i);
        out.push((collect(&c), Edit::Insertion));
        for letter in ALPHABET.chars().filter(|&l| l != chars[i]) {
            let mut c = chars.clone();
            c[i] = letter;
            out.push((collect(&c), Edit::Substitution));
        }
    }
    // The typo dropped a letter before position `i`.
    for i in 1..n {
        for letter in ALPHABET.chars() {
            let mut c = chars.clone();
            c.insert(i, letter);
            out.push((collect(&c), Edit::Deletion));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust(words: &[(&str, Provenance, i64, i64)]) -> Trust {
        words
            .iter()
            .map(|&(w, provenance, count, docs)| {
                (
                    w.to_string(),
                    Standing {
                        provenance,
                        count,
                        docs,
                    },
                )
            })
            .collect()
    }

    const OBS: Provenance = Provenance::Observed;
    const DEP: Provenance = Provenance::Dependency;
    fn nowhere(_: &str) -> bool {
        false
    }

    #[test]
    fn a_habitual_typo_is_never_trusted_however_often_it_recurs() {
        // The bar is relative: it holds while the real word stays far ahead.
        for docs in [2, 9, 90] {
            let t = trust(&[
                ("the", DEP, 5000, 900),
                ("teh", OBS, docs, docs),
                ("should", DEP, 500, 300),
                ("shoudl", OBS, docs, docs),
            ]);
            assert!(!t.trusts("teh", nowhere), "teh at {docs} docs");
            assert!(!t.trusts("shoudl", nowhere), "shoudl at {docs} docs");
        }
    }

    #[test]
    fn the_typo_names_the_word_meant() {
        let t = trust(&[("the", DEP, 600, 400), ("teh", OBS, 6, 6)]);
        let near = t.typo_of("teh", nowhere).unwrap();
        assert_eq!(near.intended, "the");
        assert_eq!(near.edit, Some(Edit::Transposition));
        assert_eq!(near.ratio, 100.0);
    }

    #[test]
    fn a_deliberate_source_is_never_vetoed() {
        let t = trust(&[
            ("trek", DEP, 900, 500),
            ("widget", DEP, 900, 500),
            ("wdiget", Provenance::Owned, 1, 1),
        ]);
        assert!(t.trusts("wdiget", nowhere));
        assert!(t.typo_of("wdiget", nowhere).is_none());
    }

    #[test]
    fn a_near_miss_at_the_edge_of_a_word_is_its_own_word() {
        // `jsonl` adds a last letter to `json`; real vocabulary grows that way.
        let t = trust(&[
            ("json", DEP, 400, 200),
            ("jsonl", OBS, 2, 2),
            ("widget", DEP, 400, 200),
            ("swidget", OBS, 2, 2),
        ]);
        assert!(t.trusts("jsonl", nowhere));
        assert!(t.trusts("swidget", nowhere));
    }

    #[test]
    fn a_rarely_written_neighbour_convicts_nothing() {
        // The neighbour must dominate: two spellings used about equally are
        // two words, or a variant, not a slip.
        let t = trust(&[("widget", DEP, 8, 8), ("wigdet", OBS, 2, 2)]);
        assert!(t.trusts("wigdet", nowhere));
    }

    #[test]
    fn a_dictionary_word_is_never_a_typo() {
        let t = trust(&[("from", DEP, 900, 500), ("form", OBS, 3, 3)]);
        assert!(t.typo_of("form", |w| w == "form").is_none());
        assert!(t.trusts("form", |w| w == "form"));
    }

    #[test]
    fn short_words_only_convict_by_transposition() {
        // `lang` is one interior letter from `long`; at four letters that says
        // nothing.
        let t = trust(&[("long", DEP, 900, 500), ("lang", OBS, 3, 3)]);
        assert!(t.trusts("lang", nowhere));
    }

    #[test]
    fn one_document_is_not_enough() {
        let t = trust(&[("zblorg", OBS, 4, 1)]);
        assert!(!t.trusts("zblorg", nowhere));
        let t = trust(&[("zblorg", OBS, 4, 2)]);
        assert!(t.trusts("zblorg", nowhere));
    }

    fn confirmed(t: Trust, pairs: &[(&str, &str)]) -> Trust {
        t.with_confirmed(
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        )
    }

    #[test]
    fn a_confirmed_typo_holds_whatever_the_counts_say() {
        // Its neighbour is rare, so the counts alone would trust it.
        let t = confirmed(
            trust(&[("widget", DEP, 3, 3), ("wigdet", OBS, 40, 40)]),
            &[("wigdet", "widget")],
        );
        assert!(!t.trusts("wigdet", nowhere));
        let pair = t.typo_of("wigdet", nowhere).unwrap();
        assert_eq!(pair.source, Provenance::User);
        assert_eq!(pair.edit, Some(Edit::Transposition));
    }

    #[test]
    fn a_deliberate_source_outranks_a_confirmation() {
        let t = confirmed(
            trust(&[("wigdet", Provenance::User, 1, 1)]),
            &[("wigdet", "widget")],
        );
        assert!(t.typo_of("wigdet", nowhere).is_none());
        assert!(t.trusts("wigdet", nowhere));
    }

    #[test]
    fn typos_lists_confirmed_pairs_first_and_skips_dictionary_words() {
        let t = confirmed(
            trust(&[
                ("the", DEP, 900, 500),
                ("teh", OBS, 9, 9),
                ("from", DEP, 900, 500),
                ("form", OBS, 9, 9),
            ]),
            &[("zblorg", "widget")],
        );
        let pairs = t.typos(|w| w == "form");
        let typos: Vec<&str> = pairs.iter().map(|p| p.typo.as_str()).collect();
        assert_eq!(typos, ["zblorg", "teh"]);
        // Ratios carry the precision their counts have.
        assert_eq!(pairs[1].ratio, 100.0);
    }
}
