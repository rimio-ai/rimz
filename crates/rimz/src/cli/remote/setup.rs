use std::io::Write as _;

use anyhow::{Context, Result, bail};

use crate::cli::GlobalFlags;
use rimz::remote::RemoteTarget;
use rimz::remote::aliases::RemoteAliases;

pub(super) fn run(alias_or_host: String, _globals: &GlobalFlags) -> Result<()> {
    let aliases = RemoteAliases::load().context("loading remote aliases")?;
    let destination = super::resolve_setup_destination(&alias_or_host, &aliases)?;
    let connect_hint = connect_hint(&alias_or_host, &aliases);
    let program = rimz::remote::ssh_program();
    which::which(&program).map_err(|_| {
        anyhow::anyhow!(
            "`{program}` is not on PATH; install an OpenSSH client to set up remote hosts"
        )
    })?;

    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: installing rimz on {} over ssh…",
        destination.host_display()
    );
    let spec =
        rimz::remote::setup::setup_install_spec(destination.as_str(), destination.host_display());
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(&spec)))?;
    if status.success() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz installed on {}; run `{connect_hint}`",
            destination.host_display()
        );
        return Ok(());
    }
    bail!(
        "remote setup on {} failed with {status}; install rimz manually: \
         https://github.com/rimio-ai/rimz/blob/main/docs/guide/installation.md",
        destination.host_display()
    )
}

fn connect_hint(input: &str, aliases: &RemoteAliases) -> String {
    if aliases.get(input).is_some() || RemoteTarget::parse(input).is_ok() {
        format!("rimz remote connect {input}")
    } else {
        format!("rimz remote connect {input}:<session-or-path>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::remote::aliases::RemoteAlias;

    fn alias(name: &str, target: &str) -> RemoteAlias {
        RemoteAlias {
            name: name.to_owned(),
            target: target.to_owned(),
            reconnect: true,
            no_resume: false,
            mux: None,
            auto_forward: true,
        }
    }

    #[test]
    fn connect_hint_keeps_alias_and_target_but_templates_bare_host() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("dev", "dev-box:query-engine"))
            .expect("alias");

        assert_eq!(connect_hint("dev", &aliases), "rimz remote connect dev");
        assert_eq!(
            connect_hint("dev-box:query-engine", &aliases),
            "rimz remote connect dev-box:query-engine"
        );
        assert_eq!(
            connect_hint("dev-box", &aliases),
            "rimz remote connect dev-box:<session-or-path>"
        );
        assert_eq!(
            connect_hint("user@[::1]", &aliases),
            "rimz remote connect user@[::1]:<session-or-path>"
        );
    }
}
