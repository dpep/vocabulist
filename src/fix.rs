//! `--fix`: write text back with the corrections that are never guesses.
//!
//! Which findings qualify is the checker's call (`Checker::correction`); this
//! module only puts the replacement where the finding was, in the case it was
//! typed in.

use std::io::{self, Write};
use std::path::Path;

use crate::types::Fix;

/// Apply `fixes` to `line`, returning the new line and the fixes that landed.
///
/// A fix lands only where the line still holds the word it names, at the
/// column it names. Columns are characters, and the checker's typography
/// folding keeps them aligned, so a mismatch means something unexpected and
/// the safe answer is to leave the text alone. Nor does one land on a word
/// joined to code (`teh.rs`, `$teh`): one rule for `--fix` and `--expand`.
pub fn apply(line: &str, fixes: Vec<Fix>) -> (String, Vec<Fix>) {
    let mut chars: Vec<char> = line.chars().collect();
    // Corrections carry a straight apostrophe; a line typed with curly ones
    // (as macOS, Slack and Gmail write them) keeps its typography.
    let curly = line.contains('\u{2019}');
    let mut landed = Vec::new();
    // Right to left, so an earlier column is not shifted by a later edit.
    let mut fixes = fixes;
    fixes.sort_by_key(|f| std::cmp::Reverse(f.col));
    for mut fix in fixes {
        let start = fix.col.saturating_sub(1);
        let end = start + fix.from.chars().count();
        let Some(span) = chars.get(start..end) else {
            continue;
        };
        let typed: String = span.iter().collect();
        if crate::text::normalize_typography(&typed) != fix.from
            || crate::text::joined_to_code(&chars, start, end)
        {
            continue;
        }
        // The report names what was replaced as it was typed, curly or not.
        fix.from = typed;
        fix.to = match_case(&fix.from, &fix.to);
        if curly {
            fix.to = fix.to.replace('\'', "\u{2019}");
        }
        chars.splice(start..end, fix.to.chars());
        landed.push(fix);
    }
    landed.reverse();
    (chars.into_iter().collect(), landed)
}

/// Replace the file at `path` with `contents`, all at once.
///
/// Written beside the file and renamed over it, so a failure part way leaves
/// the old text whole rather than half of the new. A symlink is followed, so
/// the link survives and its target changes, and the file keeps its
/// permissions. A read-only file is refused: a rename would replace it anyway.
pub fn replace_file(path: &Path, contents: &str) -> io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let permissions = std::fs::metadata(&target)?.permissions();
    if permissions.readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is read-only", path.display()),
        ));
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let temp = target.with_file_name(format!(".{name}.vocab-{}", std::process::id()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        // Before the text goes in: created with the umask's mode, a private
        // file's contents would sit world-readable until the rename.
        file.set_permissions(permissions)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// `to`, in the case `from` was typed in: `Teh` → `The`, `TEH` → `THE`.
/// Nothing reaches the last today: an all-caps token reads as an acronym and
/// is neither checked nor expanded (`text::is_checkable`).
fn match_case(from: &str, to: &str) -> String {
    let letters: Vec<char> = from.chars().filter(|c| c.is_alphabetic()).collect();
    let upper = |c: &char| c.is_uppercase();
    match letters.as_slice() {
        [first, rest @ ..] if upper(first) && rest.iter().all(upper) && !rest.is_empty() => {
            to.to_uppercase()
        }
        [first, ..] if upper(first) => {
            let mut chars = to.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
        _ => to.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FixKind;

    fn fix(col: usize, from: &str, to: &str) -> Fix {
        Fix {
            line: 1,
            col,
            from: from.to_string(),
            to: to.to_string(),
            kind: FixKind::Unknown,
        }
    }

    #[test]
    fn replaces_in_place_and_keeps_the_typed_case() {
        let (out, landed) = apply(
            "Teh plan: dont ship teh widget",
            vec![
                fix(1, "Teh", "the"),
                fix(11, "dont", "don't"),
                fix(21, "teh", "the"),
            ],
        );
        assert_eq!(out, "The plan: don't ship the widget");
        assert_eq!(landed.len(), 3);
        assert_eq!(landed[0].to, "The", "the report says what was written");
    }

    #[test]
    fn a_fix_that_no_longer_matches_the_text_is_dropped() {
        let (out, landed) = apply("the widget", vec![fix(5, "teh", "the")]);
        assert_eq!(out, "the widget");
        assert!(landed.is_empty());
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let (out, _) = apply("café — teh", vec![fix(8, "teh", "the")]);
        assert_eq!(out, "café — the");
    }

    fn column_of(line: &str, word: &str) -> usize {
        line[..line.find(word).unwrap()].chars().count() + 1
    }

    #[test]
    fn a_word_joined_to_code_is_left_alone() {
        for line in [
            "edit teh.rs",
            "set $teh",
            "pass --teh",
            "use foo::teh",
            "call teh()",
            "read x.teh",
            "a->teh",
            "teh=1",
            "teh/x/y",
            "x/teh.rs",
            "teh/",
            "_teh",
            "foo_teh_bar",
            "teh[0]",
            "#teh",
        ] {
            let col = column_of(line, "teh");
            let (out, landed) = apply(line, vec![fix(col, "teh", "the")]);
            assert_eq!(out, line, "{line}");
            assert!(landed.is_empty(), "{line}");
        }
    }

    #[test]
    fn prose_punctuation_does_not_make_a_word_code() {
        for line in [
            "teh.",
            "teh, then",
            "(teh)",
            "teh: then",
            "\"teh\"",
            "*teh*",
            "teh?",
            "[teh](x)",
            "it's teh!",
            "wait — teh",
            "_teh_",
            "__teh__",
            "~~teh~~",
            "and/teh",
            "teh/send",
            "teh(s)",
            "teh.Next",
        ] {
            let col = column_of(line, "teh");
            let (out, landed) = apply(line, vec![fix(col, "teh", "the")]);
            assert_eq!(out, line.replace("teh", "the"), "{line}");
            assert_eq!(landed.len(), 1, "{line}");
        }
    }

    #[test]
    fn a_curly_apostrophe_stays_curly() {
        let (out, landed) = apply("Teh\u{2019}s plan", vec![fix(1, "Teh's", "the's")]);
        assert_eq!(out, "The\u{2019}s plan");
        assert_eq!(
            landed[0].to, "The\u{2019}s",
            "the report says what was written"
        );
        assert_eq!(landed[0].from, "Teh\u{2019}s", "and what it replaced");
        // A contraction typed with no apostrophe gets the typographer's one
        // only when the line already uses it.
        let (out, _) = apply("it\u{2019}s fine, dont", vec![fix(12, "dont", "don't")]);
        assert_eq!(out, "it\u{2019}s fine, don\u{2019}t");
        let (out, _) = apply("it's fine, dont", vec![fix(12, "dont", "don't")]);
        assert_eq!(out, "it's fine, don't");
    }

    #[test]
    fn shouting_stays_shouting() {
        assert_eq!(match_case("TEH", "the"), "THE");
        assert_eq!(match_case("teh", "the"), "the");
        assert_eq!(match_case("Dont", "don't"), "Don't");
    }
}
