# Changelog

## 0.1.0 — 2026-08-15

First release.

- `vocab seed` builds the lexicon from ground truth — repos you own, your
  Homebrew taps, installed formulae and binaries, and dependency manifests.
  Each source carries its own provenance, and provenance only ratchets upward.
- Checking is the default command: a positional string is one blob, piped
  stdin and `--file` stream line by line. Findings report `line:col`,
  suggestions, and a confidence score. Clean input exits `0`, findings exit
  `1`, operational errors exit `2`.
- Three kinds of finding: `unknown` words, `contraction` fixes (`dont` →
  `don't`), and `real-word` errors (`form` for `from`) judged against
  collocation evidence from your own corpus — silent until it has some.
- `vocab capture` / `vocab process` stage text, fold it into counts, and drop
  the prose. Text is tracked per register (`prompt`, `slack`, `email`,
  `commit`, `pr`, `doc`, `code`) rather than summed.
- Assistant-authored text is detected by watermark and never learned from.
- `vocab sync` / `vocab unsync` export the lexicon into the spell checkers you
  already run — a cSpell custom dictionary and the macOS `LocalDictionary`
  behind Mail, Notes, and Safari. A sidecar manifest records exactly what was
  written, so uninstall never touches words you added yourself.
- `vocab hook` handles Claude Code hook payloads, for capture from prompts and
  outbound messages.
- `vocab add` / `rm` / `list` / `stats`, and `--profile`, all honoring
  `-j/--json` and `-J/--ndjson`.
