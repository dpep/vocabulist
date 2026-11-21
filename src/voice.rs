//! How someone writes, below the level of which words: punctuation, hedging,
//! person, how a sentence starts and whether a message ends (PLAN §10).
//!
//! Every marker is a count over an opportunity — per sentence, per thousand
//! words, per message — so a profile is two integers per marker and a draft
//! can be scored against it without keeping any prose. The set is the one
//! that measurably separated a user's typed prompts from an assistant's
//! replies on the same work, not a list of everything stylometry has named:
//! a marker both sides use at the same rate tells a draft nothing.

use std::collections::BTreeMap;

use crate::text;

/// What a marker is counted over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Per {
    Sentence,
    /// Reported per thousand words.
    Word,
    Message,
}

impl Per {
    pub fn denominator(self) -> &'static str {
        match self {
            Per::Sentence => "sentences",
            Per::Word => "words",
            Per::Message => "messages",
        }
    }

    /// The multiplier a rate is shown at.
    pub fn scale(self) -> f64 {
        match self {
            Per::Word => 1000.0,
            _ => 1.0,
        }
    }
}

pub struct Marker {
    pub name: &'static str,
    pub per: Per,
    /// What a reader sees: "starts a sentence lowercase".
    pub gloss: &'static str,
}

pub const MARKERS: &[Marker] = &[
    Marker {
        name: "lowercase_start",
        per: Per::Sentence,
        gloss: "starts a sentence lowercase",
    },
    Marker {
        name: "question",
        per: Per::Sentence,
        gloss: "ends a sentence with a question mark",
    },
    Marker {
        name: "comma",
        per: Per::Sentence,
        gloss: "commas",
    },
    Marker {
        name: "semicolon",
        per: Per::Sentence,
        gloss: "semicolons",
    },
    Marker {
        name: "colon",
        per: Per::Sentence,
        gloss: "colons",
    },
    Marker {
        name: "em_dash",
        per: Per::Sentence,
        gloss: "em or en dashes",
    },
    Marker {
        name: "spaced_hyphen",
        per: Per::Sentence,
        gloss: "a spaced hyphen as a dash ( - )",
    },
    Marker {
        name: "parenthesis",
        per: Per::Sentence,
        gloss: "parentheticals",
    },
    Marker {
        name: "ellipsis",
        per: Per::Sentence,
        gloss: "ellipses",
    },
    Marker {
        name: "passive",
        per: Per::Sentence,
        gloss: "passive constructions",
    },
    Marker {
        name: "long_sentence",
        per: Per::Sentence,
        gloss: "sentences of 20 words or more",
    },
    Marker {
        name: "contraction",
        per: Per::Word,
        gloss: "contractions",
    },
    Marker {
        name: "expanded",
        per: Per::Word,
        gloss: "spelled-out contractions (do not, it is)",
    },
    Marker {
        name: "hedge",
        per: Per::Word,
        gloss: "hedges (maybe, I think, seems)",
    },
    Marker {
        name: "intensifier",
        per: Per::Word,
        gloss: "intensifiers (very, really)",
    },
    Marker {
        name: "courtesy",
        per: Per::Word,
        gloss: "please, thanks, sorry",
    },
    Marker {
        name: "first_singular",
        per: Per::Word,
        gloss: "I, me, my",
    },
    Marker {
        name: "first_plural",
        per: Per::Word,
        gloss: "we, us, our, let's",
    },
    Marker {
        name: "second_person",
        per: Per::Word,
        gloss: "you, your",
    },
    Marker {
        name: "unterminated",
        per: Per::Message,
        gloss: "a message that ends without punctuation",
    },
    Marker {
        name: "request",
        per: Per::Message,
        gloss: "a message that opens as a request (can we, let's)",
    },
];

pub fn marker(name: &str) -> Option<&'static Marker> {
    MARKERS.iter().find(|m| m.name == name)
}

/// Counts for one or more texts: every marker, plus `sentences`, `words` and
/// `messages`. Absent means zero.
pub type Counts = BTreeMap<String, i64>;

const HEDGES: &[&str] = &[
    "maybe",
    "perhaps",
    "probably",
    "possibly",
    "might",
    "somewhat",
    "seems",
    "seem",
    "likely",
    "guess",
    "arguably",
    "presumably",
    "apparently",
    "roughly",
];
const HEDGE_PHRASES: &[&[&str]] = &[
    &["i", "think"],
    &["i", "guess"],
    &["kind", "of"],
    &["sort", "of"],
    &["not", "sure"],
    &["i", "wonder"],
    &["i", "suspect"],
    &["i", "feel", "like"],
];
const INTENSIFIERS: &[&str] = &[
    "very",
    "really",
    "quite",
    "extremely",
    "super",
    "totally",
    "definitely",
    "absolutely",
    "highly",
    "incredibly",
    "truly",
];
const COURTESY: &[&str] = &["please", "thanks", "thank", "sorry", "appreciate", "cheers"];
const EXPANDED: &[&[&str]] = &[
    &["do", "not"],
    &["does", "not"],
    &["did", "not"],
    &["cannot"],
    &["will", "not"],
    &["is", "not"],
    &["are", "not"],
    &["was", "not"],
    &["should", "not"],
    &["would", "not"],
    &["could", "not"],
    &["i", "am"],
    &["you", "are"],
    &["they", "are"],
    &["we", "are"],
    &["that", "is"],
    &["it", "is"],
    &["let", "us"],
];
const BE: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "being", "gets", "got",
];
const IRREGULAR_PARTICIPLES: &[&str] = &[
    "built", "done", "made", "written", "seen", "known", "given", "taken", "shown", "found",
    "kept", "left", "run", "set", "put", "held", "sent", "read", "broken", "chosen", "driven",
];
/// A request opens with one of these (lowercased, apostrophes straight).
const REQUEST_OPENERS: &[&[&str]] = &[
    &["can", "you"],
    &["can", "we"],
    &["could", "you"],
    &["could", "we"],
    &["would", "you"],
    &["will", "you"],
    &["should", "we"],
    &["shall", "we"],
    &["let's"],
    &["how", "about"],
    &["what", "if"],
    &["please"],
];
const CONTRACTION_SUFFIXES: &[&str] = &["n't", "'s", "'re", "'ve", "'ll", "'d", "'m"];

fn starts_with_seq(words: &[String], seq: &[&str]) -> bool {
    words.len() >= seq.len() && words.iter().zip(seq).all(|(w, s)| w == s)
}

fn count_seqs(words: &[String], seqs: &[&[&str]]) -> i64 {
    (0..words.len())
        .map(|i| {
            seqs.iter()
                .filter(|s| starts_with_seq(&words[i..], s))
                .count() as i64
        })
        .sum()
}

/// Is this word a past participle, by shape? `-ed` or a common irregular.
/// Shape alone over-reads (`the red`); the preceding `be` is what makes it a
/// passive, and the error that remains is the same for every writer, so it
/// cancels in a comparison.
fn participle(word: &str) -> bool {
    (word.len() > 3 && word.ends_with("ed")) || IRREGULAR_PARTICIPLES.contains(&word)
}

/// The prose lines of a text: no fenced code, no tables, no indented
/// blocks, with list bullets, heading marks and bold markers removed, since
/// that formatting is a medium's, not a voice's.
fn prose_lines(text: &str) -> Vec<String> {
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !text::is_prose_line(line) {
            continue;
        }
        let t = line.trim().trim_start_matches('#').trim_start();
        let t = ["- ", "* ", "+ "]
            .iter()
            .find_map(|b| t.strip_prefix(b))
            .unwrap_or(t);
        let t = match t.split_once(". ") {
            Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => rest,
            _ => t,
        };
        out.push(text::mask_non_prose(&t.replace("**", "")));
    }
    out
}

fn words_of(sentence: &str) -> Vec<String> {
    text::tokenize(&text::normalize_typography(sentence))
        .into_iter()
        .map(|t| t.text.to_lowercase())
        .filter(|w| w.chars().any(char::is_alphabetic))
        .collect()
}

/// Count every marker in one message.
pub fn measure(message: &str) -> Counts {
    let mut c = Counts::new();
    let lines = prose_lines(message);
    if lines.iter().all(|l| words_of(l).is_empty()) {
        return c;
    }
    let bump = |c: &mut Counts, k: &str, by: i64| {
        if by != 0 {
            *c.entry(k.to_string()).or_default() += by;
        }
    };
    bump(&mut c, "messages", 1);

    let mut first_sentence: Option<Vec<String>> = None;
    for line in &lines {
        // A line break ends a sentence too: chat leaves the period off.
        for sentence in crate::complexity::split_sentences(line) {
            let s = sentence.trim();
            let words = words_of(s);
            // A one-word sentence ("Thanks!") still carries its word, but
            // no sentence shape worth counting.
            bump(&mut c, "words", words.len() as i64);
            let has = |list: &[&str], w: &String| list.contains(&w.as_str());
            bump(
                &mut c,
                "contraction",
                words
                    .iter()
                    .filter(|w| CONTRACTION_SUFFIXES.iter().any(|s| w.ends_with(s)) && w.len() > 3)
                    .count() as i64,
            );
            bump(&mut c, "expanded", count_seqs(&words, EXPANDED));
            bump(
                &mut c,
                "hedge",
                words.iter().filter(|w| has(HEDGES, w)).count() as i64
                    + count_seqs(&words, HEDGE_PHRASES),
            );
            bump(
                &mut c,
                "intensifier",
                words.iter().filter(|w| has(INTENSIFIERS, w)).count() as i64,
            );
            bump(
                &mut c,
                "courtesy",
                words.iter().filter(|w| has(COURTESY, w)).count() as i64,
            );
            bump(
                &mut c,
                "first_singular",
                words
                    .iter()
                    .filter(|w| {
                        matches!(
                            w.as_str(),
                            "i" | "me" | "my" | "i'm" | "i've" | "i'll" | "i'd"
                        )
                    })
                    .count() as i64,
            );
            bump(
                &mut c,
                "first_plural",
                words
                    .iter()
                    .filter(|w| {
                        matches!(
                            w.as_str(),
                            "we" | "us" | "our" | "let's" | "we're" | "we've" | "we'll"
                        )
                    })
                    .count() as i64,
            );
            bump(
                &mut c,
                "second_person",
                words
                    .iter()
                    .filter(|w| {
                        matches!(w.as_str(), "you" | "your" | "you're" | "you've" | "you'll")
                    })
                    .count() as i64,
            );
            if words.len() < 2 {
                continue;
            }
            first_sentence.get_or_insert_with(|| words.clone());
            bump(&mut c, "sentences", 1);

            bump(
                &mut c,
                "lowercase_start",
                i64::from(
                    s.chars()
                        .find(|ch| ch.is_alphabetic())
                        .is_some_and(char::is_lowercase),
                ),
            );
            bump(&mut c, "question", i64::from(s.ends_with('?')));
            bump(&mut c, "comma", s.matches(',').count() as i64);
            bump(&mut c, "semicolon", s.matches(';').count() as i64);
            bump(
                &mut c,
                "colon",
                s.match_indices(':')
                    .filter(|(i, _)| s[i + 1..].chars().next().is_none_or(char::is_whitespace))
                    .count() as i64,
            );
            bump(
                &mut c,
                "em_dash",
                (s.matches('\u{2014}').count()
                    + s.matches(" \u{2013} ").count()
                    + s.matches(" -- ").count()) as i64,
            );
            bump(&mut c, "spaced_hyphen", s.matches(" - ").count() as i64);
            bump(
                &mut c,
                "parenthesis",
                s.split('(')
                    .skip(1)
                    .filter(|after| {
                        after
                            .split(')')
                            .next()
                            .is_some_and(|inner| inner.chars().any(char::is_alphabetic))
                    })
                    .count() as i64,
            );
            bump(
                &mut c,
                "ellipsis",
                (s.matches("...").count() + s.matches('\u{2026}').count()) as i64,
            );
            bump(
                &mut c,
                "passive",
                words
                    .windows(2)
                    .filter(|w| BE.contains(&w[0].as_str()) && participle(&w[1]))
                    .count() as i64
                    + words
                        .windows(3)
                        .filter(|w| {
                            BE.contains(&w[0].as_str()) && w[1].ends_with("ly") && participle(&w[2])
                        })
                        .count() as i64,
            );
            bump(&mut c, "long_sentence", i64::from(words.len() >= 20));
        }
    }

    if let Some(last) = lines.iter().rev().find(|l| !words_of(l).is_empty()) {
        let end = last.trim_end().chars().last();
        bump(
            &mut c,
            "unterminated",
            i64::from(!end.is_some_and(|ch| ".!?:)\"'`".contains(ch))),
        );
    }
    if let Some(first) = first_sentence {
        bump(
            &mut c,
            "request",
            i64::from(
                REQUEST_OPENERS
                    .iter()
                    .any(|seq| starts_with_seq(&first, seq)),
            ),
        );
    }
    c
}

/// A marker's rate in a profile, with what backs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub count: i64,
    pub opportunities: i64,
}

impl Rate {
    pub fn of(counts: &Counts, m: &Marker) -> Self {
        Rate {
            count: counts.get(m.name).copied().unwrap_or(0),
            opportunities: counts.get(m.per.denominator()).copied().unwrap_or(0),
        }
    }

    /// The rate this profile expects (the posterior mean under the prior
    /// below), so a marker one side never used is rare, not impossible.
    pub fn smoothed(self) -> f64 {
        (self.count as f64 + 0.5) / (self.opportunities as f64 + 1.0)
    }

    /// The distribution of this marker's count in `m` fresh opportunities,
    /// given what the profile has seen.
    ///
    /// A Gamma(½, 1) prior on the rate, updated by the profile, makes the
    /// count negative binomial: a profile of five sentences predicts almost
    /// anything, one of five thousand predicts tightly. That width is the
    /// point. Scoring with a point rate treated a thin profile as exact, so
    /// a baseline of one message could outvote a profile of hundreds.
    fn predictive(self, m: i64) -> NegBinomial {
        let n = self.opportunities as f64 + 1.0;
        NegBinomial {
            r: self.count as f64 + 0.5,
            p: n / (n + m as f64),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NegBinomial {
    r: f64,
    p: f64,
}

impl NegBinomial {
    fn ln_pmf(self, k: i64) -> f64 {
        let k = k as f64;
        ln_gamma(k + self.r) - ln_gamma(self.r) - ln_gamma(k + 1.0)
            + self.r * self.p.ln()
            + k * (1.0 - self.p).ln()
    }

    fn mean(self) -> f64 {
        self.r * (1.0 - self.p) / self.p
    }

    /// -ln of the tail in the direction `k` departs from the mean: how
    /// surprising this many, or this few, would be.
    ///
    /// Summed outward from `k` by the pmf's ratio recurrence, so it is linear
    /// in the terms it needs and never subtracts from one, which loses an
    /// upper tail below 1e-16 to rounding.
    fn surprise(self, k: i64) -> f64 {
        let q = 1.0 - self.p;
        let at_k = self.ln_pmf(k);
        let mut term = 1.0; // P(i) / P(k)
        let mut sum = 1.0;
        if (k as f64) > self.mean() {
            let mut i = k as f64;
            // The ratio falls toward q < 1, so the terms decay geometrically.
            while term > sum * 1e-12 {
                term *= (i + self.r) / (i + 1.0) * q;
                sum += term;
                i += 1.0;
            }
        } else {
            for i in (1..=k).rev() {
                let i = i as f64;
                term *= i / ((i - 1.0 + self.r) * q);
                sum += term;
                if term < sum * 1e-12 {
                    break;
                }
            }
        }
        (-(at_k + sum.ln())).max(0.0)
    }
}

/// ln Γ(x) for x > 0 (Lanczos, g = 7), accurate to ~1e-13.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let t = x + 7.5;
    let a = G[0]
        + G.iter()
            .enumerate()
            .skip(1)
            .map(|(i, g)| g / (x + i as f64))
            .sum::<f64>();
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// How one marker in a draft compares with the profile it is checked against.
#[derive(Debug, Clone, PartialEq)]
pub struct Departure {
    pub marker: &'static str,
    pub gloss: &'static str,
    pub per: Per,
    pub draft: Rate,
    pub yours: Rate,
    /// The assistant baseline, when there is one.
    pub baseline: Option<Rate>,
    /// -ln of the chance of a count this far from your habit, given your
    /// profile and how much of it there is.
    pub evidence: f64,
    /// ln of how much likelier the draft's count is under the assistant's
    /// profile than under yours, when there is a baseline. Positive reads as
    /// the assistant.
    pub toward_assistant: Option<f64>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Score a draft marker by marker against your profile, and the assistant
/// baseline if there is one. Every marker with an opportunity in the draft
/// is returned, strongest evidence first; the caller decides the bar.
pub fn compare(draft: &Counts, yours: &Counts, baseline: Option<&Counts>) -> Vec<Departure> {
    let mut out: Vec<Departure> = MARKERS
        .iter()
        .filter_map(|m| {
            let d = Rate::of(draft, m);
            if d.opportunities == 0 {
                return None;
            }
            let y = Rate::of(yours, m);
            let b = baseline
                .map(|b| Rate::of(b, m))
                .filter(|b| b.opportunities > 0);
            let mine = y.predictive(d.opportunities);
            Some(Departure {
                marker: m.name,
                gloss: m.gloss,
                per: m.per,
                draft: d,
                yours: y,
                baseline: b,
                evidence: round2(mine.surprise(d.count)),
                toward_assistant: b.map(|b| {
                    round2(b.predictive(d.opportunities).ln_pmf(d.count) - mine.ln_pmf(d.count))
                }),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.evidence
            .total_cmp(&a.evidence)
            .then(a.marker.cmp(b.marker))
    });
    out
}

/// The evidence a marker must carry to be reported: a count rarer than one
/// in a hundred for you, and, with a baseline, ten times likelier from the
/// assistant. Conventional bars, not tuned ones; both scores are reported
/// either way.
pub const BAR: f64 = 2.0 * std::f64::consts::LN_10;
pub const BAR_TOWARD_ASSISTANT: f64 = std::f64::consts::LN_10;

/// Does this marker depart from your profile strongly enough to report?
///
/// The baseline can only withdraw a departure, never raise one: a draft that
/// is unusual for you is reported only if the assistant's profile explains
/// it better. A baseline that could add findings would add them most where
/// it knows least, since an empty profile predicts everything a little.
pub fn departs(d: &Departure) -> bool {
    d.evidence >= BAR && d.toward_assistant.is_none_or(|t| t >= BAR_TOWARD_ASSISTANT)
}

pub fn add(into: &mut Counts, from: &Counts) {
    for (k, v) in from {
        *into.entry(k.clone()).or_default() += v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(c: &Counts, k: &str) -> i64 {
        c.get(k).copied().unwrap_or(0)
    }

    #[test]
    fn counts_a_chat_message_the_way_it_was_typed() {
        let c = measure("should we ship it? maybe - i think the widget is done\nlet's see");
        assert_eq!(get(&c, "messages"), 1);
        assert_eq!(get(&c, "sentences"), 3);
        assert_eq!(get(&c, "lowercase_start"), 3);
        assert_eq!(get(&c, "question"), 1);
        assert_eq!(get(&c, "spaced_hyphen"), 1);
        assert_eq!(get(&c, "hedge"), 2, "maybe, i think");
        assert_eq!(get(&c, "first_plural"), 2, "we, let's");
        assert_eq!(get(&c, "passive"), 1, "is done");
        assert_eq!(get(&c, "unterminated"), 1);
        assert_eq!(get(&c, "request"), 1);
    }

    #[test]
    fn counts_a_report_the_way_it_was_written() {
        let c = measure(
            "The widget was rebuilt \u{2014} each step ran cleanly. It is ready: \
             nothing remains (the cache is warm); I'll merge it.",
        );
        assert_eq!(get(&c, "lowercase_start"), 0);
        assert_eq!(get(&c, "em_dash"), 1);
        assert_eq!(get(&c, "colon"), 1);
        assert_eq!(get(&c, "semicolon"), 1);
        assert_eq!(get(&c, "parenthesis"), 1);
        assert_eq!(get(&c, "expanded"), 1, "it is");
        assert_eq!(get(&c, "contraction"), 1, "i'll");
        assert_eq!(get(&c, "unterminated"), 0);
        assert_eq!(get(&c, "request"), 0);
    }

    #[test]
    fn code_and_formatting_are_not_voice() {
        let c = measure(
            "- **Fixed:** the widget\n```\nlet x: i32 = foo(a, b); // done\n```\n\
             see `a; b: c` at https://example.com/a,b",
        );
        assert_eq!(get(&c, "semicolon"), 0);
        assert_eq!(get(&c, "comma"), 0);
        // The bullet's colon is the writer's, not the markdown's.
        assert_eq!(get(&c, "colon"), 1);
    }

    #[test]
    fn a_one_word_sign_off_keeps_its_word_but_not_a_sentence() {
        let c = measure("Looks good to me.\nThanks!");
        assert_eq!(get(&c, "sentences"), 1);
        assert_eq!(get(&c, "words"), 5);
        assert_eq!(get(&c, "courtesy"), 1);
    }

    #[test]
    fn a_message_with_no_prose_counts_nothing() {
        assert!(measure("```\nfn main() {}\n```").is_empty());
        assert!(measure("   ").is_empty());
    }

    #[test]
    fn colons_in_times_and_urls_are_not_punctuation() {
        assert_eq!(
            get(&measure("meet at 10:30 for the widget review"), "colon"),
            0
        );
    }

    fn profile(texts: &[&str]) -> Counts {
        let mut c = Counts::new();
        for t in texts {
            add(&mut c, &measure(t));
        }
        c
    }

    #[test]
    fn a_draft_in_the_assistants_habits_departs_toward_the_baseline() {
        let yours = profile(&["should we ship the widget? maybe later"; 40]);
        let baseline = profile(&["The widget is ready: it was rebuilt; nothing remains."; 40]);
        let draft = measure("The release is ready: it was tested; the notes are written.");
        let scored = compare(&draft, &yours, Some(&baseline));
        let colon = scored.iter().find(|d| d.marker == "colon").unwrap();
        assert!(colon.toward_assistant.unwrap() > 0.0, "{colon:?}");
        assert!(scored.iter().any(departs), "{scored:?}");

        let own = measure("should we ship the release? maybe tomorrow");
        let scored = compare(&own, &yours, Some(&baseline));
        assert!(!scored.iter().any(departs), "{scored:?}");
    }

    #[test]
    fn without_a_baseline_departure_is_surprise_under_your_rate() {
        let yours = profile(&["should we ship the widget? maybe later"; 40]);
        let draft = measure("The release is ready: it was tested; the notes are written.");
        let scored = compare(&draft, &yours, None);
        assert!(scored.iter().any(departs), "{scored:?}");
        let same = compare(&measure("should we ship it? maybe later"), &yours, None);
        assert!(!same.iter().any(departs), "{same:?}");
    }

    #[test]
    fn a_thin_baseline_cannot_raise_a_finding() {
        // One assistant message predicts almost anything, so it must not
        // outvote hundreds of sentences of yours.
        let yours = profile(&["should we ship the widget? maybe later"; 40]);
        let thin = profile(&["run the full widget suite"]);
        let draft = measure("thanks, should we ship the widget? maybe later");
        let scored = compare(&draft, &yours, Some(&thin));
        assert!(!scored.iter().any(departs), "{scored:?}");
    }

    #[test]
    fn a_thin_profile_of_yours_is_slow_to_call_anything_unusual() {
        let draft = measure("The widget is ready: it was rebuilt; nothing remains here.");
        let thin = compare(&draft, &profile(&["should we ship it? maybe"; 2]), None);
        let thick = compare(&draft, &profile(&["should we ship it? maybe"; 200]), None);
        let colon = |s: &[Departure]| s.iter().find(|d| d.marker == "colon").unwrap().evidence;
        assert!(
            colon(&thin) < colon(&thick),
            "{} vs {}",
            colon(&thin),
            colon(&thick)
        );
    }

    #[test]
    fn ln_gamma_matches_factorials() {
        for (n, fact) in [(1.0, 1.0), (5.0, 24.0), (11.0, 3_628_800.0f64)] {
            assert!((ln_gamma(n) - fact.ln()).abs() < 1e-9, "{n}");
        }
        assert!((ln_gamma(0.5) - std::f64::consts::PI.sqrt().ln()).abs() < 1e-9);
    }

    #[test]
    fn a_huge_count_scores_quickly_and_finitely() {
        let yours = profile(&["should we ship the widget? maybe later"; 40]);
        let draft = measure(&"you ".repeat(60_000));
        let started = std::time::Instant::now();
        let scored = compare(&draft, &yours, None);
        assert!(started.elapsed().as_secs() < 2);
        assert!(scored.iter().all(|d| d.evidence.is_finite()));
    }
}
