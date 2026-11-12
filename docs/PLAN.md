# vocabulist — project plan

A live personal dictionary, learned from the words you actually use.

See [RESEARCH.md](RESEARCH.md) for the prior art behind these decisions, the
methods they correspond to, and the word-list sources that would fix the
largest measured source of false positives.

This document is the design contract and the working plan. It captures why the
tool exists, the decisions already made (and what they were made *against*),
and what's still open. Sections marked **[open]** are decisions deferred on
purpose.

---

## 1. Purpose

Two artifacts that turn out to be one:

1. **A spell checker that never nags you about your own vocabulary.** Every
   generic checker is wrong in the same direction — it doesn't know
   `contextdb`, `iriq`, `rubocop_todo`, or the name of the repo you wrote last
   week. The fix isn't a better algorithm, it's a better dictionary.
2. **A model of how you write.** Word choice, phrasing, and register, measured
   rather than asserted — usable both as self-knowledge and as drafting
   guidance for an assistant writing on your behalf.

These share one store. A personal lexicon — word, frequency, register,
provenance, co-occurrence — is simultaneously the checker's known-word set and
the stylistic fingerprint. Building them separately would mean maintaining the
same corpus twice.

## 2. What this is not

- **Not a better spell-checking algorithm.** Edit distance is solved. The
  value is entirely in whose dictionary it consults.
- **Not a general writing-quality tool.** No grammar checking, no style
  scolding, no readability grading against a population norm. The only norm
  that matters here is *your own prior behavior*.
- **Not a corpus archive.** See §5 — retained prose is a liability, and the
  derived counts are nearly lossless for our purpose.

## 3. Relationship to the rest of the stack

`vocabulist` is **standalone by design**. It ships as its own repo, its own
crate, and its own binary, so it can be useful to someone who runs none of the
rest of this tooling.

- **`ae`** (acronym engine) is the architectural sibling: local-first SQLite
  store, leader-election over a Unix socket, pipe/JSON contract, provenance
  continuum. We copy the shape deliberately. We do *not* extend `ae` itself —
  its dictionary is pair-shaped (acronym → expansion, scored on two axes) and
  a lexicon is frequency-shaped. Forcing them together would bend both.
- **`inception`** is where the capture *hooks* live today, because that's where
  Claude Code hooks are wired. But `vocabulist` owns its own store and its own
  spool, and takes text through a CLI it defines. Nothing about the design
  depends on inception existing.
- **`contextdb`** is untouched. Words are not entities.

Data never enters a repo. The store lives at
`$XDG_DATA_HOME/vocabulist/lexicon.db`.

## 4. The confidence model

Two axes, folded into one number for consumers — the same shape `ae` uses, for
the same reason: a caller wants a single "should I trust this" score, not a
guess about which threshold to apply to which field.

**Validity — "is this a word?"** Set by provenance, which ranks by *how
deliberate the evidence was*:

| Provenance | Prior | Evidence |
|---|---|---|
| `user` | 1.00 | You typed it into `vocab add`. Never pruned. |
| `owned` | 0.95 | A repo you wrote. Maximally distinctive — nobody else's lexicon has it. |
| `tap` | 0.90 | A formula in your own Homebrew tap. |
| `installed` | 0.80 | A binary or formula on this machine. |
| `dependency` | 0.70 | Named in a manifest — someone else's library, but your working vocabulary. |
| `observed` | 0.30 | Seen in prose and nothing more. Earns trust only by recurring. |

Provenance **ratchets**: a stronger source upgrades a word, a weaker one never
downgrades it. Recurrence can lift an `observed` word but never past a
deliberate source, so seeing a typo twice doesn't outrank an installed binary.

**Register fit — "is it a word *here*?"** A word can be perfectly valid and
still wrong in this voice. Tracked as per-register counts (§6).

### The asymmetry that governs everything

A false "misspelled" is expensive: it teaches you to ignore the squiggle, and
once you do, the tool is dead. A missed typo costs almost nothing. So the
default answer is *accept*, and a word has to work to get flagged. This is the
deliberate inverse of a secret-redaction bias, where over-flagging is cheap and
under-flagging is catastrophic.

Concretely, the checker skips: tokens under 3 characters, anything containing a
digit, ALLCAPS, camelCase, URLs, paths, email addresses, inline code spans,
fenced blocks, indented lines, and any line with more punctuation than letters.

## 5. Capture: spool, not firehose

Capture stages text in a `spool` table. Processing derives counts from it, and
the row is then **deleted** — body and all. Nothing reads a processed row, so
a tombstone would be unbounded growth for nothing; dedup of re-read messages
lives in a separate `captured` table keyed by each message's own identifier.

This is a deliberate divergence from a general-purpose firehose, which retains
blobs precisely so it can re-extract when parsers improve. That trade is right
for identifiers and wrong for personal prose: here the raw text is the
liability and the aggregates carry none of the risk.

**But the voice model needs quotes.** A style profile built from adjectives —
"semi-casual, dry levity" — is unfalsifiable and drifts into parody. It needs
real quoted examples. The resolution is a **bounded** exemplar set: the N best
sentences per register, evicted as better ones arrive. Bounded by design, not
by policy.

### Authorship

Text an assistant drafted is *about* your work but isn't *in* your voice.
Learning from it would drift the lexicon toward the assistant's diction and
then feed that back as "yours" — a self-reinforcing loop with no natural
correction.

So capture records `authored_by`, detected from conventional watermarks
(`Co-Authored-By: Claude`, `🤖 Generated with …`, and the personal
"vibed with" note). Assistant-authored bodies are retired unprocessed. A body
you wrote that merely *carries* a trailer keeps its prose — the trailer is
stripped and the rest is learned.

These markers are conventions, not guarantees. Absence proves nothing, so this
is a filter on the obvious cases, never a claim of authorship. **[open]** —
whether to add a heuristic backstop (assistant prose has measurable tells) or
leave it at the literal markers.

### The prompt channel is not all prompts

The harness writes into the same field the user does, and its text carries no
watermark because no assistant drafted it. Three rules, in order of how much
they are trusted to know:

1. **Named envelopes are removed.** `hook::ENVELOPES` lists the tags the
   harness injects — reminders, task notifications, local command output.
   Named rather than "any tag", because a prompt about HTML contains `<div>`
   and is still the user's.
2. **A turn another author produced is dropped whole.** `hook::FOREIGN_MARKERS`
   lists them. A command expansion's body arrives *after* the `<command-*>`
   tags rather than inside them, so stripping leaves the skill author's
   instructions in place of a sentence; a subagent's hand-back is wrapped in
   `<agent-message from="…">` — an attribute, which is why matching bare tags
   missed it — and introduced by a frame that sits outside the tag anyway.
   There is nothing in either turn worth keeping.
3. **Identical text counts once.** Every capture claims a key — a message id
   where the source has one, otherwise a hash of the text with case,
   whitespace, and digit runs folded. A hook that fires the same template
   every session contributes one observation, not four hundred.
4. **An unattended session captures nothing.** `CLAUDE_CODE_SESSION_ATTENDED=0`
   says nobody is typing — a headless `claude -p`, which is the shape a plugin
   asking a model a question takes. Only an explicit `0` counts; absent is a
   CLI that does not set it, and reading that as unattended would turn capture
   off for everyone.

The last two generalize past the hole that prompted them; the first two only
close the hole they name. Rule 3 is deliberately an *identity* rule rather
than a repetition threshold: "seen more than N times, therefore machine"
would be a constant chosen to feel right, and it is false besides — people
repeat themselves. But four hundred copies of one string is one context
however it got there, which is the same standard `MIN_SOURCES` already
applies to words. Rule 4 asks about the session rather than the text, which
is why it needs to know nothing about what the text says.

None of the four will be the last word. Six leaks have been found so far and
every one was invisible until a person read `vocab phrases`, so the store
also gets a backstop that assumes the next capture path leaks: `templates`
(§9a) reconstructs repeated verbatim runs from the n-gram tables, which is
what boilerplate looks like from the inside regardless of how it arrived.

## 6. Registers

A single "writing style" is a fiction. You write in at least these voices:

`prompt` · `slack` · `email` · `commit` · `pr` · `doc` · `code` · `other`

Prompts to an assistant are terse, imperative, elliptical. Drafting an email in
that voice would be actively wrong. Email varies more by recipient than by you.
Commit prose follows rules you wrote down.

The capture channel labels the register **for free** — a Slack send and a
commit message arrive through different paths — so this needs no classifier and
no embeddings. Counts stay split by register rather than summed.

**[open]** — audience conditioning within a register. Email to a close
colleague and email to a vendor are different registers wearing the same label,
and the recipient is available at capture time.

## 7. Seeding from ground truth

Waiting for prose frequency to establish that `contextdb` is a word takes
months. The machine already knows. Five sources, no NLP, no human vetting:

| Source | Provenance | What it mines |
|---|---|---|
| `repos` | `owned` | Directory names and every remote's org/repo, read straight from `.git/config` |
| `tap` | `tap` | Formula and cask names in your own Homebrew taps |
| `brew` | `installed` | `brew list --formula` and `--cask` |
| `binaries` | `installed` | `PATH`, minus the system directories |
| `dependencies` | `dependency` | `Cargo.toml`, `Gemfile`, `package.json` across every repo found |

System binary directories are skipped on purpose: `awk`, `sed`, and `ls` are
either already dictionary words or generic Unix vocabulary, and say nothing
about *your* diction.

Every term contributes itself **and** its parts: `pattern-engine` yields
`pattern-engine`, `pattern`, and `engine`. Strongest provenance wins when two
sources name the same term, so insert order can't change the outcome.

*Measured on the author's machine: 2,807 words from a cold start.*

## 8. Real-word errors

The class of typo that survives into sent mail: `form` for `from`, `casual`
for `causal`, `manger` for `manager`. A dictionary is **structurally blind** to
these — both spellings are perfectly good words — so no amount of dictionary
improvement helps. Only the company a word keeps gives it away.

Mechanism: curated confusion sets, judged against bigram evidence from your own
corpus. For a word in a set, compare the collocation support for what you wrote
against each alternative. Flag only when an alternative clears both an absolute
evidence floor and a wide ratio over the written form.

With an empty corpus this produces **silence**, which is correct — a tool with
no evidence should have no opinion. Accuracy improves strictly with corpus
size, and confidence saturates below certainty because collocation evidence is
suggestive, never proof.

## 9. Collocations and phrases

Same mechanism as §8, read the other direction. N-gram counts ranked by
**Dunning's log-likelihood (G²)** — the corpus-linguistics standard, chosen
over PMI because it stays well-behaved at small counts, which is the regime a
personal corpus lives in permanently.

This is the same math family as the unigram fingerprint (§10), run at n=1,2,3.
One mechanism, three scales.

It also settles a division of labor:

- **The binary** counts, scores, ranks, stores, serves. Deterministic,
  exhaustive, milliseconds.
- **Claude / skills** judge, name registers, curate exemplars, write the voice
  document. The semantic work counting can't do.

Binary proposes, Claude disposes. Pushing collocation extraction up into the
skill layer would mean burning tokens to re-derive statistics badly.

### Phrase length, and why longer ones are affordable

Phrases were pairs and triples because storing every longer n-gram costs a row
per token per order and yields almost nothing: measured on ordinary prose,
13.9% of pairs recur and only 1.3% of five-word runs do, so four times the rows
surface an eighth as many phrases.

The way out is the *apriori* property — a phrase can never appear more often
than its own prefix. A phrase whose prefix was seen once cannot itself be seen
twice, so following it is provably wasted. Counting a longer phrase only after
its prefix has recurred keeps every row that can still become evidence and
drops the rest before they are written:

| n | stored | naive | recurring |
|---|---|---|---|
| 2 | 3,796 | 3,796 | 361 |
| 3 | 617 | ~4,400 | 25 |
| 4 | 30 | ~4,500 | 1 |
| 5 | 1 | ~4,500 | 0 |

Roughly 4,400 rows for n=2..5 against ~17,200 stored naively.

The cost is a one-occurrence lag: for n >= 3 the stored count is at least one
below the true one, because counting starts when the prefix recurs. So
`--min-count 2` on a long phrase means "said at least three times" — a
stricter bar than it looks, and the right one for a claim about habit.

### 9a. Templates — boilerplate seen from the inside **[done]**

`prune` judges one stored row, and boilerplate is ordinary words in an
ordinary order, so no row-local test can reach it. The evidence is *across*
rows: a repeated sentence leaves a chain of overlapping five-grams that all
recur about equally often. `vocab templates` chains them back — join on the
four-word overlap, highest count winning a contested link, and report the
maximal runs.

Five is the chaining width because it is the widest the store keeps and
because a four-word overlap makes a wrong join unlikely; chaining bigrams
would wander through ordinary English.

Three quantities are measured and reported rather than collapsed into a
verdict: **length** (words recurring verbatim), **repeats** (the lowest count
along the chain, so a run is credited only with what its weakest link
demonstrates), and **flatness** (lowest count over highest — a template
repeats whole, prose that merely overlaps is raggeder). Confidence is their
product, so nothing scores high on one strength alone, which is the mistake
that would flag prose.

The defaults — twelve words, five repeats — are where the real store
separates: at those settings every run found was machine text, and lowering
repeats to two pulled in prose the user had simply said twice. They gate
reading, not removing. `--apply` takes every n-gram the run contains at every
stored width, crossings included, because the template is the unit; removing
it one line at a time is why an audit ends with the same fragments at the top
of the list. Only collocations go — the words stay in the lexicon, and a
genuine pairing re-accumulates from zero.

## 10. Stylometry — beyond vocabulary **[open]**

Words are the first layer. Style is the second, and the interesting question
isn't *what to measure* — it's what each measurement **maps onto**, because a
number with no interpretation can't become drafting guidance.

The discipline that keeps this honest: **every marker needs a baseline.** "You
use 2.3 em-dashes per hundred words" means nothing alone. It means something
against a reference corpus, computed as log-odds — the same method as the
lexical fingerprint. And baselines must be **per register**, or Slack terseness
gets averaged with doc prose into a voice that exists nowhere.

### Candidate markers

**Rhythm and length**
- Word length distribution (not just the mean — the shape)
- Sentence length distribution, and its variance. Uniform sentence length reads
  as monotone; high variance reads as conversational
- Paragraph length; ratio of one-sentence paragraphs
- Clause depth / subordination rate

**Punctuation as fingerprint** — the classic authorship-attribution signal,
because it's below conscious control
- Serial (Oxford) comma: used, avoided, or inconsistent
- Em-dash, parenthesis, and colon rates — three different ways to do the same
  syntactic job, and the choice among them is highly personal
- Semicolon usage (near-binary across writers)
- Terminal punctuation: exclamation rate, question rate, sentence fragments
- Ellipses, and whether spaced or not

**Diction**
- Contraction rate (`don't` vs `do not`)
- Latinate vs Germanic word preference — the strongest single formality signal
  (`utilize`/`use`, `commence`/`start`)
- Hedging rate (`maybe`, `perhaps`, `I think`, `somewhat`)
- Intensifier rate (`very`, `really`, `quite`)
- Function-word distribution — the backbone of authorship attribution
  precisely because it's invisible to the author
- Type-token ratio: lexical variety, length-normalized

**Structure and formatting**
- Lists vs prose for enumerable content
- Sentence openers: coordinating conjunctions, participials, "So…"
- Lowercase sentence starts (register-dependent — near-universal in chat)
- Emoji and emoticon rate; heading style; code-fence habits

### What they map onto

The markers are evidence; these are the dimensions they support. This mapping
is the part to get right, and the part most likely to be wrong at first:

| Dimension | Supported by |
|---|---|
| Directness | short sentences, low subordination, low hedging, imperatives |
| Formality | Latinate preference, contraction rate, terminal punctuation |
| Density | type-token ratio, clause depth, word length |
| Warmth | second person, questions, exclamations, emoji |
| Certainty | hedge and intensifier rates, modal verbs |
| Discursiveness | em-dashes, parentheticals, sentence-length variance |

### Two directions of use

1. **Analysis** — "here is how you write, with the evidence," per register.
   Descriptive, falsifiable, quotable.
2. **Guidance** — the same profile as *checkable constraints* for drafting:
   "median sentence 14 words; hedge rate under 2%; serial comma always;
   contractions in Slack, not in docs." A generated draft can be measured
   against these and revised, which is strictly better than an adjective in a
   prompt.

**Open questions.** Which markers actually discriminate *you* from a baseline
versus merely describing English? How much corpus is needed before a marker
stabilizes? Should the profile be one document or per-register documents? Does
a marker that's stable but uninteresting (everyone uses periods) get pruned
automatically, and by what test?

## 11. Integration surfaces

**Tier 1 — write the file.** ✅ Shipped as `vocab sync` / `vocab unsync`.
Several checkers keep their personal dictionary as a newline-delimited file, so
one lexicon exports to N targets idempotently. This is how you "upgrade the
built-in checker" for a fraction of the cost of replacing it.

- **VS Code / cSpell** — a file we own, referenced from
  `cSpell.customDictionaries`. Deliberately *not* a rewrite of the user's
  `settings.json`: that file is JSONC and theirs, and mechanical edits lose
  comments.

  That decision has a cost, and for months it was paid silently. Writing a
  correctly formatted word list is only half an integration — the consumer
  still has to be told the file exists, and nothing fails when it hasn't been.
  One user's export sat correct and unread from the first sync onward, because
  neither the tool nor the person running it could tell that state apart from
  a working one. So `sync` now **checks**: it reads the editor's own settings
  and reports `inherent`, `wired`, `inert`, or `manual` per target, and prints
  the activation snippet — with `addWords: false`, so cSpell's quick-fix
  doesn't write into a file the next sync regenerates — when it finds an
  export nothing points at. Detect and report, not edit: the reason not to
  touch someone's `settings.json` hasn't changed, only the silence around it.
- **macOS `~/Library/Spelling/LocalDictionary`** — feeds `NSSpellChecker`, so
  Mail, Notes, TextEdit, and Safari all benefit. Read at app launch, so
  eventually-consistent rather than live.
- **Chromium `Custom Dictionary.txt`** — deferred. It would cover Chrome, Slack
  desktop, and every web textarea, but the file carries a trailing
  `checksum_v1` line and Chrome is understood to discard the dictionary when
  that doesn't match, so a naive append may silently disable it. **Verify
  before implementing** — this was written as the one target that can fail
  closed and silently, and that was wrong: VS Code was already doing it, one
  step earlier. A target can fail at the format *or* at the wiring, and only
  the first was being guarded against.

**Uninstall must be exact.** These files are shared with words the user added
themselves — macOS writes there on every "Learn Spelling". So each install
records a sidecar manifest of precisely what it wrote, and uninstall removes
only those lines. Sentinel comment lines would be simpler but pollute a file
where every line is treated as a word. The same manifest lets a sync take
back a word it wrote that no longer exports (a name dropped by
`--uncommon`, a word whose trust lapsed), so a shared file shrinks with the
export instead of keeping every word ever written; a word the user had
there first was never in the manifest and is never touched.

Every export is a **lossy projection** — a flat membership set, filtered to
what an ordinary dictionary doesn't already know. The rich store stays rich;
the dumbest consumer must not shape the schema.

**Reading never writes.** Other tools read the lexicon too, through the CLI,
and they must not change it. So every command declares what it needs from
the store (`cli::Access`): nothing, a read, or a write. Readers go through
`Store::open_read_only`, which never creates or stamps the database. It
refuses a missing file and a newer schema, which belongs to a newer binary.
An older schema is the one write a reader makes: one writing open migrates
it, exactly as the next writing command would, and the reader reopens
read-only. Refusing it left every check, `list` and `status` failing (exit 2)
after an upgrade until something happened to write. The migration is one
`BEGIN IMMEDIATE` transaction that rereads the version under the lock, so
two openers racing on an old lexicon migrate it once. A lexicon in a
directory the user can't write fails a WAL read, which needs a `-shm` beside
it; the reader then reopens it `immutable=1` (no locks, no `-shm`). That gives
up SQLite's guard against a concurrent writer, which is acceptable only
because nothing can write there either. Under WAL a reader never waits on a capture hook's write
lock. A writer refuses a newer schema too — stamping its own version would
hand the lexicon back to the newer binary looking older than it is — and
writes nothing on open to a lexicon already at its schema. Checking, `--fix` and `--expand` read too, with one exception: the first run
on the default lexicon opens it for writing to seed it, the one onboarding
path a new user can't be expected to know about. Against a `--db` that does
not exist they check with an empty lexicon rather than create one.

A reader that wants the user's vocabulary reads it from `vocab list` rather
than copying the rule, which would drift the first time the bar moved. Two
composable steps, each defined once:

- `--trusted` (`trust::Trust::trusts`, the checker's own rule) keeps words
  that earned trust (§12l): a deliberate source, or two distinct documents
  and not a habitual typo. Ordinary English stays,
  because a word the user really writes helps a consumer like transcription
  even when every dictionary has it. It names a set, so it lists all of it
  unless `--limit` is given.
- `--uncommon` (`sync::retain_uncommon`) drops what the dictionary knows,
  as a word or as a proper name (`latin`, `america`): SCOWL's word lists hold
  no capitalized words, so without the bundled proper names a place or a
  people passed as the user's jargon and a transcription consumer "corrected"
  `Latino` to `Latin`. A name is dropped only when it was merely observed:
  a deliberate source (`vocab add Thor`, a repo called `faraday`, an
  installed `prometheus`) is the user's word whatever else it names. On a
  real store that drops 20 of 3,425 words where dropping every name took 40,
  half of them the user's own. It is a filter, not a set, so on a plain `list` it keeps the browse's default
  limit of 50. Without a dictionary it refuses (exit 2) rather than passing
  everything through: "uncommon" is a claim the dictionary backs, and a
  warning on stderr is invisible to a `-J` consumer. The dictionary is
  bundled, so only a broken build reaches that.

`sync` exports their intersection, so `vocab list --trusted --uncommon` is
exactly the export set.

**Tier 2 — be the checker.** An LSP server serves VS Code, Neovim, Zed, and
Helix from one implementation, and its "add to lexicon" code action is the
highest-quality signal available: an explicit human vote, straight to `user`
provenance. macOS `NSSpellServer` can register a genuine system-wide
alternative checker, but needs an app bundle; the LocalDictionary file gets
most of the value for a fraction of the work.

**Tier 3 — closed.** Google Docs has no personal-dictionary API, and renders
text to canvas, so an extension can't reliably read the document either. Not
worth chasing. (Gmail *web* is ordinary contenteditable and is covered by the
Chromium export — it's specifically Docs that's walled off.)

## 12. Claude integration

A spell checker only ever *rejects* words. The lexicon can also be used
**generatively**, which is the part nothing else does:

- Draft in your vocabulary — prefer words you actually use
- Suggest your word for a word you'd never use (`utilize` → `use`)
- Measure a draft against the style profile before showing it to you

The generative direction is the one place embeddings genuinely earn their
place: finding a semantic neighbor *within your lexicon* can't be done by
counting, because the substitute may never co-occur with the original. That's
phase 3, and `ae`'s `embed.rs` (ONNX with a deterministic hash fallback) is the
model to copy. Nothing before then needs a model download.

## 12f. Learning a typo, and going blind to it **[fixed; tier 2 superseded by §12l]**

The lexicon was consulted as a flat set: a word in it was known, full stop. So
one occurrence in one document taught a word permanently — and an audit of a
real store found nine of the user's own typos learned that way:
`incidentlaly`, `owrk`, `shoudl`, `kepe`, `insstead`, `metholodies`,
`programatic`, `hools`, `dont`.

That is worse than clutter. A typo in the lexicon makes the checker
**permanently blind to that exact typo**, so the tool was quietly teaching
itself not to catch this user's characteristic mistakes — the failure
compounds in the one direction that matters.

The corroboration machinery already existed and was already applied to
*export*: `sync::is_trusted` requires `provenance > observed || sources >= 2`
before writing a word into another speller. Checking never asked. It does now,
at the same bar.

**A second tier for words that shadow a common one — retired (§12l).** A
word one edit from a common *dictionary* word needed three contexts rather
than two. It skipped words under four letters, so `teh` and `hte` were learned
anyway, and it only ever delayed a typo that kept recurring. §12l replaces it
with a test against the user's own lexicon that holds however often the typo
recurs. Its measurement, for the record:

| | before | after |
|---|---|---|
| recall | 0.665 | 0.720 |
| precision | 0.939 | 0.951 |
| FP per 1k | 1.19 | 1.02 |

## 12g. The derived cue table **[measured]**

`cue.rs` shipped with a hand-written table. Derived from Google Books Ngrams
instead — 32 shards, 38.6 GB streamed, reduced to 1,050,706 bigrams — it holds
891 cues across 44 confusable words, and **real-word recall went 26.3% → 68.4%
with precision and false positives unchanged**.

### The threshold, settled by measurement

| RATIO | cues | real-word recall | precision | FP/1k |
|---|---|---|---|---|
| 40 | 941 | 0.632 | 0.951 | 1.02 |
| **150** | **891** | **0.632** | 0.951 | 1.02 |
| 500 | 879 | 0.579 | 0.951 | 1.02 |

40 and 150 measure identically, so the stricter one wins: fewer cues is less
risk on text the eval does not cover. What 150 excludes is the point —

- `even though` outnumbers `even through` 90 to 1, and `whether or` outnumbers
  `weather or` 65 to 1. Both were in the hand-written table. Both are wrong:
  `even through the night` and `the weather or the traffic` are ordinary
  English, so those cues would fire on correct text.
- `affect the` beats `effect the` 92 to 1, and `to effect the change` is
  correct if formal.

A ratio high enough to exclude them is the difference between "the runner-up
is noise" and "the runner-up is rarer but real" — which is the entire
judgement this table encodes, and not one a person holds steady across fifty
pairs. The corpus also overturned `relationship` → `causal`, where
`casual relationship` in fact outnumbers `causal relationship` three to one.

### One cue can decide several sets

`can` selects `field` for {filed, field}, `manager` for {manger, manager}, and
`there` for {their, there}. Keying the table to one word per `(cue, position)`
silently kept whichever landed first and dropped **71 of 891** cues; fixing it
recovered five points of recall. What must never happen is one cue selecting
two members of *the same* set, which would make the correction a coin flip —
asserted rather than trusted.

### Rebuilding

`data/cue-counts.tsv` is committed, so re-tuning costs seconds and no network.
It holds only the groups that could produce a cue — every row of any
(context, set) where some member clears `MIN_COUNT` — which preserves the
ratios exactly while dropping 85% of the rows. Verified byte-identical against
the full derivation. 19 MB becomes 2.8 MB, and the published crate goes from
444 KB to 1.4 MB, which is the right trade against a 38.6 GB download.

## 12h. Reaching across a filler — tried, rejected **[measured]**

Bigrams encode adjacency and nothing else, so a cue can only ever speak about
the word immediately beside the one it decides. That misses a real class:

    will affect              cue fires
    will greatly affect      cue is two away, silent

The modal still selects a verb over a noun and the adverb does not touch that,
so the linguistic signal survives the filler even though the lookup does not.
Deriving true windowed statistics is priced out — the 3-gram shard for `th`
alone is 79.9 GB — so the only cheap option was to apply the adjacency-derived
table one step further and discount the confidence.

Measured on held-out prose:

| | real-word recall | precision | false positives |
|---|---|---|---|
| adjacent | 0.684 | 0.951 | 6 |
| skip one word | 0.684 | 0.935 | **8** |

**Zero extra catches, two new false positives.** Both were the word `from` in
correct sentences — "the flag *from* above", "the package *from* the official
sources" — and the mechanism is legible: `the` is a before-cue for `form` with
a margin of 163, one of the weakest in the table. Beside the word it decides,
`the form` is right. Two words out, `the` says nothing at all.

That also disposes of the narrower variants without testing them. Restricting
the skip — to `-ly` adverbs, or to high-margin cues only — fires on a *subset*
of what the broad rule fired on, and the broad rule caught nothing extra. No
subset can catch anything either.

The general lesson is worth keeping: the weakest cues are function words, and
function words carry their meaning only in adjacency. A rule that widens the
window uniformly widens it most for the cues that can least afford it.

## 12i. Corroboration counts days, not documents **[reversed by §12l]**

A word earned its standing from the number of distinct documents it appeared
in — the rule that keeps a typo hammered into one message from counting three
times. But typos are bursty in *time* as well as within a message: you mistype
a word twice in one sitting and never again, and two documents from that
sitting satisfied the rule.

Counting distinct days instead is the cheapest honest version of "independent
contexts", and it needs no schema change — `word_sources` already carries a
timestamp per document.

On a real store it is a substantial tightening: 237 observed words qualified by
document count, 139 by day. Almost all of the difference is harmless, because
92 of the 98 are ordinary English the bundled dictionary vouches for anyway.
The real cost is six words — `urls`, `permalink`, `recency`, `asc`,
`claudomatic`, `re-word` — that are genuinely the user's and now wait a day.
Self-correcting, and cheap against nine of that user's typos having been
learned under the old rule.

It also surfaced one: `-----------------` was a lexicon entry. `is_lexical`
allowed hyphens, as it must for `re-word`, but never required a letter, so a
separator line in a captured message became a word.

**Reversed (§12l).** On a labelled sample days did worse than documents:
jargon bursts within one session just as typos do, and `first_seen` is
processing time, so a backfill collapses months into one day. Names still
count days (§12j).

**Decay is deliberately not part of this.** Forgetting a name or a word someone
still uses is the most embarrassing false positive available, and recency
should inform *confidence* rather than membership if it is ever used at all.

## 12j. Catching a misspelled colleague **[measured]**

Names are the case a dictionary can never cover, and misspelling one is the
most socially expensive error this tool could help with. They were also
completely invisible: `is_proper_noun` skips every mid-sentence capital, which
is exactly right while nothing knows any names and exactly wrong once
something does.

That skip is also what makes this safe to attempt. The check fires **only** on
tokens the checker previously refused to look at, so ordinary word handling
cannot regress by construction — and measurement agrees: recall 72.0%,
precision 95.1%, 1.02 false positives per thousand, unchanged.

### The bar, which is the strictest in the crate

The asymmetry is worse than for words. `Jon` and `John` are frequently *both*
real people, and telling someone they misspelled a colleague when they did not
is worse than saying nothing at all. So:

- **One edit, never two.** Names are short; a second edit reaches a different
  person rather than a typo of this one.
- **Exactly one candidate.** Two known people equally close means silence,
  because there is no way to tell which was meant.
- **Two separate days.** A name seen once is as likely to be the typo as the
  target, so it cannot convict anything.
- **Not already a word.** `Field`, `Green`, and `Baker` are surnames and also
  words, and the word reading wins.
- **Four characters minimum.** Initials sit one edit from half the world.

### The run is tried before its parts

`Ada Lovelacee` is matched against `ada lovelace` as a whole, because a full
name is far stronger evidence than either half — one edit in twelve characters
rather than a judgement on `Lovelacee` alone. The longest capitalized run is
tried first and shortened only on failure, which also means a common first
name never becomes individually acceptable.

Confidence follows the evidence, as everywhere else: days seen and name length,
capped below the word paths, because this is a guess about a person.

### A bundled name opening a sentence

The bundled proper names (`data/propernames.txt`) let a
capitalized `Latino` pass. Opening a sentence the capital is no evidence, and
accepting every name there hid `Kant` for `Want`, `Thai` for `That`, `Tess`
for `Test`: 12 of 15 such slips 0.6.0 caught. So at a sentence start a name is
accepted only when no core word (`frequency::CORE`) more common than it — by
SCOWL level, the one scale names and words share — sits one edit away. That
flags 93 of 6,416 bundled names at a sentence start (`Rome`, `Chile` among
them), against all of them in 0.6.0; a name the document also uses
mid-sentence, or one in the lexicon, still passes.

## 12k. A missing space, and a spurious one **[done]**

`alot` and `infact` were already *caught* — they are in no dictionary — but the
corrections offered were `lot`, `alt`, `slot` and `intact`, `infant`, `infect`.
A missing space is not an edit this model has: it treats the token as one word
and looks for another single word nearby, and no amount of that reaches
`a lot`.

Restoring the space is now tried, and offered ahead of the single-word
candidates when it exists, because both halves being real words is far less
likely by chance than one word sitting an edit away.

**Both halves must be *common*, not merely present.** A word list deep enough
to be useful holds `al`, `ot`, `ch`, and `inf` — which made `alot` ambiguous
between `a lot` and `al ot`, so it offered neither, and turned `chail` into
`ch ail`. Requiring level 35 or better fixes all three at once. Two readings
after that is a coin flip and yields nothing.

Word accuracy is unchanged: 72.0% recall, 95.1% precision, 1.02 per thousand.

### The other direction, and why it needed no corpus at all

`luke warm` for `lukewarm` looked like the harder half, and the plan was a
corpus margin: is `lukewarm` overwhelmingly more common than `luke warm`,
where `maybe` and `may be` are merely both common? That meant another pass
over 38.6 GB.

It was not needed, and the reason is worth recording because the estimate was
wrong for an instructive reason. **`luke warm` was already being flagged.**
`luke` is in no dictionary, so it is reported as an unknown word — the failure
was never detection, it was that the corrections offered were `like`, `lake`,
`lure`. Exactly the same mistake as `alot` offering `lot`.

So the fix is the mirror of the split: when a token is *already* being
flagged, try joining it with each neighbour and offer the compound if that is
a common word.

**The dangerous cases are excluded structurally rather than by a threshold.**
`may be`/`maybe`, `in to`/`into`, `read just`/`readjust`, `in form`/`inform` —
every one of them decided by the sentence rather than the pair — all have two
*known* halves. Neither token is ever flagged, so this code is never reached
for any of them. That is a far stronger guarantee than a frequency ratio would
have been, and it costs nothing.

What stays invisible is a split compound where both halves are ordinary words
and the join is nonetheless intended. Those are precisely the ambiguous ones,
and silence is the right answer.

## 12l. One trust rule, and the habitual typo **[measured]**

`sync::is_trusted` (export, `list --trusted`) asked for a deliberate source or
two distinct documents; the checker asked for two distinct days (§12i), and
three for a word one edit from a common dictionary word (§12f). Two rules, and
neither could keep out a typo the user makes every week.

There is one rule now, `trust::Trust::trusts`, and the checker, `list
--trusted` and `sync` all call it:

> a deliberate source, **or** observed in ≥2 distinct documents **and** not a
> habitual typo.

**A habitual typo** is a non-dictionary word that is an adjacent
transposition of a word the user writes ≥5× as often (length ≥3), or a
one-letter change strictly inside it — not the first or last letter — (length
≥5). `teh`/the ×658, `shoudl`/should ×131. The neighbour is any lexicon word,
whatever its provenance, and its count is the user's own.

- **Never for a deliberate source.** `trekr` is `owned`; that the user also
  writes `trek` says nothing.
- **Never for a dictionary word**, or `form` would be a typo of `from`. The
  checker would accept it from the dictionary anyway; this keeps `--trusted`
  consistent with it.
- **Edges are where vocabulary grows.** Real near misses mostly change the
  first or last letter: `jsonl`, `readmes`. Slips mostly don't.
- **Hyphenation is spelling variance**, so a hyphenated word is never vetoed.

### Why not more recurrence

A calibration study on a copy of a real store, 590 labelled observed words
with ≥2 documents (529 real, 30 typos, 31 machine junk):

- `teh` had 9 documents across 8 days; `taht` and `shoudl` 4 each. Raising
  the bar from 2 documents to 4 kept 34% of real words with precision flat.
- Days did worse than documents (§12i reversed): jargon bursts within one
  session, and `first_seen` is processing time, so a backfill is one day.
- What separates them is the neighbour the user writes far more often, and
  where the difference sits.

| rule | real kept | typos admitted | precision |
|---|---|---|---|
| ≥2 documents (export before) | 529 | 30 | 0.946 |
| ≥2 days (checker before) | 316 | 28 | 0.919 |
| **≥2 documents, not a habitual typo** | **526** | **16** | **0.970** |

Measured on the implementation, not just the study's script: `vocab list
--trusted` on the store copy reproduces the bottom row exactly. The three
real words lost are `behaviour`, `repros` and `resolvers`. Across the whole
store, the study counted 136 single-document typos the veto blocks before
they can recur.

The checker moves from days to documents with it. Run over the labelled words
themselves, it flagged 13 of 279 real words before and none after, and caught
15 of the 30 typos against 16 before: it now catches `teh`, `hte`, `taht`,
`shoudl`, `mabye`, `experiement` and `statusbuar`, and no longer catches eight
that recur on a single day with no dominant neighbour (`guage`, `relavant`,
`indicies`, …). On held-out upstream prose (`vocab eval`, five seeds, 8k
words) it measures identically, because injected typos never collide with a
learned word:

| | before | after |
|---|---|---|
| recall | 0.703 | 0.703 |
| precision | 0.923 | 0.923 |
| FP per 1k | 0.85 | 0.85 |

On the store copy, `echo 'i think teh trekr repo shoudl work' | vocab` said
"No issues found". It now flags `teh` and `shoudl` and accepts `trekr`.

Counting documents is also cheaper: `--profile` on the store copy puts the
checker's lexicon load at ~120 ms counting distinct days and ~12 ms counting
documents, veto included.

### Typo pairs **[done]**

When the veto fires it has found the word meant, so the pair is worth keeping:
`vocab typos` lists each with its evidence — the typo's count and documents,
the intended word's count, the ratio, and the edit.

**Derived on read, not stored.** Every number in a pair is already in
`lexicon`, the scan takes a quarter of a second on a real store, and a stored
copy would go stale the moment either count moved. Only what cannot be
derived is stored: **a confirmation.** `vocab typos teh the` writes the
`typos` table (schema v7). A confirmed pair holds whatever the counts later
say, which is the ratchet: `user` outranks `observed` here as it does for
words.

- **"This word is real" is `vocab add`,** not a second table. It gives the
  word `user` provenance, which the veto never touches, and drops any
  confirmation of it. `vocab typos --rm teh` forgets a confirmation without
  claiming the word.
- **Confirming is refused** (exit 2) for a word from a deliberate source,
  where it could never apply, and for a dictionary word: `form` is only wrong
  beside `apart`, which is the real-word checker's job (§8), not a blanket
  rule.

### The checker names the word meant **[done]**

A flagged word that is a known slip leads with the intended word. The pair's
`score` (also in `vocab typos`) is derived, not chosen:

- **Confirmed by hand:** 1.
- **Derived:** `ratio / (ratio + 5)` — a coin flip at the veto's own bar,
  0.91 at ten times it, 0.99 for `teh`/the. Recurrence enters only through
  the ratio, whose denominator is the typo's own count. It is not a separate
  bonus: among the labelled pairs it did not separate slips from words, since
  `behaviour` and `repros` recur in six documents each.

The suggestion list is a mixture. With probability `score` the slip explains
the token, otherwise the edit model does, so the intended word also keeps
what the edit model gave it, and the scores still sum to one. The finding's
confidence becomes the larger of the unknown-word default and the pair's
score. No field changed (`Finding`, `Suggestion`).

`teh` went from `the 0.60, ten 0.21, te 0.18` to `the 1.00`. On held-out
prose top-1 correction rose 0.826 → 0.831 with detection unchanged: a few
injected slips happen to be ones this user makes.

### `--fix`: corrections that are never guesses **[done; widened below]**

`vocab --fix` reads the positional text, `--file`, or stdin and writes it back
with corrections applied. It edits a file only with `--in-place`, and then
atomically — written beside the file and renamed over it, through a symlink to
its target, keeping its permissions, refusing a read-only file — reporting the
fixes only once the file is written. Two cases give up atomicity and write in
place: a file with other hard links, which a rename would split from them,
and one in a directory that can't take the temporary file. A rename also
drops extended attributes; that is not handled. A temporary file a crashed
run left (named for its pid, so no live process owns it) is removed first. `--file` with positional text is a usage
error (exit 2): reading one and writing the other once overwrote a file with
the argument.

- **What qualifies:** a missing apostrophe (`dont`, an exact mapping) and a
  typo pair at tier `confirmed` or `inferred` (below). A `candidate` pair is
  suggested by the checker and never written.
- **It rides the checker.** A finding becomes a fix only if
  `Checker::correction` says so, so masking (code spans and their HTML
  spellings `<code>`, `<kbd>`, `<pre>`, `<samp>`, URLs, fenced blocks and
  `<pre>` blocks, indented lines), names the document introduces, and proper nouns all carry
  over unchanged. One consequence: a document that writes `teh` in a code span
  or URL has named it, so the prose `teh` is accepted and left alone, exactly
  as the checker would. Names are learned as lines are read, so this holds
  for a prose `teh` *below* the code span and not above it: the stream decides
  a line before it has seen the rest. Checking does the same even when it
  collects, so the two never disagree about a line.
- **The typed case is kept:** `Teh` → `The`. An all-caps token is never
  checked, since it reads as an acronym (`ae`'s job), so `TEH` and `DONT` are
  left alone.
- **A fix lands only where the line still holds the word, at its column.**
  Columns are characters, and typography folding preserves them; a mismatch
  leaves the text alone.
- **Never on a word joined to code** (`text::joined_to_code`, applied in
  `fix::apply` so `--fix` and `--expand` share it): `teh.rs`, `$teh`,
  `--dont`, `foo::teh`, `teh()`, `x.teh`, `args=1`. Backticks are not the only
  way prose names code. `.` and `:` count only with a word on their far side
  (and not `teh.Next`, a missed space), brackets only as a call or an index
  (not `teh(s)`), `/` only with a path's evidence — a second `/`, a `.ext`,
  or no word across it — so `and/teh` is prose, and matching `_`/`~~` on both
  sides is emphasis. Sentence punctuation still allows a fix. `#teh` stays
  code: a heading marker takes a space, so it's a hashtag or an anchor. A
  blockquote's `>` never gets here, the line being skipped as not prose.
  Checking skips the same words (`Checker::check_line`), so check and fix
  agree; on the prose corpus, the hunt's clean texts and `vocab eval` over
  three seeds it changed no finding and no score.
- **Byte for byte otherwise.** Lines keep their own terminators (`\r\n`
  stays), and a file without a trailing newline doesn't gain one.
- **Text on stdout, what changed on stderr.** stdout carries the corrected
  text in every format, so `vocab --fix -J < a > b` works. The report goes to
  stderr: `line:col from -> to` by default, a `Fix` object per change under
  `-J` (streamed), or one array under `-j`.
- **Streams.** stdin is fixed and written line by line, asserted by timing in
  `tests/cli_e2e.rs` as `-J` is.
- **Exit 1 when anything changed**, as checking exits 1 on a finding; 0 when
  the text was already clean; 2 on an error.

### Tiers: what `--fix` may write **[done]**

Every typo pair has a tier, shown by `vocab typos` (`tier`, and `held_back`
for a candidate):

- **confirmed:** `vocab typos teh the`. Always applied.
- **inferred:** derived from the counts, a **transposition**, and nothing
  held against it. `--fix` applies it.
- **candidate:** derived, but held back. The checker leads with it; `--fix`
  leaves it alone.

`vocab add` takes a word out of all three at once: a `user` word is trusted,
so it is never flagged and never fixed. `vocab typos --rm` forgets a
confirmation, which drops the pair back to whatever the counts say.

`trust::held_back` is the whole rule, and the checker and `vocab typos` both
read it. A derived pair stays a candidate when:

- **the slip is not a transposition** (`edit`). On the store copy's 153
  derived pairs, transpositions were 65 slips to 2 real words. One-letter
  changes were 68 slips to 18 misses: 9 real words (`repros`, `codecs`,
  `resolvers`, `langs`, `linted`, `behaviour`, `automagically`, …) and 9 slips
  aimed at the wrong word (`laywer` for `layer`, `hasing` for `having`,
  `chome` for `come`). That is one wrong rewrite in five, and no
  counter-signal tried brought it near zero.
- **it is three letters and the word meant is not in the common-English core**
  (`short`). The two real words among the transpositions were `cmp` (beside
  `mcp`, ×39) and `esc` (beside `sec`). `teh` and `hte` still qualify, because
  `the` is in the core.
- **it is the British spelling** (`variant`): `-our`, `-ise`/`-isation`,
  `-yse`, `-ence`, `-ll`, and a final `-re`. The bundled list is SCOWL's US
  English and carries none of them, so without this `centre` reads as a slip
  of `center`. Patterns, not a list: a variant list would mean a second
  generated data file for one rule.

Counter-signals measured and **not** used, because the edit rule already
excludes every word they catch:

- **Plural of a word the user writes** (`repros` = repro+s): 5 real words
  among the derived pairs, no slips. It would be the first rule to add if
  one-letter changes are ever reconsidered.
- **Shorthand** (a prefix of a dictionary word the user writes, `repro` <
  `reproduce`): it does not separate. On the labelled sample it fires on 36 of
  279 real words and 3 of 30 typos. The real hits are real shorthands (`arg`,
  `param`, `perf`, `impl`, `vec`, `repo`, `repro`, `config`-shaped), but the
  expansion is often wrong (`sha` < `shape`, `foo` < `footer`, `nan` < `nanny`,
  `wal` < `walking`, `substring` < `substrate`), and it also fires on slips
  (`simplier`, `skils`, `sym`).
- **Recurrence while the neighbour is not much more frequent** is already the
  trust rule: such a word is trusted, never flagged, never fixed.

### A slip never seen **[done]**

A word with no pair at all — not in the lexicon, or seen once beside nothing
the user writes much — gets one more chance: the checker's own suggestions.
`--fix` writes the first suggestion when it is a transposition of the typed
word, holds at least half the edit model's belief (more than every other
reading together), and `held_back` has nothing against it. `waht`, `doign`,
`peopel` are fixed on a fresh install; `whta` (not a swap) and `centre` (a
variant) are not. A lexicon never seeded (a scratch `--db`) has no frequency
table, so `Store::frequencies` falls back to the embedded core list in
memory: without it every word scored alike and `teh` was never fixed. Only
the core list, so `waht`, which the mined counts settle, stays unfixed there.

The threshold is measured. Read cold (a lexicon with nothing in it), all 7,775
observed words on the store copy produced 61 such transpositions. One was a
real word, `gary` → `gray` at 0.34; the rest were slips, `wiht` at 0.36 and
`taht` at 0.38 the lowest. On injected prose no unseen fix went to the wrong
word at any threshold. Half is the bar because it is the first one with a
meaning — this reading beats all the others combined — and it clears `gary`;
it gives up `wiht` and `taht` cold, which the inferred tier fixes as soon as
the user's counts exist.

### Measured

`vocab --fix` over the labelled sample on the store copy, one word per line;
`vocab eval` over held-out prose (two corpora, five seeds, 63k words, 693
injected errors, 164 of them transpositions), on the store copy and on an
empty lexicon:

| | before | tiers | tiers + unseen |
|---|---|---|---|
| labelled real words rewritten (of 529) | 0 | 0 | 0 |
| labelled typos fixed (of 30) | 1 | 9 | 9 |
| injected errors fixed, store copy | 0 | 7 | 94 |
| injected errors fixed, empty lexicon | 0 | 0 | 86 |
| fixed to the wrong word | 0 | 0 | 0 |
| words changed in clean prose | 0 | 0 | 0 |

"Clean prose" is `tests/corpus/prose.md` and the held-out corpus, against both
lexicons; `tests/cli_e2e.rs` asserts the first. The fixes are a fraction of
injected errors because only transpositions qualify, and half of what `eval`
injects is not a transposition: 94 is 57% of the 164 injected swaps.

`--fix` over all 7,775 observed words on the store copy rewrites 79, by hand
all slips but three: `suggets` → `suggest` (probably `suggests`), `fabel` →
`fable`, and `een` → `e'en`, which is the contraction table, not this rule.

**`--confidence` waits on this.** `validity` (store.rs) is a ranking, not a
probability: a provenance constant plus a corroboration curve, never checked
against outcomes. A `--confidence 80` filter would claim "80% of these are
real" with nothing behind it. The same labelled sample is what would calibrate
it; until then the named sets (`--trusted`, `--uncommon`) are the honest
surface.

**Smaller follow-up:** a `--min-docs N` filter for exploring (a real count).
The checker's internal "trusted" set, which collided with the public
`--trusted`, is gone: it reads `Trust::deliberate`.

## 12m. Shorthand **[done]**

A shorthand is the user's own abbreviation: `repro` for reproduce, `args`,
`perf`, `impls`, `structs`. To the checker it looks like an unknown word,
and to the typo veto it can look like a slip: `repros` is one letter from
`repos`, which the user writes ten times as often (§12l lost it as a real
word for that reason).

**Membership and meaning are separate questions,** as they are for words:

- *Is it a shorthand?* Binary, and settles the checker: never flagged, never
  touched by `--fix`, never a typo however dominant its neighbour.
- *What does it mean?* Settled only where one reading fits. `means` in JSON
  is that settled reading; `maybe` is a reading it would be a guess to write.

`vocab shorthand repro reproduce` confirms one, stored in the `shorthands`
table (schema v8), one row per form. The checker, `list --trusted` and `sync`
read it through `Trust`, so a shorthand is trusted by the one rule. Confirming
a shorthand forgets a typo confirmation of the same form and vice versa: the
latest word on a form wins. Only the exact form confirmed as a typo outranks
an inflected shorthand.

The guards are `vocab typos`'s: a dictionary word is refused (`the=then`
would rewrite every sentence that meant `the`, and so would `prod`, whatever
it costs a user who writes it), and so is a word from a deliberate source
(`trekr` is a repo, not an abbreviation). The meaning is stored as typed —
`GitHub`, "as far as I know" — unless it is all caps, which is shouting and
is lowercased (`REPRODUCE`); it is written back in the case the shorthand
was typed (`Repro` → `Reproduce`).

### Inflections

Inflections are read off a confirmed form by rule, never stored:

- `repro'd`, `reproed` → reproduced; `reproing` → reproducing. A verb
  inflection reads one way.
- `args` → arguments: an -s is a plural only when the meaning is **not** also
  a verb. `repros` is "reproduces" in "it repros on main" and
  "reproductions" in "the repros", and nothing in the word says which. A
  meaning is a verb when its -ed form is a dictionary word, so `structs`,
  `vecs` and `dups` stay guesses too. That gives up some plurals; a wrong
  one would be written into the user's email.
- The meaning's inflected spelling is whichever regular spelling is a
  dictionary word (`reproduced`, `committed`, `copies`), so no spelling rules
  are encoded.
- **A dictionary word is never an inflection.** Confirming `st` must not make
  `sting` a shorthand.
- **A confirmed form overrides the rule:** `vocab shorthand
  repros=reproductions`.

### Tiers, and why there is no inferred one

Typo pairs have three tiers (§12l). Shorthand meanings have two:
`confirmed`, and `candidate` (suggested only). An inferred meaning was
measured and does not reach a bar.

On the store copy there are 35 candidates: observed words in two documents
or more, not in the dictionary, whose stem (minus -s, -es, -ed, -ing) is a
proper prefix, two letters short or more, of a dictionary word the user
writes. `vocab shorthand` lists 34 of them, leaving out `repros`, which the
typo veto still holds back until the user confirms `repro`. I labelled them by hand
(the agent, not the user): 18 shorthands, 14 not (`foo` < footer, `sha` <
shape, `clis`, `diffs`, `skils`), 3 unclear. Two signals for the meaning,
scored on the 18:

| signal | exact expansion | right word family | fires on non-shorthands |
|---|---|---|---|
| the user's most frequent word with that prefix | 6 | 11 | all 14 |
| shared bigram neighbours (cosine) | 3 | 4 | `clis`→client 0.49, higher than any real one |

Neighbour overlap is mostly empty: 8 of the 18 share no context at all with
any expansion, because the user writes the shorthand *instead of* the long
word. Frequency picks the family more often than not and the form rarely
(`repro` → reproduced, `impls` → implementation, `struct` → structural). For
`--fix` the inferred bar was no wrong rewrite in 529 real words; one right in
three is nowhere near it. Candidates carry `maybe` and are never expanded.

### `--expand` **[done]**

`vocab --expand` writes shorthands out in full, so the user can type fast and
polish before sending. It is `--fix`'s machinery with a second source of
changes, and `--fix --expand` applies both:

- **What it writes:** a confirmed form, and an inflection whose reading is
  `means`. A `maybe` is left alone and reported: `{"line","col","word",
  "kind":"shorthand","maybe":[…]}`, with no `to`. A change is a `Fix` with
  `kind: shorthand`. `-j` puts both in one array.
- **The checker's regions and names.** `Scanner::expansions` runs only on a
  line the scanner fed to the checker (not fenced, front matter, or code),
  masks code spans and URLs the same way, skips proper nouns mid-sentence,
  and skips a word the document has named (`run \`repro\``), because then it
  is a name here.
- **Case, columns, line endings, stdout, `--in-place`, per-line streaming:**
  `fix::apply` and the `--fix` loop, unchanged. The timing test covers
  `--expand` as it does `--fix`.
- **Exit 1 when anything changed.** A shorthand left alone is not a change and
  was never wrong, so on its own it exits 0.
- **A confirmed form is written wherever it appears,** whatever part of
  speech it plays there. That is the user's word, but it can be the wrong
  one: `eval = evaluation` turns "eval the enums" into "evaluation the
  enumerations". Confirm the reading used most, or confirm the forms one by
  one.

### Measured

On the store copy:

| | changes |
|---|---|
| `--expand`, no confirmations: `tests/corpus/prose.md` + held-out corpus (14k words) | 0 |
| the same, 13 shorthands confirmed (`repro`, `arg`, `param`, `perf`, `impl`, `vec`, `struct`, `repo`, `sig`, `dup`, `dev`, `enum`, `eval`) | 0 |
| checker findings on both corpora, before and after confirming | identical |

The held-out corpus holds `vec`, `struct` and `repo` 20 times, all inside
code, URLs or fenced blocks, so nothing there is prose to expand.

Over 11 sentences written in the user's register using those shorthands,
`--fix --expand` made 25 expansions and left 3 alone:

- **15 confirmed forms:** 13 right, 2 wrong, both a confirmed noun used as
  something else ("eval the enums" → evaluation, "Repro steps" →
  Reproduce).
- **10 inflections by rule** (`repro'd`, `reproed`, `args`, `params`,
  `repos`, `devs`, `impls`, `enums`, `evals`): all right.
- **Left alone:** `repros` twice, once meaning reproduces and once
  reproductions, which is why it is left alone, and `structs`.

`reproed`, flagged before confirming (`reproved 0.99`), is accepted after.
`KNOWN_COLD_START_MISSES` is unchanged.

## 12a. Performance, and the size of the backstop **[open]**

Measured with `--profile` on a 2,807-word lexicon:

| Path | Cost | Note |
|---|---|---|
| `capture` (the hook path) | ~2ms | Already fine; hooks measure ~5ms including spawn |
| `check`, all words known | ~6ms | The backstop is never read |
| `check`, one unknown word | ~60ms | Dominated by reading 236k dictionary words |

The backstop is loaded lazily for exactly this reason — it's the single
largest cost and it's usually unnecessary. What remains is two problems worth
separating:

**The backstop was the problem, and it is now bundled.** ✅

The old backstop was `/usr/share/dict/words` — on macOS, `web2`, Webster's
Second International of 1934. It was filed as a staleness problem and was
three problems at once. It is a *headword* list: `begin` and `hold` but not
`began` or `held`, `boxcar` and `boxberry` but not `box`. It is ninety years
old. And it carries no frequency data, so `smal` drew `small`, `smalm`, and
`smalt` as equals.

SCOWL levels 10–60 replace it — 102k words, bundled rather than read from the
host. The size levels *are* a frequency ranking, so one file supplies
membership and rank together, which is what `smalt` needed (it is level 70 and
no longer present at all).

The level choice is licensing before taste: UKACD, the one component with a
real condition, enters at **level 80**. Everything at or below 70 is Moby,
Brian Kelk's list, 12Dicts, 5desk, and ENABLE — all public domain — under
Atkinson's MIT-like grant. Within that safe range the cutoff was measured:

| cutoff | cold FP/1k | cold recall | warm precision |
|---|---|---|---|
| 40 | 3.95 | 0.92 | 0.88 |
| 50 | 0.88 | 0.88 | 0.93 |
| **60** | **0.66** | **0.88** | **0.94** |
| 70 | 0.44 | 0.83 | 0.95 |

60 is the knee. 70 buys 0.2 false positives per thousand words and costs five
points of recall, because obscure words absorb real typos — the dictionary-size
tradeoff, priced.

One correction it forced: a lexicon word scores 0 against any dictionary word
that now has a frequency, so ordinary English began outranking personal
vocabulary in suggestion lists — the exact inversion this tool exists to
prevent. Lexicon membership carries a frequency floor around level 35, so
genuinely common words can still win and the rare tail cannot.

Everything moved at once, which is what "four complaints, one cause" predicted:

| | before | after |
|---|---|---|
| cold-start FP/1k | 8.12 | **0.66** |
| cold-start correction rate | — | **100%** |
| warm precision | 0.92 | **0.94** |
| warm correction rate | 0.68 | **0.89** |
| dictionary load | 27.5ms | 8.5ms |
| candidates scanned, cold pass | 8.67M | 307k |
| cold pass | 1623ms | **80ms** |

Bundling also removed the last thing the crate wanted from the host, so it now
behaves identically on a machine with no word list installed.

**And it retired the stemmer.** Lookup used to peel suffixes to a base form,
because a headword list left `shipping` and `focused` unrecognized. SCOWL
carries the inflections, so the stripper stopped paying for itself and started
costing: it over-generated deliberately, and every spurious base that happened
to be a real word accepted a typo. Removing it took recall on the reference
corpus from 87.5% to **95.8%** and left the false-positive set byte-identical.

Worth stating plainly, because the intuition runs the other way: **stemming was
never free, it was subsidized** by a dictionary bad enough to need it. If a
future word list lacks inflections the morphology comes back — but measured
against `vocab eval`, not assumed.

Note this is separate from `text::normalize`, which strips possessive `'s`
before lookup and is still right: `debugger's` is a form of `debugger`, not a
word to be listed.

**Scanning every candidate was allocation, not arithmetic.** ✅

Each suggestion cost **19.7ms**, and the fix was not the interesting one.
`bounded_distance` collected the query's characters *per candidate* — the same
query, 102k times — collected the candidate's, and allocated three DP rows.
Five heap allocations against every dictionary word, half a million per
suggestion, and the length check that rejects most candidates ran after the
first two.

Hoisting the buffers into a reusable scratch and rejecting on length first took
it to **5.6ms**. The early reject uses the *byte* count, since a UTF-8 string
is never shorter in bytes than in characters, so most candidates are dismissed
without decoding at all. A cold pass over the reference corpus went 91ms to
48ms, with identical output.

**Loading the word list was hashing, not allocating.** ✅

`--profile` reported `dictionary_load` at ~8ms — most of the ~11ms floor every
check pays, for a lookup table that is identical on every run.

The obvious cause was wrong. The map was `HashMap<String, u8>` built from
`include_str!`'d text, so it allocated 102,495 strings to hold data that was
already `'static`; borrowing instead barely moved it. The cost was **hashing**
102,495 keys.

A `Vec` is 36x cheaper to build than a `HashMap`, measured. So the list is now
emitted globally sorted as `<word>\t<level>` and a lookup is a binary search —
loading is one pass with no hashing at all.

| | before | after |
|---|---|---|
| `dictionary_load` | ~8ms | ~3.8ms |
| fixed cost of a check | 11.3ms | **6.3ms** |
| cold pass over the corpus | 91ms → 48ms | **32ms** |

Output is identical: 72.0% recall, 95.1% precision, 1.02 per thousand. The
generator was updated to emit the new shape and verified to reproduce the
committed file byte for byte.

**Trigram indexing is not worth it from here.** Indexing words by character
trigram and comparing only those that share one would take 5.6ms to perhaps
1ms — but building the index over 102k words costs tens of milliseconds, which
dominates the common case of a prompt containing zero or one unknown word. It
pays only for documents thick with typos, and the allocation fix already
covers those. Banding the DP to the |i−j| ≤ 2 diagonal stays available and is
cheaper than any index.

**Deliberately not doing yet:** BK-trees and SymSpell-style deletion
neighborhoods. `vocab` is a per-invocation CLI, so any index dies with the
process — a distance-2 delete neighborhood over 102k words costs seconds to
build and tens of MB to hold, which would have to be persisted, coupling a
derived index to the schema and to dictionary-file invalidation. These earn
their keep in the Phase 5 LSP, which is a long-lived process worth amortizing
into. Not before.

**Known, deferred from the pre-release hunt:**

- *Suggestion memoization.* A document repeating one unknown word scans the
  dictionary for it every time; a per-run cache keyed on the normalized word
  would make the repeats free. Cheap, but only documents thick with one typo
  benefit.
- *A single very long token* is still quadratic somewhere on the suggestion
  path, where the line-length fixes did not reach. Bounded in practice by
  `is_checkable` rejecting most such tokens; worth a profile before a fix.
- *`content_key` folds digit runs* so a template with a session id or count
  dedupes, which also makes "issue 123" and "issue 456" one capture. A lost
  sighting, not a wrong one; folding only long digit runs (ids, dates) would
  keep short numbers distinct.

## 12b. Cold start, and what didn't work **[open]**

Context-aware correction needs collocation evidence, and a fresh corpus has
none. Two approaches were tried; only one survives.

**Frequency alone does not work.** Knowing `from` is far more common than
`form` seems like enough to be suspicious, and it isn't. The quantity we want
is P(you meant `from` | you typed `form`), and the frequency prior has to beat
the typo rate to matter: P(typing `form` while meaning `from`) is maybe 1-in-50,
while P(typing `form` while meaning `form`) is ~1. The posterior favors
"correct as written" unless the frequency gap is enormous — and the test fires
on *every* occurrence of the rarer word regardless of the sentence. Built,
tested against this project's own README, where it flagged "the apostrophe
form usually isn't…", and removed.

**Discriminating collocates do work**, because they carry context. A small
bundled table for the top ~50 confusions — `from` follows `apart`, `away`,
`far`, `different`; `form` follows `the`, `a`, `fill in`, `order` — is a few
hundred bytes per pair, works on day one, and stays silent where it has
nothing to say. Personal collocations then supersede it, which is the same
inversion the whole tool rests on: your evidence over the general prior.

**Derived confusables.** The hardcoded confusion sets should become
edit-distance-1 neighbors derived from the dictionary, so the set scales past
what anyone would enumerate. That only identifies *candidates*, though — the
collocate table is still what decides between them.

## 12c. Code and comments **[open]**

Prose inside code is a natural target — comments, docstrings, commit
messages — and it's where a personal lexicon pays off most, since comments are
dense with the identifiers and jargon a general checker flags.

Code *lines* are a different problem and mostly not spell-checking: they're
references to things that exist, in formats (`snake_case`, `camelCase`,
`SCREAMING_SNAKE`) the tokenizer already declines to judge. But there may be a
narrower play with code-specific semantics — a misspelled identifier is
detectable *relative to the identifiers that exist in this project*, which is
a symbol-table question rather than a dictionary one, and one an LSP is
already positioned to answer.

Sequencing: this lands after the VS Code integration and the LSP, which is
where the parse tree needed to separate comment from code already lives.

## 12d. Where precision actually goes **[measured]**

Measured against held-out technical prose — four upstream READMEs never mined
into any lexicon, corrupted at known positions by `vocab eval`:

| | before | after |
|---|---|---|
| precision | 0.77 | **0.92** |
| recall | 0.63 | 0.63 |
| false positives | 30 | 9 |

Precision is the number this project's first principle cares about, and 0.77
didn't clear it. What made the difference was not a better dictionary. Sampled,
the false positives ran `mdbook`, `repology`, `burntsushi`, `ugrep`,
`zstandard`, `winget-pkgs`, `nixpkgs`, `voidlinux` — **fourteen of fifteen were
never words**. Growing the word list was next on the roadmap for precision and
would have addressed one of them.

Two things fixed it, neither of them a data source.

**A masking defect.** `mask_non_prose` located URLs by asking `iriq` for them
and then searching the line for the string it returned — which works only while
canonicalization is a no-op. It isn't: `voidlinux.org/p/?a=1` comes back as
`.../p?a=1`, matches nothing, and the whole URL stays in the text. Using the
extractor's `original` span instead recovered 9 of the 30. Repeated URLs — the
markdown badge shape, image and link naming the same target — were also masked
only once.

**Names the document gives you.** A README introduces a tool in a table row, an
install command, or a link, and only then writes a sentence about it. Those
regions are already located, because masking exists to throw them away; keeping
what was removed turns the discarded half of every line into a name list.
`names.rs` accumulates it — from masked spans on prose lines, from every token
on lines that aren't prose at all, and from proper nouns the check loop already
identifies — and an unknown word matching one is accepted.

This is an accept-only rule consulted *after* the lexicon, the mined corpus,
and the dictionary have all declined, which is why a common word landing in the
set by accident is harmless. It cost nothing in recall: 0.6335 before and after,
across all three changes.

It accumulates rather than pre-scanning, so a stream and a file behave the same
way. The cost is a name introduced *below* its first prose mention — `# axum`
as the opening heading — which stays unrecognized. A pre-pass would fix it for
`--file` and the positional argument while leaving piped stdin different, and
that inconsistency is worse than the residual.

**What's left is the dictionary after all**, but only now that names are gone:
six of the nine survivors are ordinary modern English — `iterator`, `sidebar`,
`systemwide`, `modularity` — absent from a 1934 Webster's. That is the
frequency-list work, and it is now the whole remaining term.

Real-word errors remain essentially unmeasured — the injector produced one and
the checker caught none. That's the cold-start problem in §12b, and it means
recall describes single-character damage only.

## 12e. Bundled data: what is generated, what ships **[decided]**

Two files in `data/` are generated, committed, and embedded with
`include_str!`. Both are English-only and deliberately so — accepting `colour`
*and* `color`, or one language's vocabulary inside another's, means never
flagging either.

| File | Built by | Source | License |
|---|---|---|---|
| `wordlist.txt` (982 KB) | `script/build-wordlist.sh` | SCOWL levels 10–60, `english-*` + `american-*` | public-domain sources under an MIT-like grant |
| `propernames.txt` (140 KB) | `script/build-wordlist.sh` | SCOWL levels 10–60, `english-upper` + `american-upper` | the same |
| `cues.txt` | `script/build-cues.sh` | Google Books Ngrams, `eng-all` 2-grams | CC BY 3.0 |

**The split is the point.** Deriving the cue table streams about 39 GB and
takes twenty minutes. Doing that on every install, or asking each user to do
it, would be absurd — so it happens once, here, behind `make data`, and the
*answer* is what ships. A build from crates.io needs no network at all. The
generators are the only things in this repo that touch it.

### On compressing the data

Measured rather than assumed, because the intuition points the wrong way:

| | size |
|---|---|
| `wordlist.txt` raw | 982 KB |
| gzip -9 | 288 KB |
| front-coded, no dependency | 497 KB |
| **published `.crate`** | **444 KB** |
| binary | 5.1 MB |

Compression buys nothing for **publishing**: the `.crate` is a gzipped tarball,
so the word list is already compressed in transit at no cost to us. What it
would buy is roughly 700 KB of **binary**, in exchange for a decompression
dependency and more time on a path that already spends 8.5ms loading the list.
For a tool whose hook budget is milliseconds, that is the wrong trade at a size
nobody is complaining about.

If binary size ever does matter, the better lever is not compression: it is
avoiding the 102k `String` allocations by binary-searching the embedded text in
place, which costs nothing at build time and *removes* the load entirely.
Front-coding is the fallback after that, at half the size and no dependency.

## 13. Roadmap

**Phase 1 — seed + check** ✅
`frequency.source` is the cautionary case: it was added to the create block at
schema 6 with no matching `ALTER`, and because `user_version` is stamped after
`migrate` regardless of what it did, existing stores reported schema 6 while
missing a schema-6 column. `split_frequency_by_source` therefore tests for the
column rather than trusting the stamp, and rebuilds the table because a
widening primary key is beyond `ADD COLUMN`.

Store schema, provenance model, ground-truth seeding, the conservative checker,
real-word mechanism, watermark filtering, `--json`/`--ndjson` throughout.

**Phase 2 — capture** ✅
Hooks in the myclaude plugin: `UserPromptSubmit` (prompt register),
`PostToolUse` filtered to outbound tools only (`create_draft`,
`slack_send_message`, `gh pr create`, `git commit`). Backfill via
`vocab capture` for existing repos and sent mail. Concurrency is handled by SQLite's
`BEGIN IMMEDIATE` plus a busy timeout rather than the leader election `ae`
uses: the writes here are short and infrequent, so a lock is enough and a
socket would be machinery without a purpose. A hook waits 250 ms on that
lock rather than the 5 s other commands do, and anything that stops it opening
the store — the lock, a newer schema, an unreadable path — skips the event
and exits 0: Claude Code reads a prompt hook's exit 2 as "block", which erases
what the user typed, and a lost capture costs one sighting. That bounds the
writes too: the Stop hook processes each spool row in one transaction, at
roughly 5 ms per KB, so a hook-captured body is cut to 16 KB (at a word) to
keep any row inside a capture's 250 ms. What it cuts is a paste, not prose
typed. A failed claim logs a warning. `vocab capture` and `ingest` are not
cut: a long document there is deliberate, and still holds the lock as long
as it takes the Stop hook to process it.

**Phase 3 — precision** ✅
The frequency-ranked word list landed and took the other three complaints with
it (§12a).
Name detection ✅ — precision 0.77 → 0.92 at no cost in recall (§12d).
Bundled collocate cues ✅ — real-word recall 0% → 26% on a corpus with no
history, no new false positives (§12b).
Bundled word list ✅ — cold-start false positives 8.1 → 0.7 per thousand
words, correction rate 0.68 → 0.89, and a cold pass 20x faster (§12a).

**Phase 4 — export + profile**
`vocab sync` to the Tier 1 targets. The stylometry pass and the linguist persona
that renders a voice document from lexicon + collocations + exemplars.

**Phase 5 — LSP + generative**
Language server with an add-to-lexicon code action. Embedding-backed
substitution for drafting.

## 14. Non-goals

- **A daemon.** Leader election on demand, same as `ae`; an idle leader cleans
  itself up.
- **Network calls.** Ever. The dictionary is local, the corpus is local, and
  no model is required to run.
- **Multi-user or synced lexicons.** Single machine, single person. The whole
  premise is that the vocabulary is *yours*.
- **Grammar checking.** Different problem, different tool.
