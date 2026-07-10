use std::collections::BTreeMap;

use clap::Command;

pub(crate) const GROUPS: [(&str, &[&str]); 4] = [
    (
        "Open and connect rooms",
        &[
            "start", "attach", "remote", "web", "list", "stats", "setup", "doctor",
        ],
    ),
    (
        "Work with agents",
        &[
            "agents",
            "asks",
            "answer",
            "message",
            "transcript",
            "pane",
            "channel",
            "worktree",
            "loop",
            "budget",
        ],
    ),
    ("Hooks and trust", &["hooks", "trust"]),
    (
        "Configure and maintain",
        &[
            "config",
            "coverage",
            "list-pets",
            "list-themes",
            "workspace",
            "reload",
            "reset",
            "gc",
            "uninstall",
            "ping",
        ],
    ),
];

pub(crate) fn customize(cmd: Command) -> Command {
    let commands = visible_commands(&cmd);
    let grouped = grouped_commands(&commands);
    let command_help = render_grouped_commands(&cmd, &grouped);
    let template = format!(
        "{{about-with-newline}}Run `rimz` in a project to open (or return to) its room.\n\n\
         {{usage-heading}} {{usage}}\n\n\
         {command_help}\
         {{all-args}}{{after-help}}"
    );

    cmd.help_template(template)
        .after_help("Run `rimz <command> --help` for full flags and defaults.")
        .override_usage("rimz [OPTIONS] [COMMAND]")
        .mut_subcommands(|subcmd| subcmd.hide(true))
}

fn visible_commands(cmd: &Command) -> BTreeMap<&str, &Command> {
    cmd.get_subcommands()
        .filter(|subcmd| subcmd.get_name() != "help" && !subcmd.is_hide_set())
        .map(|subcmd| (subcmd.get_name(), subcmd))
        .collect()
}

fn grouped_commands<'a>(
    commands: &BTreeMap<&'a str, &'a Command>,
) -> [(&'static str, Vec<&'a Command>); 4] {
    GROUPS.map(|(heading, names)| {
        let commands = names
            .iter()
            .filter_map(|name| commands.get(name).copied())
            .collect();
        (heading, commands)
    })
}

fn render_grouped_commands(cmd: &Command, grouped: &[(&str, Vec<&Command>)]) -> String {
    let styles = cmd.get_styles();
    let header = styles.get_header();
    let literal = styles.get_literal();
    let name_width = grouped
        .iter()
        .flat_map(|(_, commands)| commands)
        .map(|cmd| cmd.get_name().len())
        .max()
        .unwrap_or(0)
        + 2;

    let mut out = String::new();
    for (heading, commands) in grouped {
        assert_no_braces(heading);
        out.push_str(&format!("{header}{heading}:{header:#}\n"));
        for cmd in commands {
            let name = cmd.get_name();
            let about = about_line(cmd);
            assert_no_braces(name);
            assert_no_braces(&about);
            out.push_str("  ");
            out.push_str(&format!("{literal}{name}{literal:#}"));
            out.push_str(&" ".repeat(name_width.saturating_sub(name.len())));
            out.push_str(&about);
            let aliases = cmd.get_visible_aliases().collect::<Vec<_>>();
            if !aliases.is_empty() {
                out.push_str(" [alias: ");
                out.push_str(&aliases.join(", "));
                out.push(']');
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn about_line(cmd: &Command) -> String {
    cmd.get_about()
        .map(|about| about.to_string())
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn assert_no_braces(value: &str) {
    assert!(
        !value.contains(['{', '}']),
        "top-level help text contains a clap template brace: {value:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use clap::CommandFactory;
    use unicode_width::UnicodeWidthStr;

    use crate::cli::Cli;

    fn command() -> Command {
        <Cli as CommandFactory>::command()
    }

    fn visible_names(cmd: &Command) -> BTreeSet<String> {
        cmd.get_subcommands()
            .filter(|subcmd| subcmd.get_name() != "help" && !subcmd.is_hide_set())
            .map(|subcmd| subcmd.get_name().to_owned())
            .collect()
    }

    fn grouped_names() -> BTreeSet<String> {
        GROUPS
            .iter()
            .flat_map(|(_, names)| names.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn visible_commands_are_grouped_exactly_once() {
        let cmd = command();
        let visible = visible_names(&cmd);
        let grouped = grouped_names();

        assert_eq!(visible, grouped);

        let mut seen = BTreeSet::new();
        for (_, names) in GROUPS {
            for name in names {
                assert!(
                    seen.insert(name),
                    "`{name}` appears in more than one help group"
                );
            }
        }
    }

    #[test]
    fn rendered_help_has_grouped_visible_commands_only() {
        let source = command();
        let visible = visible_names(&source);
        let hidden: BTreeSet<_> = source
            .get_subcommands()
            .filter(|subcmd| subcmd.is_hide_set())
            .map(|subcmd| subcmd.get_name().to_owned())
            .collect();
        let mut cmd = customize(source);
        let help = cmd.render_help().to_string();

        let mut cursor = 0;
        for (heading, _) in GROUPS {
            let next = help[cursor..]
                .find(heading)
                .unwrap_or_else(|| panic!("missing help group heading `{heading}`"));
            cursor += next + heading.len();
        }

        for name in visible {
            assert!(
                command_line_count(&help, &name) > 0,
                "missing visible command `{name}` from rendered help"
            );
            assert_eq!(
                command_line_count(&help, &name),
                1,
                "visible command `{name}` rendered more than once"
            );
        }
        for name in hidden {
            assert!(
                command_line_count(&help, &name) == 0,
                "hidden command `{name}` leaked into rendered help"
            );
        }
        let message_line = help
            .lines()
            .find(|line| {
                line.starts_with("  ") && line.split_whitespace().next() == Some("message")
            })
            .expect("message command line");
        assert!(message_line.ends_with(" [alias: msg]"));
        assert!(help.contains("Run `rimz <command> --help` for full flags and defaults."));
    }

    fn command_line_count(help: &str, name: &str) -> usize {
        help.lines()
            .filter(|line| line.starts_with("  ") && line.split_whitespace().next() == Some(name))
            .count()
    }

    #[test]
    fn top_level_command_lines_fit_eighty_column_help() {
        let source = command();
        let visible = visible_names(&source);
        let mut cmd = customize(source);
        let help = cmd.render_help().to_string();

        for name in visible {
            let line = help
                .lines()
                .find(|line| {
                    line.starts_with("  ") && line.split_whitespace().next() == Some(name.as_str())
                })
                .unwrap_or_else(|| panic!("missing visible command `{name}`"));
            assert!(
                UnicodeWidthStr::width(line) <= 80,
                "`{name}` exceeds 80 columns in grouped help: {line}"
            );
        }
    }
}
