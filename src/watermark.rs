//! Detecting text you didn't write.
//!
//! A PR description or commit message drafted by an assistant is *about* your
//! work but isn't *in* your voice. Ingesting it would train the lexicon on
//! someone else's diction and then feed that back as "your" style — a
//! self-reinforcing drift with no natural correction.
//!
//! Detection is line-oriented and deliberately literal. These markers are
//! conventions, not guarantees: absence proves nothing, so this is a filter on
//! the obvious cases rather than a claim of authorship.

/// Markers that identify assistant-authored text, matched case-insensitively
/// against the **start of a line**.
///
/// Anchoring matters more than it looks. These are attribution trailers, which
/// always begin a line; a substring search anywhere in the body would treat a
/// Slack message *about* Claude as written by it. For someone who writes about
/// this tooling daily that inverts the whole point — the learning path would
/// quietly discard their real prose. Here a missed watermark costs a few
/// drifted counts, while a false positive throws away their actual voice, so
/// the bias runs opposite to the checker's.
const MARKERS: &[&str] = &[
    "co-authored-by: claude",
    "co-authored-by: anthropic",
    "generated with [claude code]",
    "generated with claude code",
    "🤖 generated with",
    // Personal convention: PR descriptions carry a note about being vibed.
    "vibed with claude",
    "vibe-coded with claude",
];

/// The marker this line begins with, if any.
fn line_marker(line: &str) -> Option<&'static str> {
    let lower = line.trim_start().to_lowercase();
    MARKERS.iter().copied().find(|m| lower.starts_with(m))
}

/// The marker found in `body`, if any. Returns the matched marker so callers
/// can report *why* something was rejected rather than silently dropping it.
pub fn detect(body: &str) -> Option<&'static str> {
    body.lines().find_map(line_marker)
}

/// Convenience predicate for the ingest path.
pub fn is_assistant_authored(body: &str) -> bool {
    detect(body).is_some()
}

/// Drop trailing attribution blocks from an otherwise human-written body, so a
/// commit message you wrote still contributes its prose even though the tool
/// appended a trailer. Everything from the first marker line onward goes.
/// Lowercasing is done per line rather than once over the whole body:
/// `str::to_lowercase` doesn't preserve byte length ('İ' grows), so offsets
/// computed from the original text drift against a lowercased copy and
/// eventually slice a char boundary — a panic, on a spool row that would then
/// be retried forever. Splitting on `\n` (not `lines()`) keeps the byte
/// accounting exact.
pub fn strip_trailer(body: &str) -> &str {
    let mut offset = 0;
    for line in body.split('\n') {
        if line_marker(line).is_some() {
            return body[..offset].trim_end();
        }
        offset += line.len() + 1;
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_standard_trailers() {
        assert!(is_assistant_authored(
            "Fix the thing\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
        ));
        assert!(is_assistant_authored(
            "🤖 Generated with [Claude Code](https://claude.com/claude-code)"
        ));
    }

    #[test]
    fn is_case_insensitive() {
        assert!(is_assistant_authored("co-authored-by: CLAUDE"));
    }

    #[test]
    fn leaves_ordinary_prose_alone() {
        assert!(!is_assistant_authored("ship the small focused change"));
        assert_eq!(detect("nothing to see"), None);
    }

    #[test]
    fn writing_about_claude_is_not_written_by_claude() {
        // The learning path must not discard prose that merely discusses the
        // tooling — otherwise it starves on exactly the topics written about
        // most.
        assert!(!is_assistant_authored(
            "we should compare claude opus pricing before we commit"
        ));
        assert!(!is_assistant_authored(
            "I asked whether it was generated with claude code or not"
        ));
    }

    #[test]
    fn survives_non_ascii_bodies() {
        // 'İ' lowercases to two chars, so byte offsets taken from the original
        // text drift against a lowercased copy.
        let body = "İstanbul notes\nsecond line\n\nCo-Authored-By: Claude <x>";
        assert_eq!(strip_trailer(body), "İstanbul notes\nsecond line");
        assert!(is_assistant_authored(body));
        assert_eq!(strip_trailer("İ\nplain text"), "İ\nplain text");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let body = "real prose here\r\nCo-Authored-By: Claude <x>\r\n";
        assert_eq!(strip_trailer(body), "real prose here");
    }

    #[test]
    fn reports_which_marker_matched() {
        assert_eq!(
            detect("body\n\nCo-Authored-By: Claude <x>"),
            Some("co-authored-by: claude")
        );
    }

    #[test]
    fn strips_trailer_but_keeps_body() {
        let msg = "Fix flaky spec\n\nThe wait was racy.\n\nCo-Authored-By: Claude <x>\n";
        assert_eq!(strip_trailer(msg), "Fix flaky spec\n\nThe wait was racy.");
    }

    #[test]
    fn strip_is_a_noop_without_markers() {
        assert_eq!(strip_trailer("just my words"), "just my words");
    }
}
