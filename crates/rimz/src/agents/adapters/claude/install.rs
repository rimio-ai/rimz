//! Claude `settings.json` hook and statusline integration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    CLAUDE_HOOK_TIMEOUT_SECS, CLAUDE_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE,
    SUBAGENT_STATUS_LINE,
};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::{AgentErr, Result};

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

pub(super) fn claude_settings_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(raw) = login_env
        .get("RIMZ_CLAUDE_SETTINGS")
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(raw));
    }
    super::ClaudeAdapter
        .config_home(login_env)
        .map(|home| home.join("settings.json"))
        .ok_or_else(|| AgentErr::Install {
            agent: "claude",
            reason: "$CLAUDE_CONFIG_DIR and $HOME are not set; cannot resolve Claude settings"
                .to_owned(),
        })
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    SPEC.read_json(path)
}
