use std::io::Write as _;

use anyhow::{Context, Result, bail};

use crate::cli::GlobalFlags;
use rimz::remote::aliases::RemoteAliases;

pub(super) fn run(alias_or_host: String, _globals: &GlobalFlags) -> Result<()> {
    let aliases = RemoteAliases::load().context("loading remote aliases")?;
    let destination = super::resolve_setup_destination(&alias_or_host, &aliases)?;
    let program = rimz::remote::ssh_program();
    which::which(&program).map_err(|_| {
        anyhow::anyhow!(
            "`{program}` is not on PATH; install an OpenSSH client to set up remote hosts"
        )
    })?;

    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: installing rimz on {} over ssh…",
        destination.host
    );
    let spec = rimz::remote::setup::setup_install_spec(&destination.destination, &destination.host);
    let status = spec
        .to_command()
        .status()
        .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(&spec)))?;
    if status.success() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "rimz installed on {}; run `rimz remote connect {}`",
            destination.host,
            alias_or_host,
        );
        return Ok(());
    }
    bail!(
        "remote setup on {} failed with {status}; install rimz manually: \
         https://github.com/rimio-ai/rimz/blob/main/docs/guide/installation.md",
        destination.host
    )
}
