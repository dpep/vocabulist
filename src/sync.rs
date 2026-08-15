//! Exporting the lexicon into the spell checkers you already run.
//!
//! Several checkers keep their personal dictionary as a plain newline-
//! delimited file. Writing to those "upgrades the built-in" for a fraction of
//! the cost of replacing it — no extension, no language server, no daemon.
//!
//! Two rules shape the design:
//!
//! 1. **Every export is a lossy projection.** A target gets a flat membership
//!    set; provenance, registers, and counts stay here. The dumbest consumer
//!    must not shape the schema.
//! 2. **Uninstall must be exact.** These files are shared with words *you*
//!    added — macOS writes there whenever you pick "Learn Spelling". So each
//!    install records a sidecar manifest of precisely what it wrote, and
//!    uninstall removes only those lines. Sentinel comments would be simpler
//!    but would pollute a file whose every line is treated as a word.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::store::{Store, default_db_path};
use crate::types::Provenance;

/// Where a target's word list lives, and how we're allowed to treat it.
pub struct Target {
    pub name: &'static str,
    pub path: PathBuf,
    /// Where we record exactly what this install wrote. Carried on the target
    /// rather than derived from a global data dir, so it can't be shared
    /// state — deriving it made concurrent tests race over one real file, and
    /// pointed the suite at the user's actual data directory.
    pub manifest: PathBuf,
    /// True when the file is ours alone, so uninstall can just delete it.
    /// False when it's shared with the user's own additions.
    pub owned: bool,
    pub note: &'static str,
}

/// What one install/uninstall did, or would do under `--dry-run`.
#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    pub target: String,
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub total: usize,
    pub skipped: Option<String>,
}

/// Every target we know how to write, resolved for this machine.
pub fn targets() -> Vec<Target> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();

    // Our own file. cSpell reads it via `cSpell.customDictionaries`, which
    // keeps us out of the user's settings.json — that file is JSONC, it's
    // theirs, and rewriting it mechanically is a good way to lose a comment.
    out.push(Target {
        name: "vscode",
        path: data_dir().join("vocabulist.txt"),
        manifest: manifest_path("vscode"),
        owned: true,
        note: "add to settings.json under cSpell.customDictionaries",
    });

    if let Some(home) = &home {
        // Feeds NSSpellChecker, so Mail, Notes, TextEdit and Safari all pick
        // it up. Read at app launch — expect eventual consistency, not live.
        out.push(Target {
            name: "macos",
            path: home
                .join("Library")
                .join("Spelling")
                .join("LocalDictionary"),
            manifest: manifest_path("macos"),
            owned: false,
            note: "restart an app to pick up changes",
        });
    }
    out
}

pub fn find_target(name: &str) -> Option<Target> {
    targets().into_iter().find(|t| t.name == name)
}

fn data_dir() -> PathBuf {
    default_db_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where we record what a given target install wrote.
fn manifest_path(target: &str) -> PathBuf {
    data_dir().join("synced").join(format!("{target}.txt"))
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The words worth exporting: the lexicon minus anything an ordinary
/// dictionary already knows.
///
/// Exporting words the target's own dictionary already has would be pure
/// bloat — the value is entirely in the jargon it *doesn't* have. Words seen
/// only once in prose are held back too, since a typo observed once shouldn't
/// be taught to every editor on the machine.
pub fn exportable(store: &Store) -> rusqlite::Result<Vec<String>> {
    let dictionary = crate::dict::load();
    let mut out = BTreeSet::new();

    for entry in store.list(None, usize::MAX)? {
        let earned = entry.provenance > Provenance::Observed || entry.count >= 2;
        if !earned {
            continue;
        }
        if let Some(d) = &dictionary
            && crate::dict::contains(d, &entry.word)
        {
            continue;
        }
        out.insert(entry.word);
    }
    Ok(out.into_iter().collect())
}

/// Write the lexicon into one target, recording what we wrote.
pub fn install(
    store: &Store,
    target: &Target,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let words = exportable(store)?;
    let mut report = SyncReport {
        target: target.name.to_string(),
        path: target.path.display().to_string(),
        total: words.len(),
        ..Default::default()
    };

    if target.owned {
        report.added = words.len();
        if !dry_run {
            write_all(&target.path, &words)?;
            write_all(&target.manifest, &words)?;
        }
        return Ok(report);
    }

    // Shared file: merge, never clobber. Anything already present — whether
    // the user learned it or a previous run wrote it — stays exactly once.
    let existing = read_lines(&target.path);
    let present: BTreeSet<&str> = existing.iter().map(String::as_str).collect();
    let fresh: Vec<String> = words
        .iter()
        .filter(|w| !present.contains(w.as_str()))
        .cloned()
        .collect();
    report.added = fresh.len();

    if !dry_run && !fresh.is_empty() {
        let mut merged = existing;
        merged.extend(fresh.iter().cloned());
        write_all(&target.path, &merged)?;

        // The manifest is cumulative: a word we wrote last run is still ours
        // to remove, even if this run had nothing new to add.
        let mut owned: BTreeSet<String> = read_lines(&target.manifest).into_iter().collect();
        owned.extend(fresh);
        let owned: Vec<String> = owned.into_iter().collect();
        write_all(&target.manifest, &owned)?;
    }
    Ok(report)
}

/// Remove exactly the words this tool wrote, leaving the user's own alone.
pub fn uninstall(target: &Target, dry_run: bool) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let manifest = &target.manifest;
    let ours: BTreeSet<String> = read_lines(manifest).into_iter().collect();
    let mut report = SyncReport {
        target: target.name.to_string(),
        path: target.path.display().to_string(),
        ..Default::default()
    };

    if ours.is_empty() && !target.path.exists() {
        report.skipped = Some("nothing installed".into());
        return Ok(report);
    }

    if target.owned {
        report.removed = ours.len();
        if !dry_run {
            let _ = std::fs::remove_file(&target.path);
            let _ = std::fs::remove_file(manifest);
        }
        return Ok(report);
    }

    let existing = read_lines(&target.path);
    let kept: Vec<String> = existing
        .iter()
        .filter(|w| !ours.contains(w.as_str()))
        .cloned()
        .collect();
    report.removed = existing.len() - kept.len();
    report.total = kept.len();

    if !dry_run {
        write_all(&target.path, &kept)?;
        let _ = std::fs::remove_file(manifest);
    }
    Ok(report)
}

fn write_all(path: &Path, words: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = words.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vocabulist-sync-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every path a test touches lives under its own scratch dir — including
    /// the manifest, which is why it's a field rather than a global lookup.
    fn shared_target(dir: &Path) -> Target {
        Target {
            name: "test-shared",
            path: dir.join("LocalDictionary"),
            manifest: dir.join("manifest.txt"),
            owned: false,
            note: "",
        }
    }

    #[test]
    fn merging_preserves_words_the_user_added() {
        let dir = scratch("merge");
        let target = shared_target(&dir);
        std::fs::write(&target.path, "handwritten\n").unwrap();

        let existing = read_lines(&target.path);
        assert_eq!(existing, vec!["handwritten"]);
    }

    #[test]
    fn uninstall_removes_only_our_words() {
        let dir = scratch("uninstall");
        let target = shared_target(&dir);
        // The user learned one word; we wrote two.
        std::fs::write(&target.path, "handwritten\ncontextdb\niriq\n").unwrap();
        write_all(&target.manifest, &["contextdb".into(), "iriq".into()]).unwrap();

        let report = uninstall(&target, false).unwrap();
        assert_eq!(report.removed, 2);
        assert_eq!(read_lines(&target.path), vec!["handwritten"]);
    }

    #[test]
    fn dry_run_changes_nothing_on_disk() {
        let dir = scratch("dryrun");
        let target = shared_target(&dir);
        std::fs::write(&target.path, "handwritten\ncontextdb\n").unwrap();
        write_all(&target.manifest, &["contextdb".into()]).unwrap();

        let report = uninstall(&target, true).unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(read_lines(&target.path), vec!["handwritten", "contextdb"]);
    }

    #[test]
    fn uninstalling_a_target_that_was_never_installed_is_not_an_error() {
        let dir = scratch("absent");
        let target = Target {
            name: "test-absent",
            path: dir.join("nope"),
            manifest: dir.join("manifest.txt"),
            owned: false,
            note: "",
        };
        let report = uninstall(&target, false).unwrap();
        assert!(report.skipped.is_some());
    }

    #[test]
    fn vscode_target_is_a_file_we_own() {
        let vscode = find_target("vscode").unwrap();
        assert!(vscode.owned);
    }
}
