use std::io::Write as _;

use super::hook_install::{
    detected_installable_adapters, install_disposition, render_dry_run, write_install_result,
    write_post_install_footer, write_uninstall_result, write_untrusted_hooks_notice,
};
use super::*;
use rimz::agents::HookUninstallReport;

pub(super) fn run_install(agent: Option<String>, dry_run: bool) -> Result<()> {
    if dry_run {
        return run_install_dry_run(agent);
    }

    let adapters = install_adapters(agent)?;
    let mut out = crate::cli::render::out();
    for integration in adapters {
        let disposition = install_disposition(integration);
        let report = integration.install_hooks()?;
        crate::cli::render::finish(write_install_result(&mut out, &report, disposition))?;
        crate::cli::render::finish(write_untrusted_hooks_notice(
            report.agent,
            &integration.untrusted_installed_hooks(),
            &mut out,
        ))?;
    }
    crate::cli::render::finish(write_post_install_footer(&mut out))
}

fn run_install_dry_run(agent: Option<String>) -> Result<()> {
    let mut previews = Vec::new();
    for integration in install_adapters(agent)? {
        previews.push(integration.preview_hook_install()?);
    }
    let mut out = crate::cli::render::out();
    crate::cli::render::finish(render_dry_run(&mut out, &previews))
}

pub(super) fn run_uninstall(agent: Option<String>) -> Result<()> {
    let reports = match agent {
        Some(agent) => vec![adapter_by_kind(&agent)?.uninstall_hooks()?],
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

fn install_adapters(agent: Option<String>) -> Result<Vec<&'static dyn rimz::agents::AgentAdapter>> {
    if let Some(agent) = agent {
        return Ok(vec![adapter_by_kind(&agent)?]);
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
    let adapters = rimz::agents::ADAPTERS
        .iter()
        .copied()
        .filter(|adapter| adapter.managed_hook_artifacts_present())
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    for integration in adapters {
        reports.push(integration.uninstall_hooks()?);
    }
    Ok(reports)
}
