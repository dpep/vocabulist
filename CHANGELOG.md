# Changelog

## 0.2.0 — 2026-08-15

### Added

- `vocab analyze` — vocabulary and linguistic complexity, for a text or for
  everything captured. Reports Guiraud's R alongside type-token ratio (TTR
  falls with sample length, so it can't compare texts of different sizes),
  hapax ratio, word length, and — for a text — sentence length and Flesch
  reading ease.
- `--completions <shell>` for bash, zsh, fish, elvish, and powershell.

### Fixed

- **Contractions were never detected.** The check ran only for unknown words,
  but `dont`, `didnt`, and `thats` are all in `/usr/share/dict/words`, so it
  was unreachable for the words it targets. It now runs before the known-word
  gate.
- Smart quotes are folded to ASCII before tokenizing. macOS, Slack, and Gmail
  all autocorrect `'` to `’`, and `don’t` was tokenizing as `don` + `t` —
  splitting the corpus across two spellings of one word.

### Changed

- `Finding.col` is a **character** column, as it always claimed to be. Tokens
  come from a masked copy of the line where multibyte runs became single-byte
  spaces, so byte offsets there had stopped matching the original.

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
