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
            let _ = crate::cli::process_spool(store, STOP_PROCESS_LIMIT);
        }
        _ => {}
    }
    0
}

/// The purest signal available: text the user typed themselves.
fn capture_prompt(store: &Store, input: &HookInput) {
    let prompt = input.prompt.trim();
    if prompt.is_empty() {
        return;
    }
    // A slash command is machine syntax, not the user's prose.
    if prompt.starts_with('/') {
        return;
    }
    spool(store, Register::Prompt, &input.session_id, prompt);
}

fn capture_tool(store: &Store, input: &HookInput) {
    let Some((register, body)) = outbound(&input.tool_name, &input.tool_input) else {
        return;
    };
    spool(store, register, &input.tool_name, &body);
}

fn spool(store: &Store, register: Register, source: &str, body: &str) {
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
