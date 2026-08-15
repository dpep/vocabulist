# Changelog

## Unreleased

### Added

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
