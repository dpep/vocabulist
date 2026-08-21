# Changelog

## 0.5.0 — 2026-08-20

### Breaking

- `vocab stats` is now **`vocab status`**. Two commands differing by two
  letters with identical output is worse than one — and `vocab status`
  previously spell-checked the *word* "status", found it correct, and printed
  "No issues found", so a command that did not exist reported success.
- **A word now needs two separate days to be learned, not two documents.**
  Typos are bursty in time as well as within a message, so two documents from
  one sitting was one piece of evidence counted twice. This makes an existing
  lexicon stricter: words learned in a single session must be seen again on
  another day. Nothing is ever forgotten — this is what a word must earn to
  silence the checker, not decay.
- Schema 6, migrated in place. A `kind` column records what an entry *is*,
  because provenance cannot tell a colleague's name from any other captured
  word.

### Added

- **The dictionary is bundled, and modern.** SCOWL levels 10–60 replace
  `/usr/share/dict/words` — which on macOS is Webster's Second of 1934, a
  *headword* list holding `begin` but not `began` and `boxcar` but not `box`.
  Its levels double as a frequency ranking, so one file answers both "is this
  a word" and "how common". Cold-start false positives fell from 8.1 per
  thousand words to 0.7, correction rate rose from 0.68 to 0.89, and a cold
  pass got 20× faster. `vocab` now reads nothing from the host.
- **Real-word errors are caught on day one.** A bundled table of 891
  discriminating collocates, derived from Google Books Ngrams: `apart from` is
  idiomatic and `apart form` is always a slip. Real-word recall went from 26%
  to 68% with no change in precision. Your own collocations still supersede it.
- **People are learned, and their names checked.** Every Slack `From:` line
  and GitHub login was already parsed to decide which messages were yours,
  then discarded. `Ada Lovelacee` now suggests `Ada Lovelace`, at the
  strictest bar in the crate: one edit, exactly one candidate, seen on two
  separate days, not already a word.
- **Compounds in both directions.** `alot` suggests `a lot`; `luke warm`
  suggests `lukewarm`. Both were already flagged — the corrections were `lot`
  and `like`.
- `vocab prune` removes what an older, looser capture rule let in — session
  ids, path fragments. `--strict` reaches further and will take your coinages
  with it, so read `--dry-run` first.
- `vocab phrases -n 2..5`. Longer phrases are tracked only once the shorter one
  recurs, which is sound because a phrase can never appear more often than its
  own prefix — so n=2..5 costs 4,400 rows where storing everything costs
  17,200.
- `vocab status` reports what it has *read* and which spell checkers it exports
  into. `vocab sync` gained a Wispr Flow target: dictation has more to gain
  from a personal lexicon than a checker does, since it must *choose* a word
  from audio rather than merely recognize one.
- `vocab help <option>` explains a flag, not just a subcommand.
  `--completions` defaults to the shell you are running.

### Fixed

- **A typo typed once was learned, and the checker went blind to it.** The
  lexicon was consulted as a flat set, so one occurrence taught a word
  permanently; an audit found nine of this machine's owner's own misspellings
  learned that way. Words now need corroboration — and three sightings if they
  sit one edit from a common word, because a chronic misspelling looks exactly
  like new vocabulary from the inside.
- **The harness's own text was being learned as your voice.** Prompts carry
  injected reminders, and a finished background task arrives as a whole turn,
  so `background command` and `exit code` had become characteristic phrases.
- **Names, paths, and tags were being flagged as misspelled words.** URLs were
  only masked when the extractor's canonical form appeared verbatim, which it
  often does not; markup tags were never masked; and a path was only recognized
  after whitespace. Together with recognizing the names a document introduces,
  precision went from 0.77 to 0.92 with no loss of recall.
- Suggestions no longer offer a project name for an ordinary word — 309 of the
  short names in a real lexicon sit one edit from a real word, so `navv`
  offered `navi` and `nav` above `navy`.
- Confidence is derived from evidence rather than a constant, everywhere it is
  reported, and rounded where it is built so `--json` carries the same
  precision the human output does.

## 0.4.0 — 2026-08-16

### Breaking

- `Finding.suggestions` is now `[{word, score}]` rather than `[string]`. One
  number couldn't answer two questions — how sure we are the word is wrong,
  and which replacement was meant — so `hepl` reported `help`, `hep`, and
  `heal` as equals.

### Added

- **Capture from your own past messages.** Reading a Slack channel or a pull
  request surfaces things you wrote months ago, which no forward-looking
  capture will ever see. The filter moves from direction to authorship, which
  is stricter: only messages matching a known handle are kept.
- **Identities detect themselves** from gh's config, git config, and commit
  authorship — including the work addresses `git config --get` never sees.
  `vocab self` inspects or overrides them. Capture from reads stays inert
  until one is known.
- **The lexicon seeds itself** on first use, and refreshes monthly in the
  background. Seeding is parallel and now takes ~1.5s.
- A `word` / `name` distinction, so project names stay out of both suggestion
  lists and vocabulary statistics.

### Fixed

- Suggestion ranking put a two-edit candidate above a one-edit one — `aparat`
  offered `part` over `apart` — because frequency counts span six orders of
  magnitude and swamped the edit penalty. Frequency is log-compressed now, and
  distance is decided first.
- **Re-seeding inflated the frequency table.** Mined counts were added to the
  previous ones, and seeding recurs, so a word appearing once in local
  markdown became a "real word" on the second pass — quietly dropping the
  evidence threshold to 1.
- Identity learning matched substrings, so a bare first name could match a
  different person; it's delimiter-bounded now and restricted to Slack text,
  since rendering JSON to one line destroyed the same-line rule that made it
  safe. Harvesting is likewise limited to the two tool families actually
  parsed, rather than any response containing a public login.
- The Slack parser reset a block's body but not its author, so a block missing
  `From:` inherited the previous one.
- Seeding held SQLite's write lock across the whole multi-second scan, which
  could make a concurrent capture hook time out and silently drop a prompt.
- URLs as people actually write them (`github.com/x/y`, no scheme) put `com`
  and `github` in the lexicon as words. Detection uses `iriq` now.
- `~/code` was assumed as the place repos live; the conventional roots are
  tried instead, falling back to `$HOME`.
- Identity detection no longer shells out to the GitHub API — it reads gh's
  config, which is the same answer with no network call, as this crate
  promises.

### Changed

- One shared tokenize pipeline. The sequence was copied five times and had
  drifted, so `analyze` on a text counted URL fragments as vocabulary while
  the corpus path never saw them.
- Registers are validated by clap, so an invalid one lists the valid values
  and shell completion offers them.

## 0.3.0 — 2026-08-16

### Added

- `vocab eval` — measure the checker against a labeled corpus. Known-good
  prose is corrupted in known places (Damerau's four single-character
  operations plus real-word swaps, with QWERTY-adjacent substitutions),
  seeded so runs are comparable. Reports recall, precision, correction rate,
  and a false-positive sample.
- `vocab phrases` — collocations ranked by Dunning's log-likelihood rather
  than raw frequency, which is the difference between "of the" and the
  phrases you actually use.
- `vocab ingest` — bulk-load `{body, author?, register?, source?}` as NDJSON
  or an array on stdin. Text attributed to someone else corroborates that a
  word is real but never shapes your voice.
- `vocab analyze` reports sentence metrics for the corpus, not just for a
  text, because `process` now records sentence shape while the prose still
  exists. Includes sentence-length spread, since two writers can share a mean
  and read nothing alike.
- General-English frequency, from a small embedded core plus counts mined
  from local prose. Ranks suggestions, and doubles as a fast path — ordinary
  text now resolves without reading the system dictionary at all.
- `make link` installs a symlink at `~/.claude/bin/vocab` that tracks the
  build, so a rebuild is immediately live.

### Fixed

- **Suggestions were often wrong.** Transpositions now cost one edit rather
  than two (`aviod` → `avoid`, not `avid`), and frequency breaks the ties an
  unweighted 236k-word list can't (`smal` → `small`, not `smalm`).
- **Fenced code blocks were being checked.** Only the fence marker was
  skipped, so a comment inside a block looked like prose. Front matter too.
- Mid-sentence capitals are treated as proper nouns, and pluralized acronyms
  (`URLs`, `PRs`) as acronyms.
- Modern vocabulary is no longer flagged. The backstop is `web2` — Webster's
  Second International, 1934 — with no `inline`, `download`, or `roadmap`, so
  words seen repeatedly in local prose now count as real.
- Word validity keys on how many distinct documents a word appeared in rather
  than raw occurrences: a typo repeated in one message is one piece of
  evidence, not three. The export gate uses the same test.

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
