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

/// Handle one hook event. Always returns 0 — see the fail-open note above.
pub fn run(event: &str, store: &Store, input: &HookInput) -> i32 {
    match event {
        "user-prompt-submit" => capture_prompt(store, input),
        "post-tool-use" => capture_tool(store, input),
        "stop" => {
            // Seeding lives here rather than on any synchronous hook: it takes
            // seconds, and this one is async and fires at the end of a turn,
            // so the cost is invisible. It also means someone who only ever
            // uses the plugin — never the CLI — still gets a seeded lexicon
            // and, with it, the detected identities that read-capture needs.
            maybe_seed(store);
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

/// Tags that mark the whole turn as a command expansion.
///
/// Stripping is the wrong tool here: a command's body arrives as bare
/// markdown *after* these tags, not inside them, so taking the tags off
/// leaves the skill's own instructions sitting where a sentence should be.
const COMMAND_MARKERS: &[&str] = &["<command-name>", "<command-message>"];

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

/// The purest signal available: text the user typed themselves.
fn capture_prompt(store: &Store, input: &HookInput) {
    // A slash command is machine syntax, not the user's prose — and when it
    // expands, the prose it expands into is the skill author's, not this
    // user's. Checked before stripping, because after stripping the body no
    // longer looks like a command at all: it looks like a paragraph.
    if input.prompt.trim_start().starts_with('/')
        || COMMAND_MARKERS.iter().any(|m| input.prompt.contains(m))
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
        let _ = store.transaction(|| -> rusqlite::Result<()> {
            if !store.claim_source(&message.key)? {
                return Ok(());
            }
            store.spool_with_author(
                message.register,
                Some(&message.key),
                &message.body,
                authored_by,
                Some(&message.author),
            )?;
            Ok(())
        });
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
fn content_key(register: Register, body: &str) -> String {
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
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in folded.trim().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{}:{hash:016x}", register.as_str())
}

fn spool(store: &Store, register: Register, source: &str, body: &str) {
    // Text that arrives verbatim again is one piece of evidence, not two.
    // The store already draws this line for messages read out of a channel —
    // "the same sentence would look like a habit" — and the paths that
    // capture directly never had it, which is how a fixed prompt template
    // reaches the top of `vocab phrases` by repetition alone. A claim that
    // errors drops the capture: losing a prompt costs a little recall, and
    // failing the other way quietly restores the behavior this prevents.
    if !store
        .claim_source(&content_key(register, body))
        .unwrap_or(false)
    {
        return;
    }
    let authored_by = if watermark::is_assistant_authored(body) {
        "assistant"
    } else {
        "user"
    };
    let source = (!source.is_empty()).then_some(source);
    let _ = store.spool(register, source, body, authored_by);
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
