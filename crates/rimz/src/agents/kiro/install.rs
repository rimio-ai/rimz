//! Cleanup for legacy whole-file Kiro v3 hook installs.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::agents::{AgentErr, HookUninstallReport, Result, read_optional_file};

const AGENT: &str = "kiro";
const RECLAIM_KEY: &str = "hooks feed --source kiro";
const LEGACY_EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];

pub(super) fn hooks_path() -> Result<PathBuf> {
    resolve_hooks_path(
        std::env::var_os("RIMZ_KIRO_HOOKS").as_deref(),
        std::env::var_os("KIRO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn home() -> Option<PathBuf> {
    resolve_home(
        std::env::var_os("KIRO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
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
    kiro_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(path) = override_path.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = resolve_home(kiro_home, home).ok_or_else(|| AgentErr::Install {
        agent: AGENT,
        reason: "$HOME is not set; cannot resolve ~/.kiro/hooks/rimz.json".to_owned(),
    })?;
    Ok(home.join("hooks/rimz.json"))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let original = read_optional_file(AGENT, path)?;
    let existed = original.is_some();
    let mut removed_events = Vec::new();
    if original.as_deref().is_some_and(file_is_owned) {
        std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
            agent: AGENT,
            path: path.to_path_buf(),
            source,
        })?;
        removed_events = event_names();
    }
    Ok(HookUninstallReport {
        agent: AGENT,
        config_path: path.to_path_buf(),
        removed_events,
        existed,
    })
}

pub(super) fn managed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| file_is_owned(&text))
}

fn file_is_owned(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|config| {
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
}

fn event_names() -> Vec<String> {
    LEGACY_EVENTS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}
