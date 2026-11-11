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

    // A second document is: recurrence establishes a word, and only a near
    // miss of one the user writes far more often is held back (PLAN §12l).
    say("second");
    assert_eq!(vocab(&db, &["zblorg shipped"]).code, 0);
}

/// Capture `body` as one document of the user's prose, then process it.
fn learn(db: &PathBuf, body: &str, source: &str) {
    let captured = vocab(db, &["capture", "-r", "slack", body, "--source", source]);
    assert_eq!(captured.code, 0);
    assert_eq!(vocab(db, &["process"]).code, 0);
}

#[test]
fn a_habitual_typo_is_never_learned() {
    let db = scratch_db("habitual");
    let body = "the zblorg and the widget, the rest of the day the plan was the \
                same as the last, with teh usual result";
    for source in ["first", "second", "third"] {
        learn(&db, body, source);
    }
    // Both recur in three documents; only one sits a slip from `the`.
    assert_eq!(vocab(&db, &["list", "--trusted", "zblorg"]).code, 0);
    assert_eq!(vocab(&db, &["list", "--trusted", "teh"]).code, 1);
    let out = vocab(&db, &["-j", "i think teh zblorg works"]);
    assert_eq!(out.code, 1);
    let findings: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(findings[0]["word"], "teh");
    assert_eq!(findings.as_array().unwrap().len(), 1);
}

#[test]
fn typos_are_learned_confirmed_and_retracted() {
    let db = scratch_db("typos");
    let body = "the zblorg and the widget, the rest of the day the plan was the \
                same as the last, with teh usual result";
    learn(&db, body, "first");

    let out = vocab(&db, &["typos", "-J"]);
    assert_eq!(out.code, 0);
    let pair: serde_json::Value = serde_json::from_str(out.stdout.lines().next().unwrap()).unwrap();
    assert_eq!(pair["typo"], "teh");
    assert_eq!(pair["intended"], "the");
    assert_eq!(pair["source"], "observed");
    assert_eq!(pair["edit"], "transposition");

    // Confirming holds whatever the counts later say.
    assert_eq!(vocab(&db, &["typos", "zblorg", "widget"]).code, 0);
    let out = vocab(&db, &["typos", "-J"]);
    let first: serde_json::Value =
        serde_json::from_str(out.stdout.lines().next().unwrap()).unwrap();
    assert_eq!(first["typo"], "zblorg");
    assert_eq!(first["source"], "user");

    // Saying a word is real retracts calling it a typo.
    assert_eq!(vocab(&db, &["add", "zblorg"]).code, 0);
    assert_eq!(vocab(&db, &["add", "teh"]).code, 0);
    assert_eq!(vocab(&db, &["typos"]).code, 1, "nothing left is grep's 1");
}

#[test]
fn a_typo_confirmation_that_could_never_apply_is_refused() {
    let db = scratch_db("typo-refused");
    assert_eq!(vocab(&db, &["add", "zblorg"]).code, 0);
    // A deliberate source outranks a confirmation.
    assert_eq!(vocab(&db, &["typos", "zblorg", "widget"]).code, 2);
    // A dictionary word is only wrong in context.
    assert_eq!(vocab(&db, &["typos", "form", "from"]).code, 2);
    assert_eq!(
        vocab(&db, &["typos", "teh"]).code,
        2,
        "a pair needs both words"
    );
    assert_eq!(vocab(&db, &["typos", "--rm", "teh"]).code, 1);
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
fn templates_finds_boilerplate_that_shape_cannot() {
    let db = scratch_db("templates");
    // Boilerplate: ordinary words in an ordinary order, so `prune` is blind
    // to it. Only its verbatim recurrence gives it away.
    let frame = "the text below is the final report of a widget this session \
                 delegated to it is model output not a message from you";
    for _ in 0..8 {
        vocab(&db, &["capture", "-r", "prompt", frame, "-q"]);
    }
    vocab(
        &db,
        &[
            "capture",
            "-r",
            "prompt",
            "lets ship the small focused change",
            "-q",
        ],
    );
    vocab(&db, &["process", "-q"]);

    let scan = vocab(&db, &["templates", "-j"]);
    assert_eq!(scan.code, 0);
    assert!(scan.stdout.contains("final report"), "{}", scan.stdout);
    // Graded, not binary — and a run this flat and this long scores high.
    assert!(scan.stdout.contains("\"confidence\""), "{}", scan.stdout);
    // A scan removes nothing.
    assert!(
        vocab(&db, &["phrases", "--min-count", "1", "--limit", "99"])
            .stdout
            .contains("final report")
    );

    vocab(&db, &["templates", "--apply", "-q"]);
    let after = vocab(&db, &["phrases", "--min-count", "1", "--limit", "99"]);
    assert!(!after.stdout.contains("final report"), "{}", after.stdout);
    // Crossings inside the run go too, not just the line someone noticed.
    assert!(!after.stdout.contains("delegated to"), "{}", after.stdout);
    // The user's own prose is untouched.
    assert!(after.stdout.contains("focused change"), "{}", after.stdout);

    // Nothing left to find is grep's empty result.
    assert_eq!(vocab(&db, &["templates", "-q"]).code, 1);
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

#[test]
fn a_compound_written_as_two_words_is_offered_joined() {
    let db = scratch_db("join");
    let out = vocab(&db, &["-j", "the water was luke warm"]);
    assert!(out.stdout.contains("lukewarm"), "{}", out.stdout);

    // The ambiguous pairs are decided by the sentence, not the pair, and one
    // of them must never be proposed as a join at all. Each has two known
    // halves, so neither token is flagged and the join is never reached.
    for text in [
        "may be we should go",
        "read just the summary",
        "it happens some times",
    ] {
        let clean = vocab(&db, &[text]);
        assert_eq!(clean.code, 0, "{text}: {}", clean.stdout);
    }
}

/// `-J` must emit as it reads, not at EOF.
///
/// The assertion is the *timing*, because the output is byte-identical either
/// way: a format that only appears once the producer finishes is line-shaped
/// but not a pipe, and `tail -f | vocab -J` would print nothing forever.
#[test]
fn ndjson_streams_rather_than_buffering() {
    use std::io::{BufRead, BufReader, Write};

    let db = scratch_db("ndjson_stream");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_vocab"))
        .args(["--db", db.to_str().unwrap(), "-J"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "a line with zblorgw in it").unwrap();
    stdin.flush().unwrap();

    // Read one line with stdin still open. If the child buffered until EOF
    // this blocks forever, so the test is guarded by a reader thread.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = out.read_line(&mut line);
        let _ = tx.send(line);
    });

    let first = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("no output before EOF: -J is buffering, not streaming");
    assert!(first.contains("zblorgw"), "{first}");

    drop(stdin);
    child.wait().unwrap();
}

/// The bytes and the mtime of the main database file — what a backup, a sync
/// daemon, or a second reader would notice changing.
fn fingerprint(db: &PathBuf) -> (Vec<u8>, std::time::SystemTime) {
    let bytes = std::fs::read(db).unwrap();
    let mtime = std::fs::metadata(db).unwrap().modified().unwrap();
    (bytes, mtime)
}

#[test]
fn reading_commands_leave_the_database_untouched() {
    let db = scratch_db("read_untouched");
    vocab(&db, &["add", "zblorg", "widget"]);
    let before = fingerprint(&db);

    for args in [
        &["list"][..],
        &["list", "--trusted", "--uncommon"],
        &["status"],
        &["phrases"],
        &["self"],
        &["analyze", "--lexicon"],
        &["templates"],
        &["prune", "--dry-run"],
    ] {
        let out = vocab(&db, args);
        assert_ne!(out.code, 2, "{args:?} failed");
        assert!(fingerprint(&db) == before, "{args:?} wrote the database");
    }
}

#[test]
fn reading_a_missing_database_is_refused_not_created() {
    let db = scratch_db("read_missing");
    assert_eq!(vocab(&db, &["list"]).code, 2);
    assert!(!db.exists(), "a read must not create the database");
}

#[test]
fn commands_that_never_read_the_lexicon_work_without_one() {
    let db = scratch_db("storeless");
    assert_eq!(vocab(&db, &["help", "list"]).code, 0);
    assert_eq!(vocab(&db, &["analyze", "a widget and a gadget"]).code, 0);
    assert!(!db.exists(), "nothing asked for a lexicon");
}

#[test]
fn reading_does_not_wait_on_a_writer() {
    let db = scratch_db("read_while_writing");
    vocab(&db, &["add", "zblorg"]);

    // What a capture hook does mid-process: hold the write lock.
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    writer
        .execute("UPDATE lexicon SET count = count + 1", [])
        .unwrap();

    let started = std::time::Instant::now();
    let out = vocab(&db, &["list"]);
    let waited = started.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();

    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("zblorg"));
    // Well under the busy timeout a writer would sit through.
    assert!(
        waited < std::time::Duration::from_secs(2),
        "waited {waited:?}"
    );
}

/// A lexicon that separates the trust bar from the dictionary filter.
fn trust_fixture(name: &str) -> PathBuf {
    use vocabulist::store::Store;
    use vocabulist::types::Provenance;

    let db = scratch_db(name);
    {
        let store = Store::open(&db).unwrap();
        // More than `list`'s default limit, so a truncated answer shows.
        for a in 'a'..='h' {
            for b in 'a'..='h' {
                let word = format!("zblorg{a}{b}");
                store
                    .upsert_word(&word, &word, Provenance::User, 1)
                    .unwrap();
            }
        }
        // Trusted, but ordinary English: listed by `--trusted`, never exported.
        store
            .upsert_word("widget", "widget", Provenance::User, 1)
            .unwrap();
        // Observed: one document is not enough, two are.
        for (word, docs) in [("frobnix", 1), ("quuxly", 2)] {
            store
                .upsert_word(word, word, Provenance::Observed, 1)
                .unwrap();
            for doc in 0..docs {
                store.record_word_source(word, &doc.to_string()).unwrap();
            }
        }
    }
    db
}

/// The words in a `-j` array or a `-J` stream, sorted.
fn listed_words(stdout: &str) -> Vec<String> {
    let mut words: Vec<String> = serde_json::Deserializer::from_str(stdout)
        .into_iter::<serde_json::Value>()
        .map(|v| v.unwrap())
        .flat_map(|v| match v {
            serde_json::Value::Array(items) => items,
            one => vec![one],
        })
        .map(|e| e["word"].as_str().unwrap().to_string())
        .collect();
    words.sort();
    words
}

#[test]
fn list_trusted_uncommon_is_exactly_what_sync_exports() {
    use vocabulist::store::Store;

    let db = trust_fixture("list_trusted_uncommon");
    let expected = vocabulist::sync::exportable(&Store::open(&db).unwrap()).unwrap();

    for format in ["-j", "-J"] {
        let out = vocab(&db, &["list", "--trusted", "--uncommon", format]);
        assert_eq!(out.code, 0);
        assert_eq!(listed_words(&out.stdout), expected, "{format}");
    }
    assert!(expected.contains(&"quuxly".to_string()));
    assert!(!expected.contains(&"frobnix".to_string()));
    assert!(!expected.contains(&"widget".to_string()));
}

#[test]
fn trusted_keeps_ordinary_words_and_uncommon_drops_them() {
    let db = trust_fixture("list_trusted_or_uncommon");

    let trusted = listed_words(&vocab(&db, &["list", "--trusted", "-J"]).stdout);
    assert!(trusted.contains(&"widget".to_string()));
    assert!(!trusted.contains(&"frobnix".to_string()));

    // A filter on a browse: untrusted words stay, and the default limit still
    // fills after the dictionary word is dropped.
    let uncommon = listed_words(&vocab(&db, &["list", "--uncommon", "-J"]).stdout);
    assert!(!uncommon.contains(&"widget".to_string()));
    assert!(uncommon.contains(&"frobnix".to_string()));
    assert_eq!(uncommon.len(), 50);
}
