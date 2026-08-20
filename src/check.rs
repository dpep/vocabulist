//! The checker: decide which words in a piece of text look wrong.
//!
//! The asymmetry drives every decision here. A false "misspelled" is
//! expensive — it trains you to ignore the squiggle, and once you do, the
//! tool is dead. A missed typo costs almost nothing. So the default answer is
//! *accept*, and a word has to work to get flagged.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::names::Names;
use crate::ngram;
use crate::profile::Profile;
use crate::text::{self, Token};
use crate::types::{Finding, FindingKind, Suggestion};

/// Maximum edit distance we'll suggest across.
const MAX_EDIT_DISTANCE: usize = 2;
/// Most suggestions to return per finding.
const MAX_SUGGESTIONS: usize = 3;
/// How often a word must appear in mined prose before it counts as real.
///
/// Measured on a held-out corpus, precision rises as this falls — 86% at 3,
/// 89% at 2, 91% at 1 — with recall flat. Two rather than one anyway: the
/// injections can't measure the risk that actually matters here, because a
/// synthetic typo never appears in local prose while a real one in someone's
/// README does. Requiring corroboration is the same principle the rest of the
/// store uses, and the two-point precision difference doesn't buy out of it.
/// Frequency credited to a lexicon word that no general source ranks.
const LEXICON_FLOOR: i64 = 3;

/// Shortest run worth judging as a name. Initials and two-letter tokens sit
/// one edit from half the world.
const MIN_NAME_LENGTH: usize = 4;

/// Separate days a person must have been seen on before their spelling can
/// convict another. A name typed once is as likely the typo as the target.
const NAME_CORROBORATION: i64 = 2;

/// Distinct documents an observed word needs before it stops being checked.
///
/// One sighting is not evidence of a word — it is equally evidence of a typo,
/// and typos are bursty within a single document. Two independent contexts is
/// the same bar `sync` already applies before exporting a word to another
/// speller, and it should not have been lower here: a word learned from one
/// occurrence makes the checker permanently blind to that spelling.
const MIN_SOURCES: i64 = 2;

/// What a word needs when it sits one edit from a common English word.
///
/// `owrk`, `shoudl`, `kepe`, `insstead` — a chronic misspelling looks exactly
/// like new vocabulary from the inside, and the thing that distinguishes them
/// is that the misspelling shadows a word the writer already knows. Demanding
/// more corroboration there costs a little recall on genuinely new jargon that
/// happens to resemble a common word, and buys back the typos this tool was
/// otherwise teaching itself to ignore.
const SHADOW_SOURCES: i64 = 3;

/// How common the shadowed word must be for the suspicion to apply. Level 35
/// is roughly the top 50k words — resembling something obscure says nothing.
const SHADOW_LEVEL: u8 = 35;

const MIN_CORPUS_EVIDENCE: i64 = 2;

/// How much less likely each additional edit is. Rough, and rough is enough:
/// the point is that a two-edit correction should lose badly to a one-edit
/// correction of comparable frequency.
const EDIT_PENALTY: f64 = 0.05;

/// Bonus when the typo's letters all appear, in order, inside the candidate —
/// meaning the correction is pure insertion.
///
/// Uniform edit cost is the weakest part of this model, and this is the
/// cheapest useful correction to it. `plese` → `please` inserts a letter;
/// `plese` → `these` substitutes `p` for `t`, keys nowhere near each other.
/// Both are one edit, but dropping a letter is a far commoner slip than
/// striking a key across the keyboard — and without this, the more frequent
/// word wins regardless of how implausible the edit was.
const SUBSEQUENCE_BONUS: f64 = 25.0;

/// Below this share of the belief, a suggestion is noise rather than an
/// option. The top candidate always survives regardless.
const MIN_SUGGESTION_SCORE: f32 = 0.02;

/// The resolved word sets a check runs against. The lexicon is the authority;
/// the system dictionary is the floor beneath it.
pub struct Checker {
    lexicon: HashSet<String>,
    /// General-English frequency, for breaking ties the dictionary can't.
    frequency: std::collections::HashMap<String, i64>,
    /// Loaded on first miss, not up front. Reading ~236k words costs tens of
    /// milliseconds, and text whose words are all in the lexicon never needs
    /// it — which is the common case once the lexicon is seeded.
    /// Merely-observed words, and how many distinct documents back each.
    /// Kept apart from `lexicon` because they have not earned the same trust.
    observed: std::collections::HashMap<String, i64>,
    /// Lexicon words that came from a source containing nothing but names —
    /// repos, taps, installed binaries, dependency manifests.
    from_naming_source: HashSet<String>,
    /// People seen in captured messages: normalized key to (days seen,
    /// display form). A full name is one entry.
    people: std::collections::HashMap<String, (i64, String)>,
    dictionary: OnceCell<Option<crate::dict::Dictionary>>,
    /// Contractions derived from the dictionary and your lexicon, beyond the
    /// static table. Lazy, because building it needs the dictionary and the
    /// static table already covers the common cases.
    derived_contractions: OnceCell<std::collections::HashMap<String, String>>,
    profile: Rc<Profile>,
}

impl Checker {
    /// A checker over an explicit backstop — used by tests, which supply a
    /// fixture dictionary rather than the bundled one.
    pub fn new(lexicon: HashSet<String>, dictionary: Option<crate::dict::Dictionary>) -> Self {
        let cell = OnceCell::new();
        let _ = cell.set(dictionary);
        Self {
            lexicon,
            frequency: std::collections::HashMap::new(),
            observed: std::collections::HashMap::new(),
            from_naming_source: HashSet::new(),
            people: std::collections::HashMap::new(),
            dictionary: cell,
            derived_contractions: OnceCell::new(),
            profile: Rc::new(Profile::disabled()),
        }
    }

    /// A checker that loads the bundled word list on demand.
    pub fn with_profile(lexicon: HashSet<String>, profile: Rc<Profile>) -> Self {
        Self {
            lexicon,
            frequency: std::collections::HashMap::new(),
            observed: std::collections::HashMap::new(),
            from_naming_source: HashSet::new(),
            people: std::collections::HashMap::new(),
            dictionary: OnceCell::new(),
            derived_contractions: OnceCell::new(),
            profile,
        }
    }

    /// Attach the people seen in captured messages, with their corroboration.
    pub fn with_people(mut self, people: std::collections::HashMap<String, (i64, String)>) -> Self {
        self.people = people;
        self
    }

    /// Mark which lexicon words arrived from a naming source.
    pub fn with_naming_sources(mut self, names: HashSet<String>) -> Self {
        self.from_naming_source = names;
        self
    }

    /// Is this a name rather than an ordinary word?
    ///
    /// Provenance proposes and the dictionary disposes: a dependency called
    /// `parser` really is the ordinary word, so anything the dictionary knows
    /// is a word however we came by it.
    fn is_name(&self, word: &str) -> bool {
        self.from_naming_source.contains(word)
            && !self
                .dictionary()
                .is_some_and(|d| crate::dict::contains(d, word))
    }

    /// Attach merely-observed words with their corroboration counts.
    pub fn with_observed(mut self, observed: std::collections::HashMap<String, i64>) -> Self {
        self.observed = observed;
        self
    }

    /// Attach general-English frequencies, used to rank suggestions and to
    /// judge confusions before personal evidence exists.
    pub fn with_frequency(mut self, frequency: std::collections::HashMap<String, i64>) -> Self {
        self.frequency = frequency;
        self
    }

    /// How common a word is in general English.
    ///
    /// The mined/embedded frequency table answers first; where it is silent,
    /// the bundled dictionary's SCOWL level does. That level is a real
    /// frequency signal — 10 is roughly the thousand most common words, 60 the
    /// rare tail — and it covers the whole list rather than the fraction that
    /// happens to appear in local prose, which is what left `smalt` ranked
    /// level with `small`.
    ///
    /// Scaled to sit below anything actually observed, so evidence from real
    /// corpora still wins over a coarse general prior.
    fn frequency_of(&self, word: &str) -> i64 {
        if let Some(n) = self.frequency.get(word).copied()
            && n > 0
        {
            return n;
        }
        let general = match self.dictionary().and_then(|d| crate::dict::level(d, word)) {
            // Invert: lower level means more common. Level 10 -> 6, level 60
            // -> 1, and unknown -> 0.
            Some(level) => (70 - level as i64) / 10,
            None => 0,
        };
        // A word in your lexicon is common *in your writing*, which is the
        // whole premise. Without a floor it scores 0 against any dictionary
        // word, and ordinary English would outrank your own vocabulary in
        // every suggestion list — the exact inversion this tool exists to
        // prevent. The floor sits around level 35, so the genuinely common
        // words can still win and the rare tail cannot.
        if self.lexicon.contains(word) {
            return general.max(LEXICON_FLOOR);
        }
        general
    }

    /// Contractions this installation knows about beyond the static table.
    fn derived_contraction(&self, word: &str) -> Option<&str> {
        self.derived_contractions
            .get_or_init(|| {
                let dictionary = self
                    .dictionary()
                    .into_iter()
                    .flatten()
                    .map(|(w, _)| w.as_str());
                let known = self.lexicon.iter().map(String::as_str).chain(dictionary);
                crate::contraction::derive(known, |w| self.knows_atom(w))
            })
            .get(word)
            .map(String::as_str)
    }

    /// Does this sit one edit from a word common enough to have been meant?
    ///
    /// Generates the edit-distance-1 neighbourhood and looks each candidate up,
    /// rather than scanning the dictionary: a few hundred hash lookups against
    /// a hundred thousand comparisons.
    fn shadows_a_common_word(&self, word: &str) -> bool {
        let Some(dict) = self.dictionary() else {
            return false;
        };
        let common = |candidate: &str| {
            crate::dict::level(dict, candidate).is_some_and(|level| level <= SHADOW_LEVEL)
        };
        let chars: Vec<char> = word.chars().collect();
        if chars.len() < 4 {
            // Short words are one edit from half the language; the test says
            // nothing about them.
            return false;
        }

        for i in 0..chars.len() {
            // Deletion.
            let mut candidate: Vec<char> = chars.clone();
            candidate.remove(i);
            if common(&candidate.iter().collect::<String>()) {
                return true;
            }
            // Transposition of this pair.
            if i + 1 < chars.len() {
                let mut candidate = chars.clone();
                candidate.swap(i, i + 1);
                if common(&candidate.iter().collect::<String>()) {
                    return true;
                }
            }
            // Substitution.
            for letter in b'a'..=b'z' {
                let letter = letter as char;
                if letter == chars[i] {
                    continue;
                }
                let mut candidate = chars.clone();
                candidate[i] = letter;
                if common(&candidate.iter().collect::<String>()) {
                    return true;
                }
            }
        }
        // Insertion, at every position including the end.
        for i in 0..=chars.len() {
            for letter in b'a'..=b'z' {
                let mut candidate = chars.clone();
                candidate.insert(i, letter as char);
                if common(&candidate.iter().collect::<String>()) {
                    return true;
                }
            }
        }
        false
    }

    /// Is this a misspelling of someone we know?
    ///
    /// Deliberately the strictest test in this crate, because the cost is
    /// asymmetric in a way word corrections are not: telling someone they
    /// misspelled a colleague's name and being wrong is worse than missing it,
    /// and `Jon` and `John` are frequently *both* real people.
    ///
    /// So: one edit only, never two. The candidate must be unknown as a word
    /// and unknown as a person. Exactly one known person may be that close —
    /// ambiguity is silence. And that person must have been seen on at least
    /// two separate days, so a name typed once cannot convict a name typed
    /// twice.
    fn misspelled_name(&self, candidate: &str) -> Option<(String, f32)> {
        if candidate.chars().count() < MIN_NAME_LENGTH {
            return None;
        }
        // Already a person, or an ordinary word used as a name — `Field`,
        // `Green`, `Baker` are all surnames and all words.
        if self.people.contains_key(candidate) || self.knows(candidate) {
            return None;
        }

        let mut matches = self
            .people
            .iter()
            .filter(|(_, (days, _))| *days >= NAME_CORROBORATION)
            .filter(|(known, _)| bounded_distance(candidate, known, 1).is_some());

        let (_, (days, display)) = matches.next()?;
        // Two people equally close means we cannot tell which was meant.
        if matches.next().is_some() {
            return None;
        }

        // Evidence creates confidence: a name seen across many days, and a
        // longer name where a single edit is a smaller share of the whole,
        // are both firmer ground. Capped well below the word paths — this is
        // a guess about a person.
        let corroboration = ((*days as f32) / 8.0).min(1.0);
        let length = (candidate.chars().count() as f32 / 12.0).min(1.0);
        let confidence = 0.40 + 0.25 * (0.5 * corroboration + 0.5 * length);
        Some((
            display.clone(),
            crate::types::round(confidence as f64, 2) as f32,
        ))
    }

    /// The backstop, loading it on first use.
    fn dictionary(&self) -> Option<&crate::dict::Dictionary> {
        self.dictionary
            .get_or_init(|| {
                let loaded = self.profile.time("dictionary_load", crate::dict::load);
                self.profile.count(
                    "dictionary_words",
                    loaded.as_ref().map_or(0, |d| d.len()) as u64,
                );
                loaded
            })
            .as_ref()
    }

    /// Is this word known to either set?
    ///
    /// A hyphenated compound counts as known when all of its parts are.
    /// English forms these freely and no word list can enumerate them — the
    /// system dictionary carries two hyphenated entries in 236k — so without
    /// this, `well-known`, `long-term`, and `local-first` all read as typos.
    /// That's the exact false-positive class this tool exists to remove.
    pub fn knows(&self, word: &str) -> bool {
        if self.knows_atom(word) {
            return true;
        }
        if !word.contains('-') {
            return false;
        }
        let mut parts = word.split('-').filter(|p| !p.is_empty()).peekable();
        let mut any = false;
        for part in &mut parts {
            any = true;
            // Short fragments (`e-mail`, `x-ray`) carry no signal, and the
            // checker already declines to judge words this short on their own.
            if part.chars().count() < 3 {
                continue;
            }
            if !self.knows_atom(part) {
                return false;
            }
        }
        any
    }

    /// A single word, against the lexicon, then mined prose, then the
    /// backstop dictionary.
    ///
    /// Mined prose sits above the dictionary because the dictionary is
    /// `web2` — Webster's Second International, published 1934. It has no
    /// `inline`, `download`, `roadmap`, or `pre`, so a word list alone flags
    /// ordinary modern English as misspelled. Words seen repeatedly in real
    /// prose on this machine are real words, and the repetition threshold is
    /// what keeps a typo in someone's README from qualifying.
    fn knows_atom(&self, word: &str) -> bool {
        if self.lexicon.contains(word) {
            return true;
        }
        if let Some(&sources) = self.observed.get(word) {
            let needed = if self.shadows_a_common_word(word) {
                SHADOW_SOURCES
            } else {
                MIN_SOURCES
            };
            if sources >= needed {
                return true;
            }
            // Not enough evidence yet — fall through, because the dictionary
            // may still vouch for it.
        }
        if self.frequency_of(word) >= MIN_CORPUS_EVIDENCE {
            return true;
        }
        match self.dictionary() {
            Some(d) => crate::dict::contains(d, word),
            None => false,
        }
    }

    /// Check one line as a document unto itself. Test convenience — real
    /// callers go through `Scanner`, which carries names across lines.
    #[cfg(test)]
    fn check_line_alone(
        &self,
        line: &str,
        line_no: usize,
        evidence: &mut impl FnMut(&str) -> i64,
    ) -> Vec<Finding> {
        self.check_line(line, line_no, &mut Names::new(), evidence)
    }

    /// Check one line, returning findings tagged with `line`/`col`.
    ///
    /// `evidence` supplies n-gram counts for the real-word pass; pass a
    /// closure returning 0 to skip it entirely.
    pub fn check_line(
        &self,
        line: &str,
        line_no: usize,
        names: &mut Names,
        evidence: &mut impl FnMut(&str) -> i64,
    ) -> Vec<Finding> {
        self.profile.count("lines_seen", 1);
        if !text::is_prose_line(line) {
            return Vec::new();
        }
        self.profile.count("lines_checked", 1);
        let line = text::normalize_typography(line);
        let masked = text::mask_non_prose(&line);
        // Before checking, not after: a line that links a project and then
        // names it in prose — the common markdown shape — has to vouch for
        // itself.
        names.observe(&line, &masked);
        let tokens = text::tokenize(&masked);

        let normalized: Vec<String> = tokens.iter().map(|t| text::normalize(&t.text)).collect();
        self.profile.count("tokens", tokens.len() as u64);

        let mut findings = Vec::new();
        let mut sentence_initial = true;
        for (i, token) in tokens.iter().enumerate() {
            let starts_sentence = sentence_initial;
            // The next token begins a sentence only if this one ended one.
            sentence_initial = ends_sentence(&masked, token, &tokens.get(i + 1).map(|t| t.col));

            if !text::is_checkable(&token.text) {
                continue;
            }
            if text::is_proper_noun(&token.text, starts_sentence) {
                self.profile.count("proper_nouns_skipped", 1);
                names.observe_proper_noun(&token.text);

                // Everything above skips capitalized tokens outright, which is
                // right while no dictionary can hold a name. Once people are
                // known, the same tokens become the only place a misspelled
                // colleague could ever be caught.
                //
                // The whole capitalized run is tried before its first token,
                // because a full name is far stronger evidence than either
                // half: `Ada Lovelacee` against `ada lovelace` is one edit in
                // twelve characters, where `Lovelacee` alone would have to be
                // judged on its own.
                let run_end = tokens[i..]
                    .iter()
                    .zip(normalized[i..].iter())
                    .take_while(|(t, _)| {
                        t.text
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_uppercase())
                    })
                    .count();
                for len in (1..=run_end).rev() {
                    let candidate = normalized[i..i + len].join(" ");
                    if let Some((correct, confidence)) = self.misspelled_name(&candidate) {
                        self.profile.count("name_findings", 1);
                        findings.push(Finding {
                            kind: FindingKind::Unknown,
                            word: tokens[i..i + len]
                                .iter()
                                .map(|t| t.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" "),
                            line: line_no,
                            col: token.col,
                            suggestions: vec![Suggestion {
                                word: correct,
                                score: 1.0,
                            }],
                            confidence,
                        });
                        break;
                    }
                }
                continue;
            }
            self.profile.count("tokens_checked", 1);
            let word = &normalized[i];

            // Before the known-word gate, not after: `dont`, `didnt`, and
            // `thats` are all *in* the system word list, so gating on
            // "unknown" made this unreachable for the words it targets.
            if let Some(fixed) =
                crate::contraction::expand(word).or_else(|| self.derived_contraction(word))
            {
                findings.push(Finding {
                    kind: FindingKind::Contraction,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    suggestions: vec![Suggestion {
                        word: fixed.to_string(),
                        score: 1.0,
                    }],
                    confidence: crate::contraction::CONFIDENCE,
                });
                continue;
            }

            if !self.knows(word) {
                // Last gate, and an accepting one: the document itself may
                // have named this in a URL or a code span, which no word list
                // could have.
                if names.contains(word) {
                    self.profile.count("names_skipped", 1);
                    continue;
                }
                findings.push(self.unknown_finding(token, word, line_no));
                continue;
            }

            // Known word — but is it the right known word? Collocation
            // evidence answers this best, and frequency answers it at all
            // when the corpus is still empty.
            let prev = i.checked_sub(1).map(|j| normalized[j].as_str());
            let next = normalized.get(i + 1).map(|s| s.as_str());
            if let Some(hit) = ngram::check_real_word(prev, word, next, evidence) {
                findings.push(Finding {
                    kind: FindingKind::RealWord,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    confidence: hit.confidence(),
                    suggestions: vec![Suggestion {
                        word: hit.suggestion,
                        score: 1.0,
                    }],
                });
            } else if let Some(cue) = crate::cue::check(prev, word, next, evidence) {
                // Only once the corpus has nothing to say. A bundled rule is
                // the fallback for a lexicon that hasn't seen enough yet, not
                // a second opinion about a writer it already knows.
                findings.push(Finding {
                    kind: FindingKind::RealWord,
                    word: token.text.clone(),
                    line: line_no,
                    col: token.col,
                    confidence: cue.confidence,
                    suggestions: vec![Suggestion {
                        word: cue.word,
                        score: 1.0,
                    }],
                });
            }
        }
        findings
    }

    fn unknown_finding(&self, token: &Token, word: &str, line_no: usize) -> Finding {
        // Capitalization is the only evidence available about whether a
        // *name* was meant, and it is lost by normalization, so it has to be
        // read off the token as written.
        let looks_like_a_name = token
            .text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase());
        let suggestions = self.suggest_for(word, looks_like_a_name);
        // A word with a near neighbour is more likely a typo than a coinage;
        // one with no neighbour at all is probably jargon we haven't met.
        let confidence = if suggestions.is_empty() { 0.35 } else { 0.70 };
        Finding {
            kind: FindingKind::Unknown,
            word: token.text.clone(),
            line: line_no,
            col: token.col,
            suggestions,
            confidence,
        }
    }

    /// Ranked replacements within `MAX_EDIT_DISTANCE`. Lexicon words rank
    /// above dictionary words — if you have a word for it, it's your word.
    pub fn suggest(&self, word: &str) -> Vec<Suggestion> {
        self.suggest_for(word, false)
    }

    /// Ranked replacements, told whether a name or an ordinary word was meant.
    ///
    /// The distinction matters because a personal lexicon is full of short
    /// project and binary names — 309 of them sit one edit from a real word on
    /// this machine — and without it `navv` offered `navi` and `nav` above
    /// `navy`. Two tools ranked over the English word, because lexicon
    /// membership carries a frequency floor and `navy` is merely ordinary.
    ///
    /// A name is not a candidate correction for a lowercase word, and an
    /// ordinary word is a poor one for something capitalized.
    pub fn suggest_for(&self, word: &str, want_name: bool) -> Vec<Suggestion> {
        // (distance, -frequency, -prefix, -suffix, source rank, word) — every
        // field sorts ascending, so the values that should win are negated.
        //
        // Frequency sits above shape because it's the stronger evidence when
        // it exists: `aviod` shares three leading letters with `avid` and only
        // two with `avoid`, so prefix agreement alone picks the wrong one.
        // Shape then decides among candidates no frequency list knows, and
        // outranks source — your lexicon is full of short binary names that
        // sit one edit from everything, and letting provenance win would bury
        // the obvious correction under them.
        self.profile.count("suggest_calls", 1);
        let mut scored: Vec<(usize, u8, i64, isize, isize, u8, &String)> = Vec::new();
        let dictionary = self.dictionary().into_iter().flatten();
        for (candidate, rank) in self
            .lexicon
            .iter()
            .map(|c| (c, 0u8))
            .chain(dictionary.map(|(c, _)| (c, 1u8)))
        {
            // Every known word is measured against every unknown one. This
            // counter is what makes that cost visible under --profile.
            self.profile.count("candidates_scanned", 1);
            if let Some(d) = bounded_distance(word, candidate, MAX_EDIT_DISTANCE) {
                let (prefix, suffix) = affinity(word, candidate);
                // Sorts ascending, so 0 is "the kind that was asked for".
                let wrong_kind = u8::from(self.is_name(candidate) != want_name);
                scored.push((
                    d,
                    wrong_kind,
                    -self.frequency_of(candidate),
                    -(prefix as isize),
                    -(suffix as isize),
                    rank,
                    candidate,
                ));
            }
        }
        self.profile.count("candidates_kept", scored.len() as u64);

        scored.sort();
        scored.dedup_by(|a, b| a.6 == b.6);
        let kept: Vec<(usize, &String)> = scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(d, _, _, _, _, _, w)| (d, w))
            .collect();

        // Noisy channel, in miniature: weight each candidate by how likely the
        // word is at all, times how likely this typo is given that word.
        // Frequency supplies the first term; edit distance stands in for the
        // second, since a second edit is far rarer than a first.
        let mut weighted: Vec<(f64, &String, usize)> = kept
            .iter()
            .map(|(distance, candidate)| {
                // Log-compressed, because these counts are not probabilities
                // and their raw range is enormous: synthetic core counts reach
                // a million while mined counts sit in the tens. Multiplied
                // raw, frequency swamps every other term — `part` outscored
                // `apart` for `aparat` despite needing an extra edit.
                // Compressed, an edit is worth more than a hundredfold
                // frequency difference, which is the intended ordering.
                let prior = ((self.frequency_of(candidate) + 1) as f64).ln() + 1.0;
                let mut weight = prior * EDIT_PENALTY.powi(*distance as i32);
                if is_subsequence(word, candidate) {
                    weight *= SUBSEQUENCE_BONUS;
                }
                (weight, *candidate, *distance)
            })
            .collect();

        // Distance first, score within a tier. Letting the score cross tiers
        // put `part` above `apart` for `aparat` — two edits beating one —
        // because the two frequency sources aren't on a common scale: core
        // words carry synthetic Zipf counts up to a million while mined words
        // carry real counts in the tens, so a common word missing from the
        // core looks a hundred times rarer than it is. Until those are
        // calibrated, frequency isn't entitled to outweigh an extra edit.
        weighted.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then(b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
        });

        let total: f64 = weighted.iter().map(|(w, _, _)| w).sum();
        let mut out: Vec<Suggestion> = weighted
            .into_iter()
            .map(|(weight, candidate, _)| Suggestion {
                word: candidate.clone(),
                // Normalized, so the scores read as a distribution over the
                // candidates offered rather than as unrelated magnitudes.
                // Rounded: these are estimates from a rough error model, and
                // printing 0.8823529 implies a precision that isn't there.
                score: if total > 0.0 {
                    crate::types::round(weight / total, 3) as f32
                } else {
                    0.0
                },
            })
            .collect();

        // Drop the also-rans. Offering `help 1.00, hep 0.00, heal 0.00` asks
        // the reader to weigh two options the model has already dismissed;
        // the first is always kept so a finding is never left with no fix.
        let mut index = 0;
        out.retain(|s| {
            index += 1;
            index == 1 || s.score >= MIN_SUGGESTION_SCORE
        });
        out
    }
}

/// Does a sentence end between this token and the next?
///
/// Looks at the characters separating them rather than the token itself, so
/// `e.g.` mid-sentence doesn't reset the state for every abbreviation.
fn ends_sentence(line: &str, token: &Token, next_col: &Option<usize>) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let start = token.col - 1 + token.text.chars().count();
    let end = next_col.map_or(chars.len(), |c| (c - 1).min(chars.len()));
    if start >= end {
        return false;
    }
    chars[start..end]
        .iter()
        .any(|c| matches!(c, '.' | '!' | '?' | ':'))
}

/// A stateful pass over a whole document.
///
/// Some things that shouldn't be spell-checked can't be recognized one line
/// at a time. A fenced code block is the clear case: every line inside it is
/// code, but a line reading `# returns the users nam` looks like prose in
/// isolation. Deciding that requires remembering the fence opened above.
///
/// The same reasoning covers YAML front matter, which is configuration
/// wearing a colon.
pub struct Scanner<'a> {
    checker: &'a Checker,
    fence: Option<String>,
    in_front_matter: bool,
    line_no: usize,
    /// Names the document has revealed so far. Lives on the scanner, not the
    /// checker, because it is scoped to one document rather than the machine.
    names: Names,
}

impl<'a> Scanner<'a> {
    pub fn new(checker: &'a Checker) -> Self {
        Self {
            checker,
            fence: None,
            in_front_matter: false,
            line_no: 0,
            names: Names::new(),
        }
    }

    /// Feed the next line. Returns findings for it, or nothing if the line
    /// sits in a region where spelling doesn't apply.
    pub fn feed(&mut self, line: &str, evidence: &mut impl FnMut(&str) -> i64) -> Vec<Finding> {
        self.line_no += 1;
        let trimmed = line.trim();

        // Front matter: `---` on the very first line opens it.
        if self.line_no == 1 && trimmed == "---" {
            self.in_front_matter = true;
            return Vec::new();
        }
        if self.in_front_matter {
            if trimmed == "---" || trimmed == "..." {
                self.in_front_matter = false;
            }
            return Vec::new();
        }

        // Every region spelling doesn't apply to is a region full of names.
        // A README introduces a tool in a table row or an install command and
        // only then writes a sentence about it, so the lines being skipped
        // here are what make the prose below them checkable.
        if self.fence.is_some() || !text::is_prose_line(line) {
            self.names.observe_code(line);
        }

        // Fences: ``` or ~~~, closed by the same marker. Tracking which one
        // opened the block keeps a ``` inside a ~~~ block from closing it.
        if let Some(marker) = &self.fence {
            if trimmed.starts_with(marker.as_str()) {
                self.fence = None;
            }
            return Vec::new();
        }
        if let Some(marker) = fence_marker(trimmed) {
            self.fence = Some(marker);
            return Vec::new();
        }

        self.checker
            .check_line(line, self.line_no, &mut self.names, evidence)
    }
}

/// The fence marker a line opens, if it opens one.
fn fence_marker(trimmed: &str) -> Option<String> {
    for marker in ["```", "~~~"] {
        if trimmed.starts_with(marker) {
            return Some(marker.to_string());
        }
    }
    None
}

/// Do all of `needle`'s characters appear in `haystack`, in order?
///
/// True exactly when the correction is pure insertion — the typo dropped
/// letters rather than mistyping them.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|c| chars.any(|h| h == c))
}

/// Shared leading and trailing characters, as `(prefix, suffix)`.
///
/// Edit distance alone leaves `small`, `sal`, `mal`, and `ismal` all one edit
/// from `smal`, and an alphabetical tie-break then offers the worst of them
/// first. Leading agreement is the stronger signal — people rarely fumble the
/// start of a word — so callers order on the prefix and fall back to the
/// suffix. Summing the two would be worse than either: a front insertion and a
/// back insertion both preserve the whole word, so the sum ties them.
fn affinity(a: &str, b: &str) -> (usize, usize) {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let prefix = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (prefix, suffix)
}

/// Edit distance counting a transposition as **one** edit, abandoned once it
/// exceeds `max`. Returns `None` when the words are further apart than that —
/// the common case, so bailing early is what keeps a full-lexicon scan cheap.
///
/// This is Damerau-Levenshtein (optimal string alignment) rather than plain
/// Levenshtein, because swapping two adjacent letters is one of the most
/// common ways to mistype a word and plain Levenshtein charges it as two
/// substitutions. That difference is not academic: it puts `aviod` two edits
/// from `avoid` but only one from `avid`, so the obvious correction loses to
/// a worse one.
pub fn bounded_distance(a: &str, b: &str, max: usize) -> Option<usize> {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (a_len, b_len) = (a_chars.len(), b_chars.len());
    if a_len.abs_diff(b_len) > max {
        return None;
    }

    // Three rows, because a transposition looks back two positions.
    let mut prev_prev: Vec<usize> = vec![0; b_len + 1];
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut current = vec![0usize; b_len + 1];

    for i in 0..a_len {
        current[0] = i + 1;
        let mut row_min = current[0];
        for j in 0..b_len {
            let cost = usize::from(a_chars[i] != b_chars[j]);
            let mut best = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
            if i > 0 && j > 0 && a_chars[i] == b_chars[j - 1] && a_chars[i - 1] == b_chars[j] {
                best = best.min(prev_prev[j - 1] + 1);
            }
            current[j + 1] = best;
            row_min = row_min.min(best);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev_prev, &mut prev);
        std::mem::swap(&mut prev, &mut current);
    }
    let d = prev[b_len];
    (d <= max).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(lexicon: &[&str], dictionary: &[&str]) -> Checker {
        Checker::new(
            lexicon.iter().map(|s| s.to_string()).collect(),
            Some(crate::dict::from_words(dictionary)),
        )
    }

    fn no_evidence(_: &str) -> i64 {
        0
    }

    /// Just the words, for assertions that don't care about the scores.
    fn words(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.word.as_str()).collect()
    }

    #[test]
    fn accepts_lexicon_jargon_the_dictionary_never_heard_of() {
        let c = checker(&["contextdb", "rubocop"], &["and", "are", "fine"]);
        let f = c.check_line_alone("contextdb and rubocop are fine", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn accepts_hyphenated_compounds_built_from_known_parts() {
        // No word list enumerates these; English builds them on demand.
        let c = checker(
            &["local", "first"],
            &["well", "known", "long", "term", "design"],
        );
        let f = c.check_line_alone(
            "a well-known long-term local-first design",
            1,
            &mut no_evidence,
        );
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn still_flags_a_compound_whose_part_is_misspelled() {
        let c = checker(&[], &["well", "known", "result"]);
        let f = c.check_line_alone("a well-knwon result", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].word, "well-knwon");
    }

    #[test]
    fn fixes_contractions_typed_without_an_apostrophe() {
        let c = checker(&[], &["we", "ship", "that"]);
        let f = c.check_line_alone("we dont ship that", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Contraction);
        assert_eq!(words(&f[0].suggestions), vec!["don't"]);
        assert!(f[0].confidence > 0.8);
    }

    #[test]
    fn contractions_are_caught_even_when_the_word_list_contains_them() {
        // `dont`, `didnt` and `thats` are all in /usr/share/dict/words, so a
        // check gated on "unknown word" would never reach them.
        let c = checker(&[], &["we", "dont", "ship", "that"]);
        let f = c.check_line_alone("we dont ship that", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::Contraction);
        assert_eq!(words(&f[0].suggestions), vec!["don't"]);
    }

    #[test]
    fn a_curly_apostrophe_reads_as_the_same_word() {
        // What macOS, Slack, and Gmail actually emit.
        let c = checker(&[], &["we", "don't", "ship", "that"]);
        assert!(
            c.check_line_alone("we don\u{2019}t ship that", 1, &mut no_evidence)
                .is_empty()
        );
    }

    #[test]
    fn columns_are_character_positions() {
        let c = checker(&[], &["caf\u{e9}", "the"]);
        // "café the zzzqx" — byte offsets would drift by the accent.
        let f = c.check_line_alone("caf\u{e9} the zzzqx", 1, &mut no_evidence);
        assert_eq!(f[0].word, "zzzqx");
        assert_eq!(f[0].col, 10);
    }

    #[test]
    fn handles_non_ascii_without_panicking() {
        let c = checker(&[], &["notes", "from", "the", "trip"]);
        let f = c.check_line_alone("İstanbul notes from the trip", 1, &mut no_evidence);
        // 'İstanbul' is a proper noun we don't know; the point is it survives.
        assert!(f.len() <= 1);
    }

    #[test]
    fn flags_a_genuine_typo_and_suggests() {
        let c = checker(&[], &["ship", "the", "change"]);
        let f = c.check_line_alone("shp the change", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].word, "shp");
        assert_eq!(f[0].kind, FindingKind::Unknown);
        assert!(words(&f[0].suggestions).contains(&"ship"));
    }

    #[test]
    fn reports_position() {
        let c = checker(&[], &["the", "change"]);
        let f = c.check_line_alone("the zzzqx change", 7, &mut no_evidence);
        assert_eq!((f[0].line, f[0].col), (7, 5));
    }

    #[test]
    fn never_flags_urls_paths_or_code_spans() {
        let c = checker(&[], &["see", "and", "now"]);
        let f = c.check_line_alone(
            "see https://github.com/dpep/ae and `foo_bar` now",
            1,
            &mut no_evidence,
        );
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn skips_code_shaped_lines_entirely() {
        let c = checker(&[], &["let"]);
        assert!(
            c.check_line_alone("    let zzzqx = 1;", 1, &mut no_evidence)
                .is_empty()
        );
        assert!(
            c.check_line_alone("```rust", 1, &mut no_evidence)
                .is_empty()
        );
    }

    #[test]
    fn unknown_word_without_a_neighbour_is_low_confidence() {
        let c = checker(&[], &["ship"]);
        let f = c.check_line_alone("the zzzqxwv thing", 1, &mut no_evidence);
        assert!(
            f[0].confidence < 0.5,
            "jargon should not be confidently wrong"
        );
    }

    fn observed(pairs: &[(&str, i64)], dictionary: &[&str]) -> Checker {
        Checker::new(HashSet::new(), Some(crate::dict::from_words(dictionary)))
            .with_observed(pairs.iter().map(|(w, n)| (w.to_string(), *n)).collect())
    }

    fn with_people(pairs: &[(&str, i64)]) -> Checker {
        Checker::new(HashSet::new(), Some(crate::dict::from_words(["field"]))).with_people(
            pairs
                .iter()
                .map(|(n, d)| {
                    let display: String = n
                        .split(' ')
                        .map(|w| {
                            let mut c = w.chars();
                            match c.next() {
                                Some(f) => f.to_uppercase().to_string() + c.as_str(),
                                None => String::new(),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    (n.to_string(), (*d, display))
                })
                .collect(),
        )
    }

    #[test]
    fn catches_a_misspelled_colleague() {
        let c = with_people(&[("ada lovelace", 3)]);
        let (name, _) = c.misspelled_name("ada lovelacee").unwrap();
        assert_eq!(name, "Ada Lovelace");
    }

    #[test]
    fn a_name_seen_on_one_day_cannot_convict_another() {
        // Seen once, it is as likely to be the typo as the target.
        let c = with_people(&[("ada lovelace", 1)]);
        assert!(c.misspelled_name("ada lovelacee").is_none());
    }

    #[test]
    fn two_people_equally_close_means_silence() {
        // Both are real people and the typo is one edit from each, so there is
        // no way to tell which was meant. Telling someone they misspelled a
        // colleague when they did not is worse than missing it.
        let c = with_people(&[("jon smith", 5), ("ron smith", 5)]);
        assert!(c.misspelled_name("ton smith").is_none());

        // With only one of them known, the same input is answerable.
        let c = with_people(&[("jon smith", 5)]);
        assert!(c.misspelled_name("ton smith").is_some());
    }

    #[test]
    fn a_surname_that_is_also_a_word_is_left_alone() {
        // `Field`, `Green`, `Baker` are all surnames and all words.
        let c = with_people(&[("fields", 5)]);
        assert!(c.misspelled_name("field").is_none());
    }

    #[test]
    fn two_edits_away_is_not_a_misspelling() {
        // Names are short and a second edit reaches a different person.
        let c = with_people(&[("ada lovelace", 5)]);
        assert!(c.misspelled_name("ada lovelaces").is_some());
        assert!(c.misspelled_name("eda lovelaces").is_none());
    }

    #[test]
    fn confidence_grows_with_corroboration() {
        let seldom = with_people(&[("ada lovelace", 2)]);
        let often = with_people(&[("ada lovelace", 8)]);
        let a = seldom.misspelled_name("ada lovelacee").unwrap().1;
        let b = often.misspelled_name("ada lovelacee").unwrap().1;
        assert!(b > a, "{b} should exceed {a}");
        // A guess about a person stays below the word paths.
        assert!(b <= 0.70);
    }

    #[test]
    fn a_word_seen_once_has_not_earned_silence() {
        // One sighting is equally evidence of a typo, and typos are bursty
        // inside a single document. Learning from it made the checker
        // permanently blind to that spelling.
        let c = observed(&[("zblorg", 1)], &["the", "ship"]);
        assert!(!c.knows("zblorg"));
    }

    #[test]
    fn two_independent_sightings_earn_it() {
        let c = observed(&[("zblorg", 2)], &["the", "ship"]);
        assert!(c.knows("zblorg"));
    }

    #[test]
    fn a_word_shadowing_a_common_one_is_held_to_a_higher_bar() {
        // `shoudl` is a chronic misspelling, and from the inside it looks
        // exactly like new vocabulary. That it sits one edit from a word the
        // writer already knows is the thing that tells them apart.
        //
        // The shadowed word has to be *common* for that to mean anything, so
        // this fixture states a level rather than taking the default —
        // resembling something obscure is not evidence of a slip.
        let common: crate::dict::Dictionary = [("should", 10u8), ("shout", 10), ("the", 10)]
            .into_iter()
            .map(|(w, level)| (w.to_string(), level))
            .collect();
        let with = |sources| {
            Checker::new(HashSet::new(), Some(common.clone()))
                .with_observed([("shoudl".to_string(), sources)].into_iter().collect())
        };
        assert!(!with(2).knows("shoudl"));
        assert!(with(3).knows("shoudl"));

        // Jargon that shadows nothing clears the ordinary bar at two.
        let jargon = Checker::new(HashSet::new(), Some(common))
            .with_observed([("zblorg".to_string(), 2)].into_iter().collect());
        assert!(jargon.knows("zblorg"));
    }

    #[test]
    fn a_deliberate_source_needs_no_corroboration() {
        // Installing a tool or adding a word by hand is the evidence.
        let c = Checker::new(
            HashSet::from(["contextdb".to_string()]),
            Some(crate::dict::from_words(["the"])),
        );
        assert!(c.knows("contextdb"));
    }

    #[test]
    fn a_name_does_not_outrank_a_word_for_a_lowercase_typo() {
        // 309 of the short names in a real lexicon sit one edit from an
        // ordinary word, and lexicon membership carries a frequency floor —
        // so `navv` offered the tools `navi` and `nav` above `navy`.
        let c = Checker::new(
            HashSet::from(["navi".to_string()]),
            Some(crate::dict::from_words(["navy", "nave"])),
        )
        .with_naming_sources(HashSet::from(["navi".to_string()]));

        let suggestions = c.suggest("navv");
        let ranked = words(&suggestions);
        assert!(!ranked.is_empty());
        assert_ne!(ranked[0], "navi", "a tool name beat the English word");
    }

    #[test]
    fn a_capitalized_typo_prefers_a_name() {
        let c = Checker::new(
            HashSet::from(["iriq".to_string()]),
            Some(crate::dict::from_words(["iris"])),
        )
        .with_naming_sources(HashSet::from(["iriq".to_string()]));

        let suggestions = c.suggest_for("iriqq", true);
        assert_eq!(words(&suggestions)[0], "iriq");
    }

    #[test]
    fn kind_only_breaks_ties_it_does_not_outrank_distance() {
        // Kind sorts after distance on purpose. Leading with it cost three
        // points of correction rate, because technical names are written
        // lowercase — `ripgrep`, `nixpkgs` — so demoting names for a
        // lowercase token demotes exactly the corrections that were wanted.
        let c = Checker::new(
            HashSet::from(["iriq".to_string()]),
            Some(crate::dict::from_words(["irises"])),
        )
        .with_naming_sources(HashSet::from(["iriq".to_string()]));

        // `iriq` is one edit away and a name; `irises` is further and a word.
        let suggestions = c.suggest("iriqq");
        assert_eq!(words(&suggestions)[0], "iriq");
    }

    #[test]
    fn prefers_lexicon_words_when_candidates_match_equally_well() {
        // Both are distance 1 from "shix" and agree on the same 3 characters,
        // so provenance is all that's left to separate them.
        let c = checker(&["shiv"], &["shin"]);
        assert_eq!(c.suggest("shix").first().unwrap().word, "shiv");
    }

    #[test]
    fn a_closer_dictionary_word_beats_a_scrappier_lexicon_one() {
        // `sh` is a real binary and one edit away, but `ship` keeps more of
        // the word — shape has to win or short command names bury everything.
        let c = checker(&["sh", "scp"], &["ship"]);
        assert_eq!(c.suggest("shp").first().unwrap().word, "ship");
    }

    #[test]
    fn catches_a_real_word_error_when_collocations_support_it() {
        let c = checker(&[], &["apart", "form", "from", "the", "rest"]);
        let mut evidence = |gram: &str| match gram {
            "apart from" => 20,
            "from the" => 50,
            _ => 0,
        };
        let f = c.check_line_alone("apart form the rest", 1, &mut evidence);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, FindingKind::RealWord);
        assert_eq!(words(&f[0].suggestions), vec!["from"]);
    }

    #[test]
    fn ranks_the_shape_preserving_candidate_first() {
        // All four are one edit from "smal"; only affinity separates them.
        let c = checker(&[], &["small", "sal", "mal", "ismal"]);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
    }

    #[test]
    fn affinity_weighs_the_start_of_a_word_most() {
        // A front insertion and a back insertion both preserve every
        // character, so only the leading agreement separates them.
        assert!(affinity("smal", "small").0 > affinity("smal", "ismal").0);
        assert!(affinity("shp", "ship").0 > affinity("shp", "php").0);
    }

    #[test]
    fn affinity_uses_the_suffix_to_break_a_prefix_tie() {
        let (ship_prefix, ship_suffix) = affinity("shp", "ship");
        let (sh_prefix, sh_suffix) = affinity("shp", "sh");
        assert_eq!(ship_prefix, sh_prefix);
        assert!(ship_suffix > sh_suffix);
    }

    #[test]
    fn suggestion_scores_form_a_distribution() {
        let frequency: std::collections::HashMap<String, i64> =
            [("help".to_string(), 5_000i64), ("hep".to_string(), 2i64)]
                .into_iter()
                .collect();
        let c = Checker::new(
            HashSet::new(),
            Some(crate::dict::from_words(["help", "hep", "heal"])),
        )
        .with_frequency(frequency);

        let suggestions = c.suggest("hepl");
        assert_eq!(suggestions[0].word, "help");
        let total: f32 = suggestions.iter().map(|s| s.score).sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "scores should sum to 1: {total}"
        );
    }

    #[test]
    fn an_insertion_beats_a_substitution_by_a_commoner_word() {
        // Both are one edit from `plese`, and `these` is the more frequent
        // word — but dropping a letter is a far commoner slip than striking
        // a key across the keyboard.
        let frequency: std::collections::HashMap<String, i64> = [
            ("these".to_string(), 50_000i64),
            ("please".to_string(), 3_000i64),
        ]
        .into_iter()
        .collect();
        let c = Checker::new(
            HashSet::new(),
            Some(crate::dict::from_words(["please", "these"])),
        )
        .with_frequency(frequency);

        assert_eq!(c.suggest("plese").first().unwrap().word, "please");
    }

    #[test]
    fn suggestions_are_ordered_by_their_own_scores() {
        let c = checker(&[], &["ship", "shop", "chip"]);
        let suggestions = c.suggest("shp");
        for pair in suggestions.windows(2) {
            assert!(
                pair[0].score >= pair[1].score,
                "order must agree with the scores: {suggestions:?}"
            );
        }
    }

    #[test]
    fn is_subsequence_detects_pure_insertions() {
        assert!(is_subsequence("plese", "please"));
        assert!(!is_subsequence("plese", "these"));
        assert!(is_subsequence("teh", "teach"));
        assert!(!is_subsequence("teh", "the"));
    }

    #[test]
    fn a_transposition_costs_one_edit_not_two() {
        // The reason `aviod` used to suggest `avid` over `avoid`.
        assert_eq!(bounded_distance("aviod", "avoid", 2), Some(1));
        assert_eq!(bounded_distance("teh", "the", 2), Some(1));
        assert_eq!(bounded_distance("recieve", "receive", 2), Some(1));
    }

    #[test]
    fn transposition_plus_frequency_ranks_the_intended_word_first() {
        // Both are one edit away once transposition is free, and `avid`
        // actually shares the longer prefix — so frequency is what decides.
        let frequency: std::collections::HashMap<String, i64> =
            [("avoid".to_string(), 20_000i64)].into_iter().collect();
        let c = Checker::new(
            HashSet::new(),
            Some(crate::dict::from_words(["avoid", "avid", "avian"])),
        )
        .with_frequency(frequency);
        assert_eq!(c.suggest("aviod").first().unwrap().word, "avoid");
    }

    #[test]
    fn shape_still_decides_when_no_frequency_is_known() {
        let c = checker(&[], &["small", "sal", "mal"]);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
    }

    #[test]
    fn frequency_breaks_ties_the_dictionary_cannot() {
        // All three are one edit from "smal" and agree on the same prefix;
        // only how common they are separates them.
        let frequency: std::collections::HashMap<String, i64> =
            [("small".to_string(), 50_000i64)].into_iter().collect();
        let c = Checker::new(
            HashSet::new(),
            Some(crate::dict::from_words(["small", "smalm", "smalt"])),
        )
        .with_frequency(frequency);
        assert_eq!(c.suggest("smal").first().unwrap().word, "small");
    }

    #[test]
    fn skips_mid_sentence_capitals_as_proper_nouns() {
        let c = checker(&[], &["we", "use", "and", "for", "this"]);
        let f = c.check_line_alone("we use Guiraud and Zblorgian for this", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn still_checks_a_capital_that_opens_a_sentence() {
        let c = checker(&[], &["the", "word"]);
        let f = c.check_line_alone("Zzzqxwv the word", 1, &mut no_evidence);
        assert_eq!(f.len(), 1, "sentence-initial caps carry no name signal");
    }

    #[test]
    fn resumes_checking_capitals_after_a_full_stop() {
        let c = checker(&[], &["done", "the", "word"]);
        let f = c.check_line_alone("done. Zzzqxwv the word", 1, &mut no_evidence);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn skips_pluralized_acronyms() {
        let c = checker(&[], &["and", "the"]);
        let f = c.check_line_alone("the URLs and PRs and IDs", 1, &mut no_evidence);
        assert!(f.is_empty(), "unexpected findings: {f:?}");
    }

    #[test]
    fn mined_prose_covers_words_the_1934_dictionary_lacks() {
        // web2 has no "inline", "download", or "roadmap".
        let frequency: std::collections::HashMap<String, i64> =
            [("inline".to_string(), 9i64), ("roadmap".to_string(), 5i64)]
                .into_iter()
                .collect();
        let c = Checker::new(HashSet::new(), Some(crate::dict::Dictionary::new()))
            .with_frequency(frequency);
        assert!(c.knows("inline"));
        assert!(c.knows("roadmap"));
        // But a word seen once is not yet evidence of anything.
        assert!(!c.knows("zzzqxwv"));
    }

    #[test]
    fn skips_everything_inside_a_fenced_block() {
        let c = checker(&[], &["real", "prose", "here"]);
        let doc = "real prose here\n```\nzzzqx zzzqxwv qqxjjv\n```\nreal prose here";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn a_different_fence_marker_does_not_close_the_block() {
        let c = checker(&[], &[]);
        let doc = "~~~\nzzzqx\n```\nzzzqxwv\n~~~";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn resumes_checking_after_the_block_closes() {
        let c = checker(&[], &["and", "then"]);
        let doc = "```\nzzzqx\n```\nand then zzzqxwv";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].word, "zzzqxwv");
        assert_eq!(findings[0].line, 4, "line numbers survive skipped regions");
    }

    #[test]
    fn skips_yaml_front_matter() {
        let c = checker(&[], &["real", "prose"]);
        let doc = "---\nname: zzzqx\ndescription: zzzqxwv\n---\nreal prose";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");
    }

    #[test]
    fn a_horizontal_rule_midway_is_not_front_matter() {
        // `---` only opens front matter on line 1.
        let c = checker(&[], &["some", "prose"]);
        let doc = "some prose\n---\nzzzqxwv";
        let mut scanner = Scanner::new(&c);
        let findings: Vec<_> = doc
            .lines()
            .flat_map(|l| scanner.feed(l, &mut no_evidence))
            .collect();
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn bounded_distance_bails_past_the_limit() {
        assert_eq!(bounded_distance("ship", "ship", 2), Some(0));
        assert_eq!(bounded_distance("ship", "shp", 2), Some(1));
        assert_eq!(bounded_distance("ship", "elephant", 2), None);
    }
}
