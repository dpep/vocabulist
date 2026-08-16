//! Harvesting the user's own writing out of tool *results*.
//!
//! The hook otherwise looks only at tool inputs — text being sent — on the
//! reasoning that a search result is other people's prose. That holds, but it
//! throws away the best corpus available: reading a channel or a pull request
//! surfaces plenty of messages the user wrote themselves, months or years ago,
//! which no forward-looking capture will ever see.
//!
//! So the filter changes from *direction* to *authorship*, which is the
//! stricter of the two. Everything here is discarded unless its author is a
//! known identity of the user's.
//!
//! Both parsers are deliberately literal about the shapes they expect and
//! return nothing when those shapes change. Failing closed costs a corpus that
//! was free anyway; failing open would attribute a colleague's writing to the
//! user, which is the one outcome this whole design exists to prevent.

use serde_json::Value;

use crate::types::Register;

/// One message recovered from a tool result.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Stable per-message identity, for dedup across repeated reads.
    pub key: String,
    pub author: String,
    pub register: Register,
    pub body: String,
}

/// Pull the user's own messages out of a tool result.
pub fn harvest(
    tool_name: &str,
    response: &Value,
    selves: &std::collections::HashSet<String>,
) -> Vec<Message> {
    let name = tool_name.to_lowercase();
    let found = if is_slack_tool(&name) {
        from_slack(response)
    } else if is_github_tool(&name) {
        from_github_json(response)
    } else {
        // Anything else is not a source of the user's messages. Walking every
        // tool's JSON meant a fetched page containing `{"body": …,
        // "user":{"login":"dpep"}}` — a public login — could inject text into
        // the voice tables.
        Vec::new()
    };

    found
        .into_iter()
        .filter(|m| selves.contains(&m.author.to_lowercase()))
        .filter(|m| !m.body.trim().is_empty())
        .collect()
}

/// Tools whose results carry Slack messages.
fn is_slack_tool(name: &str) -> bool {
    name.contains("slack")
}

/// Tools whose results carry GitHub comments. `gh` runs through Bash, so the
/// command has to be trusted as well as the shape.
fn is_github_tool(name: &str) -> bool {
    name.contains("github") || name == "bash"
}

/// Slack's MCP responses are formatted text rather than structured data, so
/// this reads the rendering: an author line, then a `Text:` block, repeated.
///
/// Keyed on `Message_ts` where present, since that is Slack's own per-message
/// identifier and survives re-reads.
fn from_slack(response: &Value) -> Vec<Message> {
    let text = match response {
        Value::String(s) => s.clone(),
        other => other
            .get("results")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| other.to_string()),
    };

    let mut out = Vec::new();
    let mut author: Option<String> = None;
    let mut key: Option<String> = None;
    let mut body: Option<Vec<String>> = None;

    let flush = |out: &mut Vec<Message>,
                 author: &Option<String>,
                 key: &Option<String>,
                 body: &Option<Vec<String>>| {
        if let (Some(author), Some(lines)) = (author, body) {
            let joined = lines.join("\n").trim().to_string();
            if !joined.is_empty() {
                out.push(Message {
                    // Without a timestamp, fall back to the text itself so a
                    // re-read still dedups.
                    key: key.clone().unwrap_or_else(|| format!("slack:{joined:.80}")),
                    author: author.clone(),
                    register: Register::Slack,
                    body: joined,
                });
            }
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("From:") {
            flush(&mut out, &author, &key, &body);
            body = None;
            // Prefer the parenthesised user ID; fall back to the display name.
            author = rest
                .split_once("(ID:")
                .and_then(|(_, id)| id.split(')').next())
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .or_else(|| rest.split_whitespace().next().map(str::to_string));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Message_ts:") {
            key = Some(format!("slack:{}", rest.trim()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Text:") {
            let first = rest.trim();
            body = Some(if first.is_empty() {
                Vec::new()
            } else {
                vec![first.to_string()]
            });
            continue;
        }
        // A separator or a new result block ends the current message.
        if trimmed == "---" || trimmed.starts_with("### ") {
            flush(&mut out, &author, &key, &body);
            body = None;
            key = None;
            // Author resets too. Leaving it set means a block with a missing
            // or reordered `From:` inherits the previous author — attributing
            // a colleague's message to the user, which is the failure this
            // module exists to prevent.
            author = None;
            continue;
        }
        if let Some(lines) = body.as_mut() {
            lines.push(line.to_string());
        }
    }
    flush(&mut out, &author, &key, &body);
    out
}

/// GitHub's API shape: objects carrying a `body` alongside a `user.login` or
/// `author.login`. Keyed on the comment's own id or URL.
///
/// Matched structurally rather than by command, so it works whether the JSON
/// arrived from `gh api`, `gh pr view --json`, or an MCP server.
fn from_github_json(response: &Value) -> Vec<Message> {
    // Bash results carry the payload as a string in `stdout`.
    if let Some(stdout) = response.get("stdout").and_then(Value::as_str) {
        if let Ok(parsed) = serde_json::from_str::<Value>(stdout.trim()) {
            return from_github_json(&parsed);
        }
        return Vec::new();
    }
    if let Value::String(s) = response {
        if let Ok(parsed) = serde_json::from_str::<Value>(s.trim()) {
            return from_github_json(&parsed);
        }
        return Vec::new();
    }

    let mut out = Vec::new();
    walk_github(response, &mut out);
    out
}

fn walk_github(value: &Value, out: &mut Vec<Message>) {
    match value {
        Value::Array(items) => items.iter().for_each(|i| walk_github(i, out)),
        Value::Object(map) => {
            let body = map.get("body").and_then(Value::as_str);
            let author = map
                .get("user")
                .or_else(|| map.get("author"))
                .and_then(|u| u.get("login"))
                .and_then(Value::as_str);

            if let (Some(body), Some(author)) = (body, author)
                && !body.trim().is_empty()
            {
                let key = map
                    .get("html_url")
                    .or_else(|| map.get("url"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| map.get("id").map(|id| format!("gh:{id}")))
                    .unwrap_or_else(|| format!("gh:{body:.80}"));
                out.push(Message {
                    key,
                    author: author.to_string(),
                    register: Register::Pr,
                    body: body.to_string(),
                });
            }
            // Nested payloads (a PR carrying its comments) still get walked.
            map.values().for_each(|v| walk_github(v, out));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn selves(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    const SLACK_RESULT: &str = r#"# Search Results

### Result 1 of 2
Channel: #coding (ID: C0AS6PZ20AF)
From: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)
Message_ts: 1786594876.682179
Text:
how about this pull request?

---

### Result 2 of 2
Channel: #coding (ID: C0AS6PZ20AF)
From: someone <other@example.com> (ID: U999OTHER)
Message_ts: 1786594000.111111
Text:
a colleague wrote this one

---
"#;

    #[test]
    fn keeps_only_the_users_own_slack_messages() {
        let response = json!({ "results": SLACK_RESULT });
        let found = harvest("slack_search_public", &response, &selves(&["u0e48ahqa"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "how about this pull request?");
        assert_eq!(found[0].register, Register::Slack);
    }

    #[test]
    fn a_colleagues_message_is_never_captured() {
        let response = json!({ "results": SLACK_RESULT });
        let found = harvest("slack_read_channel", &response, &selves(&["u999other"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "a colleague wrote this one");
        // ...and with no identities configured, nothing at all.
        assert!(harvest("slack_read_channel", &response, &selves(&[])).is_empty());
    }

    #[test]
    fn a_block_without_an_author_inherits_nobody() {
        // Author must reset at the separator: if Slack's rendering ever drops
        // a `From:` line, the previous author must not adopt the new text.
        let response = json!({ "results": "### Result 1\nFrom: dpepper (ID: U0E48AHQA)\nMessage_ts: 1.1\nText:\nmine\n\n---\n\n### Result 2\nMessage_ts: 2.2\nText:\nsomeone else's, unattributed\n\n---\n" });
        let found = harvest("slack_read_channel", &response, &selves(&["u0e48ahqa"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "mine");
    }

    #[test]
    fn only_slack_and_github_tools_are_harvested() {
        // A fetched page carrying a public login must not inject voice data.
        let payload = json!([{ "body": "not yours", "user": { "login": "dpep" } }]);
        assert!(harvest("WebFetch", &payload, &selves(&["dpep"])).is_empty());
        assert!(harvest("Read", &payload, &selves(&["dpep"])).is_empty());
        assert_eq!(harvest("Bash", &payload, &selves(&["dpep"])).len(), 1);
    }

    #[test]
    fn slack_messages_are_keyed_by_timestamp_for_dedup() {
        let response = json!({ "results": SLACK_RESULT });
        let found = harvest("slack_search_public", &response, &selves(&["u0e48ahqa"]));
        assert_eq!(found[0].key, "slack:1786594876.682179");
    }

    #[test]
    fn reads_github_comments_from_an_array() {
        let response = json!([
            { "body": "mine", "user": { "login": "dpep" }, "html_url": "https://x/1" },
            { "body": "theirs", "user": { "login": "someone" }, "html_url": "https://x/2" },
        ]);
        let found = harvest("Bash", &response, &selves(&["dpep"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].body, "mine");
        assert_eq!(found[0].key, "https://x/1");
        assert_eq!(found[0].register, Register::Pr);
    }

    #[test]
    fn reads_github_comments_out_of_bash_stdout() {
        let response = json!({
            "stdout": "[{\"body\":\"from stdout\",\"user\":{\"login\":\"dpep\"},\"id\":7}]"
        });
        let found = harvest("Bash", &response, &selves(&["dpep"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "gh:7");
    }

    #[test]
    fn handles_the_author_login_shape_too() {
        // `gh pr view --json comments` nests under `author` rather than `user`.
        let response = json!({ "comments": [{ "body": "mine", "author": { "login": "dpep" } }] });
        assert_eq!(harvest("Bash", &response, &selves(&["dpep"])).len(), 1);
    }

    #[test]
    fn unparseable_output_yields_nothing_rather_than_guesses() {
        let response = json!({ "stdout": "total 12\ndrwxr-xr-x  4 dpepper staff" });
        assert!(harvest("Bash", &response, &selves(&["dpep"])).is_empty());
        assert!(harvest("Bash", &json!("not json"), &selves(&["dpep"])).is_empty());
    }

    #[test]
    fn empty_bodies_are_dropped() {
        let response = json!([{ "body": "   ", "user": { "login": "dpep" } }]);
        assert!(harvest("Bash", &response, &selves(&["dpep"])).is_empty());
    }
}
