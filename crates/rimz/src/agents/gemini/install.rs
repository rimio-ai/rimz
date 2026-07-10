//! Gemini user-settings hook installer.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    agent_config_path, read_optional_file,
};
use crate::store::atomic;

use super::{INSTALLED_EVENTS, RIMZ_HOOK_COMMAND};

const HOOKS_KEY: &str = "hooks";
const HOOK_TIMEOUT_MS: u64 = 10_000;
const RIMZ_COMMAND_MARKER: &str = "rimz hooks feed --source gemini";

pub(super) fn settings_path() -> Result<PathBuf> {
    agent_config_path(
        "gemini",
        "RIMZ_GEMINI_SETTINGS",
        Path::new(".gemini/settings.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, events) = install_candidate(path)?;
    write_json(path, &root)?;
    Ok(HookInstallReport {
        agent: "gemini",
        config_path: path.to_owned(),
        installed_events: events,
        merged: existed,
    })
}

pub(super) fn preview_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("gemini", path)?;
    let (root, events) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "gemini",
        config_path: path.to_owned(),
        planned_events: events,
        original_config,
        candidate_config: render_json(&root)?,
        merged: existed,
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    if !path.exists() {
        return Ok(HookUninstallReport {
            agent: "gemini",
            config_path: path.to_owned(),
            removed_events: Vec::new(),
            existed: false,
        });
    }
    let mut root = read_json(path)?;
    let removed_events = strip_owned_hooks(&mut root);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "gemini",
        config_path: path.to_owned(),
        removed_events,
        existed: true,
    })
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_json(path) else {
        return false;
    };
    if root
        .get("hooksConfig")
        .and_then(Value::as_object)
        .and_then(|config| config.get("enabled"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return false;
    }
    let disabled = root
        .get("hooksConfig")
        .and_then(Value::as_object)
        .and_then(|config| config.get("disabled"))
        .and_then(Value::as_array);
    let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
        return false;
    };
    INSTALLED_EVENTS.iter().all(|event| {
        if disabled.is_some_and(|disabled| {
            disabled
                .iter()
                .any(|name| name.as_str().is_some_and(|name| name == hook_name(event)))
        }) {
            return false;
        }
        hooks
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|entries| entries.iter().any(canonical_entry))
    })
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    read_json(path)
        .ok()
        .and_then(|root| root.get(HOOKS_KEY).and_then(Value::as_object).cloned())
        .is_some_and(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .is_some_and(|entries| entries.iter().any(owned_entry))
            })
        })
}

fn install_candidate(path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_json(path)?;
    strip_owned_hooks(&mut root);
    let hooks = root
        .entry(HOOKS_KEY.to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| AgentErr::Install {
            agent: "gemini",
            reason: "expected `hooks` to be a JSON object".to_owned(),
        })?;

    for event in INSTALLED_EVENTS {
        let entries = hooks
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| AgentErr::Install {
                agent: "gemini",
                reason: format!("expected `hooks.{event}` to be a JSON array"),
            })?;
        entries.push(json!({
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "name": hook_name(event),
                "command": RIMZ_HOOK_COMMAND,
                "timeout": HOOK_TIMEOUT_MS,
            }],
        }));
    }
    Ok((
        root,
        INSTALLED_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect(),
    ))
}

fn strip_owned_hooks(root: &mut Map<String, Value>) -> Vec<String> {
    let Some(hooks) = root.get_mut(HOOKS_KEY).and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for (event, entries) in hooks.iter_mut() {
        let Some(entries) = entries.as_array_mut() else {
            continue;
        };
        let before = entries.len();
        entries.retain(|entry| !owned_entry(entry));
        if entries.len() != before {
            removed.push(event.clone());
        }
    }
    hooks.retain(|_, entries| !entries.as_array().is_some_and(Vec::is_empty));
    removed.sort();
    removed
}

fn canonical_entry(entry: &Value) -> bool {
    entry.get("matcher").and_then(Value::as_str) == Some("*")
        && entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str) == Some(RIMZ_HOOK_COMMAND)
                        && hook.get("timeout").and_then(Value::as_u64) == Some(HOOK_TIMEOUT_MS)
                })
            })
}

fn owned_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER))
            })
        })
}

fn hook_name(event: &str) -> String {
    format!("rimz-{event}")
}

fn read_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(root)) => Ok(root),
            Ok(_) => Err(AgentErr::Install {
                agent: "gemini",
                reason: format!(
                    "expected JSON object at the top level of {}",
                    path.display()
                ),
            }),
            Err(source) => Err(AgentErr::InstallParse {
                agent: "gemini",
                path: path.to_owned(),
                source: Box::new(source),
            }),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "gemini",
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    let rendered = render_json(root)?;
    atomic::write_bytes_atomically(path, rendered.as_bytes())?;
    Ok(())
}

fn render_json(root: &Map<String, Value>) -> Result<String> {
    serde_json::to_string_pretty(&Value::Object(root.clone()))
        .map(|text| format!("{text}\n"))
        .map_err(|source| AgentErr::InstallSerialize {
            agent: "gemini",
            source: Box::new(source),
        })
}
