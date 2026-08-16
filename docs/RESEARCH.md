# Research notes

Prior art this project draws on, methods it uses, and the data sources it
could use but doesn't yet. Kept separate from [PLAN.md](PLAN.md), which is the
design contract — this is the reading behind it.

Where a decision in the code has a name in the literature, it's noted, because
several were arrived at by reasoning from a failing example and only afterward
recognized as a known result. That's worth flagging: reinvention is fine, but
the literature usually carries the next three steps too.

---

## 1. Spelling correction

### The noisy channel — the framework we half-use

**Kernighan, Church & Gale (1990)**, *A spelling correction program based on a
noisy channel model*. Pick the correction maximizing

```
P(correction | typo)  ∝  P(typo | correction) × P(correction)
```

— an **error model** times a **language model**. This is the canonical frame
for the whole problem.

We use it in one place and not another. It's exactly the argument that killed
frequency-only confusion detection (see PLAN §12b): the frequency prior has to
beat the typo rate, and it doesn't. But suggestion ranking is still a
lexicographic sort — `(distance, frequency, prefix, suffix)` — which is a
crude approximation of the same product. `frequency` is `P(correction)`;
`distance` is a stand-in for `P(typo | correction)`.

**Open:** replace the sort with an actual product. Uniform edit costs are a
weak error model, but even `P(correction) × exp(-distance)` is better founded
than tie-breaking, and it makes confidence scores comparable across findings
instead of ordinal.

### Error types

**Damerau (1964)**, *A technique for computer detection and correction of
spelling errors*. Roughly 80% of misspellings are a single insertion,
deletion, substitution, or transposition. This is why `MAX_EDIT_DISTANCE = 2`
is a reasonable ceiling, and why transposition must cost **one** edit —
charging two put `avoid` further from `aviod` than `avid`, which is how the
bug was found. `eval.rs` injects exactly these four, plus real-word swaps.

**Brill & Moore (2000)** improved the error model from single characters to
string-to-string edits (`ph`→`f`, `ent`→`ant`) learned from data — a better
`P(typo | correction)` than uniform edit distance. Requires a corpus of real
typo/correction pairs, which the eval harness could bootstrap.

**Whitelaw et al. (2009)** built a spellchecker with no dictionary at all,
learning both models from web text. The extreme form of this project's thesis:
the corpus is the authority.

**Norvig**, *How to Write a Spelling Corrector* — the readable modern
statement of the noisy channel, and the source of the widely used `count_1w`
frequency list.

### Candidate generation

**BK-trees** (Burkhard-Keller, 1973) and **SymSpell** (deletion neighborhoods)
both beat a linear scan. Deliberately not used — see PLAN §12a: `vocab` is a
per-invocation CLI, so any index dies with the process, and a distance-2
delete neighborhood over 236k words costs seconds to build and tens of MB to
hold. They earn their keep in a long-lived process, which is what the Phase 4
LSP would be.

### Real-word errors — the target architecture

**Golding & Roth (1999)**, *A Winnow-based approach to context-sensitive
spelling correction*. Confusion sets, plus **two** feature types: context
words within ±k, and collocations (patterns of words/tags immediately around
the target). Learned weights over both. Their result is the direct answer to
"is there one best signal, or an array of them?" — the combination beats
either feature alone.

Two things follow. It supports an ensemble over a strict precedence chain,
which is worth knowing since the current design prefers precedence for
explainability and abstention. And it is **supervised**, which is precisely
why `vocab eval` had to exist first: without labels there is nothing to learn
weights from and no way to tell whether the ensemble helped.

**Mays, Damerau & Mercer (1991)** did real-word correction with trigram
language models — pure context, no confusion sets. More general, hungrier for
data.

---

## 2. Collocations

**Church & Hanks (1990)** introduced pointwise mutual information for word
association. PMI over-rewards rare pairs, which matters enormously in a
personal corpus where almost everything is rare.

**Dunning (1993)**, *Accurate methods for the statistics of surprise and
coincidence*. Log-likelihood ratio (G²) is well-behaved at low counts where
PMI and chi-square are not. That's the regime a personal corpus lives in
permanently, which is why `ngram.rs` uses G² and why `vocab phrases` ranks
`small focused` above pairs of equal frequency.

**Sinclair** and the COBUILD tradition established the **±4/±5 token
collocation window**. Current code uses strict adjacency (bigrams), which is
right for phrase ranking and too narrow for disambiguation: `fill in the form`
puts the deciding cue three words away. See PLAN §12b.

---

## 3. Stylometry

**Mosteller & Wallace (1964)**, the Federalist Papers — function-word
frequencies plus Bayesian inference. The founding work, and the reason
function words rather than content words are the backbone of authorship work:
they're frequent, topic-independent, and below conscious control.

**Burrows's Delta (2002)** — z-scored function-word frequencies compared by
distance. Still the standard for authorship attribution, and it needs only
word frequencies, which `word_registers` already stores. Relevant if the
attribution work in PLAN §14 is ever taken up.

**Zipf's law** — frequency falls roughly as 1/rank. Used directly by
`frequency::zipf_count` to give the embedded core plausible magnitudes.

**Heaps' law** — vocabulary grows sublinearly with corpus size. This is the
formal reason type-token ratio falls as a text lengthens, and therefore why
`analyze` leads with **Guiraud's R** (`types/√tokens`) instead.

**MTLD** and **vocd-D** are better lexical-diversity measures than Guiraud,
which is itself only a partial correction. Worth adopting if diversity numbers
ever carry real weight.

**Flesch Reading Ease** is used with its known caveat: the syllable count is a
heuristic, so it's a trend line, not a grade.

---

## 4. Word lists and frequency data

The current backstop is `/usr/share/dict/words`, which on macOS symlinks to
**`web2` — Webster's Second International, 1934**. It has no `inline`,
`download`, `roadmap`, or `pre`. This is not a small problem: in the first
`vocab eval` run, *every single false positive* was a modern word web2 lacks
(`bigram`, `stylometry`, `emoticon`, `textarea`, `contenteditable`,
`hardcoded`).

Verified on this machine: no hunspell or aspell dictionaries are installed,
`scowl` is not available as a Homebrew formula, and `hunspell` in brew is the
tool without any dictionary. web2 really is all there is locally.

### Candidate sources

| Source | What it is | Fit |
|---|---|---|
| **SCOWL** | Spell Checker Oriented Word Lists (Atkinson). The upstream from which hunspell/aspell English dictionaries are generated. Size-tiered (10k…95k), permissively licensed. | Best *membership* source. Modern, maintained, and the size tiers map directly onto a "how rare a word do we accept" dial. |
| **hunspell en_US** | What LibreOffice and Firefox ship. | Modern, but a separate download, and still weak on technical vocabulary. |
| **wordfreq** (Speer) | Frequencies from Wikipedia, subtitles, news, books, social. Apache-2.0. | Best *frequency* source; multi-source blending avoids any single register's skew. |
| **Norvig `count_1w`** | Top ~333k words with counts, from the Google Web Trillion Word Corpus. Plain TSV. | Simplest thing that works. Trivially parseable, no dependency. |
| **Google Books Ngrams** | Enormous, book-derived. | Skewed formal and historical — the same failure mode as web2, differently. |
| **Wikipedia dumps** | Derivable, but ~20GB, and encyclopedic vocabulary is proper-noun heavy. | Poor effort-to-value. Pre-computed frequency lists derived from it are the usable form. |

### The recommendation

**One artifact should replace two.** A frequency-ranked list *is* a dictionary
— membership becomes "ranked above position N" — plus the ranking signal we
already need. Bundling a top ~50k list (roughly 500KB) would replace both web2
as the backstop and the hand-written 300-word core in `frequency.rs`, while
keeping the offline guarantee absolute.

Keep web2 as a **third tier**, not a replacement: a 50k list will flag rare
but genuine words (`perspicacious`), and the long tail is exactly what a large
old dictionary is still good for. The lookup order becomes

```
lexicon → frequency list → web2 → unknown
```

which is the shape `knows_atom` already has.

**Before shipping any of this, check the license.** Bundling word-list data
into a published crate has implications that a link does not, and none of the
licenses above have been verified against the actual distributions.

---

## 5. Where the literature says to go next

1. **Noisy-channel ranking** — replace the lexicographic sort with a product.
   Well-understood, self-contained, makes confidences comparable.
2. **A modern word list** — the single biggest measured source of false
   positives.
3. **Golding & Roth for confusion detection** — context words *and*
   collocations with learned weights, now that there's a harness to train and
   validate against.
