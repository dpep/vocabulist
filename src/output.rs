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
                for (name, count) in &stats.by_provenance {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            if !stats.by_register.is_empty() {
                writeln!(out, "\nby register:")?;
                for (name, count) in &stats.by_register {
                    writeln!(out, "  {name:<14} {count:>6}")?;
                }
            }
            Ok(())
        }
        Format::Json => writeln!(out, "{}", serde_json::to_string_pretty(stats).unwrap()),
        Format::Ndjson => writeln!(out, "{}", serde_json::to_string(stats).unwrap()),
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
            validity: 0.96,
        }];
        render_entries(&mut buf, &entries, Format::Human).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("contextdb"));
        assert!(out.contains("owned"));
    }
}
