//! Droid `settings.json` managed hook integration.

use std::path::{Path, PathBuf};

use super::{DROID_HOOK_TIMEOUT_SECS, DROID_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER};
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::{Result, agent_config_path};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "droid",
    catalog: DROID_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: DROID_HOOK_TIMEOUT_SECS,
    sync: SyncEncoding::None,
    legacy_matcherless_blocking_events: &[],
    status_lines: &[],
};

pub(super) static MANAGED_SOURCE: ManagedSource = ManagedSource::json(&SPEC, droid_settings_path);

pub(super) fn droid_settings_path() -> Result<PathBuf> {
    agent_config_path(
        "droid",
        "RIMZ_DROID_SETTINGS",
        Path::new(".factory/settings.json"),
    )
}
