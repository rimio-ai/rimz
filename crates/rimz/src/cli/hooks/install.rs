use super::*;
use std::io::Write as _;

pub(super) fn run_install(agent: Option<String>) -> Result<()> {
    match agent {
        Some(agent) => {
            let integration = adapter_by_kind(&agent)?;
            let report = integration.install_hooks()?;
            // User-facing JSON. Report struct derives Serialize so the shape stays in
            // lockstep with `HookInstallReport`.
            let rendered = serde_json::to_string_pretty(&report)?;
            #[expect(clippy::print_stdout, reason = "user-visible install report")]
            {
                println!("{rendered}");
            }
        }
        None => {
            let adapters = super::super::hook_install::detected_installable_adapters();
            if adapters.is_empty() {
                anyhow::bail!(
                    "no supported coding agents detected on PATH ({}) - install an agent and rerun, or name one: rimz hooks install <agent>",
                    rimz::agents::known_kinds().collect::<Vec<_>>().join(", "),
                );
            }
            let mut reports = Vec::new();
            for integration in adapters {
                reports.push(integration.install_hooks()?);
            }
            let rendered = serde_json::to_string_pretty(&reports)?;
            #[expect(clippy::print_stdout, reason = "user-visible install report")]
            {
                println!("{rendered}");
            }
        }
    }
    Ok(())
}

pub(super) fn run_uninstall(agent: Option<String>) -> Result<()> {
    match agent {
        Some(agent) => {
            let integration = adapter_by_kind(&agent)?;
            let report = integration.uninstall_hooks()?;
            let rendered = serde_json::to_string_pretty(&report)?;
            #[expect(clippy::print_stdout, reason = "user-visible uninstall report")]
            {
                println!("{rendered}");
            }
        }
        None => {
            let adapters = rimz::agents::ADAPTERS
                .iter()
                .copied()
                .filter(|adapter| adapter.managed_hook_artifacts_present())
                .collect::<Vec<_>>();
            if adapters.is_empty() {
                #[expect(clippy::print_stdout, reason = "user-visible uninstall report")]
                {
                    println!("[]");
                }
                let mut stderr = std::io::stderr().lock();
                writeln!(
                    stderr,
                    "No Rimz-managed hooks are installed; nothing to uninstall."
                )?;
                return Ok(());
            }
            let mut reports = Vec::new();
            for integration in adapters {
                reports.push(integration.uninstall_hooks()?);
            }
            let rendered = serde_json::to_string_pretty(&reports)?;
            #[expect(clippy::print_stdout, reason = "user-visible uninstall report")]
            {
                println!("{rendered}");
            }
        }
    }
    Ok(())
}
