//! Hibernation admission and provider scope naming. The exec wrapper stays live.

use crate::config::HibernateConfig;
use crate::ids::WorkspaceId;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum HibernateErr {
    #[error(transparent)]
    After(#[from] crate::config::HibernateAfterErr),
    #[error("{0}")]
    Preflight(String),
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, serde::Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
pub struct PreflightReport {
    pub enabled: bool,
    pub checks: Vec<Check>,
}

trait Host {
    fn systemd_run(&self) -> bool;
    fn manager(&self) -> bool;
    fn app_slice(&self) -> Result<PathBuf, String>;
    fn read(&self, path: &Path) -> std::io::Result<String>;
    fn writable(&self, path: &Path) -> bool;
    fn root(&self) -> &Path;
}

pub fn preflight(config: &HibernateConfig) -> Result<(), HibernateErr> {
    if !cfg!(target_os = "linux") || !config.enabled {
        return Ok(());
    }
    config.after()?;
    let report = inspect(config);
    if let Some(check) = report
        .checks
        .into_iter()
        .find(|check| matches!(check.status, CheckStatus::Error))
    {
        return Err(HibernateErr::Preflight(check.detail));
    }
    Ok(())
}

pub fn inspect(config: &HibernateConfig) -> PreflightReport {
    if !cfg!(target_os = "linux") {
        return PreflightReport {
            enabled: false,
            checks: Vec::new(),
        };
    }
    inspect_with(config, &SystemHost)
}

struct SystemHost;

impl Host for SystemHost {
    fn systemd_run(&self) -> bool {
        which::which("systemd-run").is_ok()
    }
    fn manager(&self) -> bool {
        let mut command = std::process::Command::new("systemctl");
        command.args(["--user", "show-environment"]);
        crate::proc::run_bounded_output(&mut command, std::time::Duration::from_secs(3))
            .is_ok_and(|output| !output.timed_out && output.status.success())
    }
    fn app_slice(&self) -> Result<PathBuf, String> {
        crate::proc::cgroup::user_app_slice().map_err(|error| error.to_string())
    }
    fn read(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn writable(&self, path: &Path) -> bool {
        nix::unistd::access(path, nix::unistd::AccessFlags::W_OK).is_ok()
    }
    fn root(&self) -> &Path {
        Path::new("/sys/fs/cgroup")
    }
}

fn inspect_with(config: &HibernateConfig, host: &impl Host) -> PreflightReport {
    let mut report = PreflightReport {
        enabled: config.enabled,
        checks: Vec::new(),
    };
    if !config.enabled {
        return report;
    }
    if let Err(error) = config.after() {
        report.checks.push(Check {
            name: "after",
            status: CheckStatus::Error,
            detail: error.to_string(),
        });
        return report;
    }
    let mut check = |name, ok, success: &str, failure: &str| {
        report.checks.push(Check {
            name,
            status: if ok {
                CheckStatus::Ok
            } else {
                CheckStatus::Error
            },
            detail: if ok { success.into() } else { failure.into() },
        });
    };
    check(
        "systemd-run",
        host.systemd_run(),
        "found",
        "systemd-run is unavailable; install systemd or set agents.hibernate.enabled = false",
    );
    check(
        "user manager",
        host.manager(),
        "reachable",
        "systemd user manager is unreachable; run loginctl enable-linger <user> or log in through a systemd session",
    );
    let slice = host.app_slice();
    let memory = slice.as_ref().is_ok_and(|slice| {
        host.writable(slice)
            && host
                .read(&slice.join("cgroup.controllers"))
                .is_ok_and(|controllers| {
                    controllers.split_whitespace().any(|name| name == "memory")
                })
    });
    check(
        "memory controller",
        memory,
        "delegated app.slice is writable",
        "user app.slice is unavailable, unwritable or lacks memory; enable delegation with systemctl edit user@.service and Delegate=memory pids cpu",
    );
    for (name, fix) in [
        (
            "cgroup.freeze",
            "enable writable cgroup-v2 freezer delegation or set agents.hibernate.enabled = false",
        ),
        (
            "memory.reclaim",
            "use Linux 5.19 or newer with writable memory controller delegation or set agents.hibernate.enabled = false",
        ),
    ] {
        check(
            name,
            slice
                .as_ref()
                .is_ok_and(|slice| host.writable(&slice.join(name))),
            "available and writable",
            &format!("{name} is absent or unwritable; {fix}"),
        );
    }
    let swap = host
        .read(Path::new("/proc/meminfo"))
        .ok()
        .and_then(|text| crate::proc::memory::parse_meminfo(&text).ok());
    check(
        "swap",
        swap.is_some_and(|memory| memory.swap_total_bytes > 0),
        "configured",
        "swap is absent or unreadable; add zram, zswap or a swapfile; in a container, allow swap",
    );
    // A namespace can hide tighter ancestors. Only a visible zero is proof of refusal.
    let limit = slice.as_ref().ok().map(|slice| {
        for path in slice
            .ancestors()
            .take_while(|path| path.starts_with(host.root()))
        {
            match host.read(&path.join("memory.swap.max")) {
                Ok(value) if value.trim() == "max" => {}
                Ok(value) if value.trim().parse::<u64>().is_ok_and(|limit| limit > 0) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return false,
            }
        }
        true
    });
    check(
        "swap limit",
        limit != Some(false),
        "no visible zero limit (not proof of swap availability)",
        "ancestor memory.swap.max is zero or unreadable; raise the ancestor memory.swap.max limit",
    );
    if swap.is_some_and(|memory| memory.swap_total_bytes > 0 && memory.swap_free_bytes == 0) {
        report.checks.push(Check {
            name: "free swap",
            status: CheckStatus::Warning,
            detail: "swap is configured but full; free swap space".into(),
        });
    }
    report
}

/// Prefix shared by launch and workspace-scoped reconciliation.
pub fn scope_prefix(workspace: &WorkspaceId) -> String {
    let id: String = workspace
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("rimz-agent-{id}-")
}

pub(crate) fn scope_argv(workspace: &WorkspaceId, kind: &str, argv: &[String]) -> Vec<String> {
    let unit = format!(
        "{}{}.scope",
        scope_prefix(workspace),
        uuid::Uuid::now_v7().simple()
    );
    let mut wrapped: Vec<String> = [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        "--collect",
        "-p",
        "MemoryAccounting=yes",
        "--unit",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    wrapped.extend([
        unit,
        "--description".into(),
        format!("RimZ agent {kind}"),
        "--".into(),
    ]);
    wrapped.extend_from_slice(argv);
    wrapped
}

#[cfg(test)]
mod tests;
