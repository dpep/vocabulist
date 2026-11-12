//! Command-line surface: argument parsing, input resolution, dispatch.
//!
//! stdout is reserved for data; all logging goes to stderr so a consumer
//! piping `vocab` always gets clean output.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

use clap::{CommandFactory, Parser, Subcommand};

use crate::check::Checker;
use crate::profile::Profile;
use crate::store::{Store, default_db_path};
use crate::types::{Finding, Fix, Register, Rewrite};
use crate::{ngram, output, seed, sync, text, watermark};

const AFTER_HELP: &str = "\
The lexicon is yours: words you've used, tools you've installed, repos you own.
An ordinary dictionary sits underneath it as a backstop, never as the authority.

Findings come in three kinds:
  unknown       not in your lexicon and not in the backstop dictionary
  contraction   an apostrophe left out -- `dont` for `don't`
  real-word     spelled fine, wrong here -- `form` for `from`, caught by the
                company the word keeps rather than by any dictionary

Each finding scores how sure we are the word is *wrong*; each suggestion
carries its own share of \"this is what you meant\".

Checking is deliberately reluctant: a false alarm teaches you to ignore the
tool, a missed typo costs almost nothing. -j/--json and -J/--ndjson switch to
machine output on every command; -J streams a finding per line as it is read.

Exit codes (1 reads two ways, by the command's job):
  checking   0 clean, 1 findings          — drops into a hook or CI unwrapped
  querying   0 a result, 1 none           — grep's convention, so `&&` reads
  any        2 an operational error       — never a verdict

Examples:
  vocab \"ship the small change\"     check one string
  cat notes.md | vocab -J          stream stdin as NDJSON
  vocab list rubo                  lexicon entries matching \"rubo\"
  vocab typos                      your habitual typos, and what each means
  vocab shorthand repro reproduce  say `repro` is yours, and what it means
  vocab --fix -f notes.md          write it back with typos and apostrophes fixed
  vocab --fix --expand -f mail.txt  ...and your shorthands spelled out
  vocab sync                       export into cSpell, macOS and Wispr Flow
  vocab phrases                    the phrases you actually use
  vocab analyze --lexicon          vocabulary and readability of your corpus
  vocab self                       the handles believed to be yours

The lexicon seeds itself on first use; `vocab seed` re-runs it by hand.";

/// The shell to generate completions for, when `--completions` was given
/// without one.
///
/// `$SHELL` is a path — /bin/zsh, /opt/homebrew/bin/fish — so the basename is
/// the name to parse. `None` when it is unset or names a shell clap_complete
/// has no generator for.
fn shell_from_env() -> Option<clap_complete::Shell> {
    std::env::var("SHELL")
        .ok()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .and_then(|name| name.parse().ok())
}

#[derive(Parser, Debug)]
#[command(disable_help_subcommand = true)]
#[command(group = clap::ArgGroup::new("rewrite").multiple(true))]
#[command(
    name = "vocab",
    author,
    version,
    about = "A live personal dictionary — learned from the words you actually use.",
    after_help = AFTER_HELP
)]
pub struct Cli {
    /// Lexicon subcommand. Omit to check text (the default).
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Text to check. Optional when piping input via stdin.
    pub text: Option<String>,

    /// JSON output: a pretty array of findings, or an object for commands.
    #[arg(short = 'j', long, global = true, conflicts_with = "ndjson")]
    pub json: bool,

    /// NDJSON output: one compact object per finding.
    #[arg(short = 'J', long, global = true)]
    pub ndjson: bool,

    /// Read input from this file, checked line by line (like piped stdin).
    /// Not with text as well: which one to read would be a guess.
    #[arg(short, long, global = true)]
    pub file: Option<PathBuf>,

    /// Write the text back with corrections applied, instead of reporting
    /// findings.
    ///
    /// A missing apostrophe (`dont`), a typo you confirmed with `vocab typos`,
    /// and one inferred: a swap of two letters in a word you write far more
    /// often, or, for a slip never seen, in a common word the checker's
    /// suggestion all but settles. Candidates are suggested, never written; `vocab
    /// add` a word to keep `--fix` off it. Code is left alone: code spans,
    /// fenced blocks, HTML `<code>`, `<kbd>`, `<pre>` and `<samp>`, URLs, and
    /// a word joined to code (`teh.rs`, `$teh`, `teh()`). An ALLCAPS word
    /// reads as an acronym and is never checked. The typed case is kept
    /// (`Teh` -> `The`).
    /// The text goes to stdout, line by line as it is read; what changed goes
    /// to stderr, in the -j/-J format when one is given. Exits 1 when anything
    /// changed.
    #[arg(long, group = "rewrite")]
    pub fix: bool,

    /// Write the text back with your shorthands spelled out: `repro` ->
    /// `reproduce`.
    ///
    /// Only what you confirmed with `vocab shorthand`, and an inflection of
    /// it that reads one way (`repro'd` -> `reproduced`). A form whose
    /// meaning would be a guess — `repros`, reproduces or reproductions? — is
    /// left alone and reported. Otherwise it is `--fix`: the same regions
    /// skipped, case kept, text on stdout as it is read, the report on
    /// stderr. `--fix --expand` does both.
    #[arg(long, group = "rewrite")]
    pub expand: bool,

    /// With --fix or --expand, rewrite --file in place rather than printing
    /// it.
    ///
    /// The file is replaced atomically, so a failed write leaves it whole. A
    /// symlink is followed and permissions are kept; a read-only file is
    /// refused.
    #[arg(long, requires_all = ["rewrite", "file"])]
    pub in_place: bool,

    /// Lexicon database. Defaults to `$XDG_DATA_HOME/vocabulist/lexicon.db`.
    #[arg(long, env = "VOCAB_DB", global = true)]
    pub db: Option<PathBuf>,

    /// Emit telemetry to stderr.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Report phase timings and work counters to stderr. Never touches stdout,
    /// so a profiled run still pipes cleanly.
    #[arg(long, global = true)]
    pub profile: bool,

    /// Suppress stdout (the work still happens).
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Print a shell completion script. Defaults to the shell you're running.
    ///
    /// Naming a shell is the exception: usually you want the script for the
    /// one you are sitting in, which is read from `$SHELL`.
    // The outer Option is "was the flag given", the inner one "was a shell
    // named" — a plain comment, because doc comments here are user-facing.
    #[arg(long, value_name = "SHELL", num_args = 0..=1)]
    pub completions: Option<Option<clap_complete::Shell>>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Seed the lexicon from ground truth: repos you own, your Homebrew taps,
    /// installed binaries, and dependency manifests.
    Seed {
        /// Directory tree to scan for repos and manifests.
        #[arg(long)]
        scan_root: Option<PathBuf>,
    },
    /// Add words by hand. These carry top provenance and are never pruned.
    Add {
        #[arg(required = true)]
        words: Vec<String>,
    },
    /// Remove a word from the lexicon.
    Rm {
        /// The word, or with `--phrase`, the whole phrase.
        word: Vec<String>,
        /// Remove a phrase from the collocation tables instead of a word.
        ///
        /// Phrases have no other removal path: they are derived counts, and
        /// the prose they came from was dropped. `vocab prune` handles what a
        /// rule can recognize; this is for what only a reader can.
        #[arg(long)]
        phrase: bool,
    },
    /// List lexicon entries, strongest first.
    List {
        filter: Option<String>,
        /// How many to show. Defaults to 50, or to all of them with
        /// `--trusted`.
        #[arg(long)]
        limit: Option<usize>,
        /// Only words that have earned trust: a deliberate source, or two
        /// independent documents and not a habitual typo of a word you write
        /// far more often. Ordinary English words included.
        ///
        /// With `--uncommon`, exactly what `vocab sync` exports. For tools
        /// that want your vocabulary — read this rather than re-deriving the
        /// rule.
        #[arg(long)]
        trusted: bool,
        /// Drop words an ordinary dictionary already knows, leaving jargon,
        /// names, and coinages.
        #[arg(long)]
        uncommon: bool,
    },
    /// Your habitual typos and the word each one means — learned from the
    /// counts, or confirmed by hand.
    ///
    /// With no words, lists them with their tier: `confirmed` and `inferred`
    /// pairs are what `--fix` writes back, a `candidate` is only suggested,
    /// and says what held it back (`edit`: not a swap of two letters;
    /// `short`: three letters; `variant`: a British spelling).
    /// `vocab typos teh the` confirms a pair: it is then always a typo,
    /// whatever the counts say. To say a word is real instead, `vocab add` it.
    Typos {
        /// A typo and the word it means, to confirm; with `--rm`, just the
        /// typo.
        #[arg(value_name = "TYPO [INTENDED]")]
        words: Vec<String>,
        /// Forget a confirmed pair instead of adding one.
        #[arg(long)]
        rm: bool,
    },
    /// Your shorthands — `repro` for reproduce — and what each one means.
    ///
    /// A shorthand is never flagged and never touched by `--fix`, and with
    /// `--expand` it is written out in full. With no words, lists them.
    /// `vocab shorthand repro reproduce` confirms one; `repros=reproductions`
    /// confirms an inflection the rule would get wrong or leave alone.
    /// Otherwise `repro'd`, `reproed` and `reproing` read as reproduced and
    /// reproducing, and an -s reads as a plural only when the meaning is not
    /// also a verb: `args` is arguments, but `repros` stays a guess, since
    /// "it repros on main" and "the repros" mean different things. A
    /// dictionary word, or one from a deliberate source, is refused, as by
    /// `vocab typos`.
    Shorthand {
        /// A shorthand and what it means, or any number of `SHORT=MEANING`;
        /// with `--rm`, just the shorthands.
        #[arg(value_name = "SHORT [MEANING]")]
        words: Vec<String>,
        /// Forget the named shorthands instead of confirming them.
        #[arg(long)]
        rm: bool,
    },
    /// Show store-wide counts, what has been read, and where it exports to.
    Status,

    /// Remove phrases and words that today's capture rules would reject —
    /// session ids, path fragments, and anything else learned under a since
    /// fixed bug.
    Prune {
        /// Report what would go without removing it.
        #[arg(long)]
        dry_run: bool,
        /// Also remove phrases containing words nothing vouches for. Reaches
        /// residue that shape cannot, and takes your own coinages with it —
        /// read `--dry-run` first.
        #[arg(long)]
        strict: bool,
    },

    /// Find boilerplate learned as prose — a repeated verbatim run of words
    /// is something a program emits, not something a person types.
    Templates {
        /// Ignore runs recurring fewer times than this.
        #[arg(long, default_value_t = 5)]
        min_repeats: i64,
        /// Ignore runs shorter than this many words.
        #[arg(long, default_value_t = 12)]
        min_length: usize,
        /// Remove every phrase the listed runs contain. Read the scan first —
        /// there is no undo, and a run you keep costs nothing.
        #[arg(long)]
        apply: bool,
    },

    /// Explain a command or an option — `vocab help --completions` as well as
    /// `vocab help status`.
    Help {
        /// A subcommand, or an option named with or without its dashes.
        ///
        /// Global options are the exception and want the bare name — `vocab
        /// help json`, not `vocab help --json` — because with the dashes clap
        /// sees a flag that really is valid here and consumes it.
        // allow_hyphen_values or `--completions` is read as a flag on `help`.
        #[arg(allow_hyphen_values = true)]
        topic: Option<String>,
    },
    /// Stage text for learning. Assistant-authored text is recorded but never
    /// learned from.
    Capture {
        text: Option<String>,
        /// Which voice this text is in.
        #[arg(short, long, default_value = "other")]
        register: Register,
        /// Where it came from, for provenance.
        #[arg(short, long)]
        source: Option<String>,
    },
    /// Fold staged text into counts, then drop the prose.
    Process {
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Export the lexicon into the spell checkers you already run.
    Sync {
        /// Limit to one target. Defaults to all of them.
        #[arg(short, long)]
        target: Option<String>,
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// List the targets and where each one writes.
        #[arg(long)]
        list: bool,
    },
    /// Measure vocabulary and linguistic complexity — for a text, or for
    /// everything captured so far.
    Analyze {
        /// Text to analyze. Omit with --lexicon, or pipe via stdin.
        text: Option<String>,
        /// Analyze the accumulated corpus instead of a text.
        #[arg(long)]
        lexicon: bool,
        /// Limit corpus analysis to one register (one voice you write in).
        #[arg(short, long)]
        register: Option<Register>,
    },
    /// Bulk-load text from JSON on stdin — NDJSON or an array of
    /// `{body, author?, register?, source?}`.
    ///
    /// Text attributed to someone else corroborates that a word is real but
    /// never shapes your voice.
    Ingest {
        /// A handle that is you. Repeatable; matched case-insensitively.
        /// Records with no author are treated as yours.
        #[arg(long = "self", value_name = "HANDLE")]
        selves: Vec<String>,
        /// Register for records that don't name one.
        #[arg(short, long, default_value = "other")]
        register: Register,
    },
    /// Measure the checker against a labeled corpus: corrupt known-good prose
    /// in known places, then score what gets caught and what gets flagged
    /// wrongly.
    Eval {
        /// Corpus to evaluate against. Defaults to stdin.
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Corrupt roughly one prose line in this many.
        #[arg(long, default_value_t = 4)]
        rate: usize,
        /// Seed, so a run is reproducible and two runs are comparable.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Inject only this class of error. Unrestricted sampling barely
        /// produces real-word errors, so measuring them needs targeting.
        #[arg(long, value_name = "KIND")]
        kind: Option<crate::eval::ErrorKind>,
    },
    /// Rank the phrases you actually use, by association strength rather than
    /// raw frequency.
    Phrases {
        /// Limit to one register.
        #[arg(short, long)]
        register: Option<Register>,
        /// Ignore pairings seen fewer than this many times.
        #[arg(long, default_value_t = 2)]
        min_count: i64,
        /// How many words per phrase. Two is the pair you say together;
        /// longer ones are tracked only once the shorter one has recurred, so
        /// a five-word phrase here is one you genuinely repeat.
        #[arg(short = 'n', long, value_name = "N", default_value_t = 2,
              value_parser = clap::value_parser!(u16).range(2..=5))]
        words: u16,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Manage the handles that identify you — a GitHub login, a Slack user
    /// ID. Reading a channel surfaces everyone; these say which messages are
    /// yours, and capture from reads stays off until there is one. Seeding
    /// detects them from git config and `gh`; add any it missed.
    #[command(name = "self")]
    Identity {
        /// Handles to add. Omit to list what's configured.
        handles: Vec<String>,
        /// Remove the named handles instead of adding them.
        #[arg(long)]
        rm: bool,
    },
    /// Claude Code hook handler. Reads the hook payload on stdin; always
    /// exits 0 so a hook never blocks the user.
    Hook {
        /// One of: user-prompt-submit, post-tool-use, stop.
        event: String,
    },
    /// Remove previously exported words from a target, leaving words you
    /// added yourself untouched.
    Unsync {
        /// Limit to one target. Defaults to all of them.
        #[arg(short, long)]
        target: Option<String>,
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
}

/// What a command needs from the lexicon, and so how it opens it.
///
/// Reading never writes: a reader that created, migrated or stamped the
/// database could stall behind a capture hook's write lock, or drag a newer
/// lexicon back to its own schema version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    /// Never touches the lexicon, so must work without one.
    None,
    Read,
    /// Checking: a read, except the first run on the default lexicon, which
    /// seeds it.
    Check,
    Write,
}

impl Command {
    // Exhaustive on purpose: a new command has to choose.
    fn access(&self) -> Access {
        match self {
            Command::Help { .. } | Command::Unsync { .. } | Command::Sync { list: true, .. } => {
                Access::None
            }
            Command::Analyze { lexicon, .. } => {
                if *lexicon {
                    Access::Read
                } else {
                    Access::None
                }
            }
            // Sync writes the spell checkers' files, never the lexicon.
            Command::List { .. }
            | Command::Status
            | Command::Phrases { .. }
            | Command::Eval { .. }
            | Command::Sync { .. } => Access::Read,
            Command::Identity { handles, .. } if handles.is_empty() => Access::Read,
            Command::Typos { words, rm: false } if words.is_empty() => Access::Read,
            Command::Shorthand { words, rm: false } if words.is_empty() => Access::Read,
            Command::Templates { apply: false, .. } => Access::Read,
            Command::Prune { dry_run: true, .. } => Access::Read,
            Command::Identity { .. }
            | Command::Typos { .. }
            | Command::Shorthand { .. }
            | Command::Templates { .. }
            | Command::Prune { .. }
            | Command::Seed { .. }
            | Command::Add { .. }
            | Command::Rm { .. }
            | Command::Capture { .. }
            | Command::Process { .. }
            | Command::Ingest { .. }
            | Command::Hook { .. } => Access::Write,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
    Ndjson,
}

impl Cli {
    pub fn db_path(&self) -> PathBuf {
        self.db.clone().unwrap_or_else(default_db_path)
    }

    /// Whether text was given as an argument, to check or to analyze.
    fn has_text(&self) -> bool {
        match &self.command {
            None => self.text.is_some(),
            Some(Command::Analyze { text, .. }) => text.is_some(),
            Some(_) => false,
        }
    }

    /// Default is human; `-J/--ndjson` wins over `-j/--json`.
    pub fn format(&self) -> Format {
        if self.ndjson {
            Format::Ndjson
        } else if self.json {
            Format::Json
        } else {
            Format::Human
        }
    }
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    // Not `conflicts_with`: `--file` is global, and clap rejects a conflict
    // naming an argument most subcommands lack.
    if cli.file.is_some() && cli.has_text() {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--file cannot be used with text to check: give one or the other",
            )
            .exit();
    }
    env_logger::Builder::from_default_env()
        .filter_level(if cli.verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Warn
        })
        .format_timestamp(None)
        .init();

    // Before anything touches the store — generating completions must work
    // on a machine that has never run `vocab seed`.
    if let Some(requested) = cli.completions {
        let Some(shell) = requested.or_else(shell_from_env) else {
            eprintln!(
                "vocab: could not tell which shell you're running — \
                 pass one to --completions (bash, zsh, fish, elvish, powershell)"
            );
            return ExitCode::from(2);
        };
        clap_complete::generate(shell, &mut Cli::command(), "vocab", &mut io::stdout());
        return ExitCode::SUCCESS;
    }

    match dispatch(&cli) {
        Ok(code) => code,
        // `vocab list | head`: the reader had what it wanted and left. Not
        // an error, and nothing to say about it.
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vocab: {e}");
            // Distinct from 1, which means "findings". A CI step has to be
            // able to tell bad prose from a broken database.
            ExitCode::from(2)
        }
    }
}

fn dispatch(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let profile = Rc::new(Profile::new(cli.profile));
    let code = dispatch_inner(cli, &profile);
    // Profiling reports even when the command failed — a slow error path is
    // still worth seeing.
    profile.report(&mut io::stderr().lock(), cli.format())?;
    code
}

/// Seed on first use, so nobody has to know the command exists.
///
/// An unseeded lexicon flags nearly everything, which makes the first run
/// look broken. Seeding takes seconds, though, so it is announced rather than
/// slipped in — and never runs on the hook path, where it would put six
/// seconds in front of a keystroke.
fn ensure_seeded(store: &Store, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    if store.seconds_since_seed()?.is_some() {
        return Ok(());
    }
    if !quiet {
        eprintln!("vocab: first run — learning your vocabulary from this machine (once)");
    }
    seed::run(store, &seed::SeedOptions::default())?;
    store.mark_seeded()?;
    Ok(())
}

/// The lexicon, opened as `access` needs it.
fn open_store(cli: &Cli, access: Access) -> Result<Store, Box<dyn std::error::Error>> {
    let path = cli.db_path();
    // Auto-seed the *default* store only. An explicit `--db` is a deliberate,
    // scoped store — a scratch file, a test, a per-project lexicon — and
    // quietly spending six seconds scanning the whole machine into it would be
    // both surprising and wrong.
    let seeds = cli.db.is_none();
    match access {
        // An empty stand-in, so no arm needs an Option it never reads.
        Access::None => Store::open(":memory:"),
        Access::Read => Store::open_read_only(path),
        // Read-only unless this is the run that seeds: a check that writes
        // can wait seconds on a capture hook's lock.
        Access::Check if !seeds => {
            if path.exists() {
                Store::open_read_only(path)
            } else {
                Store::open(":memory:")
            }
        }
        Access::Check => {
            if path.exists() {
                let store = Store::open_read_only(&path)?;
                if store.seconds_since_seed()?.is_some() {
                    return Ok(store);
                }
            }
            let store = Store::open(&path)?;
            ensure_seeded(&store, cli.quiet)?;
            Ok(store)
        }
        Access::Write => {
            let store = Store::open(&path)?;
            // `seed` does its own.
            if seeds && !matches!(cli.command, Some(Command::Seed { .. })) {
                ensure_seeded(&store, cli.quiet)?;
            }
            Ok(store)
        }
    }
}

fn dispatch_inner(
    cli: &Cli,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if let Some(Command::Hook { event }) = &cli.command {
        return Ok(hook(cli, event));
    }
    let access = cli.command.as_ref().map_or(Access::Check, Command::access);
    let store = profile.time("store_open", || open_store(cli, access))?;
    let format = cli.format();
    let mut out = io::stdout().lock();

    match &cli.command {
        Some(Command::Seed { scan_root }) => {
            let opts = match scan_root {
                Some(root) => seed::SeedOptions {
                    scan_roots: vec![root.clone()],
                },
                None => seed::SeedOptions::default(),
            };
            let report = profile.time("seed", || seed::run(&store, &opts))?;
            store.mark_seeded()?;
            if !cli.quiet {
                output::render_seed(&mut out, &report, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Add { words }) => {
            let (mut added, mut upgraded) = (0, 0);
            for word in words {
                let normalized = text::normalize(word);
                let (is_new, stronger) =
                    store.upsert_word(&normalized, word, crate::types::Provenance::User, 1)?;
                // Saying a word is real retracts calling it a typo.
                store.forget_typo(&normalized)?;
                added += usize::from(is_new);
                upgraded += usize::from(stronger);
            }
            if !cli.quiet {
                // An upgrade is the point of adding a word already seen: it
                // now silences the checker. "added 0" read as nothing done.
                let message = if upgraded > 0 {
                    format!("added {added}, upgraded {upgraded}")
                } else {
                    format!("added {added}")
                };
                output::status(&mut out, &message, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Rm { word, phrase }) => {
            if word.is_empty() {
                return Err("nothing to remove".into());
            }
            let removed = if *phrase {
                // Joined so both `rm --phrase "background command"` and
                // `rm --phrase background command` work; a shell that already
                // split the words shouldn't change the meaning.
                let gram = word
                    .iter()
                    .map(|w| text::normalize(w))
                    .collect::<Vec<_>>()
                    .join(" ");
                store.remove_ngram(&gram)? > 0
            } else {
                store.remove(&text::normalize(&word.join(" ")))?
            };
            if !cli.quiet {
                let msg = if removed { "removed" } else { "not found" };
                output::status(&mut out, msg, format)?;
            }
            Ok(if removed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }

        Some(Command::List {
            filter,
            limit,
            trusted,
            uncommon,
        }) => {
            // `--trusted` is a set, so it's whole unless asked otherwise;
            // `--uncommon` only narrows a browse.
            let limit = limit.unwrap_or(if *trusted { usize::MAX } else { 50 });
            // The steps filter after the fetch, so they need every row to fill
            // the limit.
            let fetch = if *trusted || *uncommon {
                usize::MAX
            } else {
                limit
            };
            let mut entries = store.list(filter.as_deref(), fetch)?;
            let dictionary = (*trusted || *uncommon).then(crate::dict::load).flatten();
            if *trusted {
                sync::retain_trusted(&mut entries, &store.trust()?, dictionary.as_ref());
            }
            if *uncommon {
                sync::retain_uncommon(&mut entries, dictionary.as_ref())?;
            }
            entries.truncate(limit);
            if !cli.quiet {
                output::render_entries(&mut out, &entries, format)?;
            }
            Ok(if entries.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }

        Some(Command::Typos { words, rm }) => typos(cli, &store, words, *rm, &mut out),
        Some(Command::Shorthand { words, rm }) => shorthand(cli, &store, words, *rm, &mut out),

        Some(Command::Help { topic }) => {
            crate::help::render(&mut out, topic.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Templates {
            min_repeats,
            min_length,
            apply,
        }) => {
            let opts = crate::template::Options {
                min_repeats: *min_repeats,
                min_length: *min_length,
            };
            let report = crate::template::run(&store, &opts, *apply)?;
            if !cli.quiet {
                output::render_templates(&mut out, &report, *apply, format)?;
            }
            // Querying, so an empty result is grep's "no match".
            Ok(if report.templates.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }

        Some(Command::Prune { dry_run, strict }) => {
            let report = crate::prune::run(&store, *dry_run, *strict)?;
            if !cli.quiet {
                output::render_prune(&mut out, &report, *dry_run, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Status) => {
            let mut status = store.status()?;
            // Filled here rather than in the store: what a spell checker has
            // on disk is a fact about the filesystem, not about the lexicon.
            status.integrations = crate::sync::status();
            if !cli.quiet {
                output::render_status(&mut out, &status, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Capture {
            text: arg,
            register,
            source,
        }) => {
            let register = *register;
            let body = match arg {
                Some(t) => t.clone(),
                None => read_stdin()?,
            };
            if body.trim().is_empty() {
                return Ok(ExitCode::SUCCESS);
            }
            // Assistant-authored text is staged with its authorship recorded,
            // never silently mixed into the lexicon.
            let (authored_by, message) = match watermark::detect(&body) {
                Some(marker) => ("assistant", format!("captured as assistant ({marker})")),
                None => ("user", "captured".to_string()),
            };
            store.spool(register, source.as_deref(), &body, authored_by)?;
            if !cli.quiet {
                output::status(&mut out, &message, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Process { limit }) => {
            let processed =
                profile.time("process", || crate::process::process_spool(&store, *limit))?;
            if !cli.quiet {
                output::status(&mut out, &format!("processed {processed}"), format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Sync {
            target,
            dry_run,
            list,
        }) => {
            if *list {
                if !cli.quiet {
                    output::render_targets(&mut out, &sync::targets(), format)?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            let chosen = resolve_targets(target.as_deref())?;
            let mut reports = Vec::new();
            for t in &chosen {
                reports.push(profile.time("sync", || sync::install(&store, t, *dry_run))?);
            }
            if !cli.quiet {
                output::render_sync(&mut out, &reports, *dry_run, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Unsync { target, dry_run }) => {
            let chosen = resolve_targets(target.as_deref())?;
            let mut reports = Vec::new();
            for t in &chosen {
                reports.push(sync::uninstall(t, *dry_run)?);
            }
            if !cli.quiet {
                output::render_sync(&mut out, &reports, *dry_run, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Analyze {
            text,
            lexicon,
            register,
        }) => {
            let report = if *lexicon {
                let register = *register;
                let scope =
                    register.map_or("lexicon".to_string(), |r| format!("lexicon:{}", r.as_str()));
                // Names are excluded: `contextdb` and `polyid` are things
                // you work on, not vocabulary you command, and counting them
                // inflates every diversity measure here.
                let names: std::collections::HashSet<String> = store
                    .list(None, usize::MAX)?
                    .into_iter()
                    .filter(|e| e.kind == crate::types::Kind::Name)
                    .map(|e| e.word)
                    .collect();
                let counts: std::collections::HashMap<String, u64> = store
                    .word_counts(register)?
                    .into_iter()
                    .filter(|(word, _)| !names.contains(word))
                    .collect();
                let mut report = crate::complexity::from_counts(&scope, &counts);

                // Sentence stats recorded during process, when the prose was
                // still around. Absent only if nothing has been processed.
                let totals = store.prose_totals(register)?;
                let sentences = totals.get("sentences").copied().unwrap_or(0);
                if sentences > 0 {
                    let histogram = store.sentence_lengths(register)?;
                    let mut readability = crate::complexity::readability_from_totals(
                        sentences as u64,
                        totals.get("words").copied().unwrap_or(0) as u64,
                        totals.get("syllables").copied().unwrap_or(0) as u64,
                    );
                    readability.sentence_length_stddev =
                        crate::complexity::sentence_length_stddev(&histogram);
                    report.readability = Some(readability);
                }
                report
            } else {
                let body = match text {
                    Some(t) => t.clone(),
                    None => match &cli.file {
                        Some(path) => std::fs::read_to_string(path)?,
                        None => read_stdin()?,
                    },
                };
                if body.trim().is_empty() {
                    return Err(
                        "nothing to analyze (pass text, --file, stdin, or --lexicon)".into(),
                    );
                }
                crate::complexity::from_text("text", &body)
            };
            if !cli.quiet {
                output::render_analysis(&mut out, &report, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Ingest { selves, register }) => {
            let default_register = *register;
            let selves: std::collections::HashSet<String> =
                selves.iter().map(|s| s.to_lowercase()).collect();
            let records = crate::ingest::parse(&read_stdin()?)?;
            let report = store
                .transaction(|| crate::ingest::run(&store, &records, &selves, default_register))?;
            if !cli.quiet {
                output::render_ingest(&mut out, &report, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Eval {
            corpus,
            rate,
            seed,
            kind,
        }) => {
            let text = match corpus {
                Some(path) => std::fs::read_to_string(path)?,
                None => read_stdin()?,
            };
            if text.trim().is_empty() {
                return Err("nothing to evaluate (pass --corpus or pipe stdin)".into());
            }

            let (mutated, injections) = crate::eval::inject_kind(&text, *rate, *seed, *kind);
            let checker = build_checker(&store, profile)?;
            let mut evidence = |gram: &str| store.ngram_count(gram).unwrap_or(0);

            let mut scanner = crate::check::Scanner::new(&checker);
            let mut findings = Vec::new();
            for line in mutated.lines() {
                findings.extend(scanner.feed(line, &mut evidence));
            }

            let words = mutated
                .lines()
                .map(|l| crate::text::prose_words(l).len())
                .sum();
            let fixes: Vec<crate::types::Fix> =
                findings.iter().filter_map(|f| checker.fix(f)).collect();
            let mut report =
                crate::eval::score(&findings, &injections, mutated.lines().count(), words);
            crate::eval::score_fixes(&mut report, &fixes, &injections);
            if !cli.quiet {
                output::render_eval(&mut out, &report, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Phrases {
            register,
            min_count,
            limit,
            words,
        }) => {
            let register = *register;
            let grams = store.ngrams(*words as usize, register)?;
            let mut ranked = ngram::rank_collocations(&grams, *min_count);
            ranked.truncate(*limit);
            if !cli.quiet {
                output::render_phrases(&mut out, &ranked, format)?;
            }
            Ok(if ranked.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }

        Some(Command::Identity { handles, rm }) => {
            if handles.is_empty() {
                let known = store.identities_with_source()?;
                if !cli.quiet {
                    output::render_identities(&mut out, &known, format)?;
                }
                return Ok(if known.is_empty() {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                });
            }
            let mut changed = 0;
            for handle in handles {
                let hit = if *rm {
                    store.remove_identity(handle)?
                } else {
                    store.add_identity(handle)?
                };
                changed += usize::from(hit);
            }
            if !cli.quiet {
                let verb = if *rm { "removed" } else { "added" };
                output::status(&mut out, &format!("{verb} {changed}"), format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Hook { .. }) => unreachable!("hooks return before the store opens"),

        None if cli.fix || cli.expand => fix_input(cli, &store, format, &mut out, profile),
        None => check_input(cli, &store, format, &mut out, profile),
    }
}

/// How long a hook waits on another process's write lock before giving up.
///
/// The prompt hooks run synchronously in front of the user, and a held lock
/// can be a seed or a spool run lasting seconds, so waiting it out is never
/// worth it: losing one capture costs a sighting, waiting costs a visible
/// stall. Long enough for another hook's own capture, a few milliseconds of
/// writing, so two hooks firing together don't drop each other.
const HOOK_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// `vocab hook`: always exits 0, whatever happens.
///
/// Claude Code reads exit 2 from a prompt hook as "block this prompt" and
/// erases what the user typed, so a store that will not open — missing
/// permissions, a newer schema, a held lock — is skipped, not reported as an
/// error. It goes to stderr, which Claude Code shows only in its transcript
/// view on exit 0; stdout would be injected into the prompt.
fn hook(cli: &Cli, event: &str) -> ExitCode {
    // An unparseable payload is a no-op, never an error in front of the user.
    let body = read_stdin().unwrap_or_default();
    let input = serde_json::from_str(&body).unwrap_or_default();
    match Store::open_waiting(cli.db_path(), HOOK_BUSY_TIMEOUT) {
        Ok(store) => {
            crate::hook::run(event, &store, &input);
        }
        Err(e) => log::warn!("hook {event} skipped: {e}"),
    }
    ExitCode::SUCCESS
}

/// One named target, or all of them.
fn resolve_targets(name: Option<&str>) -> Result<Vec<sync::Target>, Box<dyn std::error::Error>> {
    match name {
        None => Ok(sync::targets()),
        Some(n) => sync::find_target(n).map(|t| vec![t]).ok_or_else(|| {
            let known: Vec<_> = sync::targets().iter().map(|t| t.name).collect();
            format!("unknown target: {n} (known: {})", known.join(", ")).into()
        }),
    }
}

/// `vocab typos`: list the pairs, confirm one, or forget one.
fn typos(
    cli: &Cli,
    store: &Store,
    words: &[String],
    rm: bool,
    out: &mut impl Write,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let format = cli.format();
    let words: Vec<String> = words.iter().map(|w| text::normalize(w)).collect();
    match (words.as_slice(), rm) {
        ([], false) => {
            let dictionary = crate::dict::load();
            let in_dictionary = |w: &str| {
                dictionary
                    .as_ref()
                    .is_some_and(|d| crate::dict::contains(d, w))
            };
            let pairs = store.trust()?.typos(in_dictionary);
            if !cli.quiet {
                output::render_typos(out, &pairs, format)?;
            }
            Ok(if pairs.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        ([typo], true) => {
            let forgotten = store.forget_typo(typo)?;
            if !cli.quiet {
                let msg = if forgotten { "removed" } else { "not found" };
                output::status(out, msg, format)?;
            }
            Ok(if forgotten {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        ([typo, intended], false) => {
            if typo == intended {
                return Err(format!("{typo} cannot be a typo of itself").into());
            }
            // `--fix` swaps one word for one word.
            if let Some(bad) = [typo, intended].into_iter().find(|w| !text::is_lexical(w)) {
                return Err(format!(
                    "`{bad}` is not a word: give one word each, of letters, \
                     apostrophes and hyphens"
                )
                .into());
            }
            refuse_a_word(
                &store.trust()?,
                crate::dict::load().as_ref(),
                typo,
                "a typo",
            )?;
            // The latest word on a form wins: it is a typo, not a shorthand.
            store.forget_shorthand(typo)?;
            store.confirm_typo(typo, intended)?;
            if !cli.quiet {
                output::status(out, &format!("confirmed {typo} -> {intended}"), format)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err("give a typo and the word it means, or `--rm` and the typo".into()),
    }
}

/// Refuse to call `word` a typo or a shorthand (`what`) when it is a word.
///
/// A blanket replacement of a dictionary word is wrong in every sentence that
/// meant the word — that is the real-word checker's job, decided in context.
/// And provenance ratchets: a word from a deliberate source outranks a
/// confirmation, so storing one would do nothing.
fn refuse_a_word(
    trust: &crate::trust::Trust,
    dictionary: Option<&crate::dict::Dictionary>,
    word: &str,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(standing) = trust.standing(word)
        && standing.provenance > crate::types::Provenance::Observed
    {
        return Err(format!(
            "{word} is in your lexicon from a deliberate source ({}) — \
             `vocab rm {word}` first if it really is {what}",
            standing.provenance.as_str()
        )
        .into());
    }
    if dictionary.is_some_and(|d| crate::dict::contains(d, word)) {
        return Err(format!("{word} is a dictionary word, so it cannot always be {what}").into());
    }
    Ok(())
}

/// `vocab shorthand`: list them, confirm some, or forget some.
fn shorthand(
    cli: &Cli,
    store: &Store,
    words: &[String],
    rm: bool,
    out: &mut impl Write,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let format = cli.format();
    if words.is_empty() && !rm {
        let dictionary = crate::dict::load();
        let in_dictionary = |w: &str| {
            dictionary
                .as_ref()
                .is_some_and(|d| crate::dict::contains(d, w))
        };
        let listed = crate::shorthand::listing(&store.trust()?, in_dictionary);
        if !cli.quiet {
            output::render_shorthands(out, &listed, format)?;
        }
        return Ok(if listed.is_empty() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }
    if rm {
        if words.is_empty() {
            return Err("name the shorthands to forget".into());
        }
        let mut forgotten = 0;
        for word in words {
            forgotten += usize::from(store.forget_shorthand(&text::normalize(word))?);
        }
        if !cli.quiet {
            output::status(out, &format!("removed {forgotten}"), format)?;
        }
        return Ok(if forgotten > 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let (pairs, positional): (Vec<&String>, Vec<&String>) =
        words.iter().partition(|w| w.contains('='));
    let mut confirmed: Vec<(String, String)> = pairs
        .iter()
        .filter_map(|w| w.split_once('='))
        .map(|(s, m)| (s.to_string(), m.to_string()))
        .collect();
    match positional.as_slice() {
        [] => {}
        [short, means] => confirmed.push((short.to_string(), means.to_string())),
        _ => return Err("give a shorthand and its meaning, or SHORT=MEANING".into()),
    }
    let confirmed: Vec<(String, String)> = confirmed
        .into_iter()
        .map(|(s, m)| (text::normalize(s.trim()), m.trim().to_lowercase()))
        .collect();
    let trust = store.trust()?;
    let dictionary = crate::dict::load();
    for (short, means) in &confirmed {
        if short.is_empty() || means.is_empty() {
            return Err("a shorthand and its meaning are both needed".into());
        }
        if short.eq_ignore_ascii_case(means) {
            return Err(format!("{short} cannot be a shorthand for itself").into());
        }
        refuse_a_word(&trust, dictionary.as_ref(), short, "a shorthand")?;
    }
    store.transaction(|| -> Result<(), rusqlite::Error> {
        for (short, means) in &confirmed {
            // The latest word on a form wins: it is a shorthand, not a typo.
            store.forget_typo(short)?;
            store.confirm_shorthand(short, means)?;
        }
        Ok(())
    })?;
    if !cli.quiet {
        let said: Vec<String> = confirmed
            .iter()
            .map(|(s, m)| format!("{s} = {m}"))
            .collect();
        output::status(out, &format!("confirmed {}", said.join(", ")), format)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The checker over this lexicon: deliberate words as the lexicon proper,
/// observed ones behind the trust rule, people, and frequencies.
fn build_checker(
    store: &Store,
    profile: &Rc<Profile>,
) -> Result<Checker, Box<dyn std::error::Error>> {
    use crate::types::Provenance;
    let trust = profile.time("lexicon_load", || store.trust())?;
    let people = store.people()?;
    let mut lexicon = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for (word, provenance) in trust.deliberate() {
        // Repos, taps, binaries and manifests hold names and nothing else.
        // Hand-added words are whatever the user meant, so they stay neutral.
        if matches!(
            provenance,
            Provenance::Owned | Provenance::Tap | Provenance::Installed | Provenance::Dependency
        ) {
            names.insert(word.to_string());
        }
        lexicon.insert(word.to_string());
    }
    // People rank as names for suggestion purposes: a colleague's name is not
    // a candidate correction for a lowercase ordinary word.
    names.extend(people.iter().map(|(n, _, _)| n.clone()));
    profile.count("people", people.len() as u64);
    profile.count("lexicon_words", lexicon.len() as u64);
    profile.count("names", names.len() as u64);

    let frequency = profile.time("frequency_load", || store.frequencies())?;
    Ok(Checker::with_profile(lexicon, Rc::clone(profile))
        .with_trust(trust)
        .with_naming_sources(names)
        .with_people(
            people
                .into_iter()
                .map(|(n, d, disp)| (n, (d, disp)))
                .collect(),
        )
        .with_frequency(frequency))
}

/// Check a positional string as one blob, or stream stdin/`--file` line by
/// line. A bare invocation with no input prints help rather than erroring.
fn check_input(
    cli: &Cli,
    store: &Store,
    format: Format,
    out: &mut impl Write,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let checker = build_checker(store, profile)?;
    let mut evidence = |gram: &str| {
        profile.count("ngram_queries", 1);
        profile.time("ngram_lookup", || store.ngram_count(gram).unwrap_or(0))
    };

    // A scanner rather than bare check_line: fenced blocks and front matter
    // can only be recognized with memory of the lines above.
    let mut scanner = crate::check::Scanner::new(&checker);

    // NDJSON emits as it reads; every other format collects and renders at the
    // end. A line-oriented format that only appears at EOF is line-*shaped*,
    // not streaming — `tail -f log | vocab -J` would print nothing, and a long
    // file would show nothing until it finished. `-j` cannot stream at all, a
    // pretty array being a single document, which is why both formats exist.
    let streaming = format == Format::Ndjson && !cli.quiet;

    let Some(lines) = input_lines(cli)? else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };

    let mut findings: Vec<Finding> = Vec::new();
    let mut found = false;
    for line in lines {
        let line = line?;
        let line = line.trim_end_matches(['\n', '\r']);
        let batch = profile.time("check", || scanner.feed(line, &mut evidence));
        found |= !batch.is_empty();
        if streaming {
            for finding in &batch {
                output::stream_finding(out, finding)?;
            }
        } else {
            findings.extend(batch);
        }
    }

    if !cli.quiet && !streaming {
        // Only for a one-word positional argument: piped or --file input was
        // never going to be mistaken for a subcommand.
        let single_word = cli
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.contains(char::is_whitespace));
        output::render_findings(out, &findings, single_word, format)?;
    }
    // Lint convention: clean input exits 0, findings exit 1, so this drops
    // into a pre-commit hook or CI step without a wrapper.
    Ok(if found {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Each line of the input — the positional text, `--file`, or piped stdin —
/// with its terminator, so `--fix` can write the text back byte for byte.
/// `None` when there is no input at all.
///
/// One source of lines for checking and fixing, so streaming is implemented
/// once and cannot drift between them.
type Lines<'a> = Box<dyn Iterator<Item = io::Result<String>> + 'a>;
fn input_lines(cli: &Cli) -> io::Result<Option<Lines<'_>>> {
    fn terminated(mut reader: impl BufRead) -> impl Iterator<Item = io::Result<String>> {
        std::iter::from_fn(move || {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => None,
                Ok(_) => Some(Ok(line)),
                Err(e) => Some(Err(e)),
            }
        })
    }
    Ok(if let Some(text) = &cli.text {
        Some(Box::new(
            text.split_inclusive('\n').map(|l| Ok(l.to_string())),
        ))
    } else if let Some(path) = &cli.file {
        Some(Box::new(terminated(io::BufReader::new(
            std::fs::File::open(path)?,
        ))))
    } else if !io::stdin().is_terminal() {
        Some(Box::new(terminated(io::stdin().lock())))
    } else {
        None
    })
}

/// `--fix` and `--expand`: the text back on stdout (or into the file), what
/// changed on stderr.
fn fix_input(
    cli: &Cli,
    store: &Store,
    format: Format,
    out: &mut impl Write,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let checker = build_checker(store, profile)?;
    let mut evidence = |gram: &str| store.ngram_count(gram).unwrap_or(0);
    let mut scanner = crate::check::Scanner::new(&checker);
    let Some(lines) = input_lines(cli)? else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };

    let mut log = io::stderr().lock();
    let mut rewritten = String::new();
    let mut report: Vec<Rewrite> = Vec::new();
    let mut changed = false;
    for raw in lines {
        let raw = raw?;
        let body = raw.trim_end_matches(['\n', '\r']);
        let ending = &raw[body.len()..];
        let findings = scanner.feed(body, &mut evidence);
        let mut wanted: Vec<Fix> = Vec::new();
        if cli.fix {
            wanted.extend(findings.iter().filter_map(|f| checker.fix(f)));
        }
        let mut left_alone = Vec::new();
        if cli.expand {
            for rewrite in scanner.expansions(body) {
                match rewrite {
                    Rewrite::Fix(fix) => wanted.push(fix),
                    other => left_alone.push(other),
                }
            }
        }
        let (fixed, landed) = crate::fix::apply(body, wanted);
        changed |= !landed.is_empty();
        let mut line_report: Vec<Rewrite> = landed.into_iter().map(Rewrite::Fix).collect();
        line_report.extend(left_alone);
        line_report.sort_by_key(Rewrite::col);
        // In place, nothing has landed until the file is written.
        if !cli.in_place {
            for rewrite in &line_report {
                output::stream_rewrite(&mut log, rewrite, format)?;
            }
        }
        report.extend(line_report);
        if cli.in_place {
            rewritten.push_str(&fixed);
            rewritten.push_str(ending);
        } else if !cli.quiet {
            // Written as each line is read, so a pipe sees it straight away.
            write!(out, "{fixed}{ending}")?;
            out.flush()?;
        }
    }
    if let (true, Some(path)) = (cli.in_place, &cli.file) {
        if changed {
            crate::fix::replace_file(path, &rewritten)?;
        }
        for rewrite in &report {
            output::stream_rewrite(&mut log, rewrite, format)?;
        }
    }
    output::render_rewrites(&mut log, &report, format)?;
    // A change is a finding, as in checking: 1 says the text was not clean.
    // A shorthand left alone is not: it was never wrong.
    Ok(if changed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn read_stdin() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().lock().read_to_string(&mut buf)?;
    Ok(buf)
}

// `read_to_string` on a locked stdin needs the Read trait in scope.
use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("vocab").chain(args.iter().copied()))
    }

    #[test]
    fn format_defaults_to_human() {
        assert_eq!(cli(&["text"]).format(), Format::Human);
    }

    #[test]
    fn ndjson_wins_over_json() {
        assert_eq!(cli(&["-J", "text"]).format(), Format::Ndjson);
    }

    #[test]
    fn json_flag_selects_json() {
        assert_eq!(cli(&["-j", "text"]).format(), Format::Json);
    }

    #[test]
    fn db_flag_overrides_the_default_path() {
        let c = cli(&["--db", "/tmp/x.db", "text"]);
        assert_eq!(c.db_path(), PathBuf::from("/tmp/x.db"));
    }

    #[test]
    fn global_flags_work_after_a_subcommand() {
        let c = cli(&["list", "-j"]);
        assert_eq!(c.format(), Format::Json);
    }
}
