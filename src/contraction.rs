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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrects_common_contractions() {
        assert_eq!(expand("dont"), Some("don't"));
        assert_eq!(expand("youre"), Some("you're"));
        assert_eq!(expand("wouldnt"), Some("wouldn't"));
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
