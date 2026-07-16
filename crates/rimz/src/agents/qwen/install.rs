//! Qwen `settings.json` hook and nested statusline integration.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{QWEN_HOOK_TIMEOUT_MS, QWEN_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE};
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::managed_statusline;
use crate::agents::{Result, agent_config_path};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "qwen",
    catalog: QWEN_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: QWEN_HOOK_TIMEOUT_MS,
    sync: SyncEncoding::HandlerAsync,
    legacy_matcherless_blocking_events: &[],
    status_lines: &[&STATUS_LINE],
};

pub(super) static MANAGED_SOURCE: ManagedSource = ManagedSource::json(&SPEC, qwen_settings_path);

pub(super) fn qwen_settings_path() -> Result<PathBuf> {
    if std::env::var_os("RIMZ_QWEN_SETTINGS").is_some() {
        return agent_config_path(
            "qwen",
            "RIMZ_QWEN_SETTINGS",
            Path::new(".qwen/settings.json"),
        );
    }
    if let Some(home) = std::env::var_os("QWEN_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("settings.json"));
    }
    agent_config_path(
        "qwen",
        "RIMZ_QWEN_SETTINGS",
        Path::new(".qwen/settings.json"),
    )
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    SPEC.read_json(path)
}

pub(super) fn wrapped_status_line_command_from(root: &Map<String, Value>) -> Option<String> {
    managed_statusline::wrapped_command(root, &STATUS_LINE)
}
