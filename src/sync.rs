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

/// Whether the consuming application is actually pointed at our file.
///
/// Writing a correctly formatted word list is only half an integration: the
/// consumer has to be told the file exists, and nothing fails loudly when it
/// hasn't been. A target sat exported and unread for months because there was
/// no way — for the tool or the person running it — to tell the difference
/// between wired up and written to disk and ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    /// The consumer reads this exact path by definition. Nothing to wire.
    Inherent,
    /// The consumer's own config was found, and it references our file.
    Wired,
    /// Our file is on disk and nothing points at it. This is the silent one.
    Inert,
    /// Needs a step whose result we cannot see — a cloud import, or a
    /// consumer that isn't installed on this machine.
    Manual,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Inherent => "inherent",
            Activation::Wired => "wired",
            Activation::Inert => "inert",
            Activation::Manual => "manual",
        }
    }

    /// Is the export actually reaching the consumer?
    pub fn live(self) -> bool {
        matches!(self, Activation::Inherent | Activation::Wired)
    }
}

/// A verdict on one target, and the evidence behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Wiring {
    pub state: Activation,
    /// The consumer config we read to decide. Absent when there was none to
    /// read — which is itself why the verdict is what it is.
    pub config: Option<PathBuf>,
    /// What the user has to do, when there is anything. The snippet alone —
    /// where it goes is `config`, so a consumer gets the two separately.
    pub hint: Option<String>,
}

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
    /// Most words the destination will accept, if it says so. `None` for the
    /// files we simply write.
    pub limit: Option<usize>,
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
    /// Whether the words just written will reach anything. Reported with the
    /// write, because that is the moment someone believes the job is done.
    pub activation: Option<Activation>,
    pub hint: Option<String>,
    /// The consumer config the verdict came from — and where a hint goes.
    pub config: Option<String>,
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
        limit: None,
        note: "add to settings.json under cSpell.customDictionaries",
    });

    // A single column of ordinary words is already valid CSV — ours contain no
    // commas or quotes — so this needs no separate writer, only the extension
    // Flow's importer expects.
    //
    // Wispr Flow keeps its dictionary in the cloud and imports from a CSV, so
    // this writes a file for you to hand it rather than one it reads. That
    // makes it the one target `unsync` cannot undo — deleting our CSV does
    // nothing to what Flow already learned.
    //
    // Dictation has more to gain from this than a spell checker does: a
    // checker only has to recognize a word you typed, while dictation has to
    // *choose* it from audio, and a name it has never heard is unrecoverable
    // rather than merely underlined.
    out.push(Target {
        name: "wisprflow",
        path: data_dir().join("wisprflow.csv"),
        manifest: manifest_path("wisprflow"),
        owned: true,
        limit: Some(1000),
        note: "import from Flow's Dictionary tab; deleting this file won't un-teach it",
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
            limit: None,
            note: "restart an app to pick up changes",
        });
    }
    out
}

/// What is on disk for each target — whether it was ever installed, how much
/// of it is ours, and how much the user put there.
///
/// Read-only and tolerant of a missing file: a target that was never synced is
/// a normal state to report, not an error.
pub fn status() -> Vec<crate::types::IntegrationStatus> {
    targets()
        .into_iter()
        .map(|t| {
            let total = read_lines(&t.path).len();
            let wiring = t.wiring();
            crate::types::IntegrationStatus {
                name: t.name.to_string(),
                path: t.path.display().to_string(),
                present: t.path.exists(),
                ours: read_lines(&t.manifest).len(),
                total,
                activation: wiring.state.as_str().to_string(),
                live: wiring.state.live(),
            }
        })
        .collect()
}

/// Editors that consume a cSpell dictionary, and where each keeps its user
/// settings. Several forks share VS Code's layout, and someone running Cursor
/// rather than Code is not a corner case — cSpell is the consumer either way,
/// and which shell it runs in is not our business.
fn cspell_settings_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let editors = ["Code", "Code - Insiders", "VSCodium", "Cursor", "Windsurf"];
    let roots = [
        home.join("Library").join("Application Support"),
        home.join(".config"),
    ];
    roots
        .iter()
        .flat_map(|root| {
            editors
                .iter()
                .map(move |e| root.join(e).join("User").join("settings.json"))
        })
        .filter(|p| p.exists())
        .collect()
}

/// The snippet that actually activates the cSpell target.
///
/// `addWords: false` is the part that isn't obvious. Without it cSpell's own
/// "add to dictionary" quick-fix writes into this file, and the next `vocab
/// sync` regenerates it wholesale — so the word silently disappears. Words
/// added through `vocab add` get permanent provenance instead and survive
/// every sync.
fn cspell_snippet(path: &Path) -> String {
    format!(
        "\"cSpell.customDictionaries\": {{\n  \"vocabulist\": {{\n    \"name\": \"vocabulist\",\n    \"path\": \"{}\",\n    \"addWords\": false\n  }}\n}}",
        path.display()
    )
}

/// Decide the cSpell verdict from a set of candidate config files.
///
/// Split from the `HOME` lookup so it can be tested against a temp dir: the
/// paths are the input, not an ambient fact.
fn cspell_wiring(configs: &[PathBuf], word_list: &Path) -> Wiring {
    if configs.is_empty() {
        return Wiring {
            state: Activation::Manual,
            config: None,
            hint: Some("no VS Code settings found on this machine".into()),
        };
    }
    // Match on the file name rather than the full path: a reference may be
    // written with ~ or ${userHome}, and any mention of our file at all means
    // they wired it.
    let needle = word_list
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let found = configs.iter().find(|c| {
        std::fs::read_to_string(c)
            .map(|text| text.contains(&needle))
            .unwrap_or(false)
    });
    match found {
        Some(config) => Wiring {
            state: Activation::Wired,
            config: Some(config.clone()),
            hint: None,
        },
        None => Wiring {
            state: Activation::Inert,
            config: configs.first().cloned(),
            hint: Some(cspell_snippet(word_list)),
        },
    }
}

impl Target {
    /// Is anything actually reading this file?
    ///
    /// Read-only, and tolerant of every absence: no consumer installed, no
    /// config written yet, an unreadable file. None of those is an error —
    /// each is a different answer to the question, and saying which one is
    /// the entire point.
    pub fn wiring(&self) -> Wiring {
        match self.name {
            // NSSpellChecker reads this exact path. There is no config step
            // to get wrong, which is why this target has always worked.
            "macos" => Wiring {
                state: Activation::Inherent,
                config: None,
                hint: None,
            },
            "vscode" => cspell_wiring(&cspell_settings_paths(), &self.path),
            // Flow keeps its dictionary in the cloud and imports from a CSV,
            // so there is no local config that could reference this file and
            // nothing to check.
            _ => Wiring {
                state: Activation::Manual,
                config: None,
                hint: (!self.note.is_empty()).then(|| self.note.to_string()),
            },
        }
    }
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
/// bloat — the value is entirely in the jargon it *doesn't* have.
///
/// The bar for an observed word is **two distinct documents**, not two
/// occurrences. Repetition inside a single message is worth nothing here: a
/// typo written three times in one prompt is still one mistake, and teaching
/// it to every editor on the machine is the most expensive false positive
/// this tool can produce — it outlives the session and shows up somewhere you
/// won't connect back to us.
pub fn exportable(store: &Store) -> rusqlite::Result<Vec<String>> {
    let dictionary = crate::dict::load();
    let mut out = BTreeSet::new();

    for entry in store.list(None, usize::MAX)? {
        let earned = entry.provenance > Provenance::Observed || entry.sources >= 2;
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
/// The `limit` most-established exportable words, strongest first.
fn strongest(store: &Store, limit: usize) -> rusqlite::Result<Vec<String>> {
    let keep: std::collections::HashSet<String> = exportable(store)?.into_iter().collect();
    let mut ranked: Vec<_> = store
        .list(None, usize::MAX)?
        .into_iter()
        .filter(|e| keep.contains(&e.word))
        .collect();
    ranked.sort_by(|a, b| {
        b.validity
            .partial_cmp(&a.validity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.sources.cmp(&a.sources))
            .then(a.word.cmp(&b.word))
    });
    ranked.truncate(limit);
    Ok(ranked.into_iter().map(|e| e.word).collect())
}

pub fn install(
    store: &Store,
    target: &Target,
    dry_run: bool,
) -> Result<SyncReport, Box<dyn std::error::Error>> {
    let mut words = exportable(store)?;
    if let Some(limit) = target.limit
        && words.len() > limit
    {
        // Strongest first, so a cap keeps the words most worth teaching
        // rather than the alphabetically luckiest.
        words = strongest(store, limit)?;
    }
    let wiring = target.wiring();
    let mut report = SyncReport {
        target: target.name.to_string(),
        path: target.path.display().to_string(),
        total: words.len(),
        activation: Some(wiring.state),
        hint: wiring.hint,
        config: wiring.config.map(|c| c.display().to_string()),
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
            limit: None,
            note: "",
        }
    }

    #[test]
    fn a_config_that_names_our_file_counts_as_wired() {
        let dir = scratch("wired");
        let settings = dir.join("settings.json");
        // ${userHome} rather than a literal path, which is why the check
        // matches on the file name and not the full path.
        std::fs::write(
            &settings,
            r#"{ "cSpell.customDictionaries": { "vocabulist": {
                 "path": "${userHome}/.local/share/vocabulist/vocabulist.txt" } } }"#,
        )
        .unwrap();
        let w = cspell_wiring(&[settings], &dir.join("vocabulist.txt"));
        assert_eq!(w.state, Activation::Wired);
        assert!(w.state.live());
    }

    #[test]
    fn a_config_that_ignores_our_file_is_inert_not_absent() {
        // The failure this exists for: a correct word list on disk that
        // nothing reads, reporting exactly like one that works.
        let dir = scratch("inert");
        let settings = dir.join("settings.json");
        std::fs::write(&settings, r#"{ "cSpell.userWords": ["zblorg"] }"#).unwrap();
        let w = cspell_wiring(&[settings], &dir.join("vocabulist.txt"));
        assert_eq!(w.state, Activation::Inert);
        assert!(!w.state.live());
        let hint = w.hint.unwrap();
        assert!(hint.contains("cSpell.customDictionaries"));
        // Without this cSpell writes into a file the next sync regenerates.
        assert!(hint.contains("\"addWords\": false"));
    }

    #[test]
    fn no_editor_installed_is_not_a_failed_wiring() {
        let w = cspell_wiring(&[], Path::new("/tmp/vocabulist.txt"));
        assert_eq!(w.state, Activation::Manual);
    }

    #[test]
    fn the_macos_target_needs_no_wiring() {
        let target = Target {
            name: "macos",
            path: PathBuf::from("/tmp/LocalDictionary"),
            manifest: PathBuf::from("/tmp/manifest.txt"),
            owned: false,
            limit: None,
            note: "",
        };
        assert_eq!(target.wiring().state, Activation::Inherent);
        assert!(target.wiring().state.live());
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
            limit: None,
            note: "",
        };
        let report = uninstall(&target, false).unwrap();
        assert!(report.skipped.is_some());
    }

    #[test]
    fn a_capped_target_keeps_the_strongest_words() {
        let store = Store::open(":memory:").unwrap();
        // Deliberate provenance outranks merely-observed corroboration.
        store
            .upsert_word("polyid", "polyid", Provenance::Owned, 1)
            .unwrap();
        for doc in ["a", "b"] {
            store
                .upsert_word("zblorg", "zblorg", Provenance::Observed, 1)
                .unwrap();
            store.record_word_source("zblorg", doc).unwrap();
        }

        let dir = scratch("capped");
        let target = Target {
            name: "capped",
            path: dir.join("out.csv"),
            manifest: dir.join("manifest.txt"),
            owned: true,
            limit: Some(1),
            note: "",
        };
        let report = install(&store, &target, false).unwrap();
        assert_eq!(report.total, 1);

        let written = std::fs::read_to_string(&target.path).unwrap();
        assert_eq!(written.trim(), "polyid", "the cap must keep the strongest");
    }

    #[test]
    fn vscode_target_is_a_file_we_own() {
        let vscode = find_target("vscode").unwrap();
        assert!(vscode.owned);
    }
}
