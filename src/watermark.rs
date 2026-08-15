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

/// Markers that identify assistant-authored text. Matched case-insensitively
/// against the whole body.
const MARKERS: &[&str] = &[
    "co-authored-by: claude",
    "generated with [claude code]",
    "generated with claude code",
    "🤖 generated with",
    "co-authored-by: anthropic",
    "claude opus",
    "claude sonnet",
    // Personal convention: PR descriptions carry a note about being vibed.
    "vibed with claude",
    "vibe-coded with claude",
];

/// The marker found in `body`, if any. Returns the matched marker so callers
/// can report *why* something was rejected rather than silently dropping it.
pub fn detect(body: &str) -> Option<&'static str> {
    let lower = body.to_lowercase();
    MARKERS.iter().copied().find(|m| lower.contains(m))
}

/// Convenience predicate for the ingest path.
pub fn is_assistant_authored(body: &str) -> bool {
    detect(body).is_some()
}

/// Drop trailing attribution blocks from an otherwise human-written body, so a
/// commit message you wrote still contributes its prose even though the tool
/// appended a trailer. Everything from the first marker line onward goes.
pub fn strip_trailer(body: &str) -> &str {
    let lower = body.to_lowercase();
    let mut cut = None;
    let mut offset = 0;
    for line in body.lines() {
        let line_lower = &lower[offset..offset + line.len()];
        if MARKERS.iter().any(|m| line_lower.contains(m)) {
            cut = Some(offset);
            break;
        }
        offset += line.len() + 1;
    }
    match cut {
        Some(i) => body[..i].trim_end(),
        None => body,
    }
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
