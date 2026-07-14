//! Claude `settings.json` hook and statusline integration.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    CLAUDE_HOOK_TIMEOUT_SECS, CLAUDE_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE,
    SUBAGENT_STATUS_LINE,
};
#[cfg(test)]
use crate::agents::StatusLineChange;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_statusline::{self, ManagedStatusLineSpec};
use crate::agents::{
    HookInstallPreview, HookInstallReport, HookUninstallReport, Result, agent_config_path,
};

const SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "claude",
    catalog: CLAUDE_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: CLAUDE_HOOK_TIMEOUT_SECS,
    sync: SyncEncoding::EntryMarker,
    status_lines: &[&STATUS_LINE, &SUBAGENT_STATUS_LINE],
};

pub(super) fn claude_settings_path() -> Result<PathBuf> {
    agent_config_path(
        "claude",
        "RIMZ_CLAUDE_SETTINGS",
        Path::new(".claude/settings.json"),
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

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    SPEC.read_json(path)
}

#[cfg(test)]
pub(super) fn upsert_rimz_status_line(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) {
    managed_statusline::upsert(root, spec);
}

#[cfg(test)]
pub(super) fn classify_status_line_change(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> StatusLineChange {
    managed_statusline::classify(root, spec).expect("Claude wraps every statusline shape")
}

pub(super) fn wrapped_status_line_command_from(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> Option<String> {
    managed_statusline::wrapped_command(root, spec)
}
