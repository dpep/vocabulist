# Changelog

## Unreleased

### Added

- `phrases` reports log-likelihood to two decimals. It is computed from a
  handful of counts, so `61.08969673590383` was claiming evidence that isn't
  there. Rounded where the value is built, so the JSON carries the same
  precision the human output does.
- `vocab rm --phrase "..."` removes a phrase. Phrases had no removal path at
  all: they are derived counts and the prose is gone, so anything a rule
  cannot recognize was permanent.
- **A Wispr Flow target for `vocab sync`.** Dictation has more to gain from a
  personal lexicon than a spell checker does: a checker only has to recognize
  a word you typed, while dictation has to *choose* it from audio, so a name
  it has never heard is unrecoverable rather than merely underlined.

  Flow keeps its dictionary in the cloud and imports from CSV, so this writes
  a file for you to hand it rather than one it reads — which makes it the one
  target `unsync` cannot undo. It caps at Flow's 1,000-item import limit,
  strongest words first.
- `vocab prune` removes what today's capture rules would reject but an older
  version already learned — session ids, tool-call ids, path fragments. The
  default judges shape alone, which cannot take anything that was ever a word.
  `--strict` also requires the dictionary or your lexicon to vouch for each
  word, which reaches residue shape cannot and takes your own coinages with
  it; `--dry-run` first.
- Phrases up to five words, via `vocab phrases -n <2..5>`. Longer ones are
  tracked only once the phrase one word shorter has recurred, which is sound
  because a phrase can never appear more often than its own prefix — so
  anything seen once cannot become a collocation and following it is provably
  wasted. That keeps n=2..5 to about 4,400 rows on the reference corpus where
  storing everything would take 17,200. Note the stored count for n>=3 runs at
  least one below the true one, since counting starts when the prefix recurs.
  The association test stays a 2x2 table by splitting at the *last* space, so
  a longer phrase is scored as "does this one extend in a surprising way".
- `vocab help <option>` works — `vocab help --completions` as well as `vocab
  help status`. clap's built-in `help` only knows subcommands, so asking about
  a flag answered "unrecognized subcommand", which is true and useless. An
  unknown topic now lists what you could have asked about instead.
- `--completions` defaults to the shell you are running, read from `$SHELL`.
  Naming the shell is now the exception rather than the price of entry.
- A one-word check now names the word: `"log" is spelled correctly`, rather
  than a bare "No issues found" that reads like a command result.
- `status` reports when the lexicon was last seeded, and paths are shown
  relative to `~` in human output. JSON keeps absolute paths, since `~` is a
  shell convention a consumer would have to know to expand.
- `vocab status` reports what it has actually **read** — bodies per register,
  messages captured per service — and which spell checkers the lexicon has
  been exported into. For a shared file it distinguishes the words it wrote
  from the ones you added yourself, which is the difference `unsync` depends
  on.
- **The dictionary is bundled, and it is a modern one.** The backstop was
  `/usr/share/dict/words` — Webster's Second International of 1934, which
  turned out to be wrong three ways: a *headword* list carrying `begin` but
  not `began` and `boxcar` but not `box`, ninety years stale, and with no
  frequency data, so `smal` offered `small`, `smalm`, and `smalt` as equals.

  SCOWL levels 10-60 replace it. The levels are themselves a frequency
  ranking, so one file answers both "is this a word" and "how common".
  Cold-start false positives fell from 8.1 per thousand words to 0.7,
  correction rate rose from 0.68 to 0.89, and a cold pass got 20x faster.

  `vocab` no longer reads anything from the host, so it behaves the same on a
  machine with no word list installed.
- Suffix stripping is gone from dictionary lookup. It existed because the old
  backstop carried headwords without inflections; the bundled list carries
  them, so the stripper only accepted typos that happened to peel back to a
  real word. Recall on the reference corpus rose from 87.5% to 95.8% with an
  identical false-positive set.
- **Contractions are derived rather than enumerated.** The hand-written table
  of 35 covered the common cases and nothing else. The same rule — an
  apostrophe form whose bare spelling is not itself a word — now runs over the
  bundled dictionary *and* your lexicon, so `mightve`, `wholl`, and `shant`
  work without anyone having listed them, and a form you personally write
  (`y'all`) starts working once it has been seen. `cant`, `wont`, and `shell`
  are still left alone, which is the whole safety property.
- **A project name no longer outranks a word in suggestions.** 309 of the
  short names in a real lexicon sit one edit from an ordinary word, and
  lexicon membership carries a frequency floor — so `navv` offered the tools
  `navi` and `nav` above `navy`. Suggestions now know whether a name or a word
  was meant, from how the token was capitalized.

  Kind sorts *after* distance, deliberately. Leading with it cost three points
  of correction rate, because technical names are written lowercase —
  `ripgrep`, `nixpkgs` — so demoting names for a lowercase token demotes
  exactly the corrections that were wanted.
- **Real-word confidence reflects the evidence behind it.** Every cue used to
  report a flat `0.6`, so `apart from` — which beats `apart form` by 1444 to 1
  — was indistinguishable from a cue that scraped past the threshold. The
  margin now sets the confidence, logarithmically, between 0.50 and 0.80. It
  stays below what corroborated personal collocations earn, because a rule
  about English is weaker evidence than a fact about you.
- **The cue table is derived from a corpus rather than written by hand.**
  891 cues from Google Books Ngrams, and real-word recall goes from 26.3% to
  **68.4%** with precision and false positives unchanged.

  The corpus overruled three of the hand-written cues. `relationship` was
  listed as selecting `causal`, where `casual relationship` in fact outnumbers
  it three to one; `even`→`though` and `or`→`whether` both looked decisive at
  90:1 and 65:1 but would fire on `even through the night` and `the weather or
  the traffic`. Judging that is not something a person holds steady across
  fifty pairs.
- **Real-word errors are caught without a corpus.** The mechanism needed
  collocation evidence, and a new lexicon has none, so it caught nothing for
  weeks — measured, 0 of 19. A bundled table of discriminating collocates
  covers the day-one case: `apart from` is idiomatic and `apart form` is
  always a slip, so `apart` settles that confusion in any sentence. Recall
  0% → 26%, no new false positives. Your own corpus still overrules it.
- `vocab eval --kind` targets one class of error. Unrestricted sampling
  produced one real-word injection in a 1,658-line corpus, which measured
  nothing. Eval also reports false positives per thousand words, which is
  comparable across corpora as precision is not.

### Fixed

- **A typo typed once was learned, and the checker went blind to it.** The
  lexicon was consulted as a flat set, so one occurrence in one document
  taught a word permanently — an audit of a real store found nine of its
  owner's own misspellings learned that way. A merely-observed word now needs
  two independent contexts before it silences the checker, which is the bar
  `sync` already applied before exporting a word elsewhere.

  Words that sit one edit from a common word need three, because a chronic
  misspelling looks exactly like new vocabulary from the inside and shadowing
  something the writer already knows is what tells them apart. On held-out
  prose this raised recall from 0.665 to 0.720 *and* precision from 0.939 to
  0.951 — the typos already learned had been suppressing real detections.
- **The harness's own text was being learned as yours.** A prompt is not only
  what you typed: reminders get appended, and a finished background task
  arrives as a whole turn of its own. So `background command`, `exit code`,
  and `completed status` had become characteristic phrases of this user. The
  hook now strips the injected envelopes before capture, and a turn that was
  only a notification captures nothing. Ordinary markup in a prompt survives —
  a question about `<div>` is still your prose.
- **Paths, tags, and identifiers were entering the lexicon as words.** A path
  was only recognized after whitespace, so `<output-file>/private/tmp/...`
  contributed `private`, `tmp`, and `output-file`. Markup tags are now masked
  too, since captured text arrives wrapped in them more often than you would
  expect.
- **Phrases were ranking session ids and path fragments above real phrases.**
  N-grams were built over the unfiltered token sequence, so a UUID appearing
  twice looked like a wildly surprising collocation. They are now built over
  runs of ordinary words, which keeps junk out *and* keeps the tokens on
  either side of it from being joined into an adjacency nobody wrote.
- **URLs were only sometimes masked**, so `voidlinux`, `mdbook`, and
  `repology` were reported as misspelled words. Spans were located by
  searching the line for the string `iriq` returned, which works only while
  canonicalization changes nothing — `voidlinux.org/p/?a=1` comes back as
  `.../p?a=1` and matches nowhere. A repeated URL, the markdown badge shape,
  was also masked only the first time.
- **Names the document introduces are no longer flagged.** A README names a
  tool in a table row, an install command, or a link before writing a sentence
  about it; those regions were already being located in order to discard them.
  Keeping them turns the discarded half of every line into a name list, and an
  unknown word matching one is accepted.

  Together: precision 0.77 → 0.92 against held-out technical prose, with recall
  unchanged. See `docs/PLAN.md` §12d.

## 0.4.0 — 2026-08-16

### Breaking

- `vocab stats` is now **`vocab status`**. It was renamed rather than aliased
  because `status` is the word people reach for — and `vocab status`
  previously spell-checked the *word* "status", found it correct, and printed
  "No issues found", so a command that did not exist reported success.
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
