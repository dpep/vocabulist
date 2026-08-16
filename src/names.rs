//! Names a document tells you about, so they stop reading as misspellings.
//!
//! Measured against held-out technical prose, most of what the checker got
//! wrong was never a word: `nixpkgs`, `winget-pkgs`, `ugrep`, `zypper`,
//! `Chocolatey`. No dictionary will ever hold these, and no frequency list
//! will either — growing the word list is the wrong lever entirely.
//!
//! The document names them, though. A project appears in a URL, a repo path,
//! or a code span before or beside the sentence that mentions it, and those
//! regions are already located — `mask_non_prose` finds them in order to throw
//! them away. Keeping what it removed turns the discarded half of every line
//! into evidence.
//!
//! This only ever *accepts*. A hit is consulted after the lexicon, the mined
//! corpus, and the dictionary have all declined, so a common word landing in
//! here by accident changes nothing.

use crate::text;
use std::collections::HashSet;

/// Names gathered from the document so far.
///
/// Deliberately accumulating rather than pre-scanned: input arrives a line at
/// a time from a pipe, and a set that grows as it reads behaves the same on a
/// stream as on a file. The cost is that a name introduced *below* its first
/// prose mention goes unrecognized — in practice the link and the sentence are
/// usually the same line, and being weaker on a stream beats being different.
#[derive(Debug, Default)]
pub struct Names {
    seen: HashSet<String>,
}

impl Names {
    pub fn new() -> Self {
        Self::default()
    }

    /// Harvest names from one line, before it is checked.
    ///
    /// `masked` is the line with every non-prose region blanked. Comparing it
    /// to the original recovers exactly what was removed — URLs, paths, code
    /// spans, mail addresses — which is where names live.
    pub fn observe(&mut self, line: &str, masked: &str) {
        let removed: String = line
            .chars()
            .zip(masked.chars())
            .map(|(original, m)| if m == ' ' { original } else { ' ' })
            .collect();

        for token in text::tokenize(&removed) {
            self.insert(&token.text);
            // A URL says `NixOS/nixpkgs` and the prose says `nixpkgs`; a repo
            // says `winget-pkgs` and the prose says `winget`. Splitting picks
            // up the halves that get referred to on their own.
            for part in text::split_identifier(&token.text) {
                self.insert(&part);
            }
        }
    }

    /// Harvest from a line that is not prose at all — a fenced code block, a
    /// shell command, a markdown table.
    ///
    /// Every token counts here, not just the masked regions, because the whole
    /// line is the masked region: `$ sudo zypper install ripgrep` and a table
    /// of package managers are pure name, and they are where a README
    /// introduces a tool before writing a sentence about it.
    pub fn observe_code(&mut self, line: &str) {
        for token in text::tokenize(line) {
            self.insert(&token.text);
            for part in text::split_identifier(&token.text) {
                self.insert(&part);
            }
        }
    }

    /// Record a token the checker identified as a proper noun.
    ///
    /// Capitalization marks a name only where it isn't just sentence position,
    /// a distinction the check loop already draws. Promoting it from
    /// per-occurrence to per-document is what lets `Chocolatey` mid-paragraph
    /// vouch for the `Chocolatey` that opens a list item.
    pub fn observe_proper_noun(&mut self, token: &str) {
        self.insert(token);
    }

    fn insert(&mut self, token: &str) {
        if token.chars().count() < 3 {
            return;
        }
        self.seen.insert(text::normalize(token));
    }

    /// Has the document named this? Ask only after the word is unknown.
    pub fn contains(&self, word: &str) -> bool {
        self.seen.contains(word)
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(line: &str) -> Names {
        let mut names = Names::new();
        let masked = text::mask_non_prose(line);
        names.observe(line, &masked);
        names
    }

    #[test]
    fn a_name_in_a_url_is_a_name_in_the_prose() {
        let names = observed("[nixpkgs](https://github.com/NixOS/nixpkgs/blob/master/x.nix)");
        assert!(names.contains("nixpkgs"));
    }

    #[test]
    fn a_repo_path_names_each_of_its_parts() {
        let names = observed("see https://github.com/microsoft/winget-pkgs for it");
        assert!(names.contains("winget-pkgs"));
        assert!(names.contains("winget"));
    }

    #[test]
    fn an_inline_code_span_names_what_it_holds() {
        let names = observed("install it with `zypper install ripgrep` today");
        assert!(names.contains("zypper"));
    }

    #[test]
    fn prose_outside_the_masked_regions_is_not_harvested() {
        let names = observed("the quick brown fox jumped");
        assert!(names.is_empty());
    }

    #[test]
    fn short_fragments_are_ignored() {
        let names = observed("see https://x.com/a/bc/def now");
        assert!(!names.contains("a"));
        assert!(!names.contains("bc"));
        assert!(names.contains("def"));
    }
}
