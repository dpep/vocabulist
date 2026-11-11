//! Shorthand: the user's own abbreviations, `repro` for reproduce, which look
//! like typos or unknown words and are deliberate (PLAN §12m).
//!
//! Membership and meaning are separate questions. A confirmed shorthand and
//! its inflections are never flagged and never "fixed" — that is a yes/no and
//! needs no meaning at all. What one *means* is settled only where it can be
//! read one way: the form the user confirmed, or an inflection whose reading
//! the word itself decides.

use std::collections::HashMap;

use crate::trust::Trust;
use crate::types::{Provenance, ShorthandEntry, Tier};

/// How many readings a candidate offers.
const MAX_MAYBE: usize = 3;

/// What `vocab shorthand` lists: the confirmed shorthands, then candidates.
///
/// A candidate is a word the user writes that is not English, whose stem
/// begins a dictionary word the user also writes: `perf`, `impls`. Its
/// readings are those words, most written first. They are suggestions only:
/// on a real store the most written one was the right expansion for one
/// shorthand in three (PLAN §12m), so nothing expands a candidate.
pub fn listing(trust: &Trust, in_dictionary: impl Fn(&str) -> bool) -> Vec<ShorthandEntry> {
    let confirmed = trust
        .shorthands()
        .confirmed()
        .into_iter()
        .map(|(word, means)| ShorthandEntry {
            word: word.to_string(),
            tier: Tier::Confirmed,
            means: Some(means.to_string()),
            maybe: Vec::new(),
        });

    let mut english: Vec<(&str, i64)> = trust
        .words()
        .filter(|(w, s)| s.count > 0 && in_dictionary(w))
        .map(|(w, s)| (w, s.count))
        .collect();
    english.sort();
    let readings = |stem: &str| -> Vec<String> {
        let from = english.partition_point(|(w, _)| *w < stem);
        let mut hits: Vec<(&str, i64)> = english[from..]
            .iter()
            .take_while(|(w, _)| w.starts_with(stem))
            .filter(|(w, _)| w.len() >= stem.len() + 2)
            .copied()
            .collect();
        hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        hits.iter()
            .take(MAX_MAYBE)
            .map(|(w, _)| w.to_string())
            .collect()
    };
    let mut candidates: Vec<(i64, ShorthandEntry)> = trust
        .words()
        .filter(|(w, s)| {
            s.provenance == Provenance::Observed
                && w.chars().all(|c| c.is_ascii_lowercase())
                && !in_dictionary(w)
                && !trust.shorthands().contains(w, &in_dictionary)
                && trust.trusts(w, &in_dictionary)
        })
        .filter_map(|(w, s)| {
            let stems = std::iter::once(w).chain(
                ["s", "es", "ed", "ing"]
                    .iter()
                    .filter_map(|suffix| w.strip_suffix(suffix))
                    .filter(|stem| !in_dictionary(stem)),
            );
            let maybe = stems
                .filter(|stem| stem.len() >= 3)
                .map(readings)
                .find(|r| !r.is_empty())?;
            Some((
                s.docs,
                ShorthandEntry {
                    word: w.to_string(),
                    tier: Tier::Candidate,
                    means: None,
                    maybe,
                },
            ))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.word.cmp(&b.1.word)));
    confirmed
        .chain(candidates.into_iter().map(|(_, c)| c))
        .collect()
}

/// The shorthands the user confirmed: each form and what it means.
#[derive(Default, Debug)]
pub struct Shorthands {
    confirmed: HashMap<String, String>,
}

/// What a shorthand form means, as far as it can be told.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    /// Settled: the form was confirmed, or it is an inflection of a confirmed
    /// form that only one reading fits.
    Means(String),
    /// What it may mean, which would be a guess to write down. Empty when no
    /// inflection of the meaning is a word.
    Maybe(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Inflection {
    S,
    Ed,
    Ing,
}

/// Longest first, so `repro'd` is not read as `repro'` + `d`.
const SUFFIXES: &[(&str, Inflection)] = &[
    ("'d", Inflection::Ed),
    ("ing", Inflection::Ing),
    ("ed", Inflection::Ed),
    ("es", Inflection::S),
    ("s", Inflection::S),
];

impl FromIterator<(String, String)> for Shorthands {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self {
            confirmed: iter.into_iter().collect(),
        }
    }
}

impl Shorthands {
    pub fn is_empty(&self) -> bool {
        self.confirmed.is_empty()
    }

    /// The confirmed forms and their meanings, alphabetically.
    pub fn confirmed(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = self
            .confirmed
            .iter()
            .map(|(s, m)| (s.as_str(), m.as_str()))
            .collect();
        out.sort();
        out
    }

    /// Is `word` a shorthand: confirmed, or an inflection of one?
    ///
    /// `in_dictionary` is asked only of an inflected form, and a dictionary
    /// word is never one: confirming `st` must not make `sting` a shorthand.
    pub fn contains(&self, word: &str, in_dictionary: impl Fn(&str) -> bool) -> bool {
        self.base(word, in_dictionary).is_some()
    }

    /// What `word` means, if it is a shorthand.
    ///
    /// `is_word` decides which spelling of an inflected meaning is real —
    /// `reproduced`, not `reproduceed` — and is the same dictionary test as
    /// `contains` takes.
    pub fn read(&self, word: &str, is_word: impl Fn(&str) -> bool) -> Option<Reading> {
        let (meaning, inflection) = self.base(word, &is_word)?;
        let Some(inflection) = inflection else {
            return Some(Reading::Means(meaning.to_string()));
        };
        Some(match inflect(meaning, inflection, &is_word) {
            None => Reading::Maybe(Vec::new()),
            // A plural and a third-person verb share the -s: `repros` is
            // "reproduces" in "it repros on main" and "reproductions" in "the
            // repros". When the meaning is also a verb, the word cannot say
            // which was typed.
            Some(word)
                if inflection == Inflection::S
                    && inflect(meaning, Inflection::Ed, &is_word).is_some() =>
            {
                Reading::Maybe(vec![word])
            }
            Some(word) => Reading::Means(word),
        })
    }

    /// The meaning `word` stands for and how it is inflected; `None` for the
    /// form confirmed as written.
    fn base(
        &self,
        word: &str,
        in_dictionary: impl Fn(&str) -> bool,
    ) -> Option<(&str, Option<Inflection>)> {
        if let Some(meaning) = self.confirmed.get(word) {
            return Some((meaning, None));
        }
        let (meaning, inflection) = SUFFIXES.iter().find_map(|&(suffix, inflection)| {
            let stem = word.strip_suffix(suffix)?;
            Some((self.confirmed.get(stem)?, inflection))
        })?;
        (!in_dictionary(word)).then_some((meaning.as_str(), Some(inflection)))
    }
}

/// `word` inflected, spelled the way that is a real word, if one is.
///
/// English spelling has too many rules to apply blind (`reproduced`,
/// `committed`, `copies`), so this offers every regular spelling and lets the
/// dictionary pick.
fn inflect(word: &str, inflection: Inflection, is_word: impl Fn(&str) -> bool) -> Option<String> {
    let doubled = word.chars().last().map(|c| format!("{word}{c}"));
    let stem_y = word.strip_suffix('y');
    let stem_e = word.strip_suffix('e');
    let candidates: Vec<String> = match inflection {
        Inflection::S => [
            Some(format!("{word}s")),
            Some(format!("{word}es")),
            stem_y.map(|s| format!("{s}ies")),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Inflection::Ed => [
            Some(format!("{word}ed")),
            stem_e.map(|_| format!("{word}d")),
            stem_y.map(|s| format!("{s}ied")),
            doubled.map(|d| format!("{d}ed")),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Inflection::Ing => [
            Some(format!("{word}ing")),
            stem_e.map(|s| format!("{s}ing")),
            doubled.map(|d| format!("{d}ing")),
        ]
        .into_iter()
        .flatten()
        .collect(),
    };
    candidates.into_iter().find(|c| is_word(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shorthands(pairs: &[(&str, &str)]) -> Shorthands {
        pairs
            .iter()
            .map(|(s, m)| (s.to_string(), m.to_string()))
            .collect()
    }

    const WORDS: &[&str] = &[
        "reproduce",
        "reproduces",
        "reproduced",
        "reproducing",
        "argument",
        "arguments",
        "commit",
        "committed",
        "copy",
        "copies",
        "copied",
        "sting",
    ];
    fn is_word(w: &str) -> bool {
        WORDS.contains(&w)
    }

    fn read(s: &Shorthands, word: &str) -> Option<Reading> {
        s.read(word, is_word)
    }
    fn means(w: &str) -> Option<Reading> {
        Some(Reading::Means(w.to_string()))
    }

    #[test]
    fn a_confirmed_form_means_what_the_user_said() {
        let s = shorthands(&[("repro", "reproduce")]);
        assert_eq!(read(&s, "repro"), means("reproduce"));
        assert_eq!(read(&s, "widget"), None);
    }

    #[test]
    fn a_verb_inflection_reads_one_way() {
        let s = shorthands(&[("repro", "reproduce"), ("cmt", "commit")]);
        assert_eq!(read(&s, "repro'd"), means("reproduced"));
        assert_eq!(read(&s, "reproed"), means("reproduced"));
        assert_eq!(read(&s, "reproing"), means("reproducing"));
        assert_eq!(read(&s, "cmted"), means("committed"));
    }

    #[test]
    fn an_s_on_a_verb_is_ambiguous() {
        // "it repros on main" or "the repros": the word cannot tell.
        let s = shorthands(&[("repro", "reproduce")]);
        assert_eq!(
            read(&s, "repros"),
            Some(Reading::Maybe(vec!["reproduces".into()]))
        );
    }

    #[test]
    fn an_s_on_a_noun_is_a_plural() {
        let s = shorthands(&[("arg", "argument"), ("cpy", "copy")]);
        assert_eq!(read(&s, "args"), means("arguments"));
        // `copy` is a verb too, so `cpys` stays a guess.
        assert_eq!(
            read(&s, "cpys"),
            Some(Reading::Maybe(vec!["copies".into()]))
        );
    }

    #[test]
    fn a_confirmed_inflection_overrides_the_rule() {
        let s = shorthands(&[("repro", "reproduce"), ("repros", "reproductions")]);
        assert_eq!(read(&s, "repros"), means("reproductions"));
    }

    #[test]
    fn an_inflection_no_word_fits_has_no_reading() {
        let s = shorthands(&[("arg", "argument")]);
        assert_eq!(read(&s, "arged"), Some(Reading::Maybe(Vec::new())));
        assert!(s.contains("arged", is_word), "still never flagged");
    }

    #[test]
    fn candidates_offer_the_words_they_begin_and_never_a_meaning() {
        use crate::trust::Standing;
        let standing = |provenance, count, docs| Standing {
            provenance,
            count,
            docs,
        };
        let obs = Provenance::Observed;
        let trust: Trust = [
            ("perf", standing(obs, 30, 9)),
            ("impls", standing(obs, 5, 3)),
            ("performance", standing(obs, 40, 20)),
            ("perform", standing(obs, 5, 5)),
            ("implementation", standing(obs, 40, 20)),
            ("zblorg", standing(obs, 9, 9)),
            ("zrepro", standing(obs, 9, 9)),
            ("repro", standing(obs, 1, 1)),
        ]
        .into_iter()
        .map(|(w, s)| (w.to_string(), s))
        .collect::<Trust>()
        .with_shorthands(shorthands(&[("zrepro", "reproduce")]));
        let english = |w: &str| ["performance", "perform", "implementation"].contains(&w);

        let listed = listing(&trust, english);
        let rows: Vec<(&str, Tier, Option<&str>, Vec<&str>)> = listed
            .iter()
            .map(|e| {
                (
                    e.word.as_str(),
                    e.tier,
                    e.means.as_deref(),
                    e.maybe.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("zrepro", Tier::Confirmed, Some("reproduce"), vec![]),
                (
                    "perf",
                    Tier::Candidate,
                    None,
                    vec!["performance", "perform"]
                ),
                ("impls", Tier::Candidate, None, vec!["implementation"]),
            ]
        );
    }

    #[test]
    fn a_dictionary_word_is_never_an_inflection() {
        let s = shorthands(&[("st", "street")]);
        assert!(!s.contains("sting", is_word));
        assert_eq!(read(&s, "sting"), None);
    }
}
