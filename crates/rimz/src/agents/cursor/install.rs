//! Cursor `hooks.json` merge installer.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    agent_config_path, read_optional_file,
};
use crate::store::atomic;

use super::{RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, WIRED_EVENTS};

pub(super) fn cursor_hooks_path() -> Result<PathBuf> {
    agent_config_path(
        "cursor",
        "RIMZ_CURSOR_HOOKS",
        Path::new(".cursor/hooks.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, events) = install_candidate(path)?;
    write_json(path, &root)?;
    Ok(HookInstallReport {
        agent: "cursor",
        config_path: path.to_path_buf(),
        installed_events: events,
        merged: existed,
        additional_config_paths: Vec::new(),
    })
}

pub(super) fn preview_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("cursor", path)?;
    let (root, events) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "cursor",
        config_path: path.to_path_buf(),
        planned_events: events,
        original_config,
        candidate_config: render_json(&root)?,
        merged: existed,
        status_line_change: None,
        subagent_status_line_change: None,
        additional_configs: Vec::new(),
    })
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    if !path.exists() {
        return Ok(HookUninstallReport {
            agent: "cursor",
            config_path: path.to_path_buf(),
            removed_events: Vec::new(),
            existed: false,
            additional_config_paths: Vec::new(),
        });
    }
    let mut root = read_existing_json(path)?;
    let removed_events = strip_owned(&mut root);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "cursor",
        config_path: path.to_path_buf(),
        removed_events,
        existed: true,
        additional_config_paths: Vec::new(),
    })
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    WIRED_EVENTS.iter().all(|event| {
        hooks
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|entries| entries.iter().any(entry_is_owned))
    })
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    root.get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.values().any(|entries| {
                entries
                    .as_array()
                    .is_some_and(|entries| entries.iter().any(entry_is_owned))
            })
        })
}

fn install_candidate(path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_existing_json(path)?;
    let _ = strip_owned(&mut root);
    root.insert("version".to_owned(), json!(1));
    let hooks = root
        .entry("hooks".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or_else(|| AgentErr::Install {
        agent: "cursor",
        reason: format!("expected `hooks` to be an object in {}", path.display()),
    })?;
    for event in WIRED_EVENTS {
        let entries = hooks
            .entry((*event).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        let entries = entries.as_array_mut().ok_or_else(|| AgentErr::Install {
            agent: "cursor",
            reason: format!(
                "expected `hooks.{event}` to be an array in {}",
                path.display()
            ),
        })?;
        entries.push(json!({ "command": RIMZ_HOOK_COMMAND }));
    }
    Ok((
        root,
        WIRED_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect(),
    ))
}

fn strip_owned(root: &mut Map<String, Value>) -> Vec<String> {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    hooks.retain(|event, entries| {
        let Some(entries) = entries.as_array_mut() else {
            return true;
        };
        let before = entries.len();
        entries.retain(|entry| !entry_is_owned(entry));
        if entries.len() != before {
            removed.push(event.clone());
        }
        !entries.is_empty()
    });
    removed
}

fn entry_is_owned(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
}

fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: "cursor",
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            value.as_object().cloned().ok_or_else(|| AgentErr::Install {
                agent: "cursor",
                reason: format!("expected a JSON object at {}", path.display()),
            })
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "cursor",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    atomic::write_bytes_atomically(path, render_json(root)?.as_bytes())?;
    Ok(())
}

fn render_json(root: &Map<String, Value>) -> Result<String> {
    let text = serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
        AgentErr::InstallSerialize {
            agent: "cursor",
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}
