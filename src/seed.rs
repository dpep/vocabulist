//! Seeding the lexicon from ground truth.
//!
//! Waiting for prose frequency to teach us that `contextdb` is a word takes
//! months. But the machine already knows: it's a repo you wrote, a formula you
//! tapped, a binary you installed, a dependency you named. That evidence needs
//! no NLP and no human vetting — if `rubocop` is in your Gemfile, it's a word.
//!
//! Every source carries its own provenance, so the confidence gradient falls
//! out of *how deliberate the install was*: a repo you own is maximally
//! distinctive, a dependency merely names someone else's work.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::store::Store;
use crate::text;
use crate::types::{Provenance, SeedReport, SeedSource};

/// Standard system directories. Their contents (`ls`, `awk`, `sed`) are either
/// already dictionary words or generic Unix vocabulary — they say nothing
/// about *your* diction, which is the whole point of seeding.
const SYSTEM_BIN_DIRS: &[&str] = &["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/libexec"];

/// How deep to hunt for git repos beneath the scan root.
const MAX_REPO_DEPTH: usize = 3;

pub struct SeedOptions {
    /// Directory tree to scan for repos and manifests.
    pub scan_root: PathBuf,
}

impl Default for SeedOptions {
    fn default() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            scan_root: home
                .map(|h| h.join("code"))
                .unwrap_or_else(|| PathBuf::from(".")),
        }
    }
}

/// One source's yield, before dedup across sources.
struct Harvest {
    name: &'static str,
    provenance: Provenance,
    terms: BTreeSet<String>,
    skipped: Option<String>,
}

/// Mine every source and write the results into the lexicon.
pub fn run(store: &Store, opts: &SeedOptions) -> rusqlite::Result<SeedReport> {
    let repos = find_repos(&opts.scan_root);

    let harvests = vec![
        harvest_owned(&repos),
        harvest_tap(),
        harvest_brew(),
        harvest_path_binaries(),
        harvest_dependencies(&repos),
    ];

    // Strongest provenance wins when two sources name the same term, so the
    // insert order can't change the outcome.
    let mut best: HashMap<String, (Provenance, String)> = HashMap::new();
    let mut sources = Vec::new();

    for h in &harvests {
        sources.push(SeedSource {
            name: h.name.to_string(),
            provenance: h.provenance,
            terms: h.terms.len(),
            skipped: h.skipped.clone(),
        });
        for term in &h.terms {
            for word in expand_term(term) {
                let entry = best
                    .entry(word.to_lowercase())
                    .or_insert((h.provenance, word.clone()));
                if h.provenance > entry.0 {
                    *entry = (h.provenance, word.clone());
                }
            }
        }
    }

    let (mut added, mut upgraded) = (0, 0);
    for (word, (provenance, display)) in best {
        let (is_new, was_upgraded) = store.upsert_word(&word, &display, provenance, 0)?;
        added += usize::from(is_new);
        upgraded += usize::from(was_upgraded);
    }

    // General-English frequency: the embedded core first, then real counts
    // mined from prose already on this machine. The core covers the head of
    // the distribution on day one; the mined counts fill in the tail with
    // language from the domain the user actually writes in.
    store.seed_core_frequencies()?;
    let frequency_words = harvest_prose_frequency(store, &repos)?;
    sources.push(SeedSource {
        name: "prose".into(),
        provenance: Provenance::Observed,
        terms: frequency_words,
        skipped: None,
    });

    Ok(SeedReport {
        sources,
        added,
        upgraded,
    })
}

/// Files worth reading for ordinary English, as opposed to code.
const PROSE_FILES: &[&str] = &["README.md", "README", "CONTRIBUTING.md", "CHANGELOG.md"];

/// Cap on markdown files read per repo, so one docs-heavy project doesn't
/// dominate the frequency table.
const MAX_PROSE_FILES_PER_REPO: usize = 60;

/// How deep to look for markdown inside a repo.
const MAX_PROSE_DEPTH: usize = 3;

/// Directories that hold other people's code or generated output. Their
/// markdown says nothing about the vocabulary of this machine's owner, and a
/// single `node_modules` would swamp everything else.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "build",
    "dist",
    ".git",
    "venv",
    "__pycache__",
];

/// Bytes of any one file we'll read. A generated changelog can be enormous
/// and adds nothing after the first few thousand words.
const MAX_PROSE_BYTES: usize = 200_000;

/// Mine word frequencies from prose files in the scanned repos.
///
/// This is not vocabulary harvesting — the words go to the `frequency` table,
/// not the lexicon. The question it answers is "how common is this word in
/// ordinary writing", which is what breaks suggestion ties and makes
/// confusion detection possible before any personal corpus exists.
fn harvest_prose_frequency(store: &Store, repos: &[PathBuf]) -> rusqlite::Result<usize> {
    let mut counts: HashMap<String, i64> = HashMap::new();

    for repo in repos {
        // All the markdown in the repo, not just the root files. This is
        // where the vocabulary a 1934 dictionary lacks actually lives —
        // `textarea` and `bigram` will never be in a general word list, but
        // they're all over the prose on this machine.
        let mut paths: Vec<PathBuf> = PROSE_FILES.iter().map(|n| repo.join(n)).collect();
        paths.extend(find_markdown(repo, MAX_PROSE_DEPTH));
        paths.sort();
        paths.dedup();
        paths.truncate(MAX_PROSE_FILES_PER_REPO);

        for path in paths {
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            let body = &body[..body.len().min(MAX_PROSE_BYTES)];
            for line in body.lines() {
                if !text::is_prose_line(line) {
                    continue;
                }
                let masked = text::mask_non_prose(&text::normalize_typography(line));
                for token in text::tokenize(&masked) {
                    let word = text::normalize(&token.text);
                    if word.chars().count() >= 2
                        && word.chars().all(|c| c.is_ascii_alphabetic() || c == '\'')
                    {
                        *counts.entry(word).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let distinct = counts.len();
    for (word, count) in counts {
        store.bump_frequency(&word, count)?;
    }
    Ok(distinct)
}

/// A term contributes itself plus its parts: `pattern-engine` is a word, and
/// so are `pattern` and `engine`.
fn expand_term(term: &str) -> Vec<String> {
    let mut out = Vec::new();
    let cleaned = term.trim().trim_matches('"');
    if cleaned.is_empty() {
        return out;
    }
    let whole = cleaned.to_lowercase();
    if whole.chars().count() >= 2
        && whole
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        out.push(whole);
    }
    out.extend(text::split_identifier(cleaned));
    out.sort();
    out.dedup();
    out
}

/// Repos you own: the local directory name, plus the org and repo from every
/// remote. Read straight from `.git/config` — 79 repos is 79 subprocesses
/// otherwise, for information already sitting in a file.
fn harvest_owned(repos: &[PathBuf]) -> Harvest {
    let mut terms = BTreeSet::new();
    for repo in repos {
        if let Some(name) = repo.file_name().and_then(|n| n.to_str()) {
            terms.insert(name.to_string());
        }
        let config = repo.join(".git").join("config");
        let Ok(text) = std::fs::read_to_string(&config) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            let Some(url) = line.strip_prefix("url = ") else {
                continue;
            };
            terms.extend(terms_from_remote(url));
        }
    }
    Harvest {
        name: "repos",
        provenance: Provenance::Owned,
        terms,
        skipped: None,
    }
}

/// `git@github.com:dpep/ae.git` / `https://github.com/dpep/ae` → `dpep`, `ae`.
fn terms_from_remote(url: &str) -> Vec<String> {
    let trimmed = url.trim().trim_end_matches(".git");
    let tail = trimmed
        .rsplit_once(':')
        .map(|(_, t)| t)
        .filter(|t| !t.starts_with("//"))
        .unwrap_or(trimmed);
    tail.rsplit('/')
        .take(2)
        .filter(|s| !s.is_empty() && !s.contains('.'))
        .map(|s| s.to_string())
        .collect()
}

/// Formulae in your own Homebrew taps — things you package, not just install.
fn harvest_tap() -> Harvest {
    let mut terms = BTreeSet::new();
    let Some(repository) = brew_repository() else {
        return Harvest {
            name: "tap",
            provenance: Provenance::Tap,
            terms,
            skipped: Some("homebrew not found".into()),
        };
    };

    let taps = repository.join("Library").join("Taps");
    for user in read_dir_names(&taps) {
        // homebrew/core and homebrew/cask are upstream, not yours.
        if user.eq_ignore_ascii_case("homebrew") {
            continue;
        }
        for tap in read_dir_names(&taps.join(&user)) {
            terms.insert(tap.trim_start_matches("homebrew-").to_string());
            for dir in ["Formula", "Casks"] {
                for file in read_dir_names(&taps.join(&user).join(&tap).join(dir)) {
                    if let Some(stem) = file.strip_suffix(".rb") {
                        terms.insert(stem.to_string());
                    }
                }
            }
        }
        terms.insert(user);
    }

    Harvest {
        name: "tap",
        provenance: Provenance::Tap,
        terms,
        skipped: None,
    }
}

/// Everything Homebrew has installed — formulae and casks alike.
fn harvest_brew() -> Harvest {
    let mut terms = BTreeSet::new();
    let mut skipped = None;

    for args in [["list", "--formula"], ["list", "--cask"]] {
        match run_command("brew", &args) {
            Some(out) => terms.extend(out.lines().map(|l| l.trim().to_string())),
            None => skipped = Some("homebrew not found".into()),
        }
    }
    terms.retain(|t| !t.is_empty());

    Harvest {
        name: "brew",
        provenance: Provenance::Installed,
        terms,
        skipped,
    }
}

/// Binaries on `PATH`, skipping the system directories.
fn harvest_path_binaries() -> Harvest {
    let mut terms = BTreeSet::new();
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if SYSTEM_BIN_DIRS.iter().any(|s| dir == Path::new(s)) {
            continue;
        }
        for name in read_dir_names(&dir) {
            if !name.starts_with('.') {
                terms.insert(name);
            }
        }
    }
    Harvest {
        name: "binaries",
        provenance: Provenance::Installed,
        terms,
        skipped: None,
    }
}

/// Dependency names across every manifest in the scanned repos. These are
/// other people's libraries — real working vocabulary, but less distinctive
/// than what you built yourself.
fn harvest_dependencies(repos: &[PathBuf]) -> Harvest {
    let mut terms = BTreeSet::new();
    for repo in repos {
        terms.extend(parse_cargo_toml(&repo.join("Cargo.toml")));
        terms.extend(parse_gemfile(&repo.join("Gemfile")));
        terms.extend(parse_package_json(&repo.join("package.json")));
    }
    Harvest {
        name: "dependencies",
        provenance: Provenance::Dependency,
        terms,
        skipped: None,
    }
}

/// Crate names from `[dependencies]`-ish tables. Line-oriented on purpose —
/// a full TOML parser buys nothing when we only want the keys.
fn parse_cargo_toml(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line.contains("dependencies");
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let key = key.trim().trim_matches('"');
            if !key.is_empty() && !key.contains('.') {
                out.push(key.to_string());
            }
        }
    }
    out
}

/// `gem "rubocop"` / `gem 'rspec', '~> 3.0'`.
fn parse_gemfile(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("gem ")?;
            let rest = rest.trim_start();
            let quote = rest.chars().next()?;
            if quote != '"' && quote != '\'' {
                return None;
            }
            rest[1..].split(quote).next().map(|s| s.to_string())
        })
        .collect()
}

fn parse_package_json(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(map) = json.get(key).and_then(|v| v.as_object()) {
            // Scoped packages (`@scope/name`) contribute both halves.
            out.extend(map.keys().map(|k| k.trim_start_matches('@').to_string()));
        }
    }
    out
}

/// Markdown files within `depth` of `root`, skipping dependency and build
/// directories.
fn find_markdown(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut frontier = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, level)) = frontier.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if path.is_dir() {
                if level < depth && !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                    frontier.push((path, level + 1));
                }
            } else if name.ends_with(".md") {
                out.push(path);
            }
        }
    }
    out
}

/// Directories containing a `.git`, to `MAX_REPO_DEPTH` below `root`.
fn find_repos(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut frontier = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = frontier.pop() {
        if dir.join(".git").is_dir() {
            out.push(dir);
            // Nested repos are rare and submodules aren't ours; stop here.
            continue;
        }
        if depth >= MAX_REPO_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if !hidden && path.is_dir() {
                frontier.push((path, depth + 1));
            }
        }
    }
    out
}

fn brew_repository() -> Option<PathBuf> {
    run_command("brew", &["--repository"]).map(|s| PathBuf::from(s.trim()))
}

fn run_command(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read_dir_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_a_term_into_whole_and_parts() {
        let words = expand_term("pattern-engine");
        assert!(words.contains(&"pattern-engine".to_string()));
        assert!(words.contains(&"pattern".to_string()));
        assert!(words.contains(&"engine".to_string()));
    }

    #[test]
    fn expansion_skips_junk() {
        assert!(expand_term("").is_empty());
        assert!(expand_term("  ").is_empty());
    }

    #[test]
    fn reads_org_and_repo_from_remotes() {
        assert_eq!(
            terms_from_remote("git@github.com:dpep/ae.git"),
            vec!["ae", "dpep"]
        );
        assert_eq!(
            terms_from_remote("https://github.com/dpep/vocabulist"),
            vec!["vocabulist", "dpep"]
        );
    }

    #[test]
    fn parses_cargo_dependency_keys() {
        let dir = std::env::temp_dir().join("vocabulist-test-cargo");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cargo.toml");
        std::fs::write(
            &path,
            "[package]\nname = \"thing\"\n\n[dependencies]\nrusqlite = \"0.32\"\nclap = { version = \"4\" }\n",
        )
        .unwrap();
        let deps = parse_cargo_toml(&path);
        assert!(deps.contains(&"rusqlite".to_string()));
        assert!(deps.contains(&"clap".to_string()));
        // `name` lives under [package], not [dependencies].
        assert!(!deps.contains(&"name".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_gemfile_entries_in_either_quote_style() {
        let dir = std::env::temp_dir().join("vocabulist-test-gemfile");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Gemfile");
        std::fs::write(
            &path,
            "source 'x'\ngem \"rubocop\"\ngem 'rspec', '~> 3.0'\n",
        )
        .unwrap();
        let gems = parse_gemfile(&path);
        assert_eq!(gems, vec!["rubocop", "rspec"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_manifests_are_not_errors() {
        assert!(parse_cargo_toml(Path::new("/nonexistent/Cargo.toml")).is_empty());
        assert!(parse_gemfile(Path::new("/nonexistent/Gemfile")).is_empty());
        assert!(parse_package_json(Path::new("/nonexistent/package.json")).is_empty());
    }
}
