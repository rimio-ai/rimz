//! Command-line refusals: an invocation clap accepts but that names nothing
//! to run. `main` exits 2 for a [`UsageError`], the code `rimz` documents for
//! an invalid command line.

use clap::CommandFactory;

/// The command line was invalid; the message carries the fix.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(String);

impl UsageError {
    pub(super) fn new(message: String) -> Self {
        Self(message)
    }

    /// `rimz <scene>` with neither a verb nor its launch `target`: the list
    /// command first, since a truncated read keeps the first line, then the
    /// scene's verbs as `--help` shows them.
    pub(super) fn missing_verb(scene: &str, target: &str, list_flags: &[&str]) -> Self {
        let list = shell_command(
            ["rimz", scene, "list"]
                .into_iter()
                .chain(list_flags.iter().copied()),
        );
        let cli = super::Cli::command();
        let verbs = cli
            .find_subcommand(scene)
            .into_iter()
            .flat_map(clap::Command::get_subcommands)
            .filter(|verb| !verb.is_hide_set())
            .map(clap::Command::get_name)
            .collect::<Vec<_>>()
            .join(", ");
        Self(format!(
            "`rimz {scene}` needs a verb or {target}; to list {scene}, run `{list}`\nverbs: {verbs}"
        ))
    }
}

/// `argv` as one shell-runnable command line.
pub(super) fn shell_command<'a>(argv: impl IntoIterator<Item = &'a str>) -> String {
    // Every word comes from the process argv or a literal, and argv cannot
    // carry the NUL byte, the only input `shlex` refuses to quote.
    shlex::try_join(argv).expect("argv words carry no NUL byte")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_verbs(scene: &str) -> Vec<String> {
        let cli = super::super::Cli::command();
        let scene = cli.find_subcommand(scene).expect("scene subcommand");
        scene
            .get_subcommands()
            .filter(|verb| !verb.is_hide_set())
            .map(|verb| verb.get_name().to_owned())
            .collect()
    }

    #[test]
    fn missing_verb_names_the_list_command_then_every_visible_verb() {
        for (scene, target) in [
            ("agents", "a launch spec"),
            ("subagents", "a profile"),
            ("teams", "a team name"),
        ] {
            let message = UsageError::missing_verb(scene, target, &["--json"]).to_string();
            let mut lines = message.lines();
            assert_eq!(
                lines.next(),
                Some(
                    format!(
                        "`rimz {scene}` needs a verb or {target}; to list {scene}, run `rimz {scene} list --json`"
                    )
                    .as_str()
                ),
                "{message}"
            );
            let verbs = lines
                .next()
                .and_then(|line| line.strip_prefix("verbs: "))
                .unwrap_or_else(|| panic!("verb line: {message}"));
            assert_eq!(verbs, visible_verbs(scene).join(", "), "{scene}");
            assert!(verbs.split(", ").any(|verb| verb == "list"), "{verbs}");
            assert_eq!(lines.next(), None, "{message}");
        }
        let hidden = super::super::Cli::command()
            .find_subcommand("agents")
            .expect("agents")
            .get_subcommands()
            .filter(|verb| verb.is_hide_set())
            .map(|verb| verb.get_name().to_owned())
            .collect::<Vec<_>>();
        assert!(!hidden.is_empty(), "agents keeps hidden internal verbs");
        let message = UsageError::missing_verb("agents", "a launch spec", &[]).to_string();
        for verb in hidden {
            assert!(
                !message.split([' ', ',']).any(|word| word == verb),
                "hidden verb `{verb}` leaked: {message}"
            );
        }
    }

    #[test]
    fn shell_command_quotes_what_the_shell_would_reinterpret() {
        assert_eq!(
            shell_command(["rimz", "agents", "list", "--all", "--json"]),
            "rimz agents list --all --json"
        );
        assert_eq!(
            shell_command(["rimz", "agents", "list", "#auth"]),
            "rimz agents list '#auth'"
        );
        let argv = ["rimz", "agents", "list", "--worktree", "it's $HOME `x`"];
        assert_eq!(
            shlex::split(&shell_command(argv)).expect("shell words"),
            argv,
            "a hostile name must come back as one literal word"
        );
    }
}
