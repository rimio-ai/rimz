//! Antigravity hook and statusline installer.
//!
//! Antigravity splits its observer surfaces across two JSON files. The hook
//! file receives only events with a documented observer-neutral response;
//! `PreToolUse` stays untouched because every documented result changes native
//! permission policy. The settings file wraps a pre-existing statusline and
//! restores its complete value on uninstall.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, StatusLineChange, agent_config_path, read_optional_file,
    settings_json::{self, PendingWrite},
};

use super::{ANTIGRAVITY_HOOKS, HOOK_TIMEOUT_SECS, RIMZ_HOOK_MARKER, STATUS_LINE};

const AGENT: &str = "antigravity";
const RIMZ_HOOK_NAME: &str = "rimz";

pub(super) fn hooks_path() -> Result<PathBuf> {
    agent_config_path(
        AGENT,
        "RIMZ_ANTIGRAVITY_HOOKS",
        Path::new(".gemini/config/hooks.json"),
    )
}

pub(super) fn settings_path() -> Result<PathBuf> {
    agent_config_path(
        AGENT,
        "RIMZ_ANTIGRAVITY_SETTINGS",
        Path::new(".gemini/antigravity-cli/settings.json"),
    )
}

pub(super) fn install(hooks_path: &Path, settings_path: &Path) -> Result<HookInstallReport> {
    let hooks_original = settings_json::read_optional_bytes(AGENT, hooks_path)?;
    let settings_original = settings_json::read_optional_bytes(AGENT, settings_path)?;
    let hooks_existed = hooks_original.is_some();
    let settings_existed = settings_original.is_some();
    let hooks = hook_candidate(hooks_path)?;
    let settings = statusline_candidate(settings_path)?.0;
    let settings_candidate = settings_json::render_json(AGENT, &settings)?;
    let hooks_candidate = settings_json::render_json(AGENT, &hooks)?;
    settings_json::commit_pair(
        AGENT,
        PendingWrite::required(settings_path, &settings_candidate),
        PendingWrite::required(hooks_path, &hooks_candidate),
        settings_original.as_deref(),
        hooks_original.as_deref(),
    )?;
    Ok(HookInstallReport {
        agent: AGENT,
        files: vec![
            HookInstallFileReport {
                path: hooks_path.to_path_buf(),
                existed: hooks_existed,
            },
            HookInstallFileReport {
                path: settings_path.to_path_buf(),
                existed: settings_existed,
            },
        ],
        installed_events: installed_event_names(),
    })
}

pub(super) fn preview(hooks_path: &Path, settings_path: &Path) -> Result<HookInstallPreview> {
    let hooks_original = read_optional_file(AGENT, hooks_path)?;
    let settings_original = read_optional_file(AGENT, settings_path)?;
    let hooks = hook_candidate(hooks_path)?;
    let (settings, status_line_change) = statusline_candidate(settings_path)?;
    Ok(HookInstallPreview {
        agent: AGENT,
        files: vec![
            HookInstallFilePreview {
                path: hooks_path.to_path_buf(),
                existed: hooks_original.is_some(),
                original: hooks_original,
                candidate: settings_json::render_json(AGENT, &hooks)?,
            },
            HookInstallFilePreview {
                path: settings_path.to_path_buf(),
                existed: settings_original.is_some(),
                original: settings_original,
                candidate: settings_json::render_json(AGENT, &settings)?,
            },
        ],
        planned_events: installed_event_names(),
        status_line_change: Some(status_line_change),
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall(hooks_path: &Path, settings_path: &Path) -> Result<HookUninstallReport> {
    let existed = hooks_path.exists();
    let settings_existed = settings_path.exists();
    let mut removed_events = Vec::new();
    if existed {
        let mut root = settings_json::read_json_object(AGENT, hooks_path)?;
        if strip_owned_hooks(&mut root) {
            removed_events = installed_event_names();
            settings_json::write_json(AGENT, hooks_path, &root)?;
        }
    }
    uninstall_statusline_file(settings_path)?;
    Ok(HookUninstallReport {
        agent: AGENT,
        files: vec![
            HookInstallFileReport {
                path: hooks_path.to_path_buf(),
                existed,
            },
            HookInstallFileReport {
                path: settings_path.to_path_buf(),
                existed: settings_existed,
            },
        ],
        removed_events,
    })
}

pub(super) fn installed(hooks_path: &Path, settings_path: &Path) -> bool {
    let Ok(root) = settings_json::read_json_object(AGENT, hooks_path) else {
        return false;
    };
    let Some(rimz) = root.get(RIMZ_HOOK_NAME).and_then(Value::as_object) else {
        return false;
    };
    let complete = ANTIGRAVITY_HOOKS.iter().all(|entry| {
        rimz.get(entry.config_event)
            .and_then(Value::as_array)
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|handler| handler_matches(handler, entry.config_matcher, entry.command))
            })
    });
    complete && statusline_managed_at(settings_path)
}

pub(super) fn managed(hooks_path: &Path, settings_path: &Path) -> bool {
    settings_json::read_json_object(AGENT, hooks_path)
        .is_ok_and(|root| value_has_owned_hook(&Value::Object(root)))
        || statusline_managed_at(settings_path)
}

pub(super) fn wrapped_statusline_command(settings_path: &Path) -> Option<String> {
    let root = settings_json::read_json_object(AGENT, settings_path).ok()?;
    super::super::managed_statusline::wrapped_command(&root, &STATUS_LINE)
}

fn installed_event_names() -> Vec<String> {
    ANTIGRAVITY_HOOKS
        .iter()
        .map(|entry| entry.hook.event.to_owned())
        .collect()
}

fn hook_candidate(path: &Path) -> Result<Map<String, Value>> {
    let mut root = settings_json::read_json_object(AGENT, path)?;
    strip_owned_hooks(&mut root);
    if root.contains_key(RIMZ_HOOK_NAME) {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "the hook name `{RIMZ_HOOK_NAME}` in {} is user-owned; rename that hook before installing RimZ",
                path.display()
            ),
        });
    }

    let mut rimz = Map::new();
    for hook in &ANTIGRAVITY_HOOKS {
        let entry = match hook.config_matcher {
            Some(matcher) => json!({
                "matcher": matcher,
                "hooks": [command_handler(hook.command)],
            }),
            None => command_handler(hook.command),
        };
        rimz.entry(hook.config_event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("fresh event array")
            .push(entry);
    }
    root.insert(RIMZ_HOOK_NAME.to_owned(), Value::Object(rimz));
    Ok(root)
}

fn command_handler(command: &str) -> Value {
    json!({
        "type": "command",
        "command": command,
        "timeout": HOOK_TIMEOUT_SECS,
    })
}

fn handler_matches(entry: &Value, matcher: Option<&str>, command: &str) -> bool {
    match matcher {
        None => command_matches(entry, command),
        Some(matcher) => {
            entry.get("matcher").and_then(Value::as_str) == Some(matcher)
                && entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| hooks.iter().any(|hook| command_matches(hook, command)))
        }
    }
}

fn command_matches(handler: &Value, command: &str) -> bool {
    handler.get("type").and_then(Value::as_str) == Some("command")
        && handler.get("command").and_then(Value::as_str) == Some(command)
        && handler.get("timeout").and_then(Value::as_u64) == Some(HOOK_TIMEOUT_SECS)
}

fn value_has_owned_hook(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            (key == "command"
                && value
                    .as_str()
                    .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER)))
                || value_has_owned_hook(value)
        }),
        Value::Array(values) => values.iter().any(value_has_owned_hook),
        _ => false,
    }
}

fn strip_owned_hooks(root: &mut Map<String, Value>) -> bool {
    let before = value_has_owned_hook(&Value::Object(root.clone()));
    root.retain(|_, hook| {
        let Some(events) = hook.as_object_mut() else {
            return true;
        };
        events.retain(|_, entries| {
            let Some(entries) = entries.as_array_mut() else {
                return true;
            };
            entries.retain_mut(|entry| {
                let Some(object) = entry.as_object_mut() else {
                    return true;
                };
                if object
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
                {
                    return false;
                }
                if let Some(hooks) = object.get_mut("hooks").and_then(Value::as_array_mut) {
                    hooks.retain(|handler| {
                        !handler
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
                    });
                    if hooks.is_empty() {
                        return false;
                    }
                }
                true
            });
            !entries.is_empty()
        });
        !events.is_empty()
    });
    before
}

fn statusline_candidate(path: &Path) -> Result<(Map<String, Value>, StatusLineChange)> {
    let mut root = settings_json::read_json_object(AGENT, path)?;
    let fresh = !root.contains_key(STATUS_LINE.key_path[0]);
    let change = super::super::managed_statusline::classify(&root, &STATUS_LINE)
        .ok_or_else(|| incompatible_statusline(path, &root))?;
    super::super::managed_statusline::upsert(&mut root, &STATUS_LINE);
    if fresh
        && let Some(statusline) = root
            .get_mut(STATUS_LINE.key_path[0])
            .and_then(Value::as_object_mut)
    {
        statusline.insert("stack_with_default".to_owned(), Value::Bool(true));
    }
    Ok((root, change))
}

fn incompatible_statusline(path: &Path, root: &Map<String, Value>) -> AgentErr {
    let found = root
        .get(STATUS_LINE.key_path[0])
        .map(settings_json::json_type_name)
        .unwrap_or("missing");
    AgentErr::Install {
        agent: AGENT,
        reason: format!(
            "expected `{}` in {} to be a JSON object; found {found}",
            STATUS_LINE.key_path[0],
            path.display(),
        ),
    }
}

fn statusline_managed_at(path: &Path) -> bool {
    settings_json::read_json_object(AGENT, path)
        .is_ok_and(|root| super::super::managed_statusline::is_managed(&root, &STATUS_LINE))
}

fn uninstall_statusline_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut root = settings_json::read_json_object(AGENT, path)?;
    if super::super::managed_statusline::strip(&mut root, &STATUS_LINE) {
        settings_json::write_json(AGENT, path, &root)?;
    }
    Ok(())
}
