//! Contractions typed without their apostrophe: `dont` → `don't`.
//!
//! Worth handling separately from ordinary typos for two reasons. The
//! correction is *unambiguous*, so it deserves far higher confidence than an
//! edit-distance guess — and edit distance actively mishandles these, since
//! the apostrophe form is usually absent from the word list, leaving `dont` to
//! be "corrected" to `font` or `dot`.
//!
//! Only forms whose apostrophe-less spelling is **not itself an English word**
//! belong here. `cant`, `wont`, `its`, `were`, `well`, `lets`, `shell`, and
//! `ill` are all real words; correcting them requires knowing the sentence,
//! which is the real-word problem in [`crate::ngram`], not this table.
//!
//! The table below is the fast path, not the whole story. [`derive`] applies
//! that same rule to any word list — the bundled dictionary, and your own
//! lexicon — so a contraction nobody enumerated is still handled, and one you
//! personally write (`y\'all`, a dialect form, a name with an apostrophe)
//! starts working once it has been seen. Curating a list by hand was always
//! going to be incomplete; the rule is the thing worth writing down.

/// `(as typed, corrected)`, lowercase. Unambiguous entries only.
const CONTRACTIONS: &[(&str, &str)] = &[
    ("dont", "don't"),
    ("didnt", "didn't"),
    ("doesnt", "doesn't"),
    ("isnt", "isn't"),
    ("arent", "aren't"),
    ("wasnt", "wasn't"),
    ("werent", "weren't"),
    ("wouldnt", "wouldn't"),
    ("couldnt", "couldn't"),
    ("shouldnt", "shouldn't"),
    ("havent", "haven't"),
    ("hasnt", "hasn't"),
    ("hadnt", "hadn't"),
    ("mustnt", "mustn't"),
    ("neednt", "needn't"),
    ("wouldve", "would've"),
    ("couldve", "could've"),
    ("shouldve", "should've"),
    ("youre", "you're"),
    ("theyre", "they're"),
    ("youve", "you've"),
    ("weve", "we've"),
    ("theyve", "they've"),
    ("youll", "you'll"),
    ("theyll", "they'll"),
    ("itll", "it'll"),
    ("youd", "you'd"),
    ("theyd", "they'd"),
    ("thats", "that's"),
    ("whats", "what's"),
    ("theres", "there's"),
    ("heres", "here's"),
    ("wheres", "where's"),
    ("whos", "who's"),
    ("oclock", "o'clock"),
];

/// Confidence for a contraction fix. High, because the mapping is exact —
/// but short of certainty, since a deliberate identifier could collide.
pub const CONFIDENCE: f32 = 0.90;

/// The apostrophe form of `word`, if it's a known apostrophe-less contraction.
pub fn expand(word: &str) -> Option<&'static str> {
    CONTRACTIONS
        .iter()
        .find(|(typed, _)| *typed == word)
        .map(|(_, fixed)| *fixed)
}

/// Derive apostrophe-less mappings from any collection of known words.
///
/// `known` supplies the apostrophe forms; `is_word` decides whether the
/// stripped spelling is already a word on its own. That second question is the
/// entire safety property — `cant`, `wont`, and `shell` are real words, and a
/// derivation that skipped the check would flag correct prose.
///
/// Anything ending in `'s` is skipped, because that is also the possessive:
/// `ability's` would otherwise teach us to "correct" `abilitys` to it when the
/// writer meant `abilities`. The `'s` contractions worth having (`that's`,
/// `what's`) are in the static table, where a human checked them.
///
/// Nothing else is required of the shape. An earlier version demanded one of
/// the standard suffixes (`n't`, `'ve`, `'ll`), which quietly excluded
/// `y'all`, `ma'am`, and every apostrophe form a person might have that a
/// grammar of English does not — exactly the words a *personal* lexicon exists
/// to hold.
pub fn derive<'a>(
    known: impl Iterator<Item = &'a str>,
    is_word: impl Fn(&str) -> bool,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for word in known {
        if !word.contains('\'') || word.ends_with("'s") {
            continue;
        }
        let bare: String = word.chars().filter(|c| *c != '\'').collect();
        if bare.chars().count() < 3 || is_word(&bare) {
            continue;
        }
        // Two *different* contractions can strip to the same spelling, and
        // then the correction is ambiguous and silence is right. Seeing the
        // same one twice is not that: `known` deliberately chains the lexicon
        // onto the dictionary, so any word in both arrives twice.
        match out.entry(bare) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if e.get() != word {
                    e.insert(String::new());
                }
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(word.to_string());
            }
        }
    }
    out.retain(|_, v| !v.is_empty());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_common_contractions() {
        assert_eq!(expand("dont"), Some("don't"));
        assert_eq!(expand("youre"), Some("you're"));
        assert_eq!(expand("wouldnt"), Some("wouldn't"));
    }

    fn derived(words: &[&str], real: &[&str]) -> std::collections::HashMap<String, String> {
        derive(words.iter().copied(), |w| real.contains(&w))
    }

    #[test]
    fn derives_a_contraction_nobody_enumerated() {
        let m = derived(&["might've", "who'll"], &[]);
        assert_eq!(m.get("mightve").map(String::as_str), Some("might've"));
        assert_eq!(m.get("wholl").map(String::as_str), Some("who'll"));
    }

    #[test]
    fn derivation_will_not_correct_a_real_word() {
        // `hell` and `shell` are words, so `he'll` and `she'll` must not
        // teach us to rewrite them. This is the whole safety property.
        let m = derived(&["he'll", "she'll", "can't"], &["hell", "shell", "cant"]);
        assert!(m.is_empty(), "{m:?}");
    }

    #[test]
    fn derivation_skips_possessives() {
        // `ability's` is a possessive, and `abilitys` is far more likely a
        // misspelled plural than a dropped apostrophe.
        let m = derived(&["ability's"], &[]);
        assert!(!m.contains_key("abilitys"));
    }

    #[test]
    fn an_ambiguous_stripping_stays_silent() {
        // Contrived, because apostrophe position rarely collides in English —
        // but the guard is what keeps a future list addition from silently
        // making a correction a coin flip.
        let m = derived(&["ne'er", "nee'r"], &[]);
        assert!(!m.contains_key("neer"), "{m:?}");
    }

    #[test]
    fn the_same_word_seen_twice_is_not_an_ambiguity() {
        // Callers chain the lexicon onto the dictionary, so a word in both
        // arrives twice. That must not read as a conflict.
        let m = derived(&["y'all", "y'all"], &[]);
        assert_eq!(m.get("yall").map(String::as_str), Some("y'all"));
    }

    #[test]
    fn leaves_ordinary_words_alone() {
        assert_eq!(expand("ship"), None);
        assert_eq!(expand("design"), None);
    }

    #[test]
    fn omits_forms_that_are_real_words() {
        // These need sentence context, not a lookup table — correcting them
        // here would flag correct prose.
        for word in [
            "cant", "wont", "its", "were", "well", "lets", "shell", "ill",
        ] {
            assert_eq!(expand(word), None, "{word} must not be auto-corrected");
        }
    }

    #[test]
    fn table_entries_are_lowercase_and_gain_an_apostrophe() {
        for (typed, fixed) in CONTRACTIONS {
            assert_eq!(*typed, typed.to_lowercase(), "{typed} should be lowercase");
            assert!(fixed.contains('\''), "{fixed} should carry an apostrophe");
        }
    }
}
