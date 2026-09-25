//! Pre-pane admission and required-server waits in the launcher's terminal.

use anyhow::Result;
use rimz::lsp::admission::{self, AdmissionRequest, Shortfall, WaitQueue};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

pub(super) fn admit(checkout: &Path) -> Result<()> {
    let machine = rimz::config::MachineConfig::load()?;
    let workspace = rimz::workspace::WorkspaceResolver::resolve(checkout, None)?;
    let effective = rimz::config::effective::load(&machine, workspace.launch_repo_root())?;
    if effective.lsp_servers.is_empty() && effective.untrusted_lsp_servers.is_empty() {
        return Ok(());
    }
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    let request = AdmissionRequest {
        root: checkout,
        servers: &effective.lsp_servers,
        untrusted_servers: &effective.untrusted_lsp_servers,
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let mut queue = WaitQueue::default();
    loop {
        let result = admission::admit_launch(&request, &mut queue)?;
        for shortfall in result.refused_optional {
            writeln!(super::render::err(), "{}", refusal(&shortfall))?;
        }
        if result.wait_for_required.is_empty() {
            return Ok(());
        }
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
    format!(
        "rimz: language server {} not started: needs {}, {} free after the {} reserve (held: {holders}); agents use grep",
        shortfall.server,
        bytes(shortfall.estimate_bytes),
        bytes(free(shortfall)),
        bytes(shortfall.reserve_bytes)
    )
}

fn bytes(bytes: u64) -> String {
    for (factor, unit) in [(1_000_000_000_u64, "GB"), (1_000_000, "MB"), (1_000, "KB")] {
        if bytes >= factor {
            let amount = format!("{:.1}", bytes as f64 / factor as f64);
            return format!("{} {unit}", amount.strip_suffix(".0").unwrap_or(&amount));
        }
    }
    format!("{bytes} B")
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
    }
}
