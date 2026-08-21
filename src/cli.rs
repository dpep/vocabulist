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
use crate::types::{Finding, Register};
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
  vocab sync                       export into cSpell and the macOS dictionary
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
    #[arg(short, long, global = true)]
    pub file: Option<PathBuf>,

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
        #[arg(long, default_value_t = 50)]
        limit: usize,
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
    /// yours, and capture from reads stays off until at least one is set.
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

fn dispatch_inner(
    cli: &Cli,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let store = profile.time("store_open", || Store::open(cli.db_path()))?;
    let format = cli.format();
    let mut out = io::stdout().lock();

    // Auto-seed the *default* store only. An explicit `--db` is a deliberate,
    // scoped store — a scratch file, a test, a per-project lexicon — and
    // quietly spending six seconds scanning the whole machine into it would be
    // both surprising and wrong. Excludes `seed`, which does its own, and
    // `hook`, which must stay fast; the Stop hook seeds asynchronously.
    if cli.db.is_none()
        && !matches!(
            cli.command,
            Some(Command::Seed { .. }) | Some(Command::Hook { .. })
        )
    {
        ensure_seeded(&store, cli.quiet)?;
    }

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
            let mut added = 0;
            for word in words {
                let normalized = text::normalize(word);
                let (is_new, _) =
                    store.upsert_word(&normalized, word, crate::types::Provenance::User, 1)?;
                added += usize::from(is_new);
            }
            if !cli.quiet {
                output::status(&mut out, &format!("added {added}"), format)?;
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

        Some(Command::List { filter, limit }) => {
            let entries = store.list(filter.as_deref(), *limit)?;
            if !cli.quiet {
                output::render_entries(&mut out, &entries, format)?;
            }
            Ok(if entries.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }

        Some(Command::Help { topic }) => {
            crate::help::render(&mut out, topic.as_deref())?;
            Ok(ExitCode::SUCCESS)
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
            let (trusted, observed, people) = store.checkable()?;
            let (lexicon, mut names) = split_trusted(trusted);
            names.extend(people.iter().map(|(n, _, _)| n.clone()));
            let checker = Checker::with_profile(lexicon, Rc::clone(profile))
                .with_observed(observed.into_iter().collect())
                .with_naming_sources(names)
                .with_people(
                    people
                        .into_iter()
                        .map(|(n, d, disp)| (n, (d, disp)))
                        .collect(),
                )
                .with_frequency(store.frequencies()?);
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
            let report = crate::eval::score(&findings, &injections, mutated.lines().count(), words);
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

        Some(Command::Hook { event }) => {
            // Fail-open throughout: an unparseable payload is a no-op, never
            // an error in front of the user.
            let body = read_stdin().unwrap_or_default();
            let input = serde_json::from_str(&body).unwrap_or_default();
            crate::hook::run(event, &store, &input);
            Ok(ExitCode::SUCCESS)
        }

        None => check_input(cli, &store, format, &mut out, profile),
    }
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

/// Split trusted lexicon entries into every word, and the subset that came
/// from a source containing nothing but names.
fn split_trusted(
    trusted: Vec<(String, crate::types::Provenance)>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    use crate::types::Provenance;
    let mut all = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for (word, provenance) in trusted {
        // Repos, taps, binaries and manifests hold names and nothing else.
        // Hand-added words are whatever the user meant, so they stay neutral.
        if matches!(
            provenance,
            Provenance::Owned | Provenance::Tap | Provenance::Installed | Provenance::Dependency
        ) {
            names.insert(word.clone());
        }
        all.insert(word);
    }
    (all, names)
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
    let (trusted, observed, people) = profile.time("lexicon_load", || store.checkable())?;
    let (lexicon, mut names) = split_trusted(trusted);
    // People rank as names for suggestion purposes: a colleague's name is not
    // a candidate correction for a lowercase ordinary word.
    profile.count("people", people.len() as u64);
    names.extend(people.iter().map(|(n, _, _)| n.clone()));
    let observed: std::collections::HashMap<String, i64> = observed.into_iter().collect();
    profile.count("lexicon_words", lexicon.len() as u64);
    profile.count("observed_words", observed.len() as u64);
    profile.count("names", names.len() as u64);

    let frequency = profile.time("frequency_load", || store.frequencies())?;
    let checker = Checker::with_profile(lexicon, Rc::clone(profile))
        .with_observed(observed)
        .with_naming_sources(names)
        .with_people(
            people
                .into_iter()
                .map(|(n, d, disp)| (n, (d, disp)))
                .collect(),
        )
        .with_frequency(frequency);
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

    // One loop over all three input sources, so streaming is not implemented
    // three times and cannot drift between them.
    let lines: Box<dyn Iterator<Item = io::Result<String>> + '_> = if let Some(text) = &cli.text {
        Box::new(text.lines().map(|line| Ok(line.to_string())))
    } else if let Some(path) = &cli.file {
        Box::new(io::BufReader::new(std::fs::File::open(path)?).lines())
    } else if !io::stdin().is_terminal() {
        Box::new(io::stdin().lock().lines())
    } else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };

    let mut findings: Vec<Finding> = Vec::new();
    let mut found = false;
    for line in lines {
        let line = line?;
        let batch = profile.time("check", || scanner.feed(&line, &mut evidence));
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
