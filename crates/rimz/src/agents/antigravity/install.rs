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
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview,
    HookInstallReport, HookUninstallReport, Result, StatusLineChange, agent_config_path,
    read_optional_file,
};
use crate::store::atomic;

use super::{
    HOOK_TIMEOUT_SECS, INSTALLED_EVENT_LABELS, POST_TOOL_EDIT_MATCHER, POST_TOOL_MUTATING_MATCHER,
    POST_TOOL_OBSERVED_MATCHER, RIMZ_HOOK_MARKER, RIMZ_STATUS_LINE_MARKER, STATUS_LINE_COMMAND,
};

const AGENT: &str = "antigravity";
const RIMZ_HOOK_NAME: &str = "rimz";
const STATUS_LINE_KEY: &str = "statusLine";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";

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
    let hooks_existed = hooks_path.exists();
    let settings_original = read_optional_file(AGENT, settings_path)?;
    let settings_existed = settings_original.is_some();
    let hooks = hook_candidate(hooks_path)?;
    let settings = statusline_candidate(settings_path)?.0;
    write_json(settings_path, &settings)?;
    if let Err(error) = write_json(hooks_path, &hooks) {
        // Keep a failed two-file install from silently leaving only the
        // statusline command active. Best-effort restoration uses the same
        // atomic writer; the original install error remains the diagnosis.
        let _ = restore_file(settings_path, settings_original.as_deref());
        return Err(error);
    }
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

fn restore_file(path: &Path, original: Option<&str>) -> Result<()> {
    match original {
        Some(original) => atomic::write_bytes_atomically(path, original.as_bytes())?,
        None if path.exists() => {
            std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
                agent: AGENT,
                path: path.to_path_buf(),
                source,
            })?;
        }
        None => {}
    }
    Ok(())
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
                candidate: render_json(&hooks)?,
            },
            HookInstallFilePreview {
                path: settings_path.to_path_buf(),
                existed: settings_original.is_some(),
                original: settings_original,
                candidate: render_json(&settings)?,
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
        let mut root = read_json_object(hooks_path)?;
        if strip_owned_hooks(&mut root) {
            removed_events = installed_event_names();
            write_json(hooks_path, &root)?;
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
    let Ok(root) = read_json_object(hooks_path) else {
        return false;
    };
    let Some(rimz) = root.get(RIMZ_HOOK_NAME).and_then(Value::as_object) else {
        return false;
    };
    let complete = canonical_handlers()
        .into_iter()
        .all(|(event, matcher, command)| {
            rimz.get(event)
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| handler_matches(entry, matcher, command))
                })
        });
    complete && statusline_managed_at(settings_path)
}

pub(super) fn managed(hooks_path: &Path, settings_path: &Path) -> bool {
    read_json_object(hooks_path).is_ok_and(|root| value_has_owned_hook(&Value::Object(root)))
        || statusline_managed_at(settings_path)
}

pub(super) fn wrapped_statusline_command(settings_path: &Path) -> Option<String> {
    let root = read_json_object(settings_path).ok()?;
    let statusline = root.get(STATUS_LINE_KEY)?.as_object()?;
    statusline
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
        .then_some(())?;
    statusline
        .get(RIMZ_WRAPPED_KEY)
        .and_then(Value::as_object)
        .and_then(|wrapped| wrapped.get("command"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn installed_event_names() -> Vec<String> {
    INSTALLED_EVENT_LABELS
        .iter()
        .map(|event| (*event).to_owned())
        .collect()
}

fn hook_candidate(path: &Path) -> Result<Map<String, Value>> {
    let mut root = read_json_object(path)?;
    strip_owned_hooks(&mut root);
    if root.contains_key(RIMZ_HOOK_NAME) {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: format!(
                "the hook name `{RIMZ_HOOK_NAME}` in {} is user-owned; rename that hook before installing Rimz",
                path.display()
            ),
        });
    }

    let mut rimz = Map::new();
    for (event, matcher, command) in canonical_handlers() {
        let entry = match matcher {
            Some(matcher) => json!({
                "matcher": matcher,
                "hooks": [command_handler(command)],
            }),
            None => command_handler(command),
        };
        rimz.entry(event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("fresh event array")
            .push(entry);
    }
    root.insert(RIMZ_HOOK_NAME.to_owned(), Value::Object(rimz));
    Ok(root)
}

fn canonical_handlers() -> Vec<(&'static str, Option<&'static str>, &'static str)> {
    vec![
        ("PreInvocation", None, super::PRE_INVOCATION_COMMAND),
        (
            "PostToolUse",
            Some(POST_TOOL_EDIT_MATCHER),
            super::POST_TOOL_EDIT_COMMAND,
        ),
        (
            "PostToolUse",
            Some(POST_TOOL_MUTATING_MATCHER),
            super::POST_TOOL_MUTATING_COMMAND,
        ),
        (
            "PostToolUse",
            Some(POST_TOOL_OBSERVED_MATCHER),
            super::POST_TOOL_OBSERVED_COMMAND,
        ),
        ("PostInvocation", None, super::POST_INVOCATION_COMMAND),
        ("Stop", None, super::STOP_COMMAND),
    ]
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
    let mut root = read_json_object(path)?;
    let existing = root.remove(STATUS_LINE_KEY);
    let (original, change) = match existing {
        Some(Value::Object(mut object))
            if object
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER)) =>
        {
            (object.remove(RIMZ_WRAPPED_KEY), StatusLineChange::Unchanged)
        }
        Some(Value::Object(object)) => {
            let change = object
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty())
                .map(|command| StatusLineChange::Wrapping {
                    original: command.to_owned(),
                })
                .unwrap_or(StatusLineChange::Added);
            (Some(Value::Object(object)), change)
        }
        Some(other) => {
            return Err(AgentErr::Install {
                agent: AGENT,
                reason: format!(
                    "expected `{STATUS_LINE_KEY}` in {} to be a JSON object; found {}",
                    path.display(),
                    json_type_name(&other)
                ),
            });
        }
        None => (None, StatusLineChange::Added),
    };

    let mut managed = Map::new();
    managed.insert("type".to_owned(), Value::String("command".to_owned()));
    managed.insert(
        "command".to_owned(),
        Value::String(STATUS_LINE_COMMAND.to_owned()),
    );
    managed.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        if let Some(stack) = original.get("stack_with_default").cloned() {
            managed.insert("stack_with_default".to_owned(), stack);
        }
        managed.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    } else {
        managed.insert("stack_with_default".to_owned(), Value::Bool(true));
    }
    root.insert(STATUS_LINE_KEY.to_owned(), Value::Object(managed));
    Ok((root, change))
}

fn statusline_managed_at(path: &Path) -> bool {
    read_json_object(path).is_ok_and(|root| {
        root.get(STATUS_LINE_KEY)
            .and_then(Value::as_object)
            .and_then(|statusline| statusline.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
    })
}

fn uninstall_statusline_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut root = read_json_object(path)?;
    let Some(Value::Object(mut statusline)) = root.remove(STATUS_LINE_KEY) else {
        return Ok(());
    };
    if !statusline
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
    {
        root.insert(STATUS_LINE_KEY.to_owned(), Value::Object(statusline));
        return Ok(());
    }
    if let Some(original) = statusline.remove(RIMZ_WRAPPED_KEY) {
        root.insert(STATUS_LINE_KEY.to_owned(), original);
    }
    write_json(path, &root)
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: AGENT,
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(root) => Ok(root),
                other => Err(AgentErr::Install {
                    agent: AGENT,
                    reason: format!(
                        "expected JSON object at the top level of {}; found {}",
                        path.display(),
                        json_type_name(&other)
                    ),
                }),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: AGENT,
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn render_json(root: &Map<String, Value>) -> Result<String> {
    let mut rendered = serde_json::to_string_pretty(root).map_err(|source| AgentErr::Install {
        agent: AGENT,
        reason: format!("failed to render hook config: {source}"),
    })?;
    rendered.push('\n');
    Ok(rendered)
}

fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    atomic::write_bytes_atomically(path, render_json(root)?.as_bytes())?;
    Ok(())
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
