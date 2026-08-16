//! `vocab help <topic>`, where a topic can be an option and not just a
//! subcommand.
//!
//! clap's built-in `help` only knows subcommands, so `vocab help
//! --completions` answers "unrecognized subcommand" — which is true and
//! useless. A flag is a thing you can ask about, and `--help` printing all of
//! them at once is not the same as being able to ask about one.

use clap::CommandFactory;
use std::io::Write;

use crate::cli::Cli;

/// Print help for `topic`, or the whole command when it's `None`.
pub fn render(out: &mut impl Write, topic: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    // Built before anything is read off it: usage strings and propagated
    // globals are filled in by `build`, and a subcommand rendered before it
    // says "Usage: status" rather than "Usage: vocab status".
    let mut cmd = Cli::command();
    cmd.build();

    let Some(topic) = topic else {
        write!(out, "{}", cmd.render_help())?;
        return Ok(());
    };

    if cmd.find_subcommand(topic).is_some() {
        let sub = cmd.find_subcommand_mut(topic).expect("just checked");
        write!(out, "{}", sub.render_help())?;
        return Ok(());
    }

    // Accept `--completions`, `-j`, or a bare `completions` — whichever the
    // user happens to type is the one they meant.
    let bare = topic.trim_start_matches('-');
    if let Some(arg) = cmd
        .get_arguments()
        .find(|a| a.get_long() == Some(bare) || a.get_id().as_str() == bare || short_is(a, bare))
    {
        write_option(out, arg)?;
        return Ok(());
    }

    Err(format!("no help for {topic:?}\n\n{}", topics(&cmd)).into())
}

fn short_is(arg: &clap::Arg, bare: &str) -> bool {
    matches!((arg.get_short(), bare.chars().next()), (Some(s), Some(c)) if s == c && bare.len() == 1)
}

fn write_option(out: &mut impl Write, arg: &clap::Arg) -> std::io::Result<()> {
    let mut flags = Vec::new();
    if let Some(short) = arg.get_short() {
        flags.push(format!("-{short}"));
    }
    if let Some(long) = arg.get_long() {
        flags.push(format!("--{long}"));
    }
    if flags.is_empty() {
        flags.push(arg.get_id().to_string());
    }

    // A boolean flag still reports a value name and a default of "false",
    // both of which are noise: `--json [<JSON>]  default: false` describes
    // nothing a reader wants.
    let takes_value = !matches!(
        arg.get_action(),
        clap::ArgAction::SetTrue
            | clap::ArgAction::SetFalse
            | clap::ArgAction::Count
            | clap::ArgAction::Help
            | clap::ArgAction::Version
    );

    let value = arg
        .get_value_names()
        .filter(|_| takes_value)
        .and_then(|names| names.first())
        .map(|name| {
            // Square brackets are clap's own notation for an optional value,
            // and worth preserving — it is the whole point of `--completions`.
            let optional = arg
                .get_num_args()
                .is_some_and(|range| range.min_values() == 0);
            if optional {
                format!(" [<{name}>]")
            } else {
                format!(" <{name}>")
            }
        })
        .unwrap_or_default();

    writeln!(out, "{}{value}", flags.join(", "))?;

    // Long help where the author wrote one, short help otherwise.
    if let Some(text) = arg.get_long_help().or_else(|| arg.get_help()) {
        writeln!(out)?;
        for line in text.to_string().lines() {
            writeln!(out, "    {line}")?;
        }
    }

    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    if !values.is_empty() {
        writeln!(out, "\n    possible values: {}", values.join(", "))?;
    }

    let defaults: Vec<String> = if !takes_value {
        Vec::new()
    } else {
        arg.get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect()
    };
    if !defaults.is_empty() {
        writeln!(out, "    default: {}", defaults.join(", "))?;
    }
    Ok(())
}

/// What you *could* have asked about — an error that lists the alternatives.
fn topics(cmd: &clap::Command) -> String {
    let mut commands: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
    commands.sort_unstable();

    let mut options: Vec<String> = cmd
        .get_arguments()
        .filter(|a| !a.is_hide_set())
        .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
        .collect();
    options.sort();

    format!(
        "commands: {}\noptions:  {}\n\nOptions can be named with or without dashes; global ones \
         (--json, --file, --db, --quiet, --verbose, --profile) want the bare name.",
        commands.join(", "),
        options.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help_for(topic: &str) -> String {
        let mut buf = Vec::new();
        render(&mut buf, Some(topic)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn explains_an_option_by_its_long_name() {
        let text = help_for("--completions");
        assert!(text.contains("--completions"), "{text}");
        assert!(text.contains("possible values"), "{text}");
    }

    #[test]
    fn the_leading_dashes_are_optional() {
        assert_eq!(help_for("completions"), help_for("--completions"));
    }

    #[test]
    fn a_short_flag_is_a_topic_too() {
        // Note this is reachable through the library but not through the CLI:
        // `vocab help -j` parses `-j` as the global --json flag, because it
        // *is* one. `vocab help json` is the spelling that works there.
        assert_eq!(help_for("-j"), help_for("--json"));
    }

    #[test]
    fn subcommands_still_work() {
        assert!(help_for("status").contains("Usage: vocab status"));
    }

    #[test]
    fn an_unknown_topic_lists_the_alternatives() {
        let mut buf = Vec::new();
        let err = render(&mut buf, Some("zzqxwv")).unwrap_err().to_string();
        assert!(err.contains("commands:"), "{err}");
        assert!(err.contains("--completions"), "{err}");
    }
}
