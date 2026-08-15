//! The backstop dictionary — the system word list, used as a floor rather
//! than an authority.
//!
//! A generic dictionary is wrong about your vocabulary in one direction only:
//! it doesn't know your jargon. It's still right about ordinary English, so we
//! keep it underneath the lexicon and let the lexicon override it.
//!
//! `/usr/share/dict/words` is deliberately spartan — it carries base forms but
//! few inflections, so a naive lookup flags `shipping` and `focused`. We fold
//! common English morphology back in rather than shipping a word list.

use std::collections::HashSet;

const WORDLIST_PATHS: &[&str] = &["/usr/share/dict/words", "/usr/dict/words"];

/// The system word list, lowercased. `None` when no list is installed — in
/// which case the lexicon stands alone and checking gets more conservative,
/// never less.
pub fn load() -> Option<HashSet<String>> {
    for path in WORDLIST_PATHS {
        if let Ok(text) = std::fs::read_to_string(path) {
            let words: HashSet<String> = text
                .lines()
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty())
                .collect();
            if !words.is_empty() {
                return Some(words);
            }
        }
    }
    None
}

/// Is `word` known, allowing for regular inflection? Checks the surface form
/// first, then peels common suffixes back to a base form.
pub fn contains(words: &HashSet<String>, word: &str) -> bool {
    if words.contains(word) {
        return true;
    }
    base_forms(word).iter().any(|b| words.contains(b))
}

/// Candidate base forms for an inflected word. Over-generates on purpose —
/// a spurious base that happens to be a real word means we accept a word we
/// might have flagged, which is the direction we want to err in.
fn base_forms(word: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: String| {
        if s.chars().count() >= 2 {
            out.push(s);
        }
    };

    // Contractions. The apostrophe isn't the morpheme boundary in the `n't`
    // family — `don't` splits to `don`, not `do` — so that case comes first.
    if let Some(stem) = word.strip_suffix("n't") {
        push(stem.to_string());
    }
    if let Some((head, _)) = word.split_once('\'') {
        push(head.to_string());
    }

    for suffix in ["s", "es", "ed", "ing", "ly", "er", "est", "ers", "ings"] {
        let Some(stem) = word.strip_suffix(suffix) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        push(stem.to_string());
        // `shipped` → `ship`, `running` → `run`: undo consonant doubling.
        let mut chars = stem.chars().rev();
        if let (Some(a), Some(b)) = (chars.next(), chars.next())
            && a == b
            && !"aeiou".contains(a)
        {
            push(stem[..stem.len() - a.len_utf8()].to_string());
        }
        // `shipping` → `ship` via the `e` that was dropped: `focusing` → `focuse`
        // is nonsense, but `focus` + `e` covers `larger` → `large`.
        push(format!("{stem}e"));
        // `carries` → `carry`, `happily` → `happy`.
        if let Some(without_i) = stem.strip_suffix('i') {
            push(format!("{without_i}y"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(words: &[&str]) -> HashSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn finds_exact_words() {
        assert!(contains(&dict(&["ship"]), "ship"));
        assert!(!contains(&dict(&["ship"]), "zzzz"));
    }

    #[test]
    fn folds_regular_inflections_back_to_the_base() {
        let d = dict(&["ship", "focus", "large", "carry"]);
        assert!(contains(&d, "ships"));
        assert!(contains(&d, "shipped"));
        assert!(contains(&d, "shipping"));
        assert!(contains(&d, "focused"));
        assert!(contains(&d, "larger"));
        assert!(contains(&d, "carries"));
    }

    #[test]
    fn handles_contractions() {
        assert!(contains(&dict(&["do"]), "don't"));
    }

    #[test]
    fn still_rejects_genuine_nonsense() {
        let d = dict(&["ship", "focus"]);
        assert!(!contains(&d, "shp"));
        assert!(!contains(&d, "flurb"));
    }
}
