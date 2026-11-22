//! Claude Code hook adapter.
//!
//! An optional integration surface, not a dependency — `vocab` is a complete
//! tool without it. It exists because the alternative (a shell script piping
//! through `jq`) puts three process spawns on the synchronous prompt path,
//! and hook payloads are trivial to parse where `serde_json` already lives.
//!
//! Every handler is **fail-open**: a hook that errors must never block the
//! user's prompt or a tool call, so problems exit 0 quietly.

use serde::Deserialize;
use serde_json::Value;

use crate::store::Store;
use crate::types::Register;
use crate::watermark;

/// The subset of a Claude Code hook payload we care about.
#[derive(Deserialize, Default, Debug)]
pub struct HookInput {
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub tool_response: Value,
}

/// How many spool rows one Stop hook will fold in. Bounded so the hook stays
/// predictable — leftovers wait for the next Stop rather than stalling one.
const STOP_PROCESS_LIMIT: usize = 200;

/// The most of one capture that is spooled.
///
/// Each spool row is processed in one transaction, and while it runs every
/// other hook's capture waits on the lock — for 250 ms, then gives up and
/// loses that prompt. A row much past this outlasts the wait. What it cuts
/// is a paste (a log, a file, a long thread), not prose anyone typed.
const MAX_CAPTURE_BYTES: usize = 16 * 1024;

/// Outbound text worth learning from, if this tool call carries any.
///
/// Only *sends* qualify, never reads: a Gmail search result is other people's
/// prose. And note what this captures is usually the assistant's drafting, not
/// the user's — it goes through the watermark check like anything else, and
/// lands as `assistant` when it carries a marker.
pub fn outbound(tool_name: &str, tool_input: &Value) -> Option<(Register, String)> {
    let name = tool_name.to_lowercase();
    let field = |key: &str| {
        tool_input
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    };

    if name.contains("gmail") {
        if name.contains("create_draft")
            || name.contains("send_message")
            || name.contains("reply")
            || name.contains("forward")
        {
            return field("body").map(|b| (Register::Email, b));
        }
        return None;
    }
    if name.contains("slack") && name.contains("send_message") {
        return field("text").map(|t| (Register::Slack, t));
    }
    None
}

/// Does this session have somebody sitting in front of it?
///
/// A headless `claude -p` has no user typing, so nothing it submits is the
/// user's voice — and plugins spawn them. The statusbar's label call fed its
/// own instructions in as prose for weeks, and the next plugin to ask a model
/// something would have done it again; the env var is the general answer,
/// where recognizing that one prompt would not have been.
///
/// Only an explicit `0` counts. Absent means a CLI that does not set it, and
/// guessing unattended there would silently turn capture off altogether.
fn unattended() -> bool {
    unattended_from(
        std::env::var("CLAUDE_CODE_SESSION_ATTENDED")
            .ok()
            .as_deref(),
    )
}

/// Split out from the environment read so the rule itself is testable —
/// setting a process-wide env var under a parallel test runner is not.
fn unattended_from(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "0")
}

/// Handle one hook event. Always returns 0 — see the fail-open note above.
pub fn run(event: &str, store: &Store, input: &HookInput) -> i32 {
    match event {
        // Seeding and spool processing still run unattended: they are
        // housekeeping over text already captured, not capture.
        _ if unattended() && event != "stop" => {}
        "user-prompt-submit" => capture_prompt(store, input),
        "post-tool-use" => capture_tool(store, input),
        "stop" => {
            // Seeding lives here rather than on any synchronous hook: it takes
            // seconds, and this one is async and fires at the end of a turn,
            // so the cost is invisible. It also means someone who only ever
            // uses the plugin — never the CLI — still gets a seeded lexicon
            // and, with it, the detected identities that read-capture needs.
            maybe_seed(store);
            // Silent: a hook's stderr reaches nobody, and the CLI says it.
            let _ = reconcile_spool(store, STOP_PROCESS_LIMIT);
            let _ = crate::process::process_spool(store, STOP_PROCESS_LIMIT);
        }
        _ => {}
    }
    0
}

/// Seed if it's never happened, or if the last one has gone stale.
///
/// Unlike the interactive path, this also refreshes: the machine acquires
/// repos and tools over time, and a lexicon describing last month's machine
/// flags this month's vocabulary. Failures are swallowed — an absent `gh` or
/// an unreadable directory must never surface at the end of a turn.
fn maybe_seed(store: &Store) {
    let stale = match store.seconds_since_seed() {
        Ok(None) => true,
        Ok(Some(age)) => age > crate::seed::SEED_TTL_SECS,
        Err(_) => return,
    };
    if !stale {
        return;
    }
    let seeded = crate::seed::run(store, &crate::seed::SeedOptions::default());
    if seeded.is_ok() {
        let _ = store.mark_seeded();
    }
}

/// Where Claude Code keeps session transcripts.
fn projects_root() -> Option<std::path::PathBuf> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".claude")))?;
    Some(config.join("projects"))
}

/// Re-attribute staged prompts nobody typed, against the transcripts in the
/// usual place. Every path that drains the spool calls this first, with the
/// limit it is about to drain, so no row is processed unchecked. Returns how
/// many sessions had no transcript to check, whose prompts will be learned
/// as typed.
pub fn reconcile_spool(store: &Store, limit: usize) -> usize {
    let root = projects_root().unwrap_or_default();
    reconcile_origins(store, &root, limit)
}

/// Claude Code's session ids are UUIDs.
fn is_session_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Re-attribute staged prompts that another session submitted.
///
/// A peer's message (a plugin notification, another session's
/// `SendMessage`) fires `UserPromptSubmit` with bare text and nothing in the
/// payload says who sent it, so it is captured as typed. The transcript does
/// say: the turn is recorded with an `origin` whose kind is not `human`. That
/// record is not there yet when the prompt hook runs, but it is by `Stop`, so
/// this runs then, before the spool is processed. Undocumented schema, so it
/// fails open: a transcript it cannot find or read changes nothing. Returns
/// how many sessions had no transcript.
fn reconcile_origins(store: &Store, projects: &std::path::Path, limit: usize) -> usize {
    let Ok(pending) = store.pending_spool(limit) else {
        return 0;
    };
    let mut by_session: std::collections::HashMap<&str, Vec<&crate::store::SpoolRow>> =
        std::collections::HashMap::new();
    for row in &pending {
        if row.register == Register::Prompt
            && row.authored_by == "user"
            && let Some(session) = row.source.as_deref()
        {
            by_session.entry(session).or_default().push(row);
        }
    }
    let mut missing = 0;
    for (session, rows) in by_session {
        let found = transcripts(projects, session);
        // An ingested prompt's source is whatever its export called it, not
        // a session; only an id shaped like Claude Code's is missed.
        missing += usize::from(found.is_empty() && is_session_id(session));
        let foreign = foreign_prompts(&found);
        for row in rows {
            if let Some(author) = foreign.get(&content_key(Register::Prompt, &row.body)) {
                let _ = store.set_spool_author(row.id, author);
            }
        }
    }
    missing
}

/// Every transcript for a session. Usually one, but the id is all the spool
/// recorded, and the same session can appear under more than one project.
fn transcripts(projects: &std::path::Path, session: &str) -> Vec<std::path::PathBuf> {
    // Any id that names a file directly inside a project dir, and nothing that
    // could climb out of one.
    if matches!(session, "" | "." | "..") || session.contains(['/', '\\']) {
        return Vec::new();
    }
    let name = format!("{session}.jsonl");
    let Ok(dirs) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    dirs.flatten()
        .map(|d| d.path().join(&name))
        .filter(|p| p.is_file())
        .collect()
}

/// Content keys of every turn in a session's transcripts that a person did
/// not type, each with who wrote it: `assistant` for a suggestion the person
/// accepted, `other` for anything another session sent.
fn foreign_prompts(
    transcripts: &[std::path::PathBuf],
) -> std::collections::HashMap<String, &'static str> {
    let mut keys = std::collections::HashMap::new();
    for transcript in transcripts {
        // Lossy, so a bad byte (say, a read that caught Claude mid-append)
        // costs the line it is on rather than the whole session.
        let Ok(bytes) = std::fs::read(transcript) else {
            continue;
        };
        foreign_lines(&String::from_utf8_lossy(&bytes), &mut keys);
    }
    keys
}

fn foreign_lines(text: &str, keys: &mut std::collections::HashMap<String, &'static str>) {
    for line in text.lines() {
        // Most lines have no origin, and a typed prompt's is `human`; skip
        // parsing both. An accepted prompt suggestion is `human` too, but the
        // assistant wrote it: the person only pressed tab.
        let suggested = line.contains("\"promptSource\":\"suggestion_accepted\"");
        if !line.contains("\"origin\"") || (line.contains("\"kind\":\"human\"") && !suggested) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = entry.pointer("/origin/kind").and_then(Value::as_str);
        let body = entry.pointer("/message/content").and_then(Value::as_str);
        let (Some(kind), Some(body)) = (kind, body) else {
            continue;
        };
        let accepted =
            entry.get("promptSource").and_then(Value::as_str) == Some("suggestion_accepted");
        if entry.get("type").and_then(Value::as_str) != Some("user")
            || (kind == "human" && !accepted)
        {
            continue;
        }
        let author = if accepted { "assistant" } else { "other" };
        keys.insert(
            content_key(Register::Prompt, &as_captured(Register::Prompt, body)),
            author,
        );
    }
}

/// Envelope tags whose contents the harness injects into a prompt.
///
/// Named explicitly rather than matched as "any tag", because a prompt about
/// HTML legitimately contains `<div>` and its text is still the user's.
const ENVELOPES: &[&str] = &[
    "system-reminder",
    "task-notification",
    "command-name",
    "command-message",
    "command-args",
    "local-command-caveat",
    "local-command-stdout",
    "local-command-stderr",
];

/// Markers that mean the whole turn was written by something other than the
/// user — a command expansion, or another agent handing back.
///
/// Stripping is the wrong tool for both: the text arrives as bare markdown
/// *around* these markers rather than inside them. A command's body follows
/// its tags, and an agent hand-back is introduced by a sentence that sits
/// outside `<agent-message>` and framed by boilerplate the harness wrote, so
/// taking the tags off leaves machine prose sitting where a sentence should
/// be. Matched as an open-tag prefix, because `<agent-message>` carries a
/// `from` attribute.
const FOREIGN_MARKERS: &[&str] = &["<command-name>", "<command-message>", "<agent-message"];

/// Remove machine-injected blocks from a prompt, leaving what was typed.
///
/// A prompt is not only what the user wrote. The harness appends reminders,
/// and a completed background task arrives as a whole turn of its own — so
/// `background command`, `exit code`, and `completed status` were showing up
/// as this user's characteristic phrases. That is the authorship rule failing
/// through a path nobody had looked at: the text is machine-generated, and it
/// was being counted as voice.
fn strip_envelopes(prompt: &str) -> String {
    let mut out = prompt.to_string();
    for tag in ENVELOPES {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = out.find(&open) {
            // An unclosed envelope runs to the end — the harness truncates,
            // and keeping the remainder would keep exactly the wrong half.
            let end = match out[start..].find(&close) {
                Some(offset) => start + offset + close.len(),
                None => out.len(),
            };
            out.replace_range(start..end, " ");
        }
    }
    out.trim().to_string()
}

/// A text as capture would have spooled it, from the copy a transcript
/// keeps: a prompt loses the framing line the transcript adds to a peer's
/// message and the harness envelopes the hook strips. Other registers are
/// spooled as they arrive.
pub(crate) fn as_captured(register: Register, raw: &str) -> String {
    if register != Register::Prompt {
        return raw.to_string();
    }
    let bare = match raw.split_once('\n') {
        Some((first, rest)) if first.ends_with(':') && first.contains("sent a message") => rest,
        _ => raw,
    };
    strip_envelopes(bare).trim().to_string()
}

/// The purest signal available: text the user typed themselves.
fn capture_prompt(store: &Store, input: &HookInput) {
    // A slash command is machine syntax, not the user's prose — and when it
    // expands, the prose it expands into is the skill author's, not this
    // user's. Checked before stripping, because after stripping the body no
    // longer looks like a command at all: it looks like a paragraph. The same
    // is true of a turn another agent produced.
    if input.prompt.trim_start().starts_with('/')
        || FOREIGN_MARKERS.iter().any(|m| input.prompt.contains(m))
    {
        return;
    }
    let prompt = strip_envelopes(&input.prompt);
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return;
    }
    spool(store, Register::Prompt, &input.session_id, prompt);
}

fn capture_tool(store: &Store, input: &HookInput) {
    if let Some((register, body)) = outbound(&input.tool_name, &input.tool_input) {
        spool(store, register, &input.tool_name, &body);
    }
    capture_own_messages(store, input);
}

/// Recover the user's own writing from what a read returned.
///
/// The filter here is authorship rather than direction, and it's the stricter
/// of the two: a channel read surfaces everyone, and only messages matching a
/// configured identity survive. With no identities configured this is inert,
/// which is the right default — guessing at who the user is would be the one
/// mistake that poisons the voice profile.
fn capture_own_messages(store: &Store, input: &HookInput) {
    let Ok(mut selves) = store.identities() else {
        return;
    };
    if selves.is_empty() {
        return;
    }

    // A service ID rendered beside an identity we already trust is that same
    // person's ID on that service — which is how the Slack handle gets
    // learned from an email that came out of git config. Done before the
    // harvest so the messages in this very response can be attributed.
    // Only Slack renders `From: name <email> (ID: U…)`, and only as text.
    // Serializing a JSON response with to_string() collapses it to a single
    // line, which destroys the "same line" requirement that makes this safe —
    // any known handle anywhere in the payload would co-occur with any ID.
    let rendered = match &input.tool_response {
        Value::String(s) if input.tool_name.to_lowercase().contains("slack") => s.clone(),
        Value::Object(map) if input.tool_name.to_lowercase().contains("slack") => map
            .get("results")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    for learned in crate::identity::learn_from_response(&rendered, &selves) {
        if store
            .add_identity_from(&learned.handle, learned.source)
            .unwrap_or(false)
        {
            selves.insert(learned.handle);
        }
    }

    // Everyone who wrote here, before the filter to your own messages throws
    // them away. Names are the case a dictionary can never cover, and these
    // arrive already parsed and already keyed for dedup.
    for (display, key) in crate::inbound::authors(&input.tool_name, &input.tool_response) {
        let _ = store.record_person(&display, &key);
    }

    for message in crate::inbound::harvest(&input.tool_name, &input.tool_response, &selves) {
        let authored_by = if watermark::is_assistant_authored(&message.body) {
            "assistant"
        } else {
            "user"
        };

        // Claim and spool together. Claiming first and spooling separately
        // loses a message for good if the write fails: the claim persists,
        // so the next read of the same channel skips it as already captured.
        //
        // The claim itself is what keeps re-reads idempotent — word_sources
        // would dedup, but registers and n-grams would not, so a second read
        // would make the same sentence look like a habit.
        let claimed = store.transaction(|| -> rusqlite::Result<()> {
            if !store.claim_source(&message.key)? {
                return Ok(());
            }
            store.spool_with_author(
                message.register,
                Some(&message.key),
                bounded(&message.body),
                authored_by,
                Some(&message.author),
            )?;
            Ok(())
        });
        if let Err(e) = claimed {
            log::warn!("dropped a {} message: {e}", message.register.as_str());
        }
    }
}

/// A stable identity for captured text, so the same text counts once.
///
/// FNV-1a rather than `DefaultHasher`: this key is persisted, and SipHash's
/// output is explicitly not guaranteed stable across Rust releases, so a
/// toolchain bump would silently void every claim ever made.
///
/// Case, whitespace runs, and digit runs are folded first, so a template that
/// interpolates a session id, a count, or a date is still recognized as the
/// same template. Nothing that folds together under that is two different
/// sentences. The hash is all that persists — the prose still goes to the
/// spool and is still dropped when processed.
pub(crate) fn content_key(register: Register, body: &str) -> String {
    let mut folded = String::with_capacity(body.len());
    let (mut space, mut digit) = (false, false);
    for c in body.chars() {
        if c.is_whitespace() {
            if !space {
                folded.push(' ');
            }
            (space, digit) = (true, false);
        } else if c.is_ascii_digit() {
            if !digit {
                folded.push('#');
            }
            (space, digit) = (false, true);
        } else {
            folded.extend(c.to_lowercase());
            (space, digit) = (false, false);
        }
    }
    format!("{}:{:016x}", register.as_str(), fnv1a(folded.trim()))
}

/// The exact text a text's counts came from, for `prune --unlearn`. Unlike
/// [`content_key`], case and digits count: two notifications differing by a
/// number taught different lines, and taking one back must not subtract the
/// other's. Only line endings and trailing spaces are ignored.
pub(crate) fn learned_key(register: Register, body: &str) -> String {
    let lines: Vec<&str> = body.lines().map(str::trim_end).collect();
    let text = lines.join("\n");
    format!(
        "{}:{:016x}",
        register.as_str(),
        fnv1a(text.trim_matches('\n'))
    )
}

fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn spool(store: &Store, register: Register, source: &str, body: &str) {
    // Text that arrives verbatim again is one piece of evidence, not two.
    // The store already draws this line for messages read out of a channel —
    // "the same sentence would look like a habit" — and the paths that
    // capture directly never had it, which is how a fixed prompt template
    // reaches the top of `vocab phrases` by repetition alone. A claim that
    // errors drops the capture: losing a prompt costs a little recall, and
    // failing the other way quietly restores the behavior this prevents.
    // Claimed and spooled together, as `capture_own_messages` does, so a
    // failed spool does not leave the text claimed and lost for good.
    let authored_by = if watermark::is_assistant_authored(body) {
        "assistant"
    } else {
        "user"
    };
    let source = (!source.is_empty()).then_some(source);
    let claimed = store.transaction(|| -> rusqlite::Result<()> {
        if store.claim_source(&content_key(register, body))? {
            store.spool(register, source, bounded(body), authored_by)?;
        }
        Ok(())
    });
    if let Err(e) = claimed {
        log::warn!("dropped a {} capture: {e}", register.as_str());
    }
}

/// `body` cut to `MAX_CAPTURE_BYTES`, at the last whitespace before it so
/// no word is split.
pub(crate) fn bounded(body: &str) -> &str {
    if body.len() <= MAX_CAPTURE_BYTES {
        return body;
    }
    let mut end = MAX_CAPTURE_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let head = &body[..end];
    head.rfind(char::is_whitespace)
        .map_or(head, |i| &head[..i])
        .trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn captures_a_typed_prompt() {
        let s = store();
        let input = HookInput {
            prompt: "wire up the hooks so we can iterate".into(),
            session_id: "abc".into(),
            ..Default::default()
        };
        run("user-prompt-submit", &s, &input);
        assert_eq!(s.pending_spool(10).unwrap().len(), 1);
    }

    #[test]
    fn a_huge_capture_is_cut_at_a_word() {
        let s = store();
        let input = HookInput {
            prompt: "widget ".repeat(20_000),
            session_id: "abc".into(),
            ..Default::default()
        };
        run("user-prompt-submit", &s, &input);
        let spooled = s.pending_spool(10).unwrap();
        let body = &spooled[0].body;
        assert!(body.len() <= MAX_CAPTURE_BYTES, "{}", body.len());
        assert!(body.ends_with("widget"), "cut between words");
    }

    #[test]
    fn a_prompt_keeps_only_what_the_user_typed() {
        let typed = "let's ship the small focused change";
        let prompt = format!(
            "<system-reminder>remember the thing</system-reminder>{typed}\n\
             <task-notification>background command completed exit code 0</task-notification>"
        );
        assert_eq!(strip_envelopes(&prompt), typed);
    }

    #[test]
    fn an_unclosed_envelope_takes_the_rest_with_it() {
        // The harness truncates, and the tail of a truncated reminder is
        // exactly the part that isn't the user's.
        assert_eq!(
            strip_envelopes("ship this <system-reminder>you should also"),
            "ship this"
        );
    }

    #[test]
    fn ordinary_markup_in_a_prompt_survives() {
        // A question about HTML is still the user's prose.
        let prompt = "why does <div>hello</div> render oddly";
        assert_eq!(strip_envelopes(prompt), prompt);
    }

    #[test]
    fn a_turn_that_is_only_a_notification_captures_nothing() {
        let s = store();
        run(
            "user-prompt-submit",
            &s,
            &HookInput {
                prompt: "<task-notification>background command completed</task-notification>"
                    .into(),
                ..Default::default()
            },
        );
        assert_eq!(s.pending_spool(10).unwrap().len(), 0);
    }

    #[test]
    fn the_same_text_is_captured_once_however_often_it_arrives() {
        let s = store();
        // Same template, different interpolated run number: still one
        // template, and a hook that fires every session must not make it
        // look like this user's favourite sentence.
        for prompt in [
            "summarize what changed in run 41 and why it matters",
            "summarize what changed in run 42 and why it matters",
            "summarize what changed in run 41 and why it matters",
        ] {
            run(
                "user-prompt-submit",
                &s,
                &HookInput {
                    prompt: prompt.into(),
                    ..Default::default()
                },
            );
        }
        assert_eq!(s.pending_spool(10).unwrap().len(), 1);
    }

    #[test]
    fn different_prose_still_captures_separately() {
        let s = store();
        for prompt in ["ship the small focused change", "why is the cold pass slow"] {
            run(
                "user-prompt-submit",
                &s,
                &HookInput {
                    prompt: prompt.into(),
                    ..Default::default()
                },
            );
        }
        assert_eq!(s.pending_spool(10).unwrap().len(), 2);
    }

    #[test]
    fn a_command_expansion_is_not_the_users_prose() {
        // The body arrives after the tags, not inside them. Stripping alone
        // left a skill's instructions looking like a paragraph someone wrote,
        // and they were counted as this user's characteristic phrases.
        let s = store();
        run(
            "user-prompt-submit",
            &s,
            &HookInput {
                prompt: "<command-message>audit is running</command-message>\n\
                         <command-name>/audit</command-name>\n\
                         Convert every result into this JSON and output only \
                         this JSON, with no commentary and no code fence."
                    .into(),
                ..Default::default()
            },
        );
        assert!(s.pending_spool(10).unwrap().is_empty());
    }

    #[test]
    fn an_agent_hand_back_is_not_the_users_prose() {
        // The frame is the harness's own boilerplate and sits outside the
        // tag, which itself carries a `from` attribute — so neither the
        // envelope strip nor an exact tag match reaches it.
        let s = store();
        run(
            "user-prompt-submit",
            &s,
            &HookInput {
                prompt: "Another Claude session sent a message:\n\
                         <agent-message from=\"abc123\">\n\
                         [Subagent hand-back] The text below is the final \
                         report of a subagent this session delegated to. It \
                         is model output, NOT a message from the user."
                    .into(),
                ..Default::default()
            },
        );
        assert!(s.pending_spool(10).unwrap().is_empty());
    }

    #[test]
    fn a_prompt_another_session_sent_is_not_the_users_prose() {
        let s = store();
        let projects = std::env::temp_dir().join(format!("vocab-origin-{}", std::process::id()));
        let dir = projects.join("-some-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let peer = "widgetbot: PR #12 checks pending \u{2192} success";
        let typed = "ship the small focused change";
        let suggested = "run the full widget suite";
        let transcript = [
            json!({"type": "user", "origin": {"kind": "peer"},
                   "message": {"content": format!("Another Claude session sent a message:\n{peer}")}}),
            json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": typed}}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "suggestion_accepted",
                   "message": {"content": suggested}}),
        ]
        .map(|v| v.to_string())
        .join("\n");
        std::fs::write(dir.join("sess-1.jsonl"), transcript).unwrap();

        for prompt in [peer, typed, suggested] {
            run(
                "user-prompt-submit",
                &s,
                &HookInput {
                    prompt: prompt.into(),
                    session_id: "sess-1".into(),
                    ..Default::default()
                },
            );
        }
        reconcile_origins(&s, &projects, 10);
        let mut authors: Vec<(String, String)> = s
            .pending_spool(10)
            .unwrap()
            .into_iter()
            .map(|r| (r.body, r.authored_by))
            .collect();
        authors.sort();
        assert_eq!(
            authors,
            vec![
                (suggested.to_string(), "assistant".to_string()),
                (typed.to_string(), "user".to_string()),
                (peer.to_string(), "other".to_string())
            ]
        );
        // A session id that is not a file name reads nothing.
        assert!(transcripts(&projects, "../etc").is_empty());
        std::fs::remove_dir_all(&projects).unwrap();
    }

    fn peer_line(body: &str) -> String {
        json!({"type": "user", "origin": {"kind": "peer"}, "message": {"content": body}})
            .to_string()
    }

    fn projects_dir(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("vocab-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn a_bad_byte_in_a_transcript_spoils_only_its_own_line() {
        // A transcript read while Claude is mid-append can end in half a
        // character; that must not hide the peer turns before it.
        let projects = projects_dir("badutf");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        let mut bytes = peer_line("the zblorg report").into_bytes();
        bytes.extend_from_slice(b"\n\xff\xfe");
        std::fs::write(projects.join("-repo/sess.jsonl"), bytes).unwrap();
        let keys = foreign_prompts(&transcripts(&projects, "sess"));
        assert!(keys.contains_key(&content_key(Register::Prompt, "the zblorg report")));
        std::fs::remove_dir_all(&projects).unwrap();
    }

    #[test]
    fn a_session_in_two_project_dirs_reads_both() {
        let projects = projects_dir("twodirs");
        for (dir, body) in [("-a", "first widget note"), ("-b", "second widget note")] {
            std::fs::create_dir_all(projects.join(dir)).unwrap();
            std::fs::write(projects.join(dir).join("sess.jsonl"), peer_line(body)).unwrap();
        }
        let keys = foreign_prompts(&transcripts(&projects, "sess"));
        assert_eq!(keys.len(), 2);
        std::fs::remove_dir_all(&projects).unwrap();
    }

    #[test]
    fn any_session_id_that_stays_in_the_projects_dir_is_read() {
        let projects = projects_dir("ids");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        std::fs::write(projects.join("-repo/sess_1.a.jsonl"), peer_line("a widget")).unwrap();
        assert_eq!(
            foreign_prompts(&transcripts(&projects, "sess_1.a")).len(),
            1
        );
        for id in ["", ".", "..", "../x", "a/b", "a\\b"] {
            assert!(transcripts(&projects, id).is_empty(), "{id:?}");
        }
        std::fs::remove_dir_all(&projects).unwrap();
    }

    #[test]
    fn reconcile_counts_the_sessions_it_could_not_check() {
        let s = store();
        let projects = projects_dir("missing");
        let (here, gone) = (
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        );
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        std::fs::write(projects.join(format!("-repo/{here}.jsonl")), peer_line("x")).unwrap();
        for (session, prompt) in [
            (here, "a widget"),
            (gone, "b widget"),
            (gone, "c widget"),
            // An ingested export's source, which no transcript was promised for.
            ("chatgpt-export", "d widget"),
        ] {
            s.spool(Register::Prompt, Some(session), prompt, "user")
                .unwrap();
        }
        assert_eq!(reconcile_origins(&s, &projects, 10), 1);
        std::fs::remove_dir_all(&projects).unwrap();
    }

    #[test]
    fn reconcile_checks_every_row_the_drain_will_take() {
        let s = store();
        let projects = projects_dir("limit");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        std::fs::write(
            projects.join("-repo/sess.jsonl"),
            peer_line("the peer widget"),
        )
        .unwrap();
        for i in 0..STOP_PROCESS_LIMIT {
            s.spool(
                Register::Prompt,
                Some("sess"),
                &format!("typed widget {i}"),
                "user",
            )
            .unwrap();
        }
        s.spool(Register::Prompt, Some("sess"), "the peer widget", "user")
            .unwrap();
        reconcile_origins(&s, &projects, STOP_PROCESS_LIMIT + 1);
        let pending = s.pending_spool(STOP_PROCESS_LIMIT + 1).unwrap();
        let peer = pending
            .iter()
            .find(|r| r.body == "the peer widget")
            .unwrap();
        assert_eq!(peer.authored_by, "other");
        std::fs::remove_dir_all(&projects).unwrap();
    }

    #[test]
    fn only_an_explicit_zero_means_nobody_is_there() {
        // Absent is an older CLI, not a headless session — reading it as
        // unattended would turn capture off for everyone.
        assert!(unattended_from(Some("0")));
        assert!(!unattended_from(Some("1")));
        assert!(!unattended_from(None));
    }

    #[test]
    fn ignores_slash_commands_and_empty_prompts() {
        let s = store();
        for prompt in ["/code-review high", "   "] {
            run(
                "user-prompt-submit",
                &s,
                &HookInput {
                    prompt: prompt.into(),
                    ..Default::default()
                },
            );
        }
        assert!(s.pending_spool(10).unwrap().is_empty());
    }

    #[test]
    fn captures_outbound_sends_only() {
        let send = json!({ "text": "shipping the small change now" });
        assert!(outbound("mcp__claude_ai_Slack__slack_send_message", &send).is_some());

        // Reads are other people's prose.
        assert!(outbound("mcp__claude_ai_Slack__slack_read_channel", &send).is_none());
        assert!(outbound("mcp__claude_ai_Gmail__search_threads", &send).is_none());
    }

    #[test]
    fn maps_each_tool_to_its_register() {
        let (register, body) = outbound(
            "mcp__claude_ai_Gmail__create_draft",
            &json!({ "body": "thanks for the review" }),
        )
        .unwrap();
        assert_eq!(register, Register::Email);
        assert_eq!(body, "thanks for the review");
    }

    #[test]
    fn an_unrecognized_tool_captures_nothing() {
        assert!(outbound("Bash", &json!({ "command": "ls" })).is_none());
        assert!(outbound("Read", &json!({ "file_path": "/tmp/x" })).is_none());
    }

    #[test]
    fn assistant_drafted_sends_are_marked_as_such() {
        let s = store();
        run(
            "post-tool-use",
            &s,
            &HookInput {
                tool_name: "mcp__claude_ai_Slack__slack_send_message".into(),
                tool_input: json!({ "text": "claudomatic: opened the PR" }),
                ..Default::default()
            },
        );
        let pending = s.pending_spool(10).unwrap();
        assert_eq!(pending[0].authored_by, "assistant");
    }

    #[test]
    fn an_unknown_event_is_a_silent_no_op() {
        let s = store();
        assert_eq!(run("nonsense", &s, &HookInput::default()), 0);
        assert!(s.pending_spool(10).unwrap().is_empty());
    }
}
