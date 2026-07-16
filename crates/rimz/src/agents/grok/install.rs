//! Additive global Grok Build hook installation.

#[cfg(test)]
use crate::agents::hook_types::HookRecord;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;

use super::{GROK_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER};

static SPEC: ManagedJsonHookSpec = ManagedJsonHookSpec {
    agent: "grok",
    catalog: GROK_HOOKS,
    command: RIMZ_HOOK_COMMAND,
    legacy_command_marker: RIMZ_HOOK_MARKER,
    timeout: 4,
    sync: SyncEncoding::None,
    legacy_matcherless_blocking_events: &[],
    status_lines: &[],
};

pub(super) static MANAGED_SOURCE: ManagedSource =
    ManagedSource::json(&SPEC, super::paths::hooks_path);

#[cfg(test)]
pub(super) fn catalog() -> &'static [HookRecord] {
    GROK_HOOKS
}
