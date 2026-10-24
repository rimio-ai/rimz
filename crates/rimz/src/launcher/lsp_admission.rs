//! Pre-pane language-server admission and required-server waits.

use crate::lsp::admission::{self, AdmissionRequest, WaitQueue};
use anyhow::Result;
use std::path::Path;
use std::time::Duration;

pub fn admit(
    checkout: &Path,
    machine: &crate::config::MachineConfig,
    interaction: &mut dyn super::Interaction,
) -> Result<()> {
    let workspace = crate::workspace::WorkspaceResolver::resolve(checkout, None)?;
    if machine.lsp.servers.is_empty() {
        let declares_servers =
            match std::fs::read_to_string(workspace.launch_repo_root().join(".rimz/config.toml")) {
                Ok(text) => toml::from_str::<toml::Value>(&text).map_or(true, |value| {
                    value
                        .get("lsp")
                        .and_then(|lsp| lsp.get("servers"))
                        .is_some_and(|servers| {
                            servers.as_table().is_none_or(|servers| !servers.is_empty())
                        })
                }),
                Err(error) => error.kind() != std::io::ErrorKind::NotFound,
            };
        if !declares_servers {
            return Ok(());
        }
    }
    let effective = crate::config::effective::load(machine, workspace.launch_repo_root())?;
    if effective.lsp_servers.is_empty() && effective.untrusted_lsp_servers.is_empty() {
        return Ok(());
    }
    let runtime = crate::RuntimePaths::for_project_root(&workspace.project_root)?;
    let request = AdmissionRequest {
        root: checkout,
        project: workspace.launch_repo_root(),
        servers: &effective.lsp_servers,
        untrusted_servers: &effective.untrusted_lsp_servers,
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let mut queue = WaitQueue::default();
    let mut shown = Vec::new();
    loop {
        let result = admission::admit_launch(&request, &mut queue)?;
        for message in result.ignored_untrusted {
            interaction.notice(super::Notice::LspIgnored(&message))?;
        }
        for message in result.startup_refused {
            interaction.notice(super::Notice::LspRefused(&message))?;
        }
        if result.wait_for_required.is_empty() {
            return Ok(());
        }
        let positions: Vec<_> = result
            .wait_for_required
            .iter()
            .map(|wait| (wait.shortfall.server.clone(), wait.position))
            .collect();
        if positions == shown {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        shown = positions;
        for wait in result.wait_for_required {
            interaction.notice(super::Notice::LspWait(&wait))?;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
