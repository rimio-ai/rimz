//! Claude `settings.json` hook and statusline integration, and the RimZ mod
//! folder the same install writes and lists in the settings `env` block.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    CLAUDE_HOOK_TIMEOUT_SECS, CLAUDE_HOOKS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, STATUS_LINE,
    SUBAGENT_STATUS_LINE,
};
use super::{mod_policy, rimz_mod};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::managed_json_hooks::{ManagedJsonHookSpec, SyncEncoding};
use crate::agents::managed_source::ManagedSource;
use crate::agents::{
    AgentErr, AgentSpec, HookInstallOptions, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, ModBlocker, ModPlan, Result,
};

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

/// The settings hooks and status lines of [`MANAGED_SOURCE`], plus the RimZ mod:
/// install writes the mod folder before the settings entry that lists it, so
/// the entry never names a folder that is not there. Uninstall removes the entry
/// and leaves the folder, which every Claude account shares and which loads in
/// no session once no settings list it; machine-wide `rimz uninstall` removes it.
/// Under a managed policy that blocks the mod ([`mod_policy::blocker`]), install
/// writes no folder and removes RimZ's own entry, keeping the hooks.
pub(super) static MANAGED_INTEGRATION: ClaudeManagedIntegration = ClaudeManagedIntegration;

pub(super) struct ClaudeManagedIntegration;

impl ManagedIntegration for ClaudeManagedIntegration {
    fn install(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: HookInstallOptions,
    ) -> Result<HookInstallReport> {
        let settings = claude_settings_path(login_env)?;
        let dir = rimz_mod::dir(login_env);
        let blocked = mod_policy::blocker(login_env).is_some();
        let entry = rimz_mod::env_path(&dir, !blocked)?;
        let written = if blocked {
            Vec::new()
        } else {
            rimz_mod::write(&dir)?
        };
        let mut report = SPEC.install_into_with(&settings, &[entry])?;
        report.files.extend(written);
        report.plugin_dir = (!blocked).then_some(dir);
        Ok(report)
    }

    fn preview(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        let dir = rimz_mod::dir(login_env);
        let blocker = mod_policy::blocker(login_env);
        let entry = rimz_mod::env_path(&dir, blocker.is_none())?;
        let mut preview = SPEC.preview_at_with(&claude_settings_path(login_env)?, &[entry])?;
        preview.plugin_dir = Some(match blocker {
            Some(blocker) => ModPlan::Skipped(blocker),
            None => {
                preview.files.extend(rimz_mod::previews(&dir)?);
                ModPlan::Listed(dir)
            }
        });
        Ok(preview)
    }

    fn uninstall(&self, login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        let dir = rimz_mod::dir(login_env);
        SPEC.uninstall_from_with(
            &claude_settings_path(login_env)?,
            &[rimz_mod::env_path(&dir, false)?],
        )
    }

    fn mod_blocker(&self, login_env: &BTreeMap<String, String>) -> Option<ModBlocker> {
        mod_policy::blocker(login_env)
    }

    fn installed(&self, login_env: &BTreeMap<String, String>) -> bool {
        MANAGED_SOURCE.installed(login_env)
    }

    fn managed_artifacts_present(&self, login_env: &BTreeMap<String, String>) -> bool {
        let dir = rimz_mod::dir(login_env);
        let (Ok(settings), Ok(entry)) = (
            claude_settings_path(login_env),
            rimz_mod::env_path(&dir, true),
        ) else {
            return MANAGED_SOURCE.managed_artifacts_present(login_env);
        };
        SPEC.managed_artifacts_at_with(&settings, &[entry])
    }

    /// The settings file when its owned hooks, status lines or mod entry differ
    /// from what install writes, and, once the settings carry RimZ's
    /// integration, each mod file that is missing or stale. Under a policy that
    /// blocks the mod, a listed entry is the drift and the folder is none.
    fn outdated_files(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: HookInstallOptions,
    ) -> Vec<PathBuf> {
        let dir = rimz_mod::dir(login_env);
        let blocked = mod_policy::blocker(login_env).is_some();
        let (Ok(settings), Ok(entry)) = (
            claude_settings_path(login_env),
            rimz_mod::env_path(&dir, !blocked),
        ) else {
            return MANAGED_SOURCE.outdated_files(login_env);
        };
        let mut files = Vec::new();
        if SPEC.outdated_at_with(&settings, &[entry]) {
            files.push(settings.clone());
        }
        if !blocked && SPEC.managed_artifacts_at(&settings) {
            files.extend(rimz_mod::outdated(&dir));
        }
        files
    }

    fn wiring_input_paths(
        &self,
        descriptor: &AgentSpec,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        MANAGED_SOURCE.wiring_input_paths(descriptor, login_env)
    }

    fn wrapped_status_line_command(&self, login_env: &BTreeMap<String, String>) -> Option<String> {
        MANAGED_SOURCE.wrapped_status_line_command(login_env)
    }

    fn wrapped_subagent_status_line_command(
        &self,
        login_env: &BTreeMap<String, String>,
    ) -> Option<String> {
        MANAGED_SOURCE.wrapped_subagent_status_line_command(login_env)
    }
}

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
