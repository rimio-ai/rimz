//! Qwen `settings.json` hook and nested statusline integration.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{QWEN_HOOK_TIMEOUT_SECS, QWEN_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::{AgentErr, Result};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "qwen",
    catalog: QWEN_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: QWEN_HOOK_TIMEOUT_SECS,
    sync: SyncEncoding::HandlerAsync,
    status_lines: &[&STATUS_LINE],
};

pub(super) static MANAGED_SOURCE: ManagedSource = ManagedSource::json(&SPEC, qwen_settings_path);

pub(super) fn qwen_settings_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    if let Some(raw) = login_env
        .get("RIMZ_QWEN_SETTINGS")
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(raw));
    }
    super::QwenAdapter
        .config_home(login_env)
        .map(|home| home.join("settings.json"))
        .ok_or_else(|| AgentErr::Install {
            agent: "qwen",
            reason: "$QWEN_HOME and $HOME are not set; cannot resolve Qwen settings".to_owned(),
        })
}

pub(super) fn qwen_home_from(
    configured: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".qwen"))
        })
}
