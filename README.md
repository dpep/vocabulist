# vocabulist

**A live personal dictionary — learned from the words you actually use.**

Every spell checker is wrong in the same direction: it doesn't know your
vocabulary. Not slang — *working* vocabulary. The repo you wrote last week, the
formula in your tap, the gem in your Gemfile, the CLI you type forty times a
day. These are deliberate, correct usage, and a generic dictionary flags every
one of them until you give up and turn it off.

`vocab` inverts the relationship. Your lexicon is the authority; an ordinary
dictionary sits underneath as a backstop.

```sh
$ vocab seed                 # runs itself on first use; this re-runs it
repos          owned           332 terms
tap            tap              16 terms
brew           installed       172 terms
binaries       installed       786 terms
dependencies   dependency     1779 terms
frequency      observed      26967 terms

3873 words added, 0 upgraded

$ vocab "the contextdb rubocop and iriq tooling shp a smal change"
1:40   shp                  unknown    ship 0.94, she 0.05, sh 0.02   0.70
1:46   smal                 unknown    small 0.99                     0.70
```

`contextdb`, `rubocop`, and `iriq` pass without a word. That's the point.

Everything runs locally: no network calls, no model, no account.

## Install

```sh
brew install dpep/tools/vocabulist   # binary `vocab`
cargo install vocabulist             # same binary, from crates.io
make install                         # from a source checkout → ~/.cargo/bin
```

The first check seeds your lexicon from the machine, which can take a minute.
After that, a check takes a fraction of a second.

## How it learns your words

### Seeded from ground truth

The machine already knows your vocabulary — it just never gets asked. Seeding
mines it directly, with no NLP and nothing to vet: if `rubocop` is in your
Gemfile, it's a word.

Each source carries its own provenance, and confidence follows **how
deliberate the evidence was**:

```
repos you own  >  your tap  >  installed  >  dependencies  >  seen in prose
     0.95          0.90         0.80           0.70              0.30
```

Provenance only ratchets upward: a word first seen in prose is *upgraded* when
it later turns up as a binary you installed, never the reverse. `vocab add`
outranks everything and is never pruned.

A word seen only in your prose earns trust by turning up in two separate
documents — unless it's a slip of a word you write far more often. `teh`
recurs as reliably as any jargon, but you write `the` hundreds of times as
often, so it stays a typo however often you make it. `trekr`, a repo you own,
is yours even though `trek` is a word.

### From Claude Code, through hooks

The richest source is what you type every day. `vocab hook` handles Claude
Code hook events; add it to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "vocab hook user-prompt-submit" }] }],
    "PostToolUse":      [{ "hooks": [{ "type": "command", "command": "vocab hook post-tool-use" }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "vocab hook stop", "async": true }] }]
  }
}
```

- **`user-prompt-submit`** captures the prompt you typed.
- **`post-tool-use`** captures mail and Slack messages you send through
  Claude, and your own messages in what a Slack or mail read returns. That
  last part stays off until vocab knows which handles are yours: `vocab self`
  lists the ones seeding detected, and `vocab self <handle>` adds one.
- **`stop`** folds captured text into counts at the end of each turn and
  re-seeds when the last seed has gone stale.

A hook always exits 0. If the lexicon is locked, unreadable, or from a newer
vocab, the hook skips the event rather than block your prompt; a lost capture
costs one sighting.

### By hand

`capture` stages text; `process` folds it into counts and **drops the prose**.

```sh
$ vocab capture -r slack "we shipped the zblorg today"
captured
$ vocab process
processed 1
```

`vocab ingest` bulk-loads JSON from stdin (NDJSON or an array of `{body,
author?, register?, source?}`), for backfilling sent mail or old commits.

Text is tracked per **register** — `prompt`, `slack`, `email`, `commit`, `pr`,
`doc`, `code`, `other` — the voice you write in for a given audience. Prompts
are terse, docs are not, and averaging them produces a voice that exists
nowhere, so the counts stay split. The capture channel labels the register for
free, so this needs no classifier.

### Only your own text shapes your voice

Text someone else wrote can show that a word is real, but it never counts as
how *you* write. Assistant-drafted text is recognized by its watermarks:

```sh
$ vocab capture -r pr -f pr-body.md
captured as assistant (co-authored-by: claude)
```

The harness is held to the same rule. Injected reminders are stripped, a turn
that expands a slash command or carries another agent's report is dropped,
identical text counts once however often it arrives, and a headless
`claude -p` session captures nothing. §5 of [docs/PLAN.md](docs/PLAN.md) has
the details.

## Checking text

```sh
vocab "text to check"          # check one string
vocab -f README.md             # check a file
cat notes.md | vocab -J        # stream stdin, one JSON object per finding
```

### Reluctant on purpose

A false "misspelled" is expensive: it teaches you to ignore the squiggle, and
once you do, the tool is dead. A missed typo costs almost nothing. So `vocab`
skips anything that isn't plausibly prose — short tokens, anything with a
digit, ALLCAPS, camelCase, URLs, paths, email addresses, inline code, fenced
blocks, indented lines — before it forms an opinion.

A proper name a dictionary knows (`Latino`, `Americans`) passes when it's
capitalized. Lowercase, it's still checked, so a slip can't hide behind one;
nor can one opening a sentence, where `Kant` one edit from the far commoner
`want` is flagged.

### What it finds

Findings come in three kinds:

- **`unknown`** — not in your lexicon and not in the dictionary, with ranked
  suggestions. A slip you're known to make suggests the word you meant first.
- **`contraction`** — `dont` for `don't`. Edit distance handles these badly,
  because the apostrophe form usually isn't in the word list at all: left
  alone, `dont` gets "corrected" to `font`. Forms that are also real words
  (`cant`, `wont`, `its`, `lets`) need the sentence, so they're left alone.
- **`real-word`** — spelled fine, wrong here: `form` for `from`, `casual` for
  `causal`. A dictionary is blind to these, because both spellings are real
  words. Only the company a word keeps gives it away.

```sh
$ vocab -j "apart form the rest"
[
  {
    "kind": "real-word",
    "word": "form",
    "line": 1,
    "col": 7,
    "suggestions": [
      {
        "word": "from",
        "score": 1.0
      }
    ],
    "confidence": 0.6
  }
]
```

This works on a lexicon created a minute ago, because `vocab` bundles a table
of collocates that settle a confusion on their own (`apart from`, never `apart
form`), derived from Google Books Ngrams rather than written by hand. Your own
collocations then supersede it: your evidence over the general prior.

Every confidence is derived from the evidence behind it, never a constant. A
bundled cue tops out below what your own corroborated phrases earn, because
it's a rule about English, not evidence about you.

### Built for pipes and agents

Every command honors `-j/--json` (a pretty array or object) and `-J/--ndjson`
(one compact object per line). `-J` streams: each finding is written as its
line is checked, so `tail -f notes.md | vocab -J` reports as you type.

Exit codes follow the job. *Checking* is a linter: clean input exits `0`,
findings exit `1`, so `vocab` drops into a pre-commit hook or CI step
unwrapped. *Querying* — `list`, `typos`, `shorthand`, `phrases`, `self` — is
grep: a result exits `0` and an empty one exits `1`, so `vocab list foo && …`
means "if any". `2` is always an operational error, never a verdict.

Reading never writes. Checking and the queries open the lexicon read-only, so
another tool can read it while the hooks keep learning.

`vocab help <topic>` explains any command or option, and
`vocab --completions` prints a completion script for your shell.

## Fixing text: `--fix`

`vocab --fix` writes the text back with corrections applied:

```sh
$ vocab --fix -f notes.md > fixed.md
1:1    Teh -> The
1:16   shoudl -> should
1:34   dont -> don't
```

It writes three things, and only these:

- a missing apostrophe (`dont`);
- a typo you confirmed with `vocab typos`;
- a typo **inferred** from your counts: a swap of two letters in a word you
  write far more often (`teh`, `shoudl`). A slip `vocab` has never seen is
  fixed the same way when it's a swap in an ordinary word and the checker's
  suggestion outweighs every other reading together (`recieve`, `doign`).

Anything less certain is a candidate: the checker suggests it, `--fix` never
writes it. A one-letter difference stays a candidate, because `repros` looks
exactly like a slip of `repos` from the counts and only you know it isn't. So
does a three-letter word, unless the word meant is a very common one (`teh`,
not `cmp` for `mcp`), and a British spelling is never a slip. If `--fix`
touches a spelling you really use, `vocab add` it and it's left alone from then
on.

The text goes to stdout line by line as it's read; what changed goes to
stderr (as JSON under `-j`/`-J`). `--in-place` rewrites the `--file` instead,
replacing it atomically. `--fix` exits `1` when anything changed. Code is
left alone: code spans, fenced blocks, `<code>` and its kin, URLs, and a word
joined to code (`teh.rs`, `$teh`, `--dont`, `teh()`), which checking doesn't
flag either. An ALLCAPS word reads as
an acronym and is never checked, so `TEH` stays `TEH`. The typed case is kept
(`Teh` → `The`), and so is a curly apostrophe.

## Your typos

When a word is held back as a slip, `vocab` knows which word you meant, and
says so with the evidence:

```sh
$ vocab typos
teh                  the                  inferred              transposition     9 docs  x660    0.99
behaviour            behavior             candidate (variant)   insertion         6 docs  x14     0.74
repros               repos                candidate (edit)      insertion         6 docs  x10     0.67
```

Each row is the typo, the word meant, its tier, the kind of slip, how many
documents it turned up in, and how much more often you write the real word.
`confirmed` and `inferred` pairs are what `--fix` writes; a `candidate` says
what held it back.

`vocab typos teh the` confirms a pair, so it holds whatever the counts later
say. `vocab add` a word to say it's real instead.

## Your shorthand, and `--expand`

`repro`, `args`, `perf`, `impls`: abbreviations you use on purpose, which a
spell checker reads as typos. Say so once, and say what each one means:

```sh
$ vocab shorthand repro reproduce
confirmed repro = reproduce
$ vocab shorthand arg=argument perf=performance
confirmed arg = argument, perf = performance
```

A shorthand is never flagged and never touched by `--fix`, and neither are
its inflections: `repros`, `repro'd`, `reproing`.

Type fast, then polish before you send: `--expand` writes them out.

```sh
$ echo "I repro'd it with the args, but the repros differ" | vocab --expand
1:3    repro'd -> reproduced
1:23   args -> arguments
1:37   repros left alone (reproduces?)
I reproduced it with the arguments, but the repros differ
```

`repros` is left alone because it's a guess: "reproduces" in "it repros on
main", "reproductions" in "the repros". An -s reads as a plural only when the
meaning isn't also a verb (`args`, `params`). `vocab shorthand
repros=reproductions` settles a form yourself.

Otherwise `--expand` is `--fix` with your shorthands as the corrections: the
same regions skipped, case kept, report on stderr. `vocab --fix --expand` does
both. A form you confirmed is written out wherever it appears, so confirm the
meaning you use most: with `eval = evaluation`, "eval the enums" becomes
"evaluation the enumerations".

`vocab shorthand` with no words lists what you've confirmed, then candidates:
words you write that begin a word you also write. They're only suggestions —
guessing the meaning from your counts gets it right about one time in three
(`struct` → structural) — so nothing is expanded until you confirm it.

`vocab list -J` marks each learned shorthand with what it means, where that's
settled:

```json
{"word":"args","kind":"shorthand","means":"arguments",...}
{"word":"repros","kind":"shorthand","maybe":["reproduces"],...}
```

## Feeding the checkers you already run

You don't have to switch tools. `vocab sync` exports your jargon — trusted
words an ordinary dictionary doesn't already know — into the personal
dictionaries other tools keep as plain files:

```sh
$ vocab sync --dry-run
vscode     +2756   -0      ~/.local/share/vocabulist/vocabulist.txt
           needs a manual step — no VS Code settings found on this machine
wisprflow  +1000   -0      ~/.local/share/vocabulist/wisprflow.csv
           needs a manual step — import from Flow's Dictionary tab; deleting this file won't un-teach it
macos      +2756   -0      ~/Library/Spelling/LocalDictionary

(dry run — nothing written)
```

- **macOS** backs `NSSpellChecker`, so Mail, Notes, TextEdit, and Safari stop
  flagging your jargon after an app restart.
- **VS Code** (cSpell) reads a file `vocab` owns. Writing it is only half the
  job: cSpell also has to be pointed at it, and nothing complains when it
  isn't. So `sync` reads your VS Code settings, and when nothing points at the
  file (or a config points at an old path), it prints the snippet to add.
  Keep its `"addWords": false`: otherwise cSpell's "add to dictionary" writes
  into a file `sync` regenerates, and the word vanishes at the next sync. Use
  `vocab add` instead.
- **Wispr Flow** keeps its dictionary in the cloud, so `sync` writes a CSV of
  up to 1,000 of your words for you to import.

In `--json`, each target carries `activation` and `live`.

`vocab unsync` reverses it. Each sync records exactly what it wrote, so
unsync removes only those words and never ones you added yourself. (It can't
reach into Wispr Flow once you've imported.)

Another tool can read the same set without copying the rule:
`vocab list --trusted --uncommon -J` is exactly what `sync` exports, and
`vocab list --trusted -J` keeps the ordinary English you really use too.

## Looking at what it learned

```sh
vocab status                   # what the store knows, what it has read, where it exports
vocab list rubo                # lexicon entries matching "rubo"
vocab phrases -n 3             # the three-word phrases you actually repeat
vocab self                     # the handles believed to be yours
vocab analyze --lexicon        # vocabulary and readability of your corpus
vocab templates                # boilerplate that got learned as your prose
vocab prune --dry-run          # what an older, looser capture rule let in
```

`status` reports what has actually been *read* and which spell checkers the
lexicon is exported into, including how much of a shared file is ours:

```
read (bodies):
  prompt            412
  slack              88

spell checkers:
  vscode         2756 words
                 ~/.local/share/vocabulist/vocabulist.txt
  macos          2756 of 2829 words ours
                 ~/Library/Spelling/LocalDictionary
```

`templates` is the backstop for machine text that slipped past capture. A
dozen words repeating verbatim with a flat count is something a program
emits, not something a person types:

```sh
$ vocab templates
   76 words  x44    flat 0.96  confidence 0.96
  the text below is the final report of a subagent this session delegated to…
```

It reports length, repeats, and flatness rather than collapsing them into a
verdict. Read the scan, then `vocab templates --apply` removes the phrases
those runs contain; the words stay. §9a of [docs/PLAN.md](docs/PLAN.md)
explains how runs are found.

`analyze` reports vocabulary and readability, for a text or for everything
captured (`--lexicon`, optionally `--register slack`). The headline number is
**Guiraud's R** (`types / √tokens`) rather than the type-token ratio, which
falls as a sample grows and so can't compare texts of different sizes. Corpus
mode's sentence metrics are recorded while `process` still has the prose,
since afterwards only counts remain.

## Data and privacy

- **Local, always.** No network calls, at install or at run time. The
  lexicon is a SQLite database at `$XDG_DATA_HOME/vocabulist/lexicon.db`
  (override with `--db` or `VOCAB_DB`).
- **Prose is a spool, not an archive.** Captured text is processed into word
  and phrase counts and then dropped.
- **Nothing to download.** The dictionary is compiled into the binary, so
  `vocab` behaves the same on a machine with no system word list. The bundled
  data is generated, not hand-written: an American-English word list and
  proper names from [SCOWL](https://wordlist.aspell.net/), and collocates
  derived from
  [Google Books Ngrams](https://storage.googleapis.com/books/ngrams/books/datasetsv3.html)
  (CC BY 3.0). Scripts in `script/` build them once and the results are
  committed — deriving the collocates streams about 39 GB, and no user should
  ever pay that.

## Where it's going

The lexicon is half of a larger idea: a personal dictionary and a model of how
you write are the same artifact seen from two sides. Word frequencies,
collocations, and register are at once the checker's known-word set and a
stylistic fingerprint — usable to export into the spell checkers you already
run, and to draft in your voice rather than a generic one.

See [docs/PLAN.md](docs/PLAN.md) for the design and the roadmap.

## License

MIT
