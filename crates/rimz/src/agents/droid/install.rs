//! Droid `settings.json` managed hook integration.

use std::path::{Path, PathBuf};

use super::{DROID_HOOK_TIMEOUT_SECS, DROID_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER};
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::{
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, agent_config_path,
};

const SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "droid",
    catalog: DROID_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: DROID_HOOK_TIMEOUT_SECS,
    sync: SyncEncoding::None,
    status_lines: &[],
};

pub(super) fn droid_settings_path() -> Result<PathBuf> {
    agent_config_path(
        "droid",
        "RIMZ_DROID_SETTINGS",
        Path::new(".factory/settings.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    SPEC.install_into(path)
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    SPEC.preview_at(path)
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    SPEC.uninstall_from(path)
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    SPEC.installed_at(path)
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    SPEC.managed_artifacts_at(path)
}
