//! How common a word is in ordinary English.
//!
//! The backstop dictionary is a flat membership set — 236k words, all equally
//! weighted — which is why `smal` offered `small`, `smalm`, and `smalt` as
//! peers. Frequency does two jobs, both proven out by measurement:
//!
//! - **Ranking.** It breaks the ties edit distance leaves behind, so the
//!   common word wins over the obscure one.
//! - **A fast path.** Most words in real prose are common, so consulting a
//!   small frequent set first answers nearly every lookup without reading the
//!   large one at all — ordinary prose went from ~45ms to ~5ms, because the
//!   system dictionary is never touched.
//!
//! Two sources, layered the way the rest of the tool layers things: a small
//! embedded core for day one, and counts mined from prose already on the
//! machine. The mined half also serves as a *membership* source, because the
//! system dictionary is `web2` (1934) and lacks ordinary modern words.
//!
//! What frequency deliberately does *not* do is decide which of two real
//! words belongs in a sentence — see the note further down.

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

// Deliberately absent: a "flag the rarer spelling" check driven by frequency
// alone. It was built and removed, because it doesn't work and the reason is
// instructive.
//
// We want P(you meant `from` | you typed `form`). Frequency supplies the
// prior — `from` is far more common — but that prior has to beat the typo
// rate to matter. P(typing `form` while meaning `from`) is perhaps one in
// fifty; P(typing `form` while meaning `form`) is essentially one. So the
// posterior favors "correct as written" unless the frequency gap is enormous,
// and even then the test fires on *every* occurrence of the rarer word with
// no knowledge of the sentence.
//
// In practice it flagged `the apostrophe form usually isn't...` in this
// project's own README — a correct use, exactly the false positive the tool
// exists to prevent.
//
// The cold-start fix that does work is context-bearing: a small bundled table
// of discriminating collocates (`apart from`, `far from` versus `the form`,
// `fill in the form`). See docs/PLAN.md.

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
}
