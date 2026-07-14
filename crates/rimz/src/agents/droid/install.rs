//! Merge installer for Droid's `~/.factory/settings.json` hook configuration.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    DROID_HOOK_TIMEOUT_SECS, HOOKS_KEY, INSTALLED_EVENTS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER,
    RIMZ_MANAGED_KEY,
};
use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, agent_config_path, read_optional_file,
};
use crate::store::atomic;

pub(super) fn droid_settings_path() -> Result<PathBuf> {
    agent_config_path(
        "droid",
        "RIMZ_DROID_SETTINGS",
        Path::new(".factory/settings.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed_events) = install_candidate(path)?;
    write_json(path, &root)?;
    Ok(HookInstallReport {
        agent: "droid",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        installed_events,
    })
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("droid", path)?;
    let (root, planned_events) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "droid",
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            original: original_config,
            candidate: render_json(&root)?,
            existed,
        }],
        planned_events,
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn install_candidate(path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_existing_json(path)?;
    let _ = strip_rimz_matchers(&mut root);
    for event in INSTALLED_EVENTS {
        upsert_rimz_matcher(&mut root, event);
    }
    Ok((
        root,
        INSTALLED_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect(),
    ))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    if !path.exists() {
        return Ok(HookUninstallReport {
            agent: "droid",
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed: false,
            }],
            removed_events: Vec::new(),
        });
    }
    let mut root = read_existing_json(path)?;
    let removed_events = strip_rimz_matchers(&mut root);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "droid",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed: true,
        }],
        removed_events,
    })
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
        return false;
    };
    INSTALLED_EVENTS.iter().all(|event| {
        hooks
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.as_object().is_some_and(canonical_entry_is_installed))
            })
    })
}

fn canonical_entry_is_installed(entry: &Map<String, Value>) -> bool {
    if !entry_is_rimz_owned(entry) || entry.contains_key("matcher") {
        return false;
    }
    let Some(commands) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    commands.len() == 1
        && commands[0].as_object().is_some_and(|command| {
            command.get("type").and_then(Value::as_str) == Some("command")
                && command.get("command").and_then(Value::as_str) == Some(RIMZ_HOOK_COMMAND)
                && command.get("timeout").and_then(Value::as_u64) == Some(DROID_HOOK_TIMEOUT_SECS)
        })
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    root.get(HOOKS_KEY)
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.values().any(|entries| {
                entries.as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.as_object().is_some_and(entry_is_rimz_owned))
                })
            })
        })
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: "droid",
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(root) => Ok(root),
                other => Err(AgentErr::Install {
                    agent: "droid",
                    reason: format!(
                        "expected JSON object at the top level of {}; found {}",
                        path.display(),
                        json_type_name(&other)
                    ),
                }),
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "droid",
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
            agent: "droid",
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}

fn upsert_rimz_matcher(root: &mut Map<String, Value>, event: &str) {
    let hooks = root
        .entry(HOOKS_KEY.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    // The branch above establishes the object shape.
    let hooks = hooks.as_object_mut().expect("hooks was set to an object");
    let entries = hooks
        .entry(event.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entries.is_array() {
        *entries = Value::Array(Vec::new());
    }
    // The branch above establishes the array shape.
    let entries = entries.as_array_mut().expect("entries was set to an array");
    entries.retain(|entry| {
        entry
            .as_object()
            .is_none_or(|entry| !entry_is_rimz_owned(entry))
    });
    entries.push(build_matcher_entry());
}

fn build_matcher_entry() -> Value {
    let mut command = Map::new();
    command.insert("type".to_owned(), Value::String("command".to_owned()));
    command.insert(
        "command".to_owned(),
        Value::String(RIMZ_HOOK_COMMAND.to_owned()),
    );
    command.insert(
        "timeout".to_owned(),
        Value::Number(DROID_HOOK_TIMEOUT_SECS.into()),
    );
    let mut entry = Map::new();
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    entry.insert(
        "hooks".to_owned(),
        Value::Array(vec![Value::Object(command)]),
    );
    Value::Object(entry)
}

fn entry_is_rimz_owned(entry: &Map<String, Value>) -> bool {
    if entry
        .get(RIMZ_MANAGED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let Some(commands) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    !commands.is_empty()
        && commands.iter().all(|command| {
            command
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
        })
}

fn strip_rimz_matchers(root: &mut Map<String, Value>) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(hooks) = root.get_mut(HOOKS_KEY).and_then(Value::as_object_mut) else {
        return removed;
    };
    let event_names: Vec<String> = hooks.keys().cloned().collect();
    for event in event_names {
        let Some(entries) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        entries.retain(|entry| {
            if entry.as_object().is_some_and(entry_is_rimz_owned) {
                removed.push(event.clone());
                false
            } else {
                true
            }
        });
        if entries.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        root.remove(HOOKS_KEY);
    }
    removed
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
