//! Command-line surface: argument parsing, input resolution, dispatch.
//!
//! stdout is reserved for data; all logging goes to stderr so a consumer
//! piping `vocab` always gets clean output.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

use crate::check::Checker;
use crate::store::{Store, default_db_path};
use crate::types::{Finding, Register};
use crate::{dict, ngram, output, seed, text, watermark};

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

    /// Suppress stdout (the work still happens).
    #[arg(short, long, global = true)]
    pub quiet: bool,
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

    match dispatch(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vocab: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let store = Store::open(cli.db_path())?;
    let format = cli.format();
    let mut out = io::stdout().lock();

    match &cli.command {
        Some(Command::Seed { scan_root }) => {
            let opts = seed::SeedOptions {
                scan_root: scan_root
                    .clone()
                    .unwrap_or_else(|| seed::SeedOptions::default().scan_root),
            };
            let report = seed::run(&store, &opts)?;
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
            let processed = process_spool(&store, *limit)?;
            if !cli.quiet {
                output::status(&mut out, &format!("processed {processed}"), format)?;
            }
            Ok(ExitCode::SUCCESS)
        }

        None => check_input(cli, &store, format, &mut out),
    }
}

/// Check a positional string as one blob, or stream stdin/`--file` line by
/// line. A bare invocation with no input prints help rather than erroring.
fn check_input(
    cli: &Cli,
    store: &Store,
    format: Format,
    out: &mut impl Write,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let lexicon = store.words()?.into_iter().collect();
    let checker = Checker::new(lexicon, dict::load());
    let mut evidence = |gram: &str| store.ngram_count(gram).unwrap_or(0);

    let mut findings: Vec<Finding> = Vec::new();

    if let Some(text) = &cli.text {
        for (i, line) in text.lines().enumerate() {
            findings.extend(checker.check_line(line, i + 1, &mut evidence));
        }
    } else if let Some(path) = &cli.file {
        let file = std::fs::File::open(path)?;
        for (i, line) in io::BufReader::new(file).lines().enumerate() {
            findings.extend(checker.check_line(&line?, i + 1, &mut evidence));
        }
    } else if !io::stdin().is_terminal() {
        for (i, line) in io::stdin().lock().lines().enumerate() {
            findings.extend(checker.check_line(&line?, i + 1, &mut evidence));
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
fn process_spool(store: &Store, limit: usize) -> Result<usize, Box<dyn std::error::Error>> {
    let pending = store.pending_spool(limit)?;
    let mut processed = 0;

    for (id, register, body, authored_by) in pending {
        // Staged for the record, but it isn't your voice — learning from it
        // would drift the lexicon toward the assistant's diction.
        if authored_by != "user" {
            store.retire_spool(id)?;
            processed += 1;
            continue;
        }
        let body = watermark::strip_trailer(&body);

        for line in body.lines() {
            if !text::is_prose_line(line) {
                continue;
            }
            let masked = text::mask_non_prose(line);
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
        store.retire_spool(id)?;
        processed += 1;
    }
    Ok(processed)
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
