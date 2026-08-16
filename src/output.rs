//! Rendering results to stdout in the requested format.
//!
//! stdout carries only data — these writers never log. Logs go to stderr via
//! `env_logger`. Every command honors the format, not just analysis, so a
//! consumer never has to special-case one subcommand.

use std::io::Write;

use serde_json::json;

use crate::cli::Format;
use crate::types::{Entry, Finding, SeedReport, StatsPayload, StatusPayload};

/// The single structured-output path for findings. `-j` is a pretty array,
/// `-J` is one object per line, so single-blob and streamed input emit an
/// identical shape.
pub fn render_findings(
    out: &mut impl Write,
    findings: &[Finding],
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            if findings.is_empty() {
                return writeln!(out, "No issues found.");
            }
            for f in findings {
                let suggestions = if f.suggestions.is_empty() {
                    "-".to_string()
                } else {
                    f.suggestions.join(", ")
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
                writeln!(
                    out,
                    "{:<28} {:<12} {:>6}  {:.2}",
                    e.word,
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

pub fn render_stats(
    out: &mut impl Write,
    stats: &StatsPayload,
    format: Format,
) -> std::io::Result<()> {
    match format {
        Format::Human => {
            writeln!(out, "db:      {}", stats.db)?;
            writeln!(out, "words:   {}", stats.words)?;
            writeln!(out, "ngrams:  {}", stats.ngrams)?;
            writeln!(out, "spooled: {}", stats.spooled)?;
            if !stats.by_provenance.is_empty() {
                writeln!(out, "\nby provenance:")?;
                for (name, count) in sorted_desc(&stats.by_provenance) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !stats.by_register.is_empty() {
                writeln!(out, "\nby register:")?;
                for (name, count) in sorted_desc(&stats.by_register) {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(stats).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(stats).unwrap()),
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
                writeln!(out, "{:<10} {:<8} {}", t.name, state, t.path.display())?;
                if !t.note.is_empty() {
                    writeln!(out, "{:<19} — {}", "", t.note)?;
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
                        r.target, r.added, r.removed, r.path
                    )?,
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
                    "{:<34} {:>6} {:>9.1}",
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
                    "  {} from others (corroboration only — not your voice)",
                    report.others
                )?;
            }
            if report.assistant > 0 {
                writeln!(
                    out,
                    "  {} assistant-authored (not learned from)",
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
    use crate::types::{FindingKind, Provenance};

    fn finding() -> Finding {
        Finding {
            kind: FindingKind::Unknown,
            word: "shp".into(),
            line: 1,
            col: 1,
            suggestions: vec!["ship".into()],
            confidence: 0.7,
        }
    }

    fn render(format: Format) -> String {
        let mut buf = Vec::new();
        render_findings(&mut buf, &[finding()], format).unwrap();
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
        render_findings(&mut buf, &[], Format::Human).unwrap();
        assert!(String::from_utf8(buf).unwrap().contains("No issues"));
    }

    #[test]
    fn empty_findings_are_an_empty_json_array() {
        let mut buf = Vec::new();
        render_findings(&mut buf, &[], Format::Json).unwrap();
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
        }];
        render_entries(&mut buf, &entries, Format::Human).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("contextdb"));
        assert!(out.contains("owned"));
    }
}
