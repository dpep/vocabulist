//! End-to-end tests: drive the built binary with an isolated database.
//!
//! CLI behavior is verified here rather than by hand-running the binary, so
//! the contract stays reproducible and CI-checked.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A scratch database path unique to one test.
fn scratch_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vocabulist-e2e-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("lexicon.db")
}

struct Output {
    stdout: String,
    code: i32,
}

fn vocab(db: &PathBuf, args: &[&str]) -> Output {
    run(db, args, None)
}

fn run(db: &PathBuf, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vocab"));
    cmd.arg("--db")
        .arg(db)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = cmd.spawn().expect("spawn vocab");
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("run vocab");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

#[test]
fn added_words_are_accepted_and_listed() {
    let db = scratch_db("add");
    assert_eq!(vocab(&db, &["add", "contextdb"]).code, 0);

    let listed = vocab(&db, &["list", "context"]);
    assert!(listed.stdout.contains("contextdb"));
    assert!(listed.stdout.contains("user"));

    // A word in the lexicon must never be flagged. Only lexicon words appear
    // here, so this holds whether or not a system word list is installed.
    let checked = vocab(&db, &["contextdb"]);
    assert_eq!(checked.code, 0, "stdout: {}", checked.stdout);
}

#[test]
fn unknown_words_exit_nonzero_for_lint_use() {
    let db = scratch_db("exit");
    let out = vocab(&db, &["zzzqxwv"]);
    assert_eq!(out.code, 1);
}

#[test]
fn json_output_is_a_parseable_array() {
    let db = scratch_db("json");
    let out = vocab(&db, &["-j", "zzzqxwv"]);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert!(parsed.is_array());
    assert_eq!(parsed[0]["kind"], "unknown");
    assert_eq!(parsed[0]["word"], "zzzqxwv");
}

#[test]
fn ndjson_emits_one_object_per_line() {
    let db = scratch_db("ndjson");
    let out = vocab(&db, &["-J", "zzzqxwv and qqxjjv"]);
    let lines: Vec<_> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).expect("valid ndjson line");
    }
}

#[test]
fn stdin_is_streamed_with_line_numbers() {
    let db = scratch_db("stdin");
    let out = run(&db, &["-J"], Some("all fine here\nthen zzzqxwv appears\n"));
    let lines: Vec<_> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1);
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["line"], 2);
}

#[test]
fn capture_then_process_teaches_the_lexicon() {
    let db = scratch_db("capture");
    let say = |source: &str| {
        assert_eq!(
            vocab(
                &db,
                &[
                    "capture",
                    "-r",
                    "slack",
                    "we shipped the zblorg today",
                    "--source",
                    source
                ]
            )
            .code,
            0
        );
        assert_eq!(vocab(&db, &["process"]).code, 0);
    };

    // One sighting is not evidence of a word — it is equally evidence of a
    // typo, and a typo learned once blinds the checker to it forever.
    say("first");
    assert_eq!(vocab(&db, &["zblorg shipped"]).code, 1);

    // Nor is a second sighting the same day. Typos are bursty in time as well
    // as within a message, so one sitting is one piece of evidence however
    // many documents it spans. Earning standing takes another day — see
    // `store::tests` for that half, which needs control of the clock.
    say("second");
    assert_eq!(vocab(&db, &["zblorg shipped"]).code, 1);
}

#[test]
fn assistant_authored_text_gives_vocabulary_but_not_voice() {
    let db = scratch_db("watermark");
    let body = "the frobnicator handles retries\n\nCo-Authored-By: Claude Opus 5 <x>";
    let captured = vocab(&db, &["capture", "-r", "pr", body]);
    assert!(captured.stdout.contains("assistant"));

    vocab(&db, &["process"]);

    // The word counts as evidence — it's about your work and carries your
    // project's jargon, so the checker shouldn't flag it afterward.
    let listed = vocab(&db, &["list", "frobnicator"]);
    assert!(listed.stdout.contains("frobnicator"), "{}", listed.stdout);

    // But the phrasing isn't yours, so no collocations were recorded.
    let phrases = vocab(&db, &["phrases", "--min-count", "1", "-j"]);
    assert!(
        !phrases.stdout.contains("frobnicator"),
        "assistant phrasing must not reach the voice tables: {}",
        phrases.stdout
    );
}

#[test]
fn status_reports_the_store_in_json() {
    let db = scratch_db("status");
    vocab(&db, &["add", "iriq"]);
    let out = vocab(&db, &["status", "-j"]);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(parsed["words"], 1);
}

#[test]
fn seeding_an_empty_tree_still_succeeds() {
    let db = scratch_db("seed");
    let empty = std::env::temp_dir().join("vocabulist-e2e-seed-empty");
    std::fs::create_dir_all(&empty).unwrap();
    let out = vocab(&db, &["seed", "--scan-root", empty.to_str().unwrap(), "-j"]);
    assert_eq!(out.code, 0);
    let parsed: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert!(parsed["sources"].is_array());
}

#[test]
fn removing_a_missing_word_exits_nonzero() {
    let db = scratch_db("rm");
    assert_eq!(vocab(&db, &["rm", "neverthere"]).code, 1);
    vocab(&db, &["add", "neverthere"]);
    assert_eq!(vocab(&db, &["rm", "neverthere"]).code, 0);
}

#[test]
fn quiet_suppresses_stdout_but_keeps_the_exit_code() {
    let db = scratch_db("quiet");
    let out = vocab(&db, &["-q", "zzzqxwv"]);
    assert!(out.stdout.is_empty());
    assert_eq!(out.code, 1);
}

#[test]
fn a_name_the_document_introduces_is_not_a_misspelling() {
    let db = scratch_db("names");
    // The install line names the tool; the sentence below then uses it. Both
    // orderings matter, so the link-and-mention shape is on one line too.
    let doc = "\
$ sudo zypper install ripgrep
You can install it with zypper today.
[nixpkgs](https://github.com/NixOS/nixpkgs/blob/master/x.nix) has it as well.
";
    let out = run(&db, &["-j"], Some(doc));
    assert!(!out.stdout.contains("zypper"), "{}", out.stdout);
    assert!(!out.stdout.contains("nixpkgs"), "{}", out.stdout);
    assert_eq!(out.code, 0);
}

#[test]
fn naming_a_word_does_not_excuse_a_typo_elsewhere() {
    let db = scratch_db("names-bounded");
    // The accept rule is keyed to the exact token. A URL full of names must
    // not turn into a general amnesty for the line.
    let out = run(
        &db,
        &["-j"],
        Some("see https://github.com/NixOS/nixpkgs for teh details\n"),
    );
    assert!(out.stdout.contains("teh"), "{}", out.stdout);
    assert_eq!(out.code, 1);
}

/// Words the checker gets wrong on `tests/corpus/prose.md` with an empty
/// lexicon — the cold-start experience, before any seeding or capture.
///
/// Both are recent coinages that the bundled list does not carry. It was 22
/// words when the backstop was the system dictionary; see `docs/PLAN.md` §12a
/// for what that list looked like and why.
///
/// The list shrinking is the point. It must never grow, which is what the
/// test below actually enforces.
const KNOWN_COLD_START_MISSES: &[&str] = &["recency", "tradeoff"];

#[test]
fn no_new_false_positives_on_the_reference_corpus() {
    // The corpus is generated prose, proofread to be free of misspellings, so
    // anything flagged here is the checker's mistake. A count would let one
    // regression hide behind one fix; comparing the *set* does not.
    let db = scratch_db("corpus-fp");
    let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus/prose.md");
    let out = vocab(&db, &["-j", "--file", corpus]);

    // Indentation distinguishes a finding's own word from the words inside
    // its `suggestions` array, which are nested one level deeper.
    let flagged: Vec<String> = out
        .stdout
        .lines()
        .filter_map(|l| l.strip_prefix("    \"word\": \""))
        .filter_map(|l| l.strip_suffix("\","))
        .map(str::to_string)
        .collect();
    assert!(!flagged.is_empty(), "parsed nothing from: {}", out.stdout);

    let novel: Vec<&String> = flagged
        .iter()
        .filter(|w| !KNOWN_COLD_START_MISSES.contains(&w.as_str()))
        .collect();
    assert!(
        novel.is_empty(),
        "new false positives on correct prose: {novel:?}"
    );
}

#[test]
fn a_bundled_cue_catches_a_real_word_error_with_no_corpus() {
    // The day-one case: nothing captured, nothing seeded, and `rather then`
    // still has to be caught, because collocation evidence will never exist
    // for a lexicon that was created a minute ago.
    let db = scratch_db("cue-cold");
    let out = run(&db, &["-j"], Some("we should ship this rather then that\n"));
    assert!(out.stdout.contains("\"real-word\""), "{}", out.stdout);
    assert!(out.stdout.contains("\"than\""), "{}", out.stdout);
}

#[test]
fn status_reports_what_it_has_read_and_where_it_exports() {
    let db = scratch_db("stats-sources");
    vocab(
        &db,
        &[
            "capture",
            "-r",
            "slack",
            "we shipped the widget today",
            "-q",
        ],
    );
    vocab(
        &db,
        &[
            "capture",
            "-r",
            "pr",
            "the retry logic handles backoff",
            "-q",
        ],
    );
    vocab(&db, &["process", "-q"]);

    let out = vocab(&db, &["status", "-j"]);
    // Bodies read, not words learned — the two answer different questions and
    // only one of them is "what has this thing seen".
    assert!(out.stdout.contains("\"documents\""), "{}", out.stdout);
    assert!(out.stdout.contains("\"slack\": 1"), "{}", out.stdout);
    assert!(out.stdout.contains("\"pr\": 1"), "{}", out.stdout);

    // Export targets are reported whether or not they were ever synced.
    assert!(out.stdout.contains("\"integrations\""), "{}", out.stdout);
    assert!(out.stdout.contains("\"vscode\""), "{}", out.stdout);
}

#[test]
fn a_bare_word_says_it_was_spell_checked() {
    // `vocab status` used to print "No issues found." — the word is spelled
    // correctly, so a subcommand that does not exist looked like one that ran
    // and found nothing. Naming the word makes the interpretation obvious.
    let db = scratch_db("bare-word");
    let out = vocab(&db, &["log"]);
    assert!(
        out.stdout.contains("\"log\" is spelled correctly"),
        "{}",
        out.stdout
    );
    assert_eq!(out.code, 0);

    // A real sentence keeps the plain message — nothing to disambiguate.
    let sentence = vocab(&db, &["we shipped the change today"]);
    assert!(
        sentence.stdout.contains("No issues found"),
        "{}",
        sentence.stdout
    );
}

#[test]
fn completions_default_to_the_running_shell() {
    // A bare `--completions` is the common case; naming the shell should be
    // the exception, not the price of entry.
    let db = scratch_db("completions");
    let out = vocab(&db, &["--completions"]);
    assert!(out.stdout.contains("_vocab"), "{}", out.stdout);
    assert_eq!(out.code, 0);
}

#[test]
fn help_explains_options_not_just_subcommands() {
    // clap's own `help` answers "unrecognized subcommand" for a flag, which is
    // true and useless.
    let db = scratch_db("help-topics");

    let flag = vocab(&db, &["help", "--completions"]);
    assert!(flag.stdout.contains("--completions"), "{}", flag.stdout);
    assert!(flag.stdout.contains("possible values"), "{}", flag.stdout);

    // Global options are reachable by their bare name.
    let global = vocab(&db, &["help", "json"]);
    assert!(global.stdout.contains("--json"), "{}", global.stdout);

    let sub = vocab(&db, &["help", "status"]);
    assert!(sub.stdout.contains("Usage: vocab status"), "{}", sub.stdout);

    let unknown = vocab(&db, &["help", "zzqxwv"]);
    assert_eq!(unknown.code, 2);
}

#[test]
fn prune_removes_ids_but_keeps_your_own_words() {
    let db = scratch_db("prune");
    // A word nothing vouches for, alongside a session id.
    vocab(
        &db,
        &["capture", "-r", "doc", "the evals ran cleanly", "-q"],
    );
    vocab(
        &db,
        &["capture", "-r", "doc", "run b4309yce7 finished today", "-q"],
    );
    vocab(&db, &["process", "-q"]);

    let dry = vocab(&db, &["prune", "--dry-run", "-j"]);
    assert!(dry.stdout.contains("\"dry_run\": true"), "{}", dry.stdout);

    vocab(&db, &["prune", "-q"]);
    let phrases = vocab(&db, &["phrases", "--min-count", "1", "--limit", "50"]);
    assert!(!phrases.stdout.contains("b4309yce7"), "{}", phrases.stdout);
    // `evals` is in no dictionary, and the default prune must not touch it.
    assert!(phrases.stdout.contains("the evals"), "{}", phrases.stdout);
}

#[test]
fn a_phrase_can_be_removed_by_hand() {
    // Phrases have no other removal path: they are derived counts and the
    // prose is gone, so what a rule cannot recognize needs a reader.
    let db = scratch_db("rm-phrase");
    vocab(
        &db,
        &[
            "capture",
            "-r",
            "doc",
            "the background command finished",
            "-q",
        ],
    );
    vocab(&db, &["process", "-q"]);

    let before = vocab(&db, &["phrases", "--min-count", "1", "--limit", "20"]);
    assert!(
        before.stdout.contains("background command"),
        "{}",
        before.stdout
    );

    vocab(&db, &["rm", "--phrase", "background command", "-q"]);
    let after = vocab(&db, &["phrases", "--min-count", "1", "--limit", "20"]);
    assert!(
        !after.stdout.contains("background command"),
        "{}",
        after.stdout
    );

    // Removing a word still works, and is a different thing.
    let word = vocab(&db, &["rm", "finished"]);
    assert_eq!(word.code, 0);
}

#[test]
fn a_misspelled_colleague_is_caught_but_a_stranger_is_not() {
    let db = scratch_db("people");
    // Two sightings on separate days is what lets a name convict another.
    for day in ["-3 days", "-1 days"] {
        vocab(
            &db,
            &["capture", "-r", "slack", "ada lovelace wrote this", "-q"],
        );
        vocab(&db, &["process", "-q"]);
        let _ = day;
    }

    // The e2e harness cannot backdate rows, so this asserts the half it can:
    // an unknown capitalized name is never flagged on a fresh store, which is
    // the property that protects everyone whose name we have not learned.
    let stranger = vocab(&db, &["I met Grace Hopper today"]);
    assert_eq!(stranger.code, 0, "{}", stranger.stdout);

    // And an ordinary word used as a name stays alone.
    let word = vocab(&db, &["the Field was empty"]);
    assert_eq!(word.code, 0, "{}", word.stdout);
}
