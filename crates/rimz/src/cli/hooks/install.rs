use std::io::Write as _;

use super::hook_install::{
    detected_installable_adapters, install_hooks_into, render_dry_run, write_post_install_footer,
    write_uninstall_result,
};
use super::*;
use rimz::agents::HookUninstallReport;

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
    let reports = match agent {
        Some(agent) => vec![definition_by_kind(&agent)?.uninstall_hooks(&login_env)?],
        None => uninstall_managed_hooks()?,
    };
    let mut out = crate::cli::render::out();
    if reports.is_empty() {
        return crate::cli::render::finish(writeln!(
            out,
            "No RimZ-managed hooks are installed; nothing to uninstall."
        ));
    }
    for report in &reports {
        crate::cli::render::finish(write_uninstall_result(&mut out, report))?;
    }
    Ok(())
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

pub(crate) fn uninstall_managed_hooks() -> Result<Vec<HookUninstallReport>> {
    let login_env = rimz::agents::ambient_env();
    let adapters = rimz::agents::all_definitions()
        .filter(|adapter| adapter.managed_hook_artifacts_present(&login_env))
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    for integration in adapters {
        reports.push(integration.uninstall_hooks(&login_env)?);
    }
    Ok(reports)
}
