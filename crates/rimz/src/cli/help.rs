use std::collections::BTreeMap;

use clap::Command;

pub(crate) const GROUPS: [(&str, &[&str]); 4] = [
    (
        "Open and connect rooms",
        &[
            "start",
            "attach",
            "sessions",
            "remote",
            "web",
            "list",
            "stats",
            "providers",
            "accounts",
            "setup",
            "doctor",
        ],
    ),
    (
        "Work with agents",
        &[
            "agents",
            "subagents",
            "wait",
            "teams",
            "asks",
            "answer",
            "message",
            "transcript",
            "events",
            "pane",
            "worktree",
            "lsp",
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
            "paths",
            "update",
            "reload",
            "reset",
            "gc",
            "uninstall",
            "ping",
        ],
    ),
];

/// `rimz agents` verbs by what they are for, as the orientation shows them.
pub(crate) const AGENTS_GROUPS: [(&str, &[&str]); 4] = [
    (
        "See",
        &["list", "show", "logs", "history", "top", "attribution"],
    ),
    (
        "Drive",
        &[
            "focus", "wait", "stop", "restart", "compact", "refresh", "fork", "budget",
        ],
    ),
    ("Start", &["launch", "resume", "profiles", "explain"]),
    ("Define", &["validate", "register", "check"]),
];

/// What `rimz agents` does, shown by its `--help` and under its no-verb refusal.
pub(crate) fn agents_orientation() -> String {
    let mut lines = vec![
        "  rimz agents list ['#lane']     agent cards (this lane; --all for the room)".to_owned(),
        "  rimz agents <SPEC> [PROMPT]    launch (same as `rimz agents launch`)".to_owned(),
        String::new(),
    ];
    lines.extend(
        AGENTS_GROUPS
            .map(|(heading, verbs)| format!("{:<9}{}", format!("{heading}:"), verbs.join("  "))),
    );
    lines.join("\n")
}

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
        .mut_subcommand("agents", customize_agents)
        .mut_subcommands(|subcmd| subcmd.hide(true))
}

/// `rimz agents --help` is the orientation; the launch flags live one
/// command away, under `rimz agents launch --help`.
fn customize_agents(cmd: Command) -> Command {
    let orientation = agents_orientation();
    assert_no_braces(&orientation);
    cmd.help_template(format!(
        "{{about-with-newline}}\n{orientation}{{after-help}}"
    ))
    .after_help(
        "Launch flags: rimz agents launch --help\n\
         One verb:     rimz agents <verb> --help",
    )
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

    #[test]
    fn attach_help_scopes_the_questions_it_skips() {
        let cmd = command();
        let attach = cmd.find_subcommand("attach").unwrap();
        assert_eq!(
            about_line(attach),
            "Enter a room by name; no first-run questions"
        );
    }

    #[test]
    fn global_options_follow_command_options() {
        let mut cmd = command();
        cmd.build();
        for path in [&["pane", "send"][..], &["transcript"][..]] {
            let mut selected = &cmd;
            for name in path {
                selected = selected.find_subcommand(name).unwrap();
            }
            let help = selected.clone().render_long_help().to_string();
            let options = help.find("Options:").expect("command options");
            let globals = help.find("Global options:");
            assert!(globals.is_some(), "{help}");
            let globals = globals.unwrap();
            assert!(globals > options, "{help}");
            for flag in ["--mux", "--zellij", "--tmux", "--root", "--color"] {
                assert!(help[globals..].contains(flag), "{flag}: {help}");
                assert!(!help[options..globals].contains(flag), "{flag}: {help}");
            }
            if path == ["pane", "send"] {
                for flag in ["--enter", "--key"] {
                    assert!(help[options..globals].contains(flag), "{flag}: {help}");
                }
            }
        }
        fn check(cmd: &Command) {
            for arg in cmd.get_arguments().filter(|arg| {
                arg.is_global_set()
                    && matches!(
                        arg.get_id().as_str(),
                        "mux" | "zellij" | "tmux" | "root" | "color"
                    )
            }) {
                assert_eq!(
                    arg.get_help_heading(),
                    Some("Global options"),
                    "{}: {}",
                    cmd.get_name(),
                    arg.get_id()
                );
            }
            for subcommand in cmd.get_subcommands() {
                check(subcommand);
            }
        }
        check(&cmd);
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
        let list_line = help
            .lines()
            .find(|line| line.starts_with("  ") && line.split_whitespace().next() == Some("list"))
            .expect("list command line");
        assert!(list_line.ends_with(" [alias: ls]"), "{list_line}");
        assert!(help.contains("Run `rimz <command> --help` for full flags and defaults."));
    }

    #[test]
    fn agents_verbs_are_grouped_exactly_once() {
        let cmd = command();
        let agents = cmd.find_subcommand("agents").expect("agents");
        let mut seen = BTreeSet::new();
        for (heading, verbs) in AGENTS_GROUPS {
            for verb in verbs {
                assert!(
                    seen.insert(verb.to_string()),
                    "`{verb}` repeats in {heading}"
                );
            }
        }
        assert_eq!(visible_names(agents), seen);
    }

    #[test]
    fn agents_help_leads_with_the_orientation_and_drops_launch_flags() {
        let mut cmd = customize(command());
        cmd.build();
        let agents = cmd.find_subcommand_mut("agents").expect("agents");
        for help in [
            agents.render_help().to_string(),
            agents.render_long_help().to_string(),
        ] {
            let orientation = agents_orientation();
            assert!(
                help.starts_with(&format!(
                    "See and drive the room's agents; launch new ones\n\n{orientation}\n"
                )),
                "{help}"
            );
            assert!(!help.contains("--description"), "{help}");
            assert!(help.contains("rimz agents launch --help"), "{help}");
            assert!(help.contains("rimz agents <verb> --help"), "{help}");
            for line in help.lines() {
                assert!(UnicodeWidthStr::width(line) <= 80, "{line}");
            }
        }
        let launch = agents.find_subcommand_mut("launch").expect("launch");
        assert!(
            launch
                .render_long_help()
                .to_string()
                .contains("--description")
        );
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
