# Changelog

## Unreleased

### Added

- `vocab sync` / `vocab unsync` — export the lexicon into the spell checkers
  you already run (a cSpell custom dictionary, and the macOS
  `LocalDictionary` behind Mail/Notes/Safari). Each install records a sidecar
  manifest of exactly what it wrote, so uninstall removes only those words and
  never touches ones you added yourself. `--dry-run` and `--list` included.
- Contraction fixes: `dont` → `don't`, as their own finding kind at high
  confidence. Only forms that aren't real words are in the table — `cant`,
  `wont`, `its`, and `lets` need sentence context, not a lookup.
- `--profile` reports phase timings and work counters to stderr.

### Fixed

- Hyphenated compounds (`well-known`, `long-term`, `local-first`) were flagged
  as unknown: the system word list holds 2 hyphenated entries out of 236k. A
  compound is now known when its parts are.
- `process` panicked on text containing characters whose lowercase form is a
  different byte length (`İ`), and the failing row was left unprocessed — so
  every later run hit it again. Same class of bug fixed in URL masking.
- Watermark detection matched anywhere in a body, so a message *about* Claude
  was treated as written by it and dropped from learning. Markers are now
  anchored to the start of a line, and bare model names are gone.
- `process` and `seed` run in a transaction; an interrupted run no longer
  leaves counts half-applied against a row that still looks pending.
- Operational errors exit `2`, distinct from `1` for findings, so a CI step
  can tell bad prose from a broken database.

### Changed

- `vocab seed` — build the lexicon from ground truth: repos you own, your
  Homebrew taps, installed formulae and binaries, and dependency manifests.
  Each source carries its own provenance, and provenance only ratchets upward.
- Checking, as the default command: a positional string is one blob, piped
  stdin and `--file` stream line by line. Findings report `line:col`,
  suggestions, and a confidence score.
- Real-word error detection — `form` for `from` — judged against collocation
  evidence from your own corpus. Silent until the corpus has something to say.
- `vocab capture` / `vocab process` — stage text, fold it into counts, drop the
  prose. Text is tracked per register (`prompt`, `slack`, `email`, `commit`,
  `pr`, `doc`, `code`).
- Assistant-authored text is detected by watermark and never learned from.
- `vocab add` / `rm` / `list` / `stats`, all honoring `-j/--json` and
  `-J/--ndjson`.
