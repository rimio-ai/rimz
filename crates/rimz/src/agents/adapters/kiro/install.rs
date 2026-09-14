//! Managed global Kiro hook file, `~/.kiro/hooks/rimz.json`.
//!
//! Kiro CLI 2.13.0 made `~/.kiro/hooks/*.json` fire in every workspace, so
//! RimZ owns one whole file there. Earlier releases ran those hooks only when
//! the workspace was the home directory, so install refuses on them. A file
//! from the older unmarked RimZ install is reclaimed rather than refused.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{KIRO_HOOKS, KIRO_MANAGED_SOURCE};
use crate::agents::managed_source::RIMZ_MANAGED_MARKER;
use crate::agents::version::{CliVersion, probe_cli_version};
use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallPreview, HookInstallReport, HookUninstallReport,
    ManagedIntegration, Result, read_optional_file,
};

const AGENT: &str = "kiro";
const RECLAIM_KEY: &str = "hooks feed --source kiro";
pub(super) const MIN_GLOBAL_HOOKS: CliVersion = CliVersion::new(2, 13, 0);

pub(super) static MANAGED_INTEGRATION: KiroManagedIntegration = KiroManagedIntegration;

pub(super) struct KiroManagedIntegration;

impl ManagedIntegration for KiroManagedIntegration {
    fn install(&self, login_env: &BTreeMap<String, String>) -> Result<HookInstallReport> {
        install_at(&hooks_path(login_env)?, installed_cli_version())
    }

    fn preview(&self, login_env: &BTreeMap<String, String>) -> Result<HookInstallPreview> {
        preview_at(&hooks_path(login_env)?, installed_cli_version())
    }

    fn uninstall(&self, login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall_from(&hooks_path(login_env)?)
    }

    fn installed(&self, login_env: &BTreeMap<String, String>) -> bool {
        KIRO_MANAGED_SOURCE.installed(login_env)
    }

    fn upgrade_available(&self, login_env: &BTreeMap<String, String>) -> bool {
        KIRO_MANAGED_SOURCE.upgrade_available(login_env)
    }

    fn managed_artifacts_present(&self, login_env: &BTreeMap<String, String>) -> bool {
        hooks_path(login_env).is_ok_and(|path| managed_at(&path))
    }

    fn install_blocker(&self) -> Option<String> {
        refuse_old_cli(installed_cli_version())
            .err()
            .map(|err| err.to_string())
    }
}

fn installed_cli_version() -> Option<CliVersion> {
    probe_cli_version("kiro-cli")?.parse().ok()
}

pub(super) fn hooks_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    resolve_hooks_path(
        std::env::var_os("RIMZ_KIRO_HOOKS").as_deref(),
        login_env.get("HOME").map(OsStr::new),
    )
}

/// The `.kiro` directory the v3 engine reads sessions and global hooks from.
/// The engine resolves it from the OS home and never reads `KIRO_HOME`, which
/// moves only the launcher's settings (verified on Kiro CLI 2.21.4).
pub(super) fn engine_home(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    resolve_home(None, login_env.get("HOME").map(OsStr::new))
}

pub(super) fn resolve_home(kiro_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    kiro_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".kiro"))
        })
}

pub(super) fn resolve_hooks_path(
    override_path: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(path) = override_path.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = resolve_home(None, home).ok_or_else(|| AgentErr::Install {
        agent: AGENT,
        reason: "$HOME is not set; cannot resolve ~/.kiro/hooks/rimz.json".to_owned(),
    })?;
    Ok(home.join("hooks/rimz.json"))
}

/// An unprobeable binary installs: the version gate only refuses a release
/// known to predate global hooks.
fn refuse_old_cli(version: Option<CliVersion>) -> Result<()> {
    match version {
        Some(found) if found < MIN_GLOBAL_HOOKS => Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "Kiro CLI {found} runs ~/.kiro/hooks only in the home workspace; upgrade to {MIN_GLOBAL_HOOKS} or later"
            ),
        }),
        _ => Ok(()),
    }
}

pub(super) fn install_at(path: &Path, version: Option<CliVersion>) -> Result<HookInstallReport> {
    refuse_old_cli(version)?;
    let legacy = legacy_owned(path)?;
    if legacy {
        std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
            agent: AGENT,
            path: path.to_path_buf(),
            source,
        })?;
    }
    let mut report = KIRO_MANAGED_SOURCE.install_into(path)?;
    report.files[0].existed |= legacy;
    Ok(report)
}

pub(super) fn preview_at(path: &Path, version: Option<CliVersion>) -> Result<HookInstallPreview> {
    refuse_old_cli(version)?;
    if !legacy_owned(path)? {
        return KIRO_MANAGED_SOURCE.preview_at(path);
    }
    Ok(HookInstallPreview {
        agent: AGENT,
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            original: read_optional_file(AGENT, path)?,
            candidate: super::HOOK_SOURCE.to_owned(),
            existed: true,
        }],
        planned_events: event_names(),
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    if !legacy_owned(path)? {
        return KIRO_MANAGED_SOURCE.uninstall_from(path);
    }
    std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
        agent: AGENT,
        path: path.to_path_buf(),
        source,
    })?;
    Ok(HookUninstallReport {
        agent: AGENT,
        files: vec![crate::agents::HookInstallFileReport {
            path: path.to_path_buf(),
            existed: true,
        }],
        removed_events: event_names(),
    })
}

pub(super) fn managed_at(path: &Path) -> bool {
    KIRO_MANAGED_SOURCE.managed_artifacts_at(path) || legacy_owned(path).unwrap_or(false)
}

/// The pre-marker RimZ install: an unmarked file whose every hook runs the
/// Kiro feed command.
fn legacy_owned(path: &Path) -> Result<bool> {
    Ok(read_optional_file(AGENT, path)?.is_some_and(|text| {
        !text
            .lines()
            .next()
            .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
            && serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|config| {
                config
                    .get("hooks")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|hooks| {
                        !hooks.is_empty()
                            && hooks.iter().all(|hook| {
                                hook.pointer("/action/command")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|command| command.contains(RECLAIM_KEY))
                            })
                    })
            })
    }))
}

fn event_names() -> Vec<String> {
    KIRO_HOOKS
        .iter()
        .map(|hook| hook.event.to_owned())
        .collect()
}
