use std::io::Write as _;

use super::hook_install::{
    detected_installable_adapters, install_hooks_into, render_dry_run, write_post_install_footer,
    write_uninstall_result,
};
use super::*;
use rimz::agents::{HookUninstallReport, LoginConfigErr};

pub(super) fn run_install(agent: Option<String>, dry_run: bool) -> Result<()> {
    let login_env = rimz::agents::ambient_env();
    if dry_run {
        return run_install_dry_run(agent);
    }

    let adapters = install_definitions(agent)?;
    let mut out = crate::cli::render::out();
    for integration in adapters {
        install_hooks_into(integration, &login_env, &mut out)?;
    }
    crate::cli::render::finish(write_post_install_footer(&mut out))
}

fn run_install_dry_run(agent: Option<String>) -> Result<()> {
    let login_env = rimz::agents::ambient_env();
    let mut previews = Vec::new();
    for integration in install_definitions(agent)? {
        previews.push(integration.preview_hook_install(&login_env)?);
    }
    let mut out = crate::cli::render::out();
    crate::cli::render::finish(render_dry_run(&mut out, &previews))
}

pub(super) fn run_uninstall(agent: Option<String>) -> Result<()> {
    let login_env = rimz::agents::ambient_env();
    let (reports, config_err) = match agent {
        Some(agent) => (
            vec![definition_by_kind(&agent)?.uninstall_hooks(&login_env)?],
            None,
        ),
        None => {
            let (reports, config_err) = uninstall_managed_hooks()?;
            (
                reports.into_iter().map(|(_, report)| report).collect(),
                config_err,
            )
        }
    };
    let mut out = crate::cli::render::out();
    if reports.is_empty() {
        crate::cli::render::finish(writeln!(
            out,
            "No RimZ-managed hooks are installed; nothing to uninstall."
        ))?;
    }
    for report in &reports {
        crate::cli::render::finish(write_uninstall_result(&mut out, report))?;
    }
    match config_err {
        Some(err) => Err(anyhow::Error::new(err).context(
            "only the providers' own homes were unhooked; fix the accounts config and rerun",
        )),
        None => Ok(()),
    }
}

fn install_definitions(
    agent: Option<String>,
) -> Result<Vec<&'static rimz::agents::AgentDefinition>> {
    if let Some(agent) = agent {
        return Ok(vec![definition_by_kind(&agent)?]);
    }

    let adapters = detected_installable_adapters();
    if adapters.is_empty() {
        anyhow::bail!(
            "no supported coding agents detected on PATH ({}) - install an agent and rerun, or name one: rimz hooks install <agent>",
            rimz::agents::known_kinds().collect::<Vec<_>>().join(", "),
        );
    }
    Ok(adapters)
}

/// A provider home's account, its adapter, and the env its hooks live under.
type ManagedHookLogin = (
    rimz::ids::LoginKey,
    &'static rimz::agents::AgentDefinition,
    std::collections::BTreeMap<String, String>,
);

/// Every provider home that carries RimZ-managed hooks: each kind's own home
/// and every declared account's, labelled by account. An accounts config the
/// catalog refuses still yields the providers' own homes, alongside its error.
pub(crate) fn managed_hook_logins() -> (Vec<ManagedHookLogin>, Option<rimz::agents::LoginConfigErr>)
{
    let ambient = rimz::agents::ambient_env();
    let (logins, config_err) =
        match rimz::agents::LoginCatalog::from_config(&crate::cli::machine_config().accounts) {
            Ok(catalog) => (catalog.all().cloned().collect::<Vec<_>>(), None),
            Err(err) => (
                rimz::agents::known_kinds()
                    .map(|kind| {
                        rimz::agents::ProviderLogin::default_for(
                            rimz::ids::AgentKind::new_unchecked(kind),
                        )
                    })
                    .collect(),
                Some(err),
            ),
        };
    let logins = logins
        .into_iter()
        .filter_map(|login| {
            let adapter = rimz::agents::find_definition(login.kind().as_str())?;
            let login_env = login.env(&ambient);
            adapter
                .managed_hook_artifacts_present(&login_env)
                .then(|| (login.key(), adapter, login_env))
        })
        .collect();
    (logins, config_err)
}

/// Each unhooked home's account and what its uninstall changed.
type LoginHookReports = Vec<(rimz::ids::LoginKey, HookUninstallReport)>;

/// Unhook every managed home [`managed_hook_logins`] finds, passing its
/// accounts config error through.
pub(crate) fn uninstall_managed_hooks() -> Result<(LoginHookReports, Option<LoginConfigErr>)> {
    let (logins, config_err) = managed_hook_logins();
    let mut reports = Vec::new();
    for (key, integration, login_env) in logins {
        reports.push((key, integration.uninstall_hooks(&login_env)?));
    }
    Ok((reports, config_err))
}
