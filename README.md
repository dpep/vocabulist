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
repos          owned           312 terms
tap            tap              13 terms
brew           installed       140 terms
binaries       installed       664 terms
dependencies   dependency     1031 terms
frequency      observed      20732 terms

2808 words added, 0 upgraded

$ vocab "the contextdb rubocop and iriq tooling shp a smal change"
1:40   shp    unknown   ship 0.49, shop 0.49, she 0.02   0.70
1:46   smal   unknown   small 0.96, seal 0.04            0.70
```

`contextdb`, `rubocop`, and `iriq` pass without a word. That's the point.

## Seeded from ground truth

The machine already knows your vocabulary — it just never gets asked. Seeding
mines it directly, with no NLP and nothing to vet: if `rubocop` is in your
Gemfile, it's a word.

Each source carries its own provenance, and the confidence gradient falls out
of **how deliberate the evidence was**:

```
repos you own  >  your tap  >  installed  >  dependencies  >  seen in prose
     0.95          0.90         0.80           0.70              0.30
```

Provenance only ever ratchets upward, so a word first noticed in prose is
*upgraded* when it later turns up as a binary you installed — never the
reverse.

## Reluctant on purpose

A false "misspelled" is expensive: it teaches you to ignore the squiggle, and
once you do, the tool is dead. A missed typo costs almost nothing. So `vocab`
skips anything that isn't plausibly prose — short tokens, anything with a
digit, ALLCAPS, camelCase, URLs, paths, email addresses, inline code, fenced
blocks, indented lines — before it forms an opinion.

## Contractions

`dont` → `don't`, as its own high-confidence finding rather than a guess. Edit
distance handles these badly, because the apostrophe form usually isn't in the
word list at all — left alone, `dont` gets "corrected" to `font`.

Only forms that aren't real words are in the table. `cant`, `wont`, `its`, and
`lets` all need to know the sentence, which is the next problem.

## Real-word errors

The typos that survive into sent mail are the ones that are spelled fine:
`form` for `from`, `casual` for `causal`, `manger` for `manager`. A dictionary is
structurally blind to these, because both spellings are real words. Only the
company a word keeps gives it away.

```sh
$ vocab -j "apart form the rest"
[
  {
    "kind": "real-word",
    "word": "form",
    "line": 1,
    "col": 7,
    "suggestions": [
      { "word": "from", "score": 1.0 }
    ],
    "confidence": 0.6
  }
]
```

This works on a lexicon created a minute ago, because `apart from` is
idiomatic and `apart form` is always a slip — a bundled table of collocates
that settle a confusion on their own, derived from a corpus rather than
written by hand. Your own collocations then supersede it, which is the same
inversion the whole tool rests on: your evidence over the general prior.

Confidence is derived, never a constant. `apart` beats its alternative by more
than a thousand to one and says so; a collocate that scraped past the
threshold says that instead.

## Feeding the checkers you already run

You don't have to switch tools. Several checkers keep their personal dictionary
as a plain file, so `sync` writes your lexicon into them:

```sh
$ vocab sync --dry-run
vscode     +1988   -0      ~/.local/share/vocabulist/vocabulist.txt
macos      +1988   -0      ~/Library/Spelling/LocalDictionary
```

The macOS target is the one that pays off quietly — it backs `NSSpellChecker`,
so Mail, Notes, TextEdit, and Safari all stop flagging your jargon.

`vocab unsync` reverses it. Each install records a sidecar manifest of exactly
what it wrote, so uninstall removes only those words and never touches ones you
added yourself.

## Learning

`capture` stages text; `process` folds it into counts and **drops the prose**.
The lexicon, the per-register frequencies, and the collocations survive; the
raw text does not.

```sh
$ vocab capture -r slack "we shipped the zblorg today"
captured
$ vocab process
processed 1
```

Text is tracked per **register** — `prompt`, `slack`, `email`, `commit`, `pr`,
`doc`, `code`.

> **register** *(linguistics)* — the variety of language a person uses in a
> particular setting, and the way it shifts with audience, medium, and
> purpose. The same speaker has a formal register and a casual one, and moves
> between them without effort or awareness. It is *not* the same as the
> source: two emails, one to a colleague and one to a vendor, come from the
> same place and are written in different registers.

A single "writing style" is a fiction: prompts are terse and imperative, docs
are not, and averaging them produces a voice that exists nowhere. Counts stay
split, and the capture channel labels the register for free — a Slack send and
a commit message arrive through different paths — so this needs no classifier.

Assistant-drafted text is recognized by its watermarks and never learned from —
it's about your work, but it isn't in your voice.

```sh
$ vocab capture -r pr "$(cat pr-body.md)"
captured as assistant (co-authored-by: claude)
```

## Measuring complexity

`analyze` reports vocabulary and readability metrics — for a text, or for
everything captured so far:

```sh
$ vocab analyze "We ship small, focused changes. Simple beats clever."
$ vocab analyze --lexicon --register slack
```

The headline number is **Guiraud's R** (`types / √tokens`) rather than the more
familiar type-token ratio, because TTR falls as a sample gets longer and so
can't compare texts of different sizes.

Corpus mode reports sentence metrics too — length, its spread, and reading
ease — because `process` records sentence shape while the prose is still
there. It has to: processing keeps counts and drops the text, so anything not
measured then can never be recovered. The spread matters as much as the mean,
since two writers can share an average sentence length and read nothing
alike.

## Built for pipes and agents

Every command honors the format flags, not just checking:

```sh
vocab "text to check"          # check one string
cat notes.md | vocab -J        # stream stdin, NDJSON per finding
vocab -f README.md -j          # check a file, pretty JSON
vocab list rubo                # lexicon entries matching "rubo"
vocab add contextdb iriq       # add by hand (top provenance, never pruned)
vocab status                   # what the store knows, and where it exports
vocab phrases -n 3             # the three-word phrases you actually repeat
vocab self                     # the handles believed to be yours
vocab prune --dry-run          # what an older, looser capture rule let in
```

`status` also reports what it has actually *read* — bodies per register, messages
captured per service — and which spell checkers the lexicon has been exported
into, including how many words in a shared file are ours rather than yours:

```
read (bodies):
  prompt            412
  slack              88

spell checkers:
  vscode         1,204 words
  macos          1,204 of 1,377 words ours
```

Exit codes follow the convention of the command's job. *Checking* is a linter:
clean input exits `0`, findings exit `1`, so it drops into a pre-commit hook or
a CI step without a wrapper. *Querying* — `list`, `phrases`, `self` — is grep:
a result exits `0` and an empty one exits `1`, so `vocab list foo && …` means
"if any". `2` is always an operational error, never a verdict.

## Local, always

No network calls, at install or at run time. The lexicon is a SQLite database
at `$XDG_DATA_HOME/vocabulist/lexicon.db`, and the dictionary is compiled into
the binary — so it behaves the same on a machine that has no system word list,
and nothing is downloaded to run any of it.

The bundled data is generated, not hand-written: an American-English word list
from [SCOWL](https://wordlist.aspell.net/), and a table of discriminating
collocates derived from
[Google Books Ngrams](https://storage.googleapis.com/books/ngrams/books/datasetsv3.html)
(CC BY 3.0). Both are built by scripts in `script/`, run once by a maintainer,
and committed — deriving the collocates streams about 39 GB, and no user should
ever pay that.

## Install

```sh
brew install dpep/tools/vocabulist   # binary `vocab`
cargo install vocabulist             # same binary, from crates.io
make install                         # from a source checkout → ~/.cargo/bin
```

## Where it's going

The lexicon is half of a larger idea: a personal dictionary and a model of how
you write are the same artifact seen from two sides. Word frequencies,
collocations, and register are simultaneously the checker's known-word set and
a stylistic fingerprint — usable to export into the spell checkers you already
run, and to draft in your voice rather than a generic one.

See [docs/PLAN.md](docs/PLAN.md) for the design and the roadmap.

## License

MIT
