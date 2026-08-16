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
for `causal`, `pubic` for `public`. A dictionary is **structurally blind** to
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
- **macOS `~/Library/Spelling/LocalDictionary`** — feeds `NSSpellChecker`, so
  Mail, Notes, TextEdit, and Safari all benefit. Read at app launch, so
  eventually-consistent rather than live.
- **Chromium `Custom Dictionary.txt`** — deferred. It would cover Chrome, Slack
  desktop, and every web textarea, but the file carries a trailing
  `checksum_v1` line and Chrome is understood to discard the dictionary when
  that doesn't match, so a naive append may silently disable it. **Verify
  before implementing** — this is the one target that can fail closed and
  silently.

**Uninstall must be exact.** These files are shared with words the user added
themselves — macOS writes there on every "Learn Spelling". So each install
records a sidecar manifest of precisely what it wrote, and uninstall removes
only those lines. Sentinel comment lines would be simpler but pollute a file
where every line is treated as a word.

Every export is a **lossy projection** — a flat membership set, filtered to
what an ordinary dictionary doesn't already know. The rich store stays rich;
the dumbest consumer must not shape the schema.

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

**Deliberately not doing yet:** BK-trees and SymSpell-style deletion
neighborhoods. `vocab` is a per-invocation CLI, so any index dies with the
process — a distance-2 delete neighborhood over 236k words costs seconds to
build and tens of MB to hold, which would have to be persisted, coupling a
derived index to the schema and to dictionary-file invalidation. These earn
their keep in the Phase 5 LSP, which is a long-lived process worth amortizing
into. Not before.

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

## 13. Roadmap

**Phase 1 — seed + check** ✅
Store schema, provenance model, ground-truth seeding, the conservative checker,
real-word mechanism, watermark filtering, `--json`/`--ndjson` throughout.

**Phase 2 — capture** ✅
Hooks in the myclaude plugin: `UserPromptSubmit` (prompt register),
`PostToolUse` filtered to outbound tools only (`create_draft`,
`slack_send_message`, `gh pr create`, `git commit`). Backfill via
`vocab capture` for existing repos and sent mail. Concurrency is handled by SQLite's
`BEGIN IMMEDIATE` plus a busy timeout rather than the leader election `ae`
uses: the writes here are short and infrequent, so a lock is enough and a
socket would be machinery without a purpose.

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
