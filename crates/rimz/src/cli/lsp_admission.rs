//! Pre-pane admission and required-server waits in the launcher's terminal.

use anyhow::Result;
use rimz::lsp::admission::{self, AdmissionRequest, Shortfall, WaitQueue};
use rimz::utils::size::decimal_bytes as bytes;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

pub(super) fn admit(checkout: &Path, machine: &rimz::config::MachineConfig) -> Result<()> {
    let workspace = rimz::workspace::WorkspaceResolver::resolve(checkout, None)?;
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
    let effective = rimz::config::effective::load(machine, workspace.launch_repo_root())?;
    if effective.lsp_servers.is_empty() && effective.untrusted_lsp_servers.is_empty() {
        return Ok(());
    }
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
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
        for message in result.startup_refused {
            writeln!(super::render::err(), "rimz: {message}; agents use grep")?;
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
            writeln!(
                super::render::err(),
                "rimz: waiting to start language server {}: needs {}, {} free; position {} in the queue; giving up in {}",
                wait.shortfall.server,
                bytes(wait.shortfall.estimate_bytes),
                bytes(free(&wait.shortfall)),
                wait.position,
                rimz::utils::time::format_duration_compact(wait.remaining)
            )?;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn free(shortfall: &Shortfall) -> u64 {
    shortfall
        .available_bytes
        .saturating_sub(shortfall.committed_bytes)
        .saturating_sub(shortfall.reserve_bytes)
}
