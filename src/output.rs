//! Rendering results to stdout in the requested format.
//!
//! stdout carries only data — these writers never log. Logs go to stderr via
//! `env_logger`. Every command honors the format, not just analysis, so a
//! consumer never has to special-case one subcommand.

use std::io::Write;

use serde_json::json;

use crate::cli::Format;
use crate::types::{
    Entry, Finding, Rewrite, SeedReport, ShorthandEntry, StatusPayload, StoreStatus, TypoPair,
};

/// The single structured-output path for findings. `-j` is a pretty array,
/// `-J` is one object per line, so single-blob and streamed input emit an
/// identical shape.
/// Emit one finding immediately, for the streaming path.
///
/// The flush is insurance, not the mechanism: Rust's `StdoutLock` is a
/// `LineWriter`, so a `writeln!` already reaches the pipe. It is here because
/// wrapping `out` in a `BufWriter` — an ordinary thing to do for speed —
/// would silently turn that into 8K block buffering, and the symptom is
/// invisible in the output bytes.
pub fn stream_finding(out: &mut impl Write, finding: &Finding) -> std::io::Result<()> {
    writeln!(out, "{}", serde_json::to_string(finding).unwrap())?;
    out.flush()
}

/// One applied fix, or a shorthand left alone. Human and `-J` report it as
/// it happens; `-j` collects them for `render_rewrites`.
pub fn stream_rewrite(
    out: &mut impl Write,
    rewrite: &Rewrite,
    format: Format,
) -> std::io::Result<()> {
    match (format, rewrite) {
        (Format::Human, Rewrite::Fix(fix)) => writeln!(
            out,
            "{}:{:<4} {} -> {}",
            fix.line, fix.col, fix.from, fix.to
        )?,
        (Format::Human, Rewrite::LeftAlone(left)) => {
            let maybe = left.maybe.join("? ");
            let maybe = if maybe.is_empty() {
                maybe
            } else {
                format!(" ({maybe}?)")
            };
            writeln!(
                out,
                "{}:{:<4} {} left alone{maybe}",
                left.line, left.col, left.word
            )?
        }
        (Format::Ndjson, _) => writeln!(out, "{}", serde_json::to_string(rewrite).unwrap())?,
        (Format::Json, _) => return Ok(()),
    }
    out.flush()
}

/// The report as one `-j` document; the other formats already streamed it.
pub fn render_rewrites(
    out: &mut impl Write,
    rewrites: &[Rewrite],
    format: Format,
) -> std::io::Result<()> {
    if format == Format::Json {
        writeln!(out, "{}", serde_json::to_string_pretty(rewrites).unwrap())?;
    }
    Ok(())
}

pub fn render_findings(
    out: &mut impl Write,
    findings: &[Finding],
    checked: Option<&str>,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if findings.is_empty() {
                // Name what was checked when it was a single bare word. A
                // mistyped or imagined subcommand — `vocab status`, `vocab
                // log` — is valid English, so it checks clean and a bare "no
                // issues found" reads as if the subcommand ran. Echoing the
                // word makes the interpretation obvious at a glance.
                return match checked {
                    Some(word) => writeln!(out, "\"{word}\" is spelled correctly."),
                    None => writeln!(out, "No issues found."),
                };
            }
            for f in findings {
                // Each suggestion carries its own share of the belief, so
                // "help 0.94, hep 0.04" reads very differently from a flat list.
                let suggestions = if f.suggestions.is_empty() {
                    "-".to_string()
                } else {
                    f.suggestions
                        .iter()
                        .map(|s| format!("{} {:.2}", s.word, s.score))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                writeln!(
                    out,
                    "{}:{:<4} {:<20} {:<10} {:<30} {:.2}",
                    f.line,
                    f.col,
                    f.word,
                    f.kind.as_str(),
                    suggestions,
                    f.confidence
                )?;
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(findings).unwrap()),
        Format::Ndjson => {
            for f in findings {
                writeln!(out, "{}", serde_json::to_string(f).unwrap())?;
            }
            Ok(())
        }
    }
}

pub fn render_entries(
    out: &mut impl Write,
    entries: &[Entry],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if entries.is_empty() {
                return writeln!(out, "No words.");
            }
            for e in entries {
                // A settled meaning plainly, a guess with its question mark.
                let meaning = match (&e.means, e.maybe.as_slice()) {
                    (Some(m), _) => format!("  = {m}"),
                    (None, []) => String::new(),
                    (None, maybe) => format!("  = {}?", maybe.join("? ")),
                };
                writeln!(
                    out,
                    "{:<28} {:<9} {:<12} {:>6}  {:.2}{meaning}",
                    e.word,
                    e.kind.as_str(),
                    e.provenance.as_str(),
                    e.count,
                    e.validity
                )?;
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(entries).unwrap()),
        Format::Ndjson => {
            for e in entries {
                writeln!(out, "{}", serde_json::to_string(e).unwrap())?;
            }
            Ok(())
        }
    }
}

pub fn render_seed(
    out: &mut impl Write,
    report: &SeedReport,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            for s in &report.sources {
                match &s.skipped {
                    Some(reason) => writeln!(
                        out,
                        "{:<14} {:<12} skipped ({reason})",
                        s.name,
                        s.provenance.as_str()
                    )?,
                    None => writeln!(
                        out,
                        "{:<14} {:<12} {:>6} terms",
                        s.name,
                        s.provenance.as_str(),
                        s.terms
                    )?,
                }
            }
            writeln!(
                out,
                "\n{} words added, {} upgraded",
                report.added, report.upgraded
            )
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(report).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(report).unwrap()),
    }
}

pub fn render_templates(
    out: &mut impl Write,
    report: &crate::template::TemplateReport,
    apply: bool,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if report.templates.is_empty() {
                return writeln!(out, "No repeated runs — nothing reads like boilerplate.");
            }
            for t in &report.templates {
                writeln!(
                    out,
                    "{:>5} words  x{:<4}  flat {:.2}  confidence {:.2}",
                    t.length, t.repeats, t.flatness, t.confidence
                )?;
                writeln!(out, "  {}\n", t.text)?;
            }
            if apply {
                writeln!(out, "removed {} phrases", report.ngrams_removed)
            } else {
                writeln!(out, "(scan only — nothing removed; --apply to remove)")
            }
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(report).unwrap()),
        Format::Ndjson => {
            for t in &report.templates {
                writeln!(out, "{}", serde_json::to_string(t).unwrap())?;
            }
            Ok(())
        }
    }
}

pub fn render_prune(
    out: &mut impl Write,
    report: &crate::prune::PruneReport,
    dry_run: bool,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            let verb = if dry_run { "would remove" } else { "removed" };
            writeln!(out, "{verb} {} phrases", report.ngrams_removed)?;
            if report.words_removed > 0 {
                writeln!(out, "{verb} {} words", report.words_removed)?;
            }
            if !report.sample.is_empty() {
                writeln!(out, "\nsample:")?;
                for item in &report.sample {
                    writeln!(out, "  {item}")?;
                }
            }
            if dry_run {
                writeln!(out, "\n(dry run — nothing removed)")?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "ngrams_removed": report.ngrams_removed,
                "words_removed": report.words_removed,
                "sample": report.sample,
                "dry_run": dry_run,
            }),
            format,
        ),
    }
}

pub fn render_unlearn(
    out: &mut impl Write,
    report: &crate::process::UnlearnReport,
    dry_run: bool,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            let verb = if dry_run {
                "would take back"
            } else {
                "took back"
            };
            writeln!(
                out,
                "{verb} {} phrase counts, {} word counts, {} word documents and {} style counts from {} texts",
                report.phrase_counts,
                report.word_counts,
                report.word_sources,
                report.voice_counts,
                report.texts
            )?;
            if report.phrases_emptied > 0 || report.exemplars > 0 {
                writeln!(
                    out,
                    "{} phrases and {} exemplars gone entirely",
                    report.phrases_emptied, report.exemplars
                )?;
            }
            if !report.sample.is_empty() {
                writeln!(out, "\nmost affected:")?;
                for (gram, n) in &report.sample {
                    writeln!(out, "  {gram:<36} -{n}")?;
                }
            }
            if dry_run {
                writeln!(out, "\n(dry run — nothing changed)")?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "texts": report.texts,
                "unmatched": report.unmatched,
                "near_copies": report.near_copies,
                "phrase_counts": report.phrase_counts,
                "phrases_emptied": report.phrases_emptied,
                "word_counts": report.word_counts,
                "word_sources": report.word_sources,
                "voice_counts": report.voice_counts,
                "exemplars": report.exemplars,
                "sample": report.sample.iter().map(|(g, n)| json!({"phrase": g, "taken": n})).collect::<Vec<_>>(),
                "dry_run": dry_run,
            }),
            format,
        ),
    }
}

fn voice_rate(rate: crate::voice::Rate, per: crate::voice::Per) -> Option<f64> {
    rate.shown(per)
}

fn per_label(per: crate::voice::Per) -> &'static str {
    match per {
        crate::voice::Per::Sentence => "per sentence",
        crate::voice::Per::Word => "per 1000 words",
        crate::voice::Per::Message => "per message",
    }
}

pub fn render_voice_profile(
    out: &mut impl Write,
    register: crate::types::Register,
    yours: &crate::voice::Counts,
    baseline: &crate::voice::Counts,
    format: Format,
) -> std::io::Result<()> {
    use crate::voice::{MARKERS, Rate};
    let total = |c: &crate::voice::Counts, k: &str| c.get(k).copied().unwrap_or(0);
    let rows: Vec<_> = MARKERS
        .iter()
        .map(|m| {
            let y = Rate::of(yours, m);
            let b = Rate::of(baseline, m);
            // How many times the baseline's rate yours is, smoothed so a
            // marker one side never used is a large ratio, not infinity.
            let ratio = (b.opportunities > 0 && y.opportunities > 0)
                .then(|| crate::voice::significant(y.smoothed() / b.smoothed(), 2));
            (m, y, b, ratio)
        })
        .collect();
    match format {
        Format::Human => {
            writeln!(
                out,
                "{}: {} messages, {} sentences, {} words of yours",
                register.as_str(),
                total(yours, "messages"),
                total(yours, "sentences"),
                total(yours, "words")
            )?;
            let has_baseline = total(baseline, "sentences") > 0;
            if has_baseline {
                writeln!(
                    out,
                    "baseline: {} sentences the assistant drafted",
                    total(baseline, "sentences")
                )?;
            }
            if total(yours, "sentences") == 0 {
                return Ok(());
            }
            writeln!(out)?;
            for (m, y, b, ratio) in &rows {
                let Some(rate) = voice_rate(*y, m.per) else {
                    continue;
                };
                write!(out, "  {:<48} {rate:>7} {}", m.gloss, per_label(m.per))?;
                if let (Some(br), Some(ratio)) = (voice_rate(*b, m.per), ratio) {
                    write!(out, "   assistant {br}  x{ratio}")?;
                }
                writeln!(out)?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "register": register.as_str(),
                "messages": total(yours, "messages"),
                "sentences": total(yours, "sentences"),
                "words": total(yours, "words"),
                "baseline_sentences": total(baseline, "sentences"),
                "markers": rows.iter().map(|(m, y, b, ratio)| json!({
                    "marker": m.name,
                    "per": m.per.denominator(),
                    "count": y.count,
                    "rate": voice_rate(*y, m.per),
                    "baseline_count": b.count,
                    "baseline_rate": voice_rate(*b, m.per),
                    "ratio": ratio,
                })).collect::<Vec<_>>(),
            }),
            format,
        ),
    }
}

pub fn render_voice_check(
    out: &mut impl Write,
    register: crate::types::Register,
    scored: &[crate::voice::Departure],
    against_baseline: bool,
    format: Format,
) -> std::io::Result<()> {
    let flagged: Vec<_> = scored.iter().filter(|d| crate::voice::departs(d)).collect();
    // Every marker has words, sentences or messages as its denominator, so a
    // draft with none of them was not checked at all.
    let checked = !scored.is_empty();
    match format {
        Format::Human => {
            if !checked {
                writeln!(out, "no prose to check")?;
                return Ok(());
            }
            if flagged.is_empty() {
                writeln!(out, "reads like your {} writing", register.as_str())?;
                return Ok(());
            }
            let against = if against_baseline {
                "unlike you, and more like the assistant"
            } else {
                "unlike you"
            };
            writeln!(
                out,
                "{} markers read {against} ({}):",
                flagged.len(),
                register.as_str()
            )?;
            for d in flagged {
                let draft = voice_rate(d.draft, d.per).unwrap_or(0.0);
                let yours = voice_rate(d.yours, d.per).unwrap_or(0.0);
                write!(out, "  {:<48} draft {draft}  you {yours}", d.gloss)?;
                if let Some(b) = d.baseline.and_then(|b| voice_rate(b, d.per)) {
                    write!(out, "  assistant {b}")?;
                }
                writeln!(out, "   {} (evidence {})", per_label(d.per), d.evidence)?;
            }
            Ok(())
        }
        _ => {
            let row = |d: &crate::voice::Departure| {
                json!({
                    "marker": d.marker,
                    "per": d.per.denominator(),
                    "draft_count": d.draft.count,
                    "draft_opportunities": d.draft.opportunities,
                    "draft_rate": voice_rate(d.draft, d.per),
                    "your_rate": voice_rate(d.yours, d.per),
                    "baseline_rate": d.baseline.and_then(|b| voice_rate(b, d.per)),
                    "evidence": d.evidence,
                    "toward_assistant": d.toward_assistant,
                    "departs": crate::voice::departs(d),
                })
            };
            if format == Format::Ndjson {
                for d in scored {
                    emit(out, &row(d), format)?;
                }
                return Ok(());
            }
            emit(
                out,
                &json!({
                    "register": register.as_str(),
                    "against": if against_baseline { "baseline" } else { "you" },
                    "checked": checked,
                    "departures": flagged.len(),
                    "markers": scored.iter().map(row).collect::<Vec<_>>(),
                }),
                format,
            )
        }
    }
}

pub fn render_voice_describe(
    out: &mut impl Write,
    register: crate::types::Register,
    text: &crate::voice::Counts,
    yours: &crate::voice::Counts,
    baseline: &crate::voice::Counts,
    format: Format,
) -> std::io::Result<()> {
    use crate::voice::{MARKERS, Rate};
    let total = |c: &crate::voice::Counts, k: &str| c.get(k).copied().unwrap_or(0);
    // A marker the text gave no chance to show (no sentences) says nothing.
    let rows: Vec<_> = MARKERS
        .iter()
        .map(|m| (m, Rate::of(text, m)))
        .filter(|(_, r)| r.opportunities > 0)
        .map(|(m, r)| {
            let (y, b) = (Rate::of(yours, m), Rate::of(baseline, m));
            (m, r, voice_rate(y, m.per), voice_rate(b, m.per))
        })
        .collect();
    let row = |(m, r, y, b): &(&crate::voice::Marker, Rate, Option<f64>, Option<f64>)| {
        json!({
            "marker": m.name,
            "per": m.per.denominator(),
            "count": r.count,
            "opportunities": r.opportunities,
            "rate": voice_rate(*r, m.per),
            "your_rate": y,
            "baseline_rate": b,
        })
    };
    match format {
        Format::Human => {
            writeln!(
                out,
                "{} sentences, {} words",
                total(text, "sentences"),
                total(text, "words")
            )?;
            if !rows.is_empty() && total(yours, "messages") == 0 {
                writeln!(out, "no {} profile of yours yet", register.as_str())?;
            }
            for (m, r, y, b) in &rows {
                let rate = voice_rate(*r, m.per).unwrap_or(0.0);
                let counted = format!("({}/{})", r.count, r.opportunities);
                let mut line = format!(
                    "  {:<50} {rate:>7} {:<15} {counted:<9}",
                    m.gloss,
                    per_label(m.per)
                );
                if let Some(y) = y {
                    line += &format!("  your {} {y}", register.as_str());
                }
                if let Some(b) = b {
                    line += &format!("  assistant {b}");
                }
                writeln!(out, "{}", line.trim_end())?;
            }
            Ok(())
        }
        Format::Ndjson => {
            for r in &rows {
                emit(out, &row(r), format)?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "register": register.as_str(),
                "messages": total(text, "messages"),
                "sentences": total(text, "sentences"),
                "words": total(text, "words"),
                "markers": rows.iter().map(row).collect::<Vec<_>>(),
            }),
            format,
        ),
    }
}

pub fn render_voice_load(
    out: &mut impl Write,
    profiles: &std::collections::BTreeMap<&str, crate::voice::Counts>,
    author: &str,
    skipped: usize,
    empty: usize,
    format: Format,
) -> std::io::Result<()> {
    let sentences = |c: &crate::voice::Counts| c.get("sentences").copied().unwrap_or(0);
    match format {
        Format::Human => {
            for (register, counts) in profiles {
                writeln!(
                    out,
                    "replaced {author} {register}: {} sentences",
                    sentences(counts)
                )?;
            }
            if skipped > 0 {
                writeln!(out, "skipped {skipped} not written by you")?;
            }
            if empty > 0 {
                writeln!(out, "skipped {empty} with no prose")?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "author": author,
                "skipped": skipped,
                "empty": empty,
                "registers": profiles.iter().map(|(r, c)| json!({
                    "register": r,
                    "messages": c.get("messages").copied().unwrap_or(0),
                    "sentences": sentences(c),
                })).collect::<Vec<_>>(),
            }),
            format,
        ),
    }
}

pub fn render_status(
    out: &mut impl Write,
    status: &StoreStatus,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            writeln!(out, "db:      {}", tilde(&status.db))?;
            writeln!(out, "seeded:  {}", age(status.seeded_secs_ago))?;
            writeln!(out, "words:   {}", status.words)?;
            writeln!(out, "ngrams:  {}", status.ngrams)?;
            writeln!(out, "spooled: {}", status.spooled)?;
            if !status.by_provenance.is_empty() {
                writeln!(out, "\nby provenance:")?;
                for (name, count) in sorted_desc(&status.by_provenance) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !status.by_register.is_empty() {
                writeln!(out, "\nby register:")?;
                for (name, count) in sorted_desc(&status.by_register) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !status.documents.is_empty() {
                writeln!(out, "\nread (bodies):")?;
                for (name, count) in sorted_desc(&status.documents) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !status.messages.is_empty() {
                writeln!(out, "\nmessages captured:")?;
                for (name, count) in sorted_desc(&status.messages) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !status.integrations.is_empty() {
                writeln!(out, "\nspell checkers:")?;
                for i in &status.integrations {
                    // `ours` and `total` differ where the file is shared with
                    // the user's own additions, and that difference is the
                    // whole reason `unsync` is safe — worth showing.
                    let state = match (i.present, i.ours) {
                        (false, _) => "not synced".to_string(),
                        (true, 0) => format!("{} words, none ours", i.total),
                        (true, ours) if ours == i.total => format!("{ours} words"),
                        (true, ours) => format!("{ours} of {} words ours", i.total),
                    };
                    // "present" answers whether we wrote the file, which is
                    // the question people mistake for whether it works.
                    let reach = match i.activation.as_str() {
                        _ if i.live => "",
                        // Flow imports from the cloud: unknowable, not broken.
                        "manual" => "  (imported by hand)",
                        _ => "  (not in effect)",
                    };
                    let synced = match i.synced_secs_ago {
                        Some(_) => format!(", synced {}", age(i.synced_secs_ago)),
                        None => String::new(),
                    };
                    writeln!(out, "  {:<14} {state}{synced}{reach}", i.name)?;
                    writeln!(out, "  {:<14} {}", "", tilde(&i.path))?;
                    if i.withdrawn > 0 {
                        writeln!(
                            out,
                            "  {:<14} stale: {} exported words no longer qualify — run `vocab sync`",
                            "", i.withdrawn
                        )?;
                    }
                    if let Some(hint) = &i.hint {
                        for line in hint.lines() {
                            writeln!(out, "  {:<14} {line}", "")?;
                        }
                    }
                }
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(status).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(status).unwrap()),
    }
}

pub fn render_targets(
    out: &mut impl Write,
    targets: &[crate::sync::Target],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            for t in targets {
                let state = if t.path.exists() { "present" } else { "absent" };
                let wiring = t.wiring();
                writeln!(
                    out,
                    "{:<10} {:<8} {:<9} {}",
                    t.name,
                    state,
                    wiring.state.as_str(),
                    tilde(&t.path.display().to_string())
                )?;
                if !t.note.is_empty() {
                    writeln!(out, "{:<19} — {}", "", t.note)?;
                }
                if wiring.state == crate::sync::Activation::Inert {
                    writeln!(out, "{:<19} — not referenced by any editor config", "")?;
                }
            }
            Ok(())
        }
        _ => {
            let rows: Vec<_> = targets
                .iter()
                .map(|t| {
                    json!({
                        "target": t.name,
                        "path": t.path.display().to_string(),
                        "owned": t.owned,
                        "exists": t.path.exists(),
                        "activation": t.wiring().state.as_str(),
                        "live": t.wiring().state.live(),
                        "note": t.note,
                    })
                })
                .collect();
            emit(out, &json!(rows), format)
        }
    }
}

pub fn render_sync(
    out: &mut impl Write,
    reports: &[crate::sync::SyncReport],
    dry_run: bool,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            for r in reports {
                match &r.skipped {
                    Some(reason) => writeln!(out, "{:<10} skipped ({reason})", r.target)?,
                    None => writeln!(
                        out,
                        "{:<10} +{:<6} -{:<6} {}",
                        r.target,
                        r.added,
                        r.removed,
                        tilde(&r.path)
                    )?,
                }
                if !r.withdrawn.is_empty() {
                    let named: Vec<String> = r
                        .withdrawn
                        .iter()
                        .map(|w| match &w.typo_of {
                            Some(intended) => format!("{} (a typo of {intended}?)", w.word),
                            None => w.word.clone(),
                        })
                        .collect();
                    writeln!(out, "{:<10} withdrawn: {}", "", named.join(", "))?;
                    writeln!(
                        out,
                        "{:<10} a real word among them stays with `vocab add`, a \
                         shorthand with `vocab shorthand`",
                        ""
                    )?;
                }
                // Written is not the same as in effect, and the difference
                // used to be invisible: a correct file nothing was pointed at
                // reported exactly like one that worked.
                match r.activation {
                    Some(crate::sync::Activation::Inert) => {
                        writeln!(
                            out,
                            "{:<10} not in effect — nothing reads this file yet",
                            ""
                        )?;
                        // The hint says where it goes as well as what to
                        // paste; `config` carries the path on its own for a
                        // consumer that wants it structured.
                        if let Some(hint) = &r.hint {
                            for line in hint.lines() {
                                writeln!(out, "{:<12} {line}", "")?;
                            }
                        }
                    }
                    Some(crate::sync::Activation::Manual) => {
                        if let Some(hint) = &r.hint {
                            writeln!(out, "{:<10} needs a manual step — {hint}", "")?;
                        }
                    }
                    _ => {}
                }
            }
            if dry_run {
                writeln!(out, "\n(dry run — nothing written)")?;
            }
            Ok(())
        }
        _ => {
            let rows: Vec<_> = reports
                .iter()
                .map(|r| {
                    json!({
                        "target": r.target,
                        "path": r.path,
                        "added": r.added,
                        "removed": r.removed,
                        "total": r.total,
                        "skipped": r.skipped,
                        "activation": r.activation.map(|a| a.as_str()),
                        "live": r.activation.map(crate::sync::Activation::live),
                        "hint": r.hint,
                        "config": r.config,
                        "withdrawn": r.withdrawn,
                        "dry_run": dry_run,
                    })
                })
                .collect();
            emit(out, &json!(rows), format)
        }
    }
}

/// Biggest first, for human reading. The payload itself is a map — keyed by
/// name so JSON consumers get an object — so display order is a rendering
/// concern rather than something the data has to carry.
/// A coarse "how long ago", for facts where the exact second is noise.
fn age(seconds: Option<i64>) -> String {
    let Some(s) = seconds else {
        return "never".to_string();
    };
    match s {
        s if s < 90 => "just now".to_string(),
        s if s < 5400 => format!("{} minutes ago", s / 60),
        s if s < 172_800 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

/// Abbreviate `$HOME` to `~` for display.
///
/// Human output only. JSON keeps the absolute path, because `~` is a shell
/// convention rather than a filesystem one — a consumer that reads the field
/// and opens it would have to know to expand it, and most won't.
pub fn tilde(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    // Only at a path boundary: `/Users/dan-old/x` must not become `~-old/x`.
    match path.strip_prefix(home.as_ref()) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

fn sorted_desc(counts: &std::collections::BTreeMap<String, i64>) -> Vec<(&String, &i64)> {
    let mut rows: Vec<_> = counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    rows
}

/// Write a JSON value in the caller's flavor: pretty for `--json`, compact
/// one-per-line for `--ndjson`.
fn emit(out: &mut impl Write, value: &serde_json::Value, format: Format) -> std::io::Result<()> {
    match format {
        Format::Ndjson => match value.as_array() {
            Some(rows) => {
                for row in rows {
                    writeln!(out, "{}", serde_json::to_string(row).unwrap())?;
                }
                Ok(())
            }
            None => writeln!(out, "{}", serde_json::to_string(value).unwrap()),
        },
        _ => writeln!(out, "{}", serde_json::to_string_pretty(value).unwrap()),
    }
}

pub fn render_analysis(
    out: &mut impl Write,
    report: &crate::complexity::Report,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            let v = &report.vocabulary;
            writeln!(out, "{}", report.scope)?;
            writeln!(out, "  {:<22} {:>10}", "tokens", v.tokens)?;
            writeln!(out, "  {:<22} {:>10}", "types", v.types)?;
            writeln!(
                out,
                "  {:<22} {:>10.3}",
                "type-token ratio", v.type_token_ratio
            )?;
            writeln!(out, "  {:<22} {:>10.2}", "guiraud R", v.guiraud_r)?;
            writeln!(out, "  {:<22} {:>10.3}", "hapax ratio", v.hapax_ratio)?;
            writeln!(
                out,
                "  {:<22} {:>10.2}",
                "mean word length", v.mean_word_length
            )?;
            writeln!(
                out,
                "  {:<22} {:>10.3}",
                "long word ratio", v.long_word_ratio
            )?;

            match &report.readability {
                Some(r) => {
                    writeln!(out, "\n  {:<22} {:>10}", "sentences", r.sentences)?;
                    writeln!(
                        out,
                        "  {:<22} {:>10.2}",
                        "mean sentence length", r.mean_sentence_length
                    )?;
                    writeln!(
                        out,
                        "  {:<22} {:>10.2}",
                        "sentence length sd", r.sentence_length_stddev
                    )?;
                    writeln!(
                        out,
                        "  {:<22} {:>10.2}",
                        "syllables per word", r.mean_syllables_per_word
                    )?;
                    writeln!(
                        out,
                        "  {:<22} {:>10.1}",
                        "flesch reading ease", r.flesch_reading_ease
                    )
                }
                // Say why, rather than leaving a silent gap.
                None => writeln!(
                    out,
                    "\n  (no sentence metrics — processing keeps counts, not prose)"
                ),
            }
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(report).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(report).unwrap()),
    }
}

pub fn render_phrases(
    out: &mut impl Write,
    phrases: &[crate::ngram::Collocation],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if phrases.is_empty() {
                return writeln!(out, "No phrases yet — capture and process some text first.");
            }
            for p in phrases {
                writeln!(
                    out,
                    "{:<34} {:>6} {:>9.2}",
                    p.gram, p.count, p.log_likelihood
                )?;
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(phrases).unwrap()),
        Format::Ndjson => {
            for p in phrases {
                writeln!(out, "{}", serde_json::to_string(p).unwrap())?;
            }
            Ok(())
        }
    }
}

pub fn render_ingest(
    out: &mut impl Write,
    report: &crate::ingest::IngestReport,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            writeln!(out, "ingested {}", report.ingested)?;
            if report.others > 0 {
                writeln!(
                    out,
                    "  {} from others (vocabulary only — not your voice)",
                    report.others
                )?;
            }
            if report.assistant > 0 {
                writeln!(
                    out,
                    "  {} assistant-authored (vocabulary only — not your voice)",
                    report.assistant
                )?;
            }
            if report.skipped > 0 {
                writeln!(out, "  {} skipped (empty)", report.skipped)?;
            }
            Ok(())
        }
        _ => emit(
            out,
            &json!({
                "ingested": report.ingested,
                "others": report.others,
                "assistant": report.assistant,
                "skipped": report.skipped,
            }),
            format,
        ),
    }
}

pub fn render_eval(
    out: &mut impl Write,
    report: &crate::eval::EvalReport,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            writeln!(out, "  {:<22} {:>8}", "lines", report.lines)?;
            writeln!(out, "  {:<22} {:>8}", "words checked", report.words)?;
            writeln!(out, "  {:<22} {:>8}", "injected errors", report.injected)?;
            writeln!(out, "  {:<22} {:>8}", "findings", report.findings)?;
            writeln!(out)?;
            writeln!(out, "  {:<22} {:>8}", "caught", report.caught)?;
            writeln!(
                out,
                "  {:<22} {:>8}",
                "false positives", report.false_positives
            )?;
            let per_1k = report
                .false_positive_rate
                .map_or("n/a".to_string(), |r| format!("{r:.2}"));
            writeln!(out, "  {:<22} {:>8}", "  per 1k words", per_1k)?;
            writeln!(out)?;
            writeln!(out, "  {:<22} {:>9}", "recall", percent(report.recall))?;
            writeln!(
                out,
                "  {:<22} {:>9}",
                "precision",
                percent(report.precision)
            )?;
            writeln!(
                out,
                "  {:<22} {:>9}",
                "correction rate",
                percent(report.correction_rate)
            )?;
            writeln!(
                out,
                "  {:<22} {:>9}",
                "  right one first",
                percent(report.top1_rate)
            )?;

            if !report.by_kind.is_empty() {
                writeln!(out, "\n  by error kind (caught/injected, first/caught):")?;
                for k in &report.by_kind {
                    writeln!(
                        out,
                        "    {:<16} {:>4}/{:<4} {:>4.0}%   {:>4}/{:<4} {:>5}",
                        k.kind,
                        k.caught,
                        k.injected,
                        k.recall * 100.0,
                        k.corrected_top1,
                        k.caught,
                        whole_percent(k.top1_rate)
                    )?;
                }
            }
            writeln!(out, "\n  --fix (fixed/injected, misfixed, false fixes):")?;
            writeln!(
                out,
                "    {:>4}/{:<4} {:>5}   {:>4}   {:>4}",
                report.fixed,
                report.injected,
                whole_percent(report.fix_rate),
                report.misfixed,
                report.false_fixes
            )?;
            for line in &report.wrong_fixes {
                writeln!(out, "    {line}")?;
            }
            if !report.false_positive_sample.is_empty() {
                writeln!(out, "\n  false positives (sample):")?;
                for line in &report.false_positive_sample {
                    writeln!(out, "    {line}")?;
                }
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(report).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(report).unwrap()),
    }
}

/// A rate as a percentage, or `n/a` when there was nothing to divide by.
fn percent(rate: Option<f64>) -> String {
    rate.map_or("n/a".to_string(), |r| format!("{:.1}%", r * 100.0))
}

fn whole_percent(rate: Option<f64>) -> String {
    rate.map_or("n/a".to_string(), |r| format!("{:.0}%", r * 100.0))
}

pub fn render_identities(
    out: &mut impl Write,
    handles: &[(String, String)],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if handles.is_empty() {
                return writeln!(
                    out,
                    "No identities — capture from reads is off.\nRun `vocab seed` to detect them, or add one: vocab self <handle>"
                );
            }
            for (handle, source) in handles {
                writeln!(out, "{handle:<34} {source}")?;
            }
            Ok(())
        }
        _ => {
            let rows: Vec<_> = handles
                .iter()
                .map(|(handle, source)| json!({ "handle": handle, "source": source }))
                .collect();
            emit(out, &json!(rows), format)
        }
    }
}

pub fn render_names(
    out: &mut impl Write,
    reports: &[crate::types::NameReport],
    format: Format,
) -> std::io::Result<()> {
    if format != Format::Human {
        return emit(out, &json!(reports), format);
    }
    for r in reports {
        let mut parts = Vec::new();
        if !r.known {
            parts.push("not a name we know".to_string());
        }
        if let Some(g) = r.given {
            parts.push(format!("given {g}/M"));
        }
        if let Some(s) = r.surname {
            parts.push(format!("surname {s}/M"));
        }
        if r.word {
            parts.push("also a word".to_string());
        }
        if !r.full_names.is_empty() {
            parts.push(format!("short for {}", r.full_names.join(", ")));
        }
        if !r.nicknames.is_empty() {
            parts.push(format!("nicknames {}", r.nicknames.join(", ")));
        }
        let matched = |m: &[crate::types::NameMatch]| {
            m.iter()
                .map(|m| format!("{} ({})", m.name, m.how.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !r.people.is_empty() {
            parts.push(format!("you write {}", matched(&r.people)));
        }
        if let Some(m) = &r.matches {
            parts.push(if m.is_empty() {
                "matches no one".to_string()
            } else {
                format!("matches {}", matched(m))
            });
        }
        writeln!(out, "{:<16} {}", r.name, parts.join(" · "))?;
    }
    Ok(())
}

pub fn render_typos(
    out: &mut impl Write,
    pairs: &[TypoPair],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if pairs.is_empty() {
                return writeln!(out, "No typos.");
            }
            for p in pairs {
                let edit = p.edit.map_or("", |e| e.as_str());
                // A candidate says what held it back; the others need no reason.
                let tier = match p.held_back {
                    Some(why) => format!("{} ({})", p.tier.as_str(), why.as_str()),
                    None => p.tier.as_str().to_string(),
                };
                writeln!(
                    out,
                    "{:<20} {:<20} {:<21} {:<14} {:>4} docs  x{:<6} {:.2}",
                    p.typo, p.intended, tier, edit, p.docs, p.ratio, p.score
                )?;
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(pairs).unwrap()),
        Format::Ndjson => {
            for p in pairs {
                writeln!(out, "{}", serde_json::to_string(p).unwrap())?;
            }
            Ok(())
        }
    }
}

pub fn render_shorthands(
    out: &mut impl Write,
    shorthands: &[ShorthandEntry],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if shorthands.is_empty() {
                return writeln!(out, "No shorthands.");
            }
            for s in shorthands {
                let means = match &s.means {
                    Some(m) => m.clone(),
                    None => s.maybe.join("? ") + "?",
                };
                writeln!(out, "{:<20} {:<40} {}", s.word, means, s.tier.as_str())?;
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(shorthands).unwrap()),
        Format::Ndjson => {
            for s in shorthands {
                writeln!(out, "{}", serde_json::to_string(s).unwrap())?;
            }
            Ok(())
        }
    }
}

/// A command result — success or failure — in the caller's format.
pub fn status(out: &mut impl Write, message: &str, format: Format) -> std::io::Result<()> {
    match format {
        Format::Human => writeln!(out, "{message}"),
        Format::Json => writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&StatusPayload {
                status: message.to_string(),
                detail: None,
            })
            .unwrap()
        ),
        Format::Ndjson => writeln!(out, "{}", json!({ "status": message })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FindingKind, Provenance, Suggestion};

    fn finding() -> Finding {
        Finding {
            kind: FindingKind::Unknown,
            word: "shp".into(),
            line: 1,
            col: 1,
            suggestions: vec![Suggestion {
                word: "ship".into(),
                score: 1.0,
            }],
            confidence: 0.7,
        }
    }

    fn render(format: Format) -> String {
        let mut buf = Vec::new();
        render_findings(&mut buf, &[finding()], None, format).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn json_is_a_pretty_array() {
        let out = render(Format::Json);
        assert!(out.trim_start().starts_with('['));
        assert!(out.contains("\"word\": \"shp\""));
    }

    #[test]
    fn ndjson_is_one_compact_object_per_line() {
        let out = render(Format::Ndjson);
        assert_eq!(out.lines().count(), 1);
        assert!(out.contains("{\"kind\":\"unknown\""));
    }

    #[test]
    fn human_output_mentions_the_word_and_suggestion() {
        let out = render(Format::Human);
        assert!(out.contains("shp"));
        assert!(out.contains("ship"));
    }

    #[test]
    fn empty_findings_still_say_something_in_human_mode() {
        let mut buf = Vec::new();
        render_findings(&mut buf, &[], None, Format::Human).unwrap();
        assert!(String::from_utf8(buf).unwrap().contains("No issues"));
    }

    #[test]
    fn empty_findings_are_an_empty_json_array() {
        let mut buf = Vec::new();
        render_findings(&mut buf, &[], None, Format::Json).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap().trim(), "[]");
    }

    #[test]
    fn status_honors_the_format() {
        let mut buf = Vec::new();
        status(&mut buf, "seeded", Format::Json).unwrap();
        assert!(
            String::from_utf8(buf)
                .unwrap()
                .contains("\"status\": \"seeded\"")
        );
    }

    #[test]
    fn entries_render_provenance() {
        let mut buf = Vec::new();
        let entries = vec![Entry {
            word: "contextdb".into(),
            provenance: Provenance::Owned,
            count: 3,
            sources: 2,
            validity: 0.96,
            kind: crate::types::Kind::Name,
            means: None,
            maybe: Vec::new(),
        }];
        render_entries(&mut buf, &entries, Format::Human).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("contextdb"));
        assert!(out.contains("owned"));
    }

    #[test]
    fn tilde_abbreviates_only_at_a_path_boundary() {
        // SAFETY: single-threaded test, and HOME is read, not cached.
        unsafe { std::env::set_var("HOME", "/Users/dan") };
        assert_eq!(tilde("/Users/dan/x"), "~/x");
        assert_eq!(tilde("/Users/dan"), "~");
        // `dan-old` merely starts with the same characters.
        assert_eq!(tilde("/Users/dan-old/x"), "/Users/dan-old/x");
        assert_eq!(tilde("/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn age_reads_coarsely() {
        assert_eq!(age(None), "never");
        assert_eq!(age(Some(10)), "just now");
        assert_eq!(age(Some(600)), "10 minutes ago");
        assert_eq!(age(Some(7200)), "2 hours ago");
        assert_eq!(age(Some(864_000)), "10 days ago");
    }
}
