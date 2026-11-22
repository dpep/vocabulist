//! Bulk ingestion from outside sources.
//!
//! Deliberately dumb: records arrive as JSON on stdin and land in the spool.
//! The *adapters* — pulling PR comments with `gh`, exporting Slack — live
//! outside this binary, as scripts or plugin hooks. That boundary is the
//! point. Building fetchers in here would mean owning credentials, rate
//! limits, and sync cursors for every service, and would duplicate an
//! ingestion layer the surrounding ecosystem already has.
//!
//! ## Authorship
//!
//! Records carry an optional `author`, and it splits the two things other
//! people's writing is good for:
//!
//! - **Corroboration.** A word three colleagues also use is shared jargon,
//!   not your typo. Their text counts toward whether a word is *real*.
//! - **Voice.** How they write is not how you write. Their text must not
//!   touch register frequencies, collocations, exemplars, or prose stats,
//!   or the profile drifts toward an average of everyone you talk to.
//!
//! So text from others contributes to the lexicon and to source diversity,
//! and to nothing else. `--self` names the handles that are you.

use std::collections::HashSet;

use serde::Deserialize;

use crate::store::Store;
use crate::types::Register;
use crate::watermark;

/// One unit of text to ingest. `body` is the only required field.
#[derive(Deserialize, Debug, Default)]
pub struct Record {
    pub body: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub register: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

impl Record {
    /// The record's own register, else `default`. A name that is no register
    /// falls back to `default` too.
    pub fn register_or(&self, default: Register) -> Register {
        self.register
            .as_deref()
            .and_then(Register::parse)
            .unwrap_or(default)
    }

    /// Who wrote it, as the spool records it: `assistant` when it carries an
    /// assistant's marker, else `user` when it has no author or one of
    /// `selves`, else `other`. An absent author means the user, since that's
    /// what unattributed capture has always meant.
    pub fn authored_by(&self, selves: &HashSet<String>) -> &'static str {
        if watermark::is_assistant_authored(&self.body) {
            "assistant"
        } else if self
            .author
            .as_ref()
            .is_none_or(|a| selves.contains(&a.to_lowercase()))
        {
            "user"
        } else {
            "other"
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct IngestReport {
    pub ingested: usize,
    /// Records attributed to someone else — corroboration only.
    pub others: usize,
    /// Records skipped as assistant-authored.
    pub assistant: usize,
    pub skipped: usize,
}

/// Parse a stream that is either NDJSON (one record per line) or a single
/// JSON array. Both shapes turn up in the wild — `gh --jq` emits lines,
/// `gh --json` emits an array — so accepting both saves every adapter a
/// conversion step.
///
/// One bad record fails the whole parse, and the error names it: a partial
/// load can't be re-run without counting the good records twice.
pub fn parse(input: &str) -> Result<Vec<Record>, String> {
    let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    if input.trim_start().starts_with('[') {
        let values: Vec<serde_json::Value> =
            serde_json::from_str(input).map_err(|e| e.to_string())?;
        return values
            .into_iter()
            .enumerate()
            .map(|(i, v)| serde_json::from_value(v).map_err(|e| format!("record {}: {e}", i + 1)))
            .collect();
    }
    let mut out = Vec::new();
    for (i, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).map_err(|e| {
            // serde places it within the record, which is always line 1.
            let message = e.to_string();
            let within = format!(" at line {} column {}", e.line(), e.column());
            let message = message.strip_suffix(&within).unwrap_or(&message);
            format!("line {}, column {}: {message}", i + 1, e.column())
        })?);
    }
    Ok(out)
}

/// Stage records in the spool. Nothing is learned until `process` runs, so
/// this stays a cheap, interruptible write.
pub fn run(
    store: &Store,
    records: &[Record],
    selves: &HashSet<String>,
    default_register: Register,
) -> Result<IngestReport, Box<dyn std::error::Error>> {
    let mut report = IngestReport::default();

    for record in records {
        if record.body.trim().is_empty() {
            report.skipped += 1;
            continue;
        }
        let register = record.register_or(default_register);
        let authored_by = record.authored_by(selves);
        match authored_by {
            "assistant" => report.assistant += 1,
            "other" => report.others += 1,
            _ => {}
        }

        store.spool_with_author(
            register,
            record.source.as_deref(),
            &record.body,
            authored_by,
            record.author.as_deref(),
        )?;
        report.ingested += 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selves(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn parses_ndjson_and_arrays_alike() {
        let ndjson = "{\"body\":\"one\"}\n{\"body\":\"two\"}\n";
        assert_eq!(parse(ndjson).unwrap().len(), 2);

        let array = "[{\"body\":\"one\"},{\"body\":\"two\"}]";
        assert_eq!(parse(array).unwrap().len(), 2);
    }

    #[test]
    fn a_bad_record_is_named_by_its_place_in_the_input() {
        let ndjson = "{\"body\":\"one\"}\n\n{body}\n";
        let err = parse(ndjson).unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");

        let array = "[{\"body\":\"one\"},{\"text\":\"two\"}]";
        let err = parse(array).unwrap_err().to_string();
        assert!(err.contains("record 2"), "{err}");
    }

    #[test]
    fn a_byte_order_mark_is_accepted() {
        assert_eq!(parse("\u{FEFF}{\"body\":\"x\"}\n").unwrap().len(), 1);
        assert_eq!(parse("\u{FEFF}[{\"body\":\"x\"}]").unwrap().len(), 1);
    }

    #[test]
    fn body_is_the_only_required_field() {
        let records = parse("{\"body\":\"just text\"}").unwrap();
        assert_eq!(records[0].body, "just text");
        assert!(records[0].author.is_none());
    }

    #[test]
    fn attributes_records_to_self_or_others() {
        let s = store();
        let records = vec![
            Record {
                body: "my own words".into(),
                author: Some("dpep".into()),
                ..Default::default()
            },
            Record {
                body: "a colleague wrote this".into(),
                author: Some("someone-else".into()),
                ..Default::default()
            },
        ];
        let report = run(&s, &records, &selves(&["dpep"]), Register::Pr).unwrap();
        assert_eq!(report.ingested, 2);
        assert_eq!(report.others, 1);
    }

    #[test]
    fn self_matching_ignores_case() {
        let s = store();
        let records = vec![Record {
            body: "mine".into(),
            author: Some("DPep".into()),
            ..Default::default()
        }];
        assert_eq!(
            run(&s, &records, &selves(&["dpep"]), Register::Pr)
                .unwrap()
                .others,
            0
        );
    }

    #[test]
    fn an_unattributed_record_is_treated_as_yours() {
        let s = store();
        let records = vec![Record {
            body: "no author given".into(),
            ..Default::default()
        }];
        assert_eq!(
            run(&s, &records, &selves(&[]), Register::Doc)
                .unwrap()
                .others,
            0
        );
    }

    #[test]
    fn assistant_authored_records_are_flagged_whoever_sent_them() {
        let s = store();
        let records = vec![Record {
            body: "claudomatic: opened the PR".into(),
            author: Some("dpep".into()),
            ..Default::default()
        }];
        let report = run(&s, &records, &selves(&["dpep"]), Register::Pr).unwrap();
        assert_eq!(report.assistant, 1);
    }

    #[test]
    fn empty_bodies_are_skipped_not_stored() {
        let s = store();
        let records = vec![Record {
            body: "   ".into(),
            ..Default::default()
        }];
        let report = run(&s, &records, &selves(&[]), Register::Doc).unwrap();
        assert_eq!(report.ingested, 0);
        assert_eq!(report.skipped, 1);
    }

    #[test]
    fn a_records_own_register_beats_the_default() {
        let s = store();
        let records = vec![Record {
            body: "some prose here".into(),
            register: Some("slack".into()),
            ..Default::default()
        }];
        run(&s, &records, &selves(&[]), Register::Pr).unwrap();
        assert_eq!(s.pending_spool(10).unwrap()[0].register, Register::Slack);
    }
}
