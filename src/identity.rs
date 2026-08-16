//! Working out which handles are the user's, without asking them.
//!
//! Read-capture only functions once the tool knows who the user is, and a
//! feature that depends on someone remembering to configure it is a feature
//! that stays off. Almost everything needed is already on the machine.
//!
//! Two mechanisms. Local detection reads git and `gh` at seed time. Then
//! **cross-service learning** covers the handles no local file knows: Slack
//! renders messages as `From: name <email> (ID: U0E48AHQA)`, so an email
//! learned from git config identifies the Slack ID the first time any Slack
//! response goes past. One service bootstraps the next.
//!
//! The safety rule throughout: never infer an identity from proximity alone.
//! A wrong identity attributes a colleague's writing to the user, which is
//! the one failure this whole design exists to prevent — so learning requires
//! an already-trusted identity on the same line.

use std::process::Command;

/// A handle, and how we came to believe it's the user's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub handle: String,
    pub source: &'static str,
}

/// Identities derivable from this machine.
///
/// Deliberately excludes the most-common-remote-owner heuristic, which
/// measured badly: on the author's machine the top owners are `git.heroku.com`
/// and a former employer's org, both ahead of the actual account. Anyone with
/// a work laptop would learn their employer's org as their own name.
pub fn detect_local() -> Vec<Detected> {
    let mut out = Vec::new();

    // Authoritative when available — it's the authenticated account.
    if let Some(login) = run("gh", &["api", "user", "--jq", ".login"]) {
        push(&mut out, login.trim(), "gh");
    }
    // The email is the more valuable of the two: it's what other services
    // render alongside their own IDs, so it's the bridge to Slack.
    if let Some(email) = run("git", &["config", "--get", "user.email"]) {
        push(&mut out, email.trim(), "gitconfig");
    }
    if let Some(name) = run("git", &["config", "--get", "user.name"]) {
        push(&mut out, name.trim(), "gitconfig");
    }
    out
}

/// How many commits to read per repo when hunting for addresses.
const COMMITS_PER_REPO: usize = 40;

/// Emails the user has actually committed under, beyond the global config.
///
/// The global `user.email` is one address; work is usually another, set
/// per-repo or per-machine and invisible to `git config --get`. Commits
/// record both. Matching on the author *name* is what keeps this safe —
/// every co-worker's address is in the same log, and only commits bearing a
/// name we already trust contribute.
///
/// This matters more than it looks: an email is the bridge token to Slack,
/// so without the work address the work workspace stays unrecognized.
pub fn detect_from_commits(repos: &[std::path::PathBuf], known: &[String]) -> Vec<Detected> {
    let names: Vec<String> = known.iter().map(|k| k.to_lowercase()).collect();
    if names.is_empty() {
        return Vec::new();
    }
    // One `git log` per repo, and they're independent — across scores of
    // repos that's most of a second spent waiting on subprocesses in turn.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(repos.len().max(1));
    let chunk = repos.len().div_ceil(threads.max(1));

    let mut seen = std::collections::BTreeSet::new();
    let partials: Vec<std::collections::BTreeSet<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = repos
            .chunks(chunk.max(1))
            .map(|slice| {
                let names = &names;
                scope.spawn(move || {
                    let mut found = std::collections::BTreeSet::new();
                    for repo in slice {
                        let Some(log) = run(
                            "git",
                            &[
                                "-C",
                                &repo.to_string_lossy(),
                                "log",
                                "--format=%an|%ae",
                                "-n",
                                &COMMITS_PER_REPO.to_string(),
                            ],
                        ) else {
                            continue;
                        };
                        for line in log.lines() {
                            let Some((author, email)) = line.split_once('|') else {
                                continue;
                            };
                            let author = author.trim().to_lowercase();
                            let email = email.trim().to_lowercase();
                            if email.contains('@') && names.contains(&author) {
                                found.insert(email);
                            }
                        }
                    }
                    found
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    for partial in partials {
        seen.extend(partial);
    }

    seen.into_iter()
        .map(|handle| Detected {
            handle,
            source: "commits",
        })
        .collect()
}

/// Does `needle` appear in `haystack` delimited by non-word characters?
fn appears_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Word characters for boundary purposes. `.` and `@` count, so an email
/// stays one token rather than three.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'@' || b == b'_' || b == b'-'
}

fn push(out: &mut Vec<Detected>, handle: &str, source: &'static str) {
    if handle.is_empty() || handle.len() > 128 {
        return;
    }
    out.push(Detected {
        handle: handle.to_lowercase(),
        source,
    });
}

fn run(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        .filter(|s| !s.trim().is_empty())
}

/// Learn new handles from a tool response, given the ones already trusted.
///
/// Looks for a service ID rendered on the same line as a handle we already
/// believe — `From: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)`. The
/// co-occurrence is the evidence; without a known identity on the line,
/// nothing is learned.
pub fn learn_from_response(
    response: &str,
    known: &std::collections::HashSet<String>,
) -> Vec<Detected> {
    let mut out = Vec::new();
    if known.is_empty() {
        return out;
    }

    for line in response.lines() {
        let lower = line.to_lowercase();
        // Bounded, not substring. `contains` let a bare first name from git
        // config match a *different* person's line — `daniel` inside
        // `daniela` — and the ID learned there becomes a fully trusted
        // identity whose messages are then captured as the user's.
        if !known.iter().any(|k| appears_bounded(&lower, k)) {
            continue;
        }
        // `(ID: U0E48AHQA)` — the service's own identifier for this person.
        let Some((_, rest)) = lower.split_once("(id:") else {
            continue;
        };
        let Some(id) = rest.split(')').next() else {
            continue;
        };
        let id = id.trim();
        // Channel IDs share the shape; only user IDs are wanted here, and a
        // line naming a person is the discriminator.
        if !id.is_empty() && id.len() <= 32 && !known.contains(id) {
            push(&mut out, id, "observed");
        }
    }
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(items: &[&str]) -> std::collections::HashSet<String> {
        items.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn learns_a_slack_id_from_a_known_email() {
        let response = "From: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)";
        let found = learn_from_response(response, &known(&["pepper.daniel@gmail.com"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].handle, "u0e48ahqa");
    }

    #[test]
    fn learns_nothing_from_a_stranger() {
        let response = "From: someone <other@example.com> (ID: U999OTHER)";
        assert!(learn_from_response(response, &known(&["pepper.daniel@gmail.com"])).is_empty());
    }

    #[test]
    fn learns_nothing_without_a_trusted_starting_point() {
        let response = "From: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)";
        assert!(learn_from_response(response, &known(&[])).is_empty());
    }

    #[test]
    fn ignores_ids_on_lines_that_name_nobody_known() {
        // A channel ID sits on its own line and must not be mistaken for a
        // person just because it's nearby.
        let response = "Channel: #coding (ID: C0AS6PZ20AF)\nFrom: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)";
        let found = learn_from_response(response, &known(&["pepper.daniel@gmail.com"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].handle, "u0e48ahqa");
    }

    #[test]
    fn does_not_relearn_what_is_already_known() {
        let response = "From: dpepper <pepper.daniel@gmail.com> (ID: U0E48AHQA)";
        let found =
            learn_from_response(response, &known(&["pepper.daniel@gmail.com", "u0e48ahqa"]));
        assert!(found.is_empty());
    }

    #[test]
    fn a_near_miss_handle_does_not_bridge() {
        // `daniel` must not match `daniela` — the ID learned from a different
        // person's line becomes a fully trusted identity.
        let response = "From: Daniela Rossi <daniela@example.com> (ID: U999OTHER)";
        assert!(learn_from_response(response, &known(&["daniel"])).is_empty());
        // The same name, properly delimited, still bridges.
        let mine = "From: Daniel Pepper (ID: U0E48AHQA)";
        assert_eq!(learn_from_response(mine, &known(&["daniel"])).len(), 1);
    }

    #[test]
    fn commit_detection_needs_a_trusted_name_to_filter_by() {
        // Every colleague's address is in the same log, so with nothing to
        // match against this must find nothing rather than everything.
        assert!(detect_from_commits(&[std::path::PathBuf::from(".")], &[]).is_empty());
    }

    #[test]
    fn a_matching_display_name_also_bridges() {
        let response = "From: Daniel Pepper (ID: U0E48AHQA)";
        assert_eq!(
            learn_from_response(response, &known(&["daniel pepper"]))[0].handle,
            "u0e48ahqa"
        );
    }
}
