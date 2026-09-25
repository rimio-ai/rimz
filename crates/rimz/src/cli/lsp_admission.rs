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
        for shortfall in result.refused_optional {
            writeln!(super::render::err(), "{}", refusal(&shortfall))?;
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

fn refusal(shortfall: &Shortfall) -> String {
    let holders = shortfall
        .holders
        .iter()
        .map(|holder| {
            format!(
                "{} {} {}",
                holder.root.display(),
                holder.server,
                bytes(holder.rss_bytes)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let holders = if holders.is_empty() {
        String::new()
    } else {
        format!(" (held: {holders})")
    };
    format!(
        "rimz: language server {} not started: needs {}, {} free after the {} reserve{holders}; agents use grep",
        shortfall.server,
        bytes(shortfall.estimate_bytes),
        bytes(free(shortfall)),
        bytes(shortfall.reserve_bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_refusal_names_reserve_and_holders_in_decimal_bytes() {
        let shortfall = Shortfall {
            root: "/checkout".into(),
            server: "rust".into(),
            estimate_bytes: 8_000_000_000,
            available_bytes: 15_800_000_000,
            committed_bytes: 1_000_000_000,
            reserve_bytes: 9_600_000_000,
            holders: vec![admission::Holder {
                root: "/held".into(),
                server: "rust".into(),
                rss_bytes: 6_300_000_000,
            }],
        };
        insta::assert_snapshot!(refusal(&shortfall), @"rimz: language server rust not started: needs 8 GB, 5.2 GB free after the 9.6 GB reserve (held: /held rust 6.3 GB); agents use grep");
        let mut shortfall = shortfall;
        shortfall.holders.clear();
        assert!(!refusal(&shortfall).contains("held:"));
    }
}
