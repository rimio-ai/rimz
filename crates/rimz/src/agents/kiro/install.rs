//! Whole-file Kiro v3 hook installation.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::WIRED_EVENTS;
use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    read_optional_file,
};
use crate::store::atomic;

const AGENT: &str = "kiro";
const RECLAIM_KEY: &str = "hooks feed --source kiro";

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct HookConfig {
    version: String,
    hooks: Vec<HookEntry>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct HookEntry {
    trigger: String,
    name: String,
    action: HookAction,
    timeout: u64,
    enabled: bool,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct HookAction {
    #[serde(rename = "type")]
    kind: String,
    command: String,
}

pub(super) fn hooks_path() -> Result<PathBuf> {
    resolve_hooks_path(
        std::env::var_os("RIMZ_KIRO_HOOKS").as_deref(),
        std::env::var_os("KIRO_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(super) fn resolve_hooks_path(
    override_path: Option<&OsStr>,
    kiro_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf> {
    if let Some(path) = override_path.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(home) = kiro_home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("hooks/rimz.json"));
    }
    let home = home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AgentErr::Install {
            agent: AGENT,
            reason: "$HOME is not set; cannot resolve ~/.kiro/hooks/rimz.json".to_owned(),
        })?;
    Ok(home.join(".kiro/hooks/rimz.json"))
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let original = read_optional_file(AGENT, path)?;
    refuse_unowned(path, original.as_deref())?;
    let candidate = canonical_config()?;
    atomic::write_bytes_atomically(path, candidate.as_bytes())?;
    Ok(HookInstallReport {
        agent: AGENT,
        config_path: path.to_path_buf(),
        installed_events: event_names(),
        merged: original.is_some(),
    })
}

pub(super) fn preview_at(path: &Path) -> Result<HookInstallPreview> {
    let original = read_optional_file(AGENT, path)?;
    refuse_unowned(path, original.as_deref())?;
    Ok(HookInstallPreview {
        agent: AGENT,
        config_path: path.to_path_buf(),
        planned_events: event_names(),
        candidate_config: canonical_config()?,
        merged: original.is_some(),
        original_config: original,
        status_line_change: None,
        subagent_status_line_change: None,
    })
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

pub(super) fn installed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| {
        let Ok(installed) = serde_json::from_str::<HookConfig>(&text) else {
            return false;
        };
        canonical_config()
            .ok()
            .and_then(|candidate| serde_json::from_str::<HookConfig>(&candidate).ok())
            .is_some_and(|candidate| installed == candidate)
    })
}

pub(super) fn managed_at(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| file_is_owned(&text))
}

fn canonical_config() -> Result<String> {
    let executable = std::env::current_exe().map_err(|source| AgentErr::Install {
        agent: AGENT,
        reason: format!("cannot resolve the current rimz executable: {source}"),
    })?;
    let executable = executable.to_str().ok_or_else(|| AgentErr::Install {
        agent: AGENT,
        reason: format!(
            "the current rimz executable path is not valid UTF-8: {}",
            executable.display()
        ),
    })?;
    let quoted = shlex::try_quote(executable).map_err(|source| AgentErr::Install {
        agent: AGENT,
        reason: format!("cannot shell-quote the current rimz executable: {source}"),
    })?;
    let hooks = WIRED_EVENTS
        .iter()
        .map(|event| HookEntry {
            trigger: (*event).to_owned(),
            name: format!("rimz-{}", trigger_kebab(event)),
            action: HookAction {
                kind: "command".to_owned(),
                command: format!("{quoted} hooks feed --source kiro --event {event}"),
            },
            timeout: 10,
            enabled: true,
        })
        .collect();
    let text = serde_json::to_string_pretty(&HookConfig {
        version: "v1".to_owned(),
        hooks,
    })
    .map_err(|source| AgentErr::InstallSerialize {
        agent: AGENT,
        source: Box::new(source),
    })?;
    Ok(format!("{text}\n"))
}

fn trigger_kebab(trigger: &str) -> String {
    let mut out = String::with_capacity(trigger.len() + 3);
    for (index, character) in trigger.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            out.push('-');
        }
        out.push(character.to_ascii_lowercase());
    }
    out
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

fn refuse_unowned(path: &Path, original: Option<&str>) -> Result<()> {
    if original.is_some_and(|text| !file_is_owned(text)) {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "refusing to overwrite an unmarked user hook config at {}; move it aside or remove it to let Rimz manage this file",
                path.display()
            ),
        });
    }
    Ok(())
}

fn event_names() -> Vec<String> {
    WIRED_EVENTS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}
