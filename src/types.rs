//! The serialized contract between the engine and its consumers.
//!
//! Field names here are stable — consumers parse them, so renames are
//! breaking changes.

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

/// Which voice a piece of text was written in. A single "writing style" is a
/// fiction — prompts, commits, and email are different registers, and
/// conflating them produces parody. The capture channel labels this for free.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingKind::Unknown => "unknown",
            FindingKind::RealWord => "real-word",
        }
    }
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
    /// 1-based column of the token's first byte.
    pub col: usize,
    /// Ranked replacements, best first. May be empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    /// How sure we are this is *wrong*. Deliberately conservative: a false
    /// "misspelled" trains you to ignore the squiggle, a missed typo costs
    /// almost nothing.
    pub confidence: f32,
}

/// One lexicon entry, as surfaced by `list` / `stats`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub word: String,
    pub provenance: Provenance,
    pub count: i64,
    /// Prior validity from provenance, folded with recurrence.
    pub validity: f32,
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
pub struct StatsPayload {
    pub db: String,
    pub words: i64,
    pub ngrams: i64,
    pub spooled: i64,
    pub by_provenance: Vec<(String, i64)>,
    pub by_register: Vec<(String, i64)>,
}

/// A generic command result, so `--json` works on every command and not just
/// analysis.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StatusPayload {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
