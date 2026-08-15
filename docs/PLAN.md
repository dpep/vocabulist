# vocabulist — project plan

A live personal dictionary, learned from the words you actually use.

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

Capture stages text in a `spool` table. Processing derives counts from it. The
raw text is then **dropped** — the row survives with its body emptied, so
dedup and provenance still work.

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

**Tier 1 — write the file.** Several checkers keep their personal dictionary as
a newline-delimited file. A `sync` subcommand exports one lexicon to N targets,
idempotently. This is how you "upgrade the built-in checker" for a fraction of
the cost of replacing it.

- VS Code / `cSpell` — via `cSpell.customDictionaries` pointing at a file we
  own, rather than writing into the user's JSONC settings
- macOS `~/Library/Spelling/LocalDictionary` — feeds `NSSpellChecker`, so Mail,
  Notes, TextEdit, and Safari all benefit. Read at app launch, so
  eventually-consistent
- Chromium `Custom Dictionary.txt` — covers Chrome, Slack desktop, and every
  web textarea. Chrome rewrites the file on exit, so writes are best-effort

Every export is a **lossy projection** — a flat membership set. The rich store
stays rich; the dumbest consumer must not shape the schema.

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

## 13. Roadmap

**Phase 1 — seed + check** ✅
Store schema, provenance model, ground-truth seeding, the conservative checker,
real-word mechanism, watermark filtering, `--json`/`--ndjson` throughout.

**Phase 2 — capture**
Hooks in the myclaude plugin: `UserPromptSubmit` (prompt register),
`PostToolUse` filtered to outbound tools only (`create_draft`,
`slack_send_message`, `gh pr create`, `git commit`). Backfill via
`vocab capture` for existing repos and sent mail. Leader election over a Unix
socket, copied from `ae/src/ipc.rs` — no daemon to manage.

**Phase 3 — export + profile**
`vocab sync` to the Tier 1 targets. The stylometry pass and the linguist persona
that renders a voice document from lexicon + collocations + exemplars.

**Phase 4 — LSP + generative**
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
