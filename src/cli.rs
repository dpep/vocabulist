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

Findings come in two kinds:
  unknown     not in your lexicon and not in the backstop dictionary
  real-word   spelled fine, wrong here -- `form` for `from`, caught by the
              company the word keeps rather than by any dictionary

Checking is deliberately reluctant: a false alarm teaches you to ignore the
tool, a missed typo costs almost nothing. -j/--json and -J/--ndjson switch to
machine output on every command.

Examples:
  vocab seed                       mine repos, taps, binaries, and manifests
  vocab \"ship the small change\"     check one string
  cat notes.md | vocab -J          stream stdin as NDJSON
  vocab capture -r slack \"...\"      stage text for learning
  vocab process                    fold staged text into counts, drop the prose
  vocab list rubo                  lexicon entries matching \"rubo\"";

#[derive(Parser, Debug)]
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

    /// Print a shell completion script (bash, zsh, fish, elvish, powershell).
    #[arg(long, value_name = "SHELL")]
    pub completions: Option<clap_complete::Shell>,
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
    Rm { word: String },
    /// List lexicon entries, strongest first.
    List {
        filter: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show store-wide counts.
    Stats,
    /// Stage text for learning. Assistant-authored text is recorded but never
    /// learned from.
    Capture {
        text: Option<String>,
        /// Which voice this text is in.
        #[arg(short, long, default_value = "other")]
        register: String,
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
        register: Option<String>,
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
        register: String,
    },
    /// Rank the phrases you actually use, by association strength rather than
    /// raw frequency.
    Phrases {
        /// Limit to one register.
        #[arg(short, long)]
        register: Option<String>,
        /// Ignore pairings seen fewer than this many times.
        #[arg(long, default_value_t = 2)]
        min_count: i64,
        #[arg(long, default_value_t = 25)]
        limit: usize,
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
    if let Some(shell) = cli.completions {
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

fn dispatch_inner(
    cli: &Cli,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let store = profile.time("store_open", || Store::open(cli.db_path()))?;
    let format = cli.format();
    let mut out = io::stdout().lock();

    match &cli.command {
        Some(Command::Seed { scan_root }) => {
            let opts = seed::SeedOptions {
                scan_root: scan_root
                    .clone()
                    .unwrap_or_else(|| seed::SeedOptions::default().scan_root),
            };
            let report = profile.time("seed", || store.transaction(|| seed::run(&store, &opts)))?;
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

        Some(Command::Rm { word }) => {
            let removed = store.remove(&text::normalize(word))?;
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

        Some(Command::Stats) => {
            let stats = store.stats()?;
            if !cli.quiet {
                output::render_stats(&mut out, &stats, format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        Some(Command::Capture {
            text: arg,
            register,
            source,
        }) => {
            let register =
                Register::parse(register).ok_or_else(|| format!("unknown register: {register}"))?;
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
            let processed = profile.time("process", || process_spool(&store, *limit))?;
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
                let register = match register {
                    Some(r) => {
                        Some(Register::parse(r).ok_or_else(|| format!("unknown register: {r}"))?)
                    }
                    None => None,
                };
                let scope =
                    register.map_or("lexicon".to_string(), |r| format!("lexicon:{}", r.as_str()));
                let counts = store.word_counts(register)?;
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
            let default_register =
                Register::parse(register).ok_or_else(|| format!("unknown register: {register}"))?;
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

        Some(Command::Phrases {
            register,
            min_count,
            limit,
        }) => {
            let register = match register {
                Some(r) => {
                    Some(Register::parse(r).ok_or_else(|| format!("unknown register: {r}"))?)
                }
                None => None,
            };
            let bigrams = store.ngrams(2, register)?;
            let mut ranked = ngram::rank_collocations(&bigrams, *min_count);
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

/// Check a positional string as one blob, or stream stdin/`--file` line by
/// line. A bare invocation with no input prints help rather than erroring.
fn check_input(
    cli: &Cli,
    store: &Store,
    format: Format,
    out: &mut impl Write,
    profile: &Rc<Profile>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let lexicon: std::collections::HashSet<String> = profile
        .time("lexicon_load", || store.words())?
        .into_iter()
        .collect();
    profile.count("lexicon_words", lexicon.len() as u64);

    let frequency = profile.time("frequency_load", || store.frequencies())?;
    let checker = Checker::with_profile(lexicon, Rc::clone(profile)).with_frequency(frequency);
    let mut evidence = |gram: &str| {
        profile.count("ngram_queries", 1);
        store.ngram_count(gram).unwrap_or(0)
    };

    let mut findings: Vec<Finding> = Vec::new();
    // A scanner rather than bare check_line: fenced blocks and front matter
    // can only be recognized with memory of the lines above.
    let mut scanner = crate::check::Scanner::new(&checker);

    if let Some(text) = &cli.text {
        for line in text.lines() {
            findings.extend(scanner.feed(line, &mut evidence));
        }
    } else if let Some(path) = &cli.file {
        let file = std::fs::File::open(path)?;
        for line in io::BufReader::new(file).lines() {
            findings.extend(scanner.feed(&line?, &mut evidence));
        }
    } else if !io::stdin().is_terminal() {
        for line in io::stdin().lock().lines() {
            findings.extend(scanner.feed(&line?, &mut evidence));
        }
    } else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    }

    if !cli.quiet {
        output::render_findings(out, &findings, format)?;
    }
    // Lint convention: clean input exits 0, findings exit 1, so this drops
    // into a pre-commit hook or CI step without a wrapper.
    Ok(if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Drain the spool into counts. Words, per-register frequencies, n-grams, and
/// a bounded exemplar sample survive; the prose itself does not.
pub fn process_spool(store: &Store, limit: usize) -> Result<usize, Box<dyn std::error::Error>> {
    let pending = store.pending_spool(limit)?;
    let mut processed = 0;

    for row in pending {
        // One transaction per row: the counts and the retirement land together
        // or not at all, so an interrupted run can't re-apply a row it already
        // half-counted.
        store.transaction(|| -> Result<(), Box<dyn std::error::Error>> {
            process_one(
                store,
                row.id,
                row.register,
                &row.body,
                &row.authored_by,
                &row.doc,
            )
        })?;
        processed += 1;
    }
    Ok(processed)
}

/// Fold one spool row into counts and retire it. Called inside a transaction.
fn process_one(
    store: &Store,
    id: i64,
    register: Register,
    body: &str,
    authored_by: &str,
    doc: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // An assistant draft is nobody's vocabulary evidence — it was generated,
    // not observed in the wild — so it contributes nothing at all.
    if authored_by == "assistant" {
        store.retire_spool(id)?;
        return Ok(());
    }
    let body = watermark::strip_trailer(body);

    // Someone else's writing corroborates that a word is real without
    // saying anything about how *you* write. It reaches the lexicon and the
    // source-diversity table, and stops there — no register counts, no
    // collocations, no exemplars, no prose stats. Otherwise the voice
    // profile drifts toward an average of everyone you correspond with.
    if authored_by != "user" {
        for word in prose_words(body) {
            store.upsert_word(&word, &word, crate::types::Provenance::Observed, 0)?;
            store.record_word_source(&word, doc)?;
        }
        store.retire_spool(id)?;
        return Ok(());
    }

    {
        for line in body.lines() {
            if !text::is_prose_line(line) {
                continue;
            }
            // Same normalization the checker applies, so a word captured
            // from Slack (curly apostrophe) counts as the word it is.
            let line = text::normalize_typography(line);
            let masked = text::mask_non_prose(&line);
            let tokens: Vec<String> = text::tokenize(&masked)
                .iter()
                .map(|t| text::normalize(&t.text))
                .filter(|t| t.chars().count() >= 2)
                .collect();

            for word in &tokens {
                if !word
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '-')
                {
                    continue;
                }
                store.upsert_word(word, word, crate::types::Provenance::Observed, 1)?;
                store.bump_register(word, register, 1)?;
                store.record_word_source(word, doc)?;
            }
            for n in [2usize, 3] {
                for gram in ngram::ngrams(&tokens, n) {
                    store.bump_ngram(&gram, n, register, 1)?;
                }
            }
            if tokens.len() >= 6 {
                store.add_exemplar(register, line.trim(), tokens.len() as f64)?;
            }
        }

        // Sentence-level facts, recorded now because the prose is about to be
        // deleted and none of this can be derived from word counts later.
        record_prose_shape(store, register, body)?;
    }
    store.retire_spool(id)?;
    Ok(())
}

/// The prose words of a body, normalized — the shared front half of both
/// processing paths.
fn prose_words(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        if !text::is_prose_line(line) {
            continue;
        }
        let normalized = text::normalize_typography(line);
        let masked = text::mask_non_prose(&normalized);
        for token in text::tokenize(&masked) {
            let word = text::normalize(&token.text);
            if word.chars().count() >= 2
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '-')
            {
                out.push(word);
            }
        }
    }
    out
}

/// Fold one body's sentence shape into the running per-register stats.
fn record_prose_shape(
    store: &Store,
    register: Register,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut words = 0i64;
    let mut syllables = 0i64;
    let mut sentences = 0i64;

    for sentence in crate::complexity::split_sentences(body) {
        let normalized = text::normalize_typography(&sentence);
        let masked = text::mask_non_prose(&normalized);
        let length: Vec<String> = text::tokenize(&masked)
            .iter()
            .map(|t| text::normalize(&t.text))
            .filter(|w| w.chars().any(char::is_alphabetic))
            .collect();
        if length.is_empty() {
            continue;
        }
        sentences += 1;
        words += length.len() as i64;
        syllables += length
            .iter()
            .map(|w| crate::complexity::count_syllables(w) as i64)
            .sum::<i64>();
        store.bump_sentence_length(register, length.len() as i64)?;
    }

    store.bump_prose(register, "sentences", sentences)?;
    store.bump_prose(register, "words", words)?;
    store.bump_prose(register, "syllables", syllables)?;
    Ok(())
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

    #[test]
    fn processing_skips_assistant_authored_text() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Pr,
                None,
                "Some prose here about widgets\n\nCo-Authored-By: Claude <x>",
                "assistant",
            )
            .unwrap();
        assert_eq!(process_spool(&store, 10).unwrap(), 1);
        assert!(!store.contains("widgets").unwrap());
    }

    #[test]
    fn processing_learns_words_and_ngrams_from_your_own_text() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Slack,
                None,
                "ship the small focused change",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert!(store.contains("focused").unwrap());
        assert_eq!(store.ngram_count("small focused").unwrap(), 1);
    }

    #[test]
    fn text_from_others_corroborates_without_shaping_voice() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool_with_author(
                Register::Pr,
                Some("pr"),
                "the zblorg handles retries",
                "other",
                Some("colleague"),
            )
            .unwrap();
        process_spool(&store, 10).unwrap();

        // Their word counts as evidence the word is real...
        assert!(store.contains("zblorg").unwrap());
        assert_eq!(store.source_count("zblorg").unwrap(), 1);
        // ...but says nothing about how *you* write.
        assert_eq!(store.ngram_count("the zblorg").unwrap(), 0);
        let totals = store.prose_totals(None).unwrap();
        assert_eq!(totals.get("sentences").copied().unwrap_or(0), 0);
    }

    #[test]
    fn your_own_text_feeds_both_evidence_and_voice() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Slack,
                Some("slack"),
                "the zblorg ships today",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();

        assert_eq!(store.source_count("zblorg").unwrap(), 1);
        assert_eq!(store.ngram_count("the zblorg").unwrap(), 1);
        assert!(store.prose_totals(None).unwrap()["sentences"] > 0);
    }

    #[test]
    fn a_human_message_keeps_its_prose_despite_a_trailer() {
        let store = Store::open(":memory:").unwrap();
        store
            .spool(
                Register::Commit,
                None,
                "Fix the flaky widget spec\n\nCo-Authored-By: Claude <x>",
                "user",
            )
            .unwrap();
        process_spool(&store, 10).unwrap();
        assert!(store.contains("widget").unwrap());
        assert!(!store.contains("claude").unwrap());
    }
}
