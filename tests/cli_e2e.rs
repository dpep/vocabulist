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
    assert_eq!(
        vocab(
            &db,
            &["capture", "-r", "slack", "we shipped the zblorg today"]
        )
        .code,
        0
    );
    assert_eq!(vocab(&db, &["process"]).code, 0);

    // The coinage is now known, so checking it is clean. Every word here came
    // from the captured text, so no system word list is required.
    assert_eq!(vocab(&db, &["zblorg shipped"]).code, 0);
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
fn stats_reports_the_store_in_json() {
    let db = scratch_db("stats");
    vocab(&db, &["add", "iriq"]);
    let out = vocab(&db, &["stats", "-j"]);
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
