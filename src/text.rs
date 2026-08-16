//! Tokenization and the "don't even look at this" heuristics.
//!
//! Most false positives in a spell checker aren't bad suggestions — they're
//! things that were never prose to begin with: URLs, hex digests, flags,
//! file paths. Skipping those up front is worth more than any amount of
//! clever ranking downstream.

/// Fold smart typography down to its ASCII equivalent.
///
/// This is not "supporting Unicode" — it's handling English as editors
/// actually produce it. macOS, Slack, and Gmail all autocorrect `'` to `’`,
/// so captured prose is full of curly quotes; left alone, `don’t` tokenizes
/// as `don` + `t` and splits the corpus across two spellings of the same
/// word. Every mapping here is one char to one char, so column positions
/// survive unchanged.
pub fn normalize_typography(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\u{2019}' | '\u{02BC}' => '\'', // right single quote, modifier apostrophe
            '\u{2018}' => '\'',              // left single quote
            '\u{201C}' | '\u{201D}' => '"',  // curly double quotes
            '\u{2013}' | '\u{2014}' => '-',  // en dash, em dash
            '\u{00A0}' => ' ',               // non-breaking space
            other => other,
        })
        .collect()
}

/// One token lifted out of a line, with where it sat.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub text: String,
    /// 1-based **character** column. Characters, not bytes, because tokens
    /// are lifted from a masked copy of the line — masking replaces runs
    /// (possibly multibyte) with single-byte spaces, so byte offsets there
    /// stop matching the original. Char counts survive both masking and
    /// typography folding.
    pub col: usize,
}

/// Split a line into word-ish tokens. Internal apostrophes and hyphens stay
/// attached (`don't`, `well-known`) so contractions aren't shredded into
/// fragments that then fail the dictionary lookup.
pub fn tokenize(line: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    // Byte offset → char offset, so tokens can report char columns.
    let char_of: std::collections::HashMap<usize, usize> = line
        .char_indices()
        .enumerate()
        .map(|(n, (byte, _))| (byte, n))
        .collect();

    for (i, ch) in line.char_indices() {
        let wordish = ch.is_alphanumeric() || ch == '_' || ch == '\'' || ch == '-';
        match (wordish, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                push_token(&mut out, line, s, i, &char_of);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        push_token(&mut out, line, s, line.len(), &char_of);
    }
    out
}

fn push_token(
    out: &mut Vec<Token>,
    line: &str,
    start: usize,
    end: usize,
    char_of: &std::collections::HashMap<usize, usize>,
) {
    // Leading/trailing punctuation-ish characters aren't part of the word:
    // `--flag` and `don't.` should surface as `flag` and `don't`.
    let raw = &line[start..end];
    let trimmed = raw.trim_matches(|c: char| c == '-' || c == '\'' || c == '_');
    if trimmed.is_empty() {
        return;
    }
    let offset = raw.find(trimmed).unwrap_or(0);
    let byte = start + offset;
    out.push(Token {
        text: trimmed.to_string(),
        col: char_of.get(&byte).copied().unwrap_or(byte) + 1,
    });
}

/// Should this token be checked at all? Anything that isn't plausibly prose
/// is skipped outright — the conservative bias starts here.
pub fn is_checkable(token: &str) -> bool {
    if token.chars().count() < 3 {
        return false;
    }
    // Digits anywhere means an identifier, version, or hash — never prose.
    if token.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '-')
    {
        return false;
    }
    // SHOUTED words and Mixed-Case tokens are acronyms, constants, or proper
    // nouns; `ae` already owns acronyms and we shouldn't second-guess names.
    let alpha: String = token.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if alpha.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    // A pluralized acronym — `URLs`, `PRs`, `IDs` — is still an acronym.
    if let Some(stem) = alpha.strip_suffix('s')
        && stem.len() >= 2
        && stem.chars().all(|c| c.is_ascii_uppercase())
    {
        return false;
    }
    if is_camel_case(&alpha) {
        return false;
    }
    true
}

/// Is this token capitalized in a position where that marks a proper noun?
///
/// Mid-sentence capitals are names — of people, products, files, libraries —
/// and no dictionary will hold them. Guessing corrections for them produces
/// exactly the confident-but-wrong suggestion that teaches a user to stop
/// reading the output. Sentence-initial capitals carry no such signal, so
/// they stay checkable.
pub fn is_proper_noun(token: &str, sentence_initial: bool) -> bool {
    !sentence_initial && token.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// `camelCase` / `PascalCase` — an internal capital after a lowercase letter.
fn is_camel_case(word: &str) -> bool {
    word.chars()
        .zip(word.chars().skip(1))
        .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase())
}

/// Does this line look like code or markup rather than prose? Fenced blocks,
/// indented code, and lines dense with punctuation are skipped wholesale.
pub fn is_prose_line(line: &str) -> bool {
    // Indentation is the signal, so this must run before trimming.
    if line.starts_with("    ") || line.starts_with('\t') {
        return false;
    }
    let t = line.trim();
    if t.is_empty() || t.starts_with("```") {
        return false;
    }
    if t.starts_with('$') || t.starts_with('>') || t.starts_with('|') {
        return false;
    }
    // A line with more punctuation than letters is a diff, a table, or config.
    let letters = t.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let punct = t
        .chars()
        .filter(|c| c.is_ascii_punctuation() && *c != '\'' && *c != '.' && *c != ',')
        .count();
    letters > punct
}

/// Strip the parts of a line that are never prose — URLs, paths, inline code,
/// email addresses — replacing them with spaces so column offsets survive.
pub fn mask_non_prose(line: &str) -> String {
    let mut out: Vec<char> = line.chars().collect();
    // ASCII-only lowering, because it maps one char to one char. Full
    // `to_lowercase` can change the character count ('İ' becomes two), which
    // would desynchronize these indices from `out` and read out of bounds.
    // Every marker below is ASCII, so nothing is lost.
    let lower: String = line.chars().map(|c| c.to_ascii_lowercase()).collect();

    // Inline code spans.
    mask_delimited(&mut out, line, '`', '`');
    // Filesystem paths, which aren't IRIs and so aren't iriq's job.
    for marker in ["/", "~/", "./"] {
        mask_runs_from(&mut out, &lower, marker);
    }
    // Email addresses: mask the whole run around an '@'.
    mask_around(&mut out, line, '@');
    // Everything URL-shaped, via iriq.
    mask_iris(&mut out, line);
    out.into_iter().collect()
}

/// Mask IRIs using `iriq` rather than hand-rolled patterns.
///
/// Two URL bugs shipped before this: paths only masked at token boundaries,
/// then bare domains (`github.com/dpep/polyid/pull/43`) not masked at all,
/// which put `com`, `dpep`, and `pull` in the lexicon as words. The tail kept
/// going — ports, query strings, ticket keys — and a purpose-built extractor
/// is simply better at it than accumulated regexes.
///
/// Uses `Extractor` rather than `parse` per token, deliberately: `parse`
/// accepts `e.g.` and `etc.The` as IRIs, so per-token parsing would silently
/// skip ordinary prose. The extractor applies context and doesn't.
fn mask_iris(out: &mut [char], line: &str) {
    // Built once. Constructing an extractor compiles regexes, and this runs
    // per line — seeding alone reads hundreds of thousands of them.
    static EXTRACTOR: std::sync::OnceLock<iriq::Extractor> = std::sync::OnceLock::new();
    let extractor = EXTRACTOR.get_or_init(iriq::Extractor::new);

    for iri in extractor.extract_strings(line) {
        // The extractor normalizes (adding a scheme, among other things), so
        // the string it returns may not appear verbatim in the source. Fall
        // back to the scheme-less form to locate the original span.
        let needle = if line.contains(&iri) {
            Some(iri.clone())
        } else {
            iri.split_once("://")
                .map(|(_, rest)| rest.to_string())
                .filter(|rest| line.contains(rest.as_str()))
        };
        let Some(needle) = needle else { continue };
        let Some(byte_start) = line.find(&needle) else {
            continue;
        };
        let start = line[..byte_start].chars().count();
        for c in out.iter_mut().skip(start).take(needle.chars().count()) {
            *c = ' ';
        }
    }
}

fn mask_delimited(out: &mut [char], line: &str, open: char, close: char) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == open
            && let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == close)
        {
            for c in out.iter_mut().take(end + 1).skip(i) {
                *c = ' ';
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
}

fn mask_runs_from(out: &mut [char], lower: &str, marker: &str) {
    let chars: Vec<char> = lower.chars().collect();
    let m: Vec<char> = marker.chars().collect();
    let mut i = 0;
    while i + m.len() <= chars.len() {
        if chars[i..i + m.len()] == m[..] {
            // A bare '/' only starts a path at a token boundary.
            let boundary = i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(';
            if marker != "/" || boundary {
                let mut j = i;
                while j < chars.len() && !chars[j].is_whitespace() {
                    out[j] = ' ';
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
}

fn mask_around(out: &mut [char], line: &str, needle: char) {
    let chars: Vec<char> = line.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != needle {
            continue;
        }
        let mut s = i;
        while s > 0 && !chars[s - 1].is_whitespace() {
            s -= 1;
        }
        let mut e = i;
        while e < chars.len() && !chars[e].is_whitespace() {
            e += 1;
        }
        for c in out.iter_mut().take(e).skip(s) {
            *c = ' ';
        }
    }
}

/// The prose words of one line: typography folded, non-prose masked,
/// tokenized, normalized, and filtered to things that are actually words.
///
/// One definition rather than five. This pipeline was copied into the
/// spool processor, the frequency miner, and the complexity analyzer, and the
/// copies had drifted: one allowed hyphens and the others didn't, and the
/// analyzer applied neither masking nor the prose-line test — so `analyze` on
/// a text counted URL fragments as vocabulary while the corpus path never saw
/// them, despite the two claiming to be comparable.
pub fn prose_words(line: &str) -> Vec<String> {
    if !is_prose_line(line) {
        return Vec::new();
    }
    let masked = mask_non_prose(&normalize_typography(line));
    tokenize(&masked)
        .iter()
        .map(|t| normalize(&t.text))
        .filter(|w| {
            w.chars().count() >= 2
                && w.chars()
                    .all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '-')
        })
        .collect()
}

/// Split an identifier into its parts: `rubocop_todo` → `rubocop`, `todo`;
/// `pattern-engine` → `pattern`, `engine`; `camelCase` → `camel`, `case`.
/// The whole identifier is a word in its own right — the caller keeps it too.
pub fn split_identifier(ident: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for chunk in ident.split(['_', '-', '.', '/']) {
        if chunk.is_empty() {
            continue;
        }
        let mut current = String::new();
        for ch in chunk.chars() {
            if ch.is_ascii_uppercase() && !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            current.push(ch.to_ascii_lowercase());
        }
        if !current.is_empty() {
            parts.push(current);
        }
    }
    parts.retain(|p| p.chars().count() >= 2 && p.chars().all(|c| c.is_ascii_alphabetic()));
    parts
}

/// Normalize a word for storage and lookup: lowercased, surrounding
/// punctuation already gone. Possessives collapse to the base word so
/// `Daniel's` and `Daniel` are one entry.
pub fn normalize(word: &str) -> String {
    let w = word.to_lowercase();
    w.strip_suffix("'s").unwrap_or(&w).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_with_columns() {
        let toks = tokenize("ship the MVP");
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].text, "ship");
        assert_eq!(toks[0].col, 1);
        assert_eq!(toks[2].text, "MVP");
        assert_eq!(toks[2].col, 10);
    }

    #[test]
    fn keeps_contractions_and_hyphens_whole() {
        let toks = tokenize("don't ship half-baked work");
        let words: Vec<_> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(words, vec!["don't", "ship", "half-baked", "work"]);
    }

    #[test]
    fn strips_leading_punctuation_from_flags() {
        let toks = tokenize("pass --verbose here");
        assert_eq!(toks[1].text, "verbose");
    }

    #[test]
    fn skips_non_prose_tokens() {
        assert!(is_checkable("ship"));
        assert!(!is_checkable("to")); // too short
        assert!(!is_checkable("sha256")); // digits
        assert!(!is_checkable("JSON")); // acronym
        assert!(!is_checkable("camelCase")); // identifier
    }

    #[test]
    fn masks_urls_and_code_spans() {
        let masked = mask_non_prose("see https://github.com/dpep/ae and `foo_bar` now");
        assert!(!masked.contains("github"));
        assert!(!masked.contains("foo_bar"));
        assert!(masked.contains("see"));
        assert!(masked.contains("now"));
        assert_eq!(
            masked.chars().count(),
            "see https://github.com/dpep/ae and `foo_bar` now"
                .chars()
                .count()
        );
    }

    #[test]
    fn masking_survives_characters_that_change_length_when_lowercased() {
        // 'İ' lowercases to two chars; masking must not desynchronize on it.
        let line = "İstanbul https://example.com/x notes";
        let masked = mask_non_prose(line);
        assert_eq!(masked.chars().count(), line.chars().count());
        assert!(!masked.contains("example"));
        assert!(masked.contains("notes"));
    }

    #[test]
    fn rejects_code_shaped_lines() {
        assert!(is_prose_line("we should ship this"));
        assert!(!is_prose_line("```rust"));
        assert!(!is_prose_line("    let x = 1;"));
        assert!(!is_prose_line("| a | b |"));
    }

    #[test]
    fn prose_words_masks_before_counting() {
        // The analyzer used to skip masking, so a URL's fragments counted as
        // vocabulary in one mode and not the other.
        let words = prose_words("see github.com/dpep/polyid for the design");
        assert!(!words.contains(&"github".to_string()));
        assert!(!words.contains(&"polyid".to_string()));
        assert!(words.contains(&"design".to_string()));
    }

    #[test]
    fn prose_words_skips_non_prose_lines() {
        assert!(prose_words("    let x = compute();").is_empty());
        assert!(prose_words("| a | b |").is_empty());
    }

    #[test]
    fn splits_identifiers_into_parts() {
        assert_eq!(split_identifier("rubocop_todo"), vec!["rubocop", "todo"]);
        assert_eq!(
            split_identifier("pattern-engine"),
            vec!["pattern", "engine"]
        );
        assert_eq!(split_identifier("camelCase"), vec!["camel", "case"]);
    }

    #[test]
    fn normalizes_possessives() {
        assert_eq!(normalize("Daniel's"), "daniel");
        assert_eq!(normalize("Ship"), "ship");
    }
}
