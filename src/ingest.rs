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
pub fn parse(input: &str) -> Result<Vec<Record>, serde_json::Error> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed);
    }
    let mut out = Vec::new();
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
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
        let register = record
            .register
            .as_deref()
            .and_then(Register::parse)
            .unwrap_or(default_register);

        // An author we don't recognize as the user is someone else's voice.
        // An absent author means the user, since that's what unattributed
        // capture has always meant.
        let is_self = match &record.author {
            None => true,
            Some(a) => selves.contains(&a.to_lowercase()),
        };

        let authored_by = if watermark::is_assistant_authored(&record.body) {
            report.assistant += 1;
            "assistant"
        } else if is_self {
            "user"
        } else {
            report.others += 1;
            "other"
        };

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
