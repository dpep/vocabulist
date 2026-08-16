//! The serialized contract between the engine and its consumers.
//!
//! Field names here are stable — consumers parse them, so renames are
//! breaking changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a word came from. Ordered by how deliberate the evidence is: a repo
/// you wrote outranks a formula you installed, which outranks a word merely
/// seen in prose. This is the *validity* axis — "is this a word?" — kept
/// separate from register fit, which answers "is it a word *here*?".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Seen in prose and nothing more. Earns trust only by recurring.
    Observed,
    /// Named in a dependency manifest (Cargo.toml, Gemfile, package.json).
    Dependency,
    /// An installed binary, Homebrew formula, or cask on this machine.
    Installed,
    /// A formula in your own Homebrew tap.
    Tap,
    /// A repo you own — maximally distinctive; nobody else's lexicon has it.
    Owned,
    /// A human typed it. Top of the continuum, never pruned.
    User,
}

impl Provenance {
    /// Prior probability that this is a legitimate word, by source.
    pub fn validity(self) -> f32 {
        match self {
            Provenance::Observed => 0.30,
            Provenance::Dependency => 0.70,
            Provenance::Installed => 0.80,
            Provenance::Tap => 0.90,
            Provenance::Owned => 0.95,
            Provenance::User => 1.00,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Observed => "observed",
            Provenance::Dependency => "dependency",
            Provenance::Installed => "installed",
            Provenance::Tap => "tap",
            Provenance::Owned => "owned",
            Provenance::User => "user",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "observed" => Provenance::Observed,
            "dependency" => Provenance::Dependency,
            "installed" => Provenance::Installed,
            "tap" => Provenance::Tap,
            "owned" => Provenance::Owned,
            "user" => Provenance::User,
            _ => return None,
        })
    }
}

/// Whether an entry is ordinary English or a name.
///
/// A second axis from [`Provenance`], which says *where we learned a word*
/// rather than *what sort of thing it is*. Conflating them has costs:
/// `polyid` could be offered as the correction for a mistyped ordinary word,
/// and `contextdb` inflates lexical-diversity numbers that are supposed to
/// describe vocabulary rather than the number of projects you have.
///
/// Two values rather than three. Splitting names from project jargon is the
/// distinction a *shared* team vocabulary would need — jargon is shareable,
/// colleagues' names are not — but that boundary is genuinely fuzzy (`dpep`
/// is a handle, `polyid` a project, `rubocop` both a tool and a proper noun)
/// and there's nothing yet to test a guess against.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Ordinary English — in a dictionary, or used as a common word.
    Word,
    /// A name or piece of jargon: a project, tool, handle, or person.
    Name,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Word => "word",
            Kind::Name => "name",
        }
    }
}

/// Which voice a piece of text was written in. A single "writing style" is a
/// fiction — prompts, commits, and email are different registers, and
/// conflating them produces parody. The capture channel labels this for free.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Register {
    Prompt,
    Slack,
    Email,
    Commit,
    Pr,
    Doc,
    Code,
    Other,
}

impl Register {
    pub fn as_str(self) -> &'static str {
        match self {
            Register::Prompt => "prompt",
            Register::Slack => "slack",
            Register::Email => "email",
            Register::Commit => "commit",
            Register::Pr => "pr",
            Register::Doc => "doc",
            Register::Code => "code",
            Register::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "prompt" => Register::Prompt,
            "slack" => Register::Slack,
            "email" => Register::Email,
            "commit" => Register::Commit,
            "pr" => Register::Pr,
            "doc" => Register::Doc,
            "code" => Register::Code,
            "other" => Register::Other,
            _ => return None,
        })
    }
}

/// What kind of problem a finding reports.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FindingKind {
    /// Not in the lexicon and not in the backstop dictionary.
    Unknown,
    /// A correctly-spelled word that looks wrong *here* — `form` for `from`.
    /// A dictionary can never catch these; only collocation evidence can.
    RealWord,
    /// A contraction typed without its apostrophe — `dont` for `don't`.
    Contraction,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::Unknown => "unknown",
            FindingKind::RealWord => "real-word",
            FindingKind::Contraction => "contraction",
        }
    }
}

/// One candidate correction, with its share of the belief.
///
/// Separate from the finding's own confidence because they answer different
/// questions: the finding says *how sure we are the word is wrong*, and this
/// says *which word was probably meant*. Collapsing them meant `hepl` offered
/// `help`, `hep`, and `heal` as equals, when `help` is overwhelmingly likelier.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub word: String,
    /// Share of the probability mass across the candidates offered, so they
    /// sum to 1. A lone suggestion scores 1.0 — certainty about the ranking,
    /// not about the correction being needed at all.
    pub score: f32,
}

/// One flagged word. The unit of structured output: `-j` is a pretty array,
/// `-J` is one object per line, and the shape is identical whether the input
/// was a single blob or a streamed line.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Finding {
    pub kind: FindingKind,
    /// The token exactly as it appeared.
    pub word: String,
    /// 1-based line number; 1 for a single-blob analysis.
    pub line: usize,
    /// 1-based **character** column — see `text::Token::col` for why not bytes.
    pub col: usize,
    /// Ranked replacements, best first. May be empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Suggestion>,
    /// How sure we are this word is *wrong* — independent of which
    /// replacement is right. Deliberately conservative: a false "misspelled"
    /// trains you to ignore the squiggle, a missed typo costs almost nothing.
    pub confidence: f32,
}

/// One lexicon entry, as surfaced by `list` / `stats`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub word: String,
    pub provenance: Provenance,
    /// Total occurrences.
    pub count: i64,
    /// Distinct documents this word appeared in. The evidence that matters —
    /// see [`crate::store::validity`].
    #[serde(default)]
    pub sources: i64,
    /// Prior validity from provenance, folded with corroboration.
    pub validity: f32,
    /// Ordinary English, or a name/jargon term.
    #[serde(default = "default_kind")]
    pub kind: Kind,
}

fn default_kind() -> Kind {
    Kind::Word
}

/// What one `seed` run found, per source.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SeedReport {
    pub sources: Vec<SeedSource>,
    /// Distinct words added to the lexicon by this run.
    pub added: usize,
    /// Words already present whose provenance this run upgraded.
    pub upgraded: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeedSource {
    pub name: String,
    pub provenance: Provenance,
    /// Distinct terms this source contributed (before dedup across sources).
    pub terms: usize,
    /// Set when the source was unavailable on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// Store-wide counts, for `stats`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct StoreStatus {
    pub db: String,
    /// How long ago the lexicon was seeded from ground truth. `None` means
    /// never, which on a live store means auto-seeding has not run yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeded_secs_ago: Option<i64>,
    pub words: i64,
    pub ngrams: i64,
    pub spooled: i64,
    /// Keyed maps rather than arrays of pairs: `{"installed": 640}` is what a
    /// JSON consumer expects, and `[["installed", 640]]` would have been
    /// awkward to fix once anyone scripted against it.
    pub by_provenance: BTreeMap<String, i64>,
    pub by_register: BTreeMap<String, i64>,
    /// Bodies processed per register — how much has been read, as opposed to
    /// how many distinct words came out of it.
    #[serde(default)]
    pub documents: BTreeMap<String, i64>,
    /// Messages captured per origin, from the dedup table's key prefixes.
    #[serde(default)]
    pub messages: BTreeMap<String, i64>,
    /// The spell checkers this lexicon has been exported into.
    #[serde(default)]
    pub integrations: Vec<IntegrationStatus>,
}

/// One export target, and what is actually on disk for it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct IntegrationStatus {
    pub name: String,
    pub path: String,
    /// Whether the target's word list exists at all.
    pub present: bool,
    /// Words this tool wrote, per its own manifest.
    pub ours: usize,
    /// Words in the file altogether. Larger than `ours` for a shared file the
    /// user also adds to by hand, which is the case `unsync` must respect.
    pub total: usize,
}

/// Round to `places` decimals, for numbers that cross the output boundary.
///
/// Anything derived from a handful of counts has a few significant figures at
/// most, and printing seventeen claims a precision the model does not have. Round
/// where the value is built, not where it is displayed, or the JSON keeps the
/// noise the human output was careful to hide.
pub fn round(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    (value * factor).round() / factor
}

/// A generic command result, so `--json` works on every command and not just
/// analysis.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StatusPayload {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
