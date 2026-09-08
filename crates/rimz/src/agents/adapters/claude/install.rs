//! Claude `settings.json` hook and statusline integration.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    CLAUDE_HOOK_TIMEOUT_SECS, CLAUDE_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE,
    SUBAGENT_STATUS_LINE,
};
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::{Result, agent_config_path};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "claude",
    catalog: CLAUDE_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: CLAUDE_HOOK_TIMEOUT_SECS,
    sync: SyncEncoding::EntryMarker,
    status_lines: &[&STATUS_LINE, &SUBAGENT_STATUS_LINE],
};

pub(super) static MANAGED_SOURCE: ManagedSource = ManagedSource::json(&SPEC, claude_settings_path);

pub(super) fn claude_settings_path() -> Result<PathBuf> {
    agent_config_path(
        "claude",
        "RIMZ_CLAUDE_SETTINGS",
        Path::new(".claude/settings.json"),
    )
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    SPEC.read_json(path)
}
