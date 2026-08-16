//! How common a word is in ordinary English.
//!
//! The backstop dictionary is a flat membership set — 236k words, all equally
//! weighted — which is why `smal` offered `small`, `smalm`, and `smalt` as
//! peers. Frequency breaks those ties, and does two more jobs besides:
//!
//! - **Cold-start confusion detection.** Before there's any personal
//!   collocation evidence, knowing `from` is vastly more common than `form`
//!   is enough to be suspicious in the right direction.
//! - **A fast path.** Most words in real prose are common words, so checking
//!   a small frequent set first answers most lookups without touching the
//!   large one.
//!
//! Two sources, layered the same way the rest of the tool layers things:
//! a small embedded core for day one, and counts mined from prose already on
//! the machine. Personal evidence accumulates on top of the general prior
//! rather than replacing it.

/// The most frequent English words, in descending order of frequency.
///
/// Short on purpose. This isn't trying to be a corpus — it's the head of the
/// distribution, which is where the value is concentrated: function words are
/// simultaneously the most common words and the ones confusion pairs are made
/// of (`from`/`form`, `then`/`than`, `their`/`there`, `its`/`it's`,
/// `to`/`too`, `were`/`where`). A few hundred entries covers nearly every
/// real-word confusion; the long tail is what mined prose is for.
pub const CORE: &[&str] = &[
    "the",
    "be",
    "to",
    "of",
    "and",
    "a",
    "in",
    "that",
    "have",
    "i",
    "it",
    "for",
    "not",
    "on",
    "with",
    "he",
    "as",
    "you",
    "do",
    "at",
    "this",
    "but",
    "his",
    "by",
    "from",
    "they",
    "we",
    "say",
    "her",
    "she",
    "or",
    "an",
    "will",
    "my",
    "one",
    "all",
    "would",
    "there",
    "their",
    "what",
    "so",
    "up",
    "out",
    "if",
    "about",
    "who",
    "get",
    "which",
    "go",
    "me",
    "when",
    "make",
    "can",
    "like",
    "time",
    "no",
    "just",
    "him",
    "know",
    "take",
    "people",
    "into",
    "year",
    "your",
    "good",
    "some",
    "could",
    "them",
    "see",
    "other",
    "than",
    "then",
    "now",
    "look",
    "only",
    "come",
    "its",
    "over",
    "think",
    "also",
    "back",
    "after",
    "use",
    "two",
    "how",
    "our",
    "work",
    "first",
    "well",
    "way",
    "even",
    "new",
    "want",
    "because",
    "any",
    "these",
    "give",
    "day",
    "most",
    "us",
    "is",
    "are",
    "was",
    "were",
    "been",
    "has",
    "had",
    "did",
    "said",
    "made",
    "find",
    "here",
    "thing",
    "many",
    "such",
    "where",
    "much",
    "before",
    "through",
    "same",
    "should",
    "each",
    "between",
    "own",
    "under",
    "last",
    "right",
    "still",
    "need",
    "too",
    "does",
    "off",
    "again",
    "few",
    "while",
    "might",
    "must",
    "since",
    "against",
    "during",
    "without",
    "another",
    "around",
    "however",
    "both",
    "those",
    "being",
    "very",
    "may",
    "down",
    "part",
    "place",
    "case",
    "point",
    "number",
    "group",
    "problem",
    "fact",
    "change",
    "small",
    "large",
    "long",
    "great",
    "little",
    "high",
    "different",
    "next",
    "early",
    "young",
    "important",
    "public",
    "bad",
    "able",
    "better",
    "best",
    "sure",
    "clear",
    "real",
    "full",
    "open",
    "close",
    "start",
    "end",
    "help",
    "show",
    "move",
    "keep",
    "let",
    "begin",
    "seem",
    "talk",
    "turn",
    "ask",
    "try",
    "leave",
    "call",
    "feel",
    "become",
    "leave",
    "put",
    "mean",
    "run",
    "set",
    "read",
    "write",
    "send",
    "build",
    "add",
    "check",
    "test",
    "fix",
    "ship",
    "review",
    "code",
    "file",
    "line",
    "team",
    "user",
    "data",
    "system",
    "issue",
    "value",
    "name",
    "type",
    "list",
    "state",
    "form",
    "order",
    "report",
    "result",
    "process",
    "service",
    "version",
    "update",
    "quite",
    "quiet",
    "affect",
    "effect",
    "lose",
    "loose",
    "accept",
    "except",
    "advice",
    "advise",
    "avoid",
    "though",
    "thought",
    "through",
    "thorough",
    "trial",
    "trail",
    "casual",
    "causal",
    "public",
    "angle",
    "angel",
    "field",
    "filed",
    "united",
    "untied",
    "manager",
    "manger",
    "principal",
    "principle",
    "discrete",
    "discreet",
    "complement",
    "compliment",
    "stationary",
    "stationery",
    "weather",
    "whether",
    "country",
    "county",
    "nuclear",
    "unclear",
];

/// Synthetic count for the core list, from Zipf's law: frequency falls
/// roughly as 1/rank. Gives the head of the distribution sane magnitudes
/// relative to counts mined from real prose, so the two can simply be summed
/// rather than needing separate scales.
pub fn zipf_count(rank: usize) -> i64 {
    const SCALE: i64 = 1_000_000;
    SCALE / (rank as i64 + 1)
}

/// The core list as `(word, synthetic count)` pairs.
pub fn core_counts() -> Vec<(&'static str, i64)> {
    CORE.iter()
        .enumerate()
        .map(|(rank, word)| (*word, zipf_count(rank)))
        .collect()
}

/// How much more common `a` is than `b`, as a ratio. Used to decide whether a
/// confusion is worth raising before there's any personal evidence.
pub fn asymmetry(a: i64, b: i64) -> f64 {
    (a as f64 + 1.0) / (b as f64 + 1.0)
}

/// How lopsided two candidates must be before frequency alone justifies
/// suspicion. This fires with no context evidence at all, so it should only
/// trigger on pairs that really are far apart.
///
/// Calibrated against what the embedded core can actually express, not
/// against real English. A few hundred entries compress the range — `from`
/// outranks `form` by three orders of magnitude in a real corpus but only
/// tenfold here — so the core gives reliable *ordering* and approximate
/// magnitudes. Mined prose widens the gaps as it accumulates.
pub const MIN_ASYMMETRY: f64 = 8.0;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn core_is_ordered_most_common_first() {
        let counts = core_counts();
        assert_eq!(counts[0].0, "the");
        assert!(counts[0].1 > counts[counts.len() - 1].1);
    }

    #[test]
    fn core_covers_the_confusable_function_words() {
        let words: HashSet<&str> = CORE.iter().copied().collect();
        for word in [
            "from", "form", "then", "than", "their", "there", "its", "to", "too", "were", "where",
            "quite", "quiet", "lose", "loose", "affect", "effect",
        ] {
            assert!(words.contains(word), "{word} missing from the core list");
        }
    }

    #[test]
    fn common_words_dominate_rare_ones() {
        let counts = core_counts();
        let lookup = |w: &str| {
            counts
                .iter()
                .find(|(c, _)| *c == w)
                .map(|(_, n)| *n)
                .unwrap()
        };
        // The whole point: `from` should overwhelm `form`.
        assert!(asymmetry(lookup("from"), lookup("form")) > MIN_ASYMMETRY);
    }

    #[test]
    fn asymmetry_is_symmetric_in_the_other_direction() {
        assert!(asymmetry(10, 1000) < 1.0);
        assert!(asymmetry(1000, 10) > 1.0);
    }

    #[test]
    fn asymmetry_handles_absent_words() {
        // Zero counts must not divide by zero.
        assert!(asymmetry(0, 0).is_finite());
        assert_eq!(asymmetry(0, 0), 1.0);
    }
}
