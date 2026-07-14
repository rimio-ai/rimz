//! Qwen settings installer for `settings.json`.
//!
//! This module owns managed hook merge/uninstall, hook ownership predicates, blocking-hook sync validation, and session statusline wrapping.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, StatusLineChange, agent_config_path, read_optional_file,
};
use crate::store::atomic;

use super::{
    BLOCKING_EVENTS, HOOKS_KEY, INSTALLED_EVENTS, QWEN_HOOK_TIMEOUT_MS, RIMZ_HOOK_COMMAND,
    RIMZ_HOOK_MARKER, RIMZ_MANAGED_KEY, RIMZ_STATUS_LINE_MARKER, RIMZ_WRAPPED_KEY,
    STATUS_LINE_COMMAND,
};

pub(super) fn qwen_settings_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_QWEN_SETTINGS`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    if std::env::var_os("RIMZ_QWEN_SETTINGS").is_some() {
        return agent_config_path(
            "qwen",
            "RIMZ_QWEN_SETTINGS",
            Path::new(".qwen/settings.json"),
        );
    }
    if let Some(home) = std::env::var_os("QWEN_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("settings.json"));
    }
    agent_config_path(
        "qwen",
        "RIMZ_QWEN_SETTINGS",
        Path::new(".qwen/settings.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path)?;
    write_json(path, &root)?;

    Ok(HookInstallReport {
        agent: "qwen",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        installed_events: installed,
    })
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("qwen", path)?;
    let existing = read_existing_json(path)?;
    let status_line_change = classify_status_line_change(&existing);
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "qwen",
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            original: original_config,
            candidate: render_json(&root)?,
            existed,
        }],
        planned_events: installed,
        status_line_change,
        subagent_status_line_change: None,
    })
}

fn install_candidate(path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
    let mut root = read_existing_json(path)?;

    // Defensive: a tampered or stale Rimz write may carry a `_rimz_sync =
    // false` marker on a blocking event. Refuse — installing a blocking
    // hook as non-sync is a hard error.
    reject_async_blocking_in_existing(&root)?;

    // Strip any existing Rimz-managed matchers before writing the fresh
    // set. Mirrors codex's "single source of truth = installer constants".
    let _ = strip_rimz_matchers(&mut root);

    let mut installed = Vec::new();
    for &(event, matcher) in INSTALLED_EVENTS {
        upsert_rimz_matcher(&mut root, event, matcher);
        installed.push(event_label(event, matcher));
    }

    // Wrap the session render command so Rimz captures Qwen's rich per-render
    // JSON. Idempotent by construction: a prior Rimz-managed value carries the user's original
    // under `_rimz_wrapped`, which the upsert reads back rather than re-wrapping.
    upsert_rimz_status_line(&mut root);

    Ok((root, installed))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    if !existed {
        return Ok(HookUninstallReport {
            agent: "qwen",
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed: false,
            }],
            removed_events: Vec::new(),
        });
    }
    let mut root = read_existing_json(path)?;
    let removed = strip_rimz_matchers(&mut root);
    // Restore the render command (or drop the field if Rimz added it).
    strip_rimz_status_line(&mut root);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "qwen",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed: true,
        }],
        removed_events: removed,
    })
}

/// Whether `path` carries a usable Rimz-owned hook entry for every canonical
/// event. Best-effort: a missing file or parse error reads as "not installed".
/// Uses [`entry_is_rimz_owned`] (the same ownership predicate as
/// install/uninstall) so entries whose `_rimz_managed` marker was stripped by an
/// external tool but whose command is still the rimz feed command are still
/// detected. Blocking entries marked async are not usable.
pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
        return false;
    };
    let hooks_complete = INSTALLED_EVENTS.iter().all(|(event, matcher)| {
        hooks
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .as_object()
                        .is_some_and(|obj| canonical_entry_is_installed(obj, event, *matcher))
                })
            })
    });
    hooks_complete && status_line_install_satisfied(&root)
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    let has_hook_artifact = root
        .get(HOOKS_KEY)
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.values().any(|entries| {
                entries.as_array().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.as_object().is_some_and(entry_is_rimz_owned))
                })
            })
        });
    has_hook_artifact || status_line_is_rimz_managed(&root)
}

fn canonical_entry_is_installed(
    obj: &Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
) -> bool {
    let actual_matcher = obj.get("matcher").and_then(Value::as_str);
    matcher_matches(matcher, actual_matcher)
        && entry_is_rimz_owned(obj)
        && blocking_entry_is_sync(obj, event, matcher)
}

fn blocking_entry_is_sync(obj: &Map<String, Value>, event: &str, matcher: Option<&str>) -> bool {
    let blocking = BLOCKING_EVENTS
        .iter()
        .any(|&(e, m)| e == event && matcher_matches(m, matcher));
    if !blocking {
        return true;
    }
    obj.get(HOOKS_KEY)
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .all(|hook| hook.get("async").and_then(Value::as_bool) != Some(true))
        })
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: "qwen",
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(map) => Ok(map),
                other => Err(AgentErr::Install {
                    agent: "qwen",
                    reason: format!(
                        "expected JSON object at the top level of {}; found {}",
                        path.display(),
                        json_type_name(&other),
                    ),
                }),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "qwen",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json(path: &Path, root: &Map<String, Value>) -> Result<()> {
    let text = render_json(root)?;
    atomic::write_bytes_atomically(path, text.as_bytes())?;
    Ok(())
}

fn render_json(root: &Map<String, Value>) -> Result<String> {
    let text = serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
        AgentErr::InstallSerialize {
            agent: "qwen",
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Walk the `hooks.<Event>` arrays and reject any matcher tagged
/// `_rimz_managed = true` that targets a blocking event but carries
/// `async: true`. The set of "must block" events is owned by
/// [`BLOCKING_EVENTS`] — not the on-disk file.
fn reject_async_blocking_in_existing(root: &Map<String, Value>) -> Result<()> {
    let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
        return Ok(());
    };
    for &(event, expected_matcher) in BLOCKING_EVENTS {
        let Some(entries) = hooks.get(event).and_then(Value::as_array) else {
            continue;
        };
        for entry in entries {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            if !is_rimz_managed_object(obj) {
                continue;
            }
            // Match on the same matcher we'd install — entries that don't
            // line up with our (event, matcher) tuple aren't ours even if
            // tagged.
            let actual_matcher = obj.get("matcher").and_then(Value::as_str);
            let async_handler = obj
                .get(HOOKS_KEY)
                .and_then(Value::as_array)
                .is_some_and(|hooks| {
                    hooks
                        .iter()
                        .any(|hook| hook.get("async").and_then(Value::as_bool) == Some(true))
                });
            let matches_owned_blocking_entry = matcher_matches(expected_matcher, actual_matcher)
                || (event == "PreToolUse" && actual_matcher.is_none());
            if matches_owned_blocking_entry && async_handler {
                return Err(AgentErr::Install {
                    agent: "qwen",
                    reason: format!(
                        "existing config marks blocking hook `{}` as async; refusing to install",
                        event_label(event, expected_matcher)
                    ),
                });
            }
        }
    }
    Ok(())
}

fn matcher_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (expected, actual) {
        (None, None) => true,
        (None, Some("")) => true,
        (Some(e), Some(a)) => e == a,
        _ => false,
    }
}

/// Insert or replace the rimz-managed matcher for `(event, matcher)`. Any
/// existing matcher tagged `_rimz_managed = true` with the same matcher value
/// is replaced; user-managed matchers are left untouched.
fn upsert_rimz_matcher(root: &mut Map<String, Value>, event: &str, matcher: Option<&str>) {
    let hooks = root
        .entry(HOOKS_KEY.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks_obj = match hooks {
        Value::Object(map) => map,
        _ => {
            // User had a non-object `hooks` value (unusual). Replace it.
            *hooks = Value::Object(Map::new());
            hooks.as_object_mut().expect("just inserted an object")
        }
    };
    let entries = hooks_obj
        .entry(event.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let entries_arr = match entries {
        Value::Array(arr) => arr,
        _ => {
            *entries = Value::Array(Vec::new());
            entries.as_array_mut().expect("just inserted an array")
        }
    };
    // Drop any pre-existing rimz-managed entry for the same matcher.
    entries_arr.retain(|entry| {
        let Some(obj) = entry.as_object() else {
            return true;
        };
        if !is_rimz_managed_object(obj) {
            return true;
        }
        let actual = obj.get("matcher").and_then(Value::as_str);
        !matcher_matches(matcher, actual)
    });
    entries_arr.push(build_matcher_entry(event, matcher));
}

fn build_matcher_entry(_event: &str, matcher: Option<&str>) -> Value {
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".to_owned(), Value::String(m.to_owned()));
    }
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    // No `--event`: the helper reads `hook_event_name` from the hook's stdin
    // payload, so every installed command is identical (`RIMZ_HOOK_COMMAND`).
    let command = RIMZ_HOOK_COMMAND.to_owned();
    let mut hook = Map::new();
    hook.insert("type".to_owned(), Value::String("command".to_owned()));
    hook.insert("command".to_owned(), Value::String(command));
    hook.insert(
        "timeout".to_owned(),
        Value::Number(QWEN_HOOK_TIMEOUT_MS.into()),
    );
    entry.insert("hooks".to_owned(), Value::Array(vec![Value::Object(hook)]));
    Value::Object(entry)
}

/// Whether a `hooks.<Event>` entry belongs to Rimz: either tagged
/// `_rimz_managed = true`, or its handlers are *solely* the rimz feed command in
/// any historical form (with `--event`, without `exec`). The "solely" guard
/// leaves a user entry that merely embeds a rimz command alongside their own
/// untouched. The command-substring arm reclaims legacy and unmarked entries
/// older builds left behind, so reinstall never stacks duplicates.
fn entry_is_rimz_owned(obj: &Map<String, Value>) -> bool {
    if is_rimz_managed_object(obj) {
        return true;
    }
    let Some(handlers) = obj.get(HOOKS_KEY).and_then(Value::as_array) else {
        return false;
    };
    !handlers.is_empty()
        && handlers.iter().all(|handler| {
            handler
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
        })
}

/// Remove every rimz-owned matcher across the hook tree (see
/// [`entry_is_rimz_owned`]). Returns the labels of removed entries (`Event` or
/// `Event:matcher`).
fn strip_rimz_matchers(root: &mut Map<String, Value>) -> Vec<String> {
    let mut removed = Vec::new();
    let Some(hooks_value) = root.get_mut(HOOKS_KEY) else {
        return removed;
    };
    let Some(hooks_obj) = hooks_value.as_object_mut() else {
        return removed;
    };
    let event_names: Vec<String> = hooks_obj.keys().cloned().collect();
    for event in event_names {
        let Some(entries) = hooks_obj.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        entries.retain(|entry| {
            let Some(obj) = entry.as_object() else {
                return true;
            };
            if entry_is_rimz_owned(obj) {
                let matcher = obj.get("matcher").and_then(Value::as_str);
                removed.push(event_label(&event, matcher));
                false
            } else {
                true
            }
        });
        if entries.is_empty() {
            hooks_obj.remove(&event);
        }
    }
    if hooks_obj.is_empty() {
        root.remove(HOOKS_KEY);
    }
    removed
}

fn event_label(event: &str, matcher: Option<&str>) -> String {
    match matcher {
        Some(m) if !m.is_empty() => format!("{event}:{m}"),
        _ => event.to_owned(),
    }
}

fn is_rimz_managed_object(obj: &Map<String, Value>) -> bool {
    obj.get(RIMZ_MANAGED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn status_line(root: &Map<String, Value>) -> Option<&Value> {
    root.get("ui")?.as_object()?.get("statusLine")
}

fn status_line_is_rimz_managed(root: &Map<String, Value>) -> bool {
    matches!(status_line(root), Some(Value::Object(obj)) if is_rimz_managed_object(obj))
}

fn status_line_install_satisfied(root: &Map<String, Value>) -> bool {
    if root.get("ui").is_some_and(|ui| !ui.is_object()) {
        return true;
    }
    match status_line(root) {
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => obj
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(is_rimz_status_line_command),
        Some(Value::Object(obj)) => obj.get("type").and_then(Value::as_str) != Some("command"),
        Some(_) => true,
        None => false,
    }
}

fn ui_object_mut(root: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let ui = root
        .entry("ui".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !ui.is_object() {
        *ui = Value::Object(Map::new());
    }
    match ui {
        Value::Object(object) => object,
        // The branch above replaces every non-object value.
        _ => unreachable!("ui was replaced with an object"),
    }
}

/// Wrap an absent or command-mode `ui.statusLine`; preset modes stay untouched.
pub(super) fn upsert_rimz_status_line(root: &mut Map<String, Value>) {
    if root.get("ui").is_some_and(|ui| !ui.is_object()) {
        return;
    }
    let existing = status_line(root).cloned();
    let wrap_allowed = match existing.as_ref() {
        None => true,
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => true,
        Some(Value::Object(obj)) => obj.get("type").and_then(Value::as_str) == Some("command"),
        Some(_) => false,
    };
    if !wrap_allowed {
        return;
    }
    let original = match &existing {
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => obj
            .get(RIMZ_WRAPPED_KEY)
            .cloned()
            .and_then(non_recursive_status_line_value),
        Some(other) => non_recursive_status_line_value(other.clone()),
        None => None,
    };
    let mut entry = Map::new();
    // Carry rendering options forward (everything but the command we're
    // replacing and our own markers). Prefer the currently effective object so
    // a repaired managed statusline keeps its visual settings even when its
    // wrapped command is discarded as recursive.
    if let Some(Value::Object(orig)) = existing.as_ref().or(original.as_ref()) {
        for (key, value) in orig {
            if key == "command" || key == RIMZ_MANAGED_KEY || key == RIMZ_WRAPPED_KEY {
                continue;
            }
            entry.insert(key.clone(), value.clone());
        }
    }
    entry.insert("type".to_owned(), Value::String("command".to_owned()));
    entry.insert(
        "command".to_owned(),
        Value::String(STATUS_LINE_COMMAND.to_owned()),
    );
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        entry.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    }
    ui_object_mut(root).insert("statusLine".to_owned(), Value::Object(entry));
}

/// Restore the user's original command under `ui.statusLine`. When the current one is
/// Rimz-managed, replace it with the captured `_rimz_wrapped` value, or remove
/// the key entirely when nothing was wrapped. A non-Rimz value is left
/// untouched. Returns whether a Rimz-managed value was found.
fn strip_rimz_status_line(root: &mut Map<String, Value>) -> bool {
    let managed = status_line_is_rimz_managed(root);
    if !managed {
        return false;
    }
    let original = match root
        .get_mut("ui")
        .and_then(Value::as_object_mut)
        .and_then(|ui| ui.remove("statusLine"))
    {
        Some(Value::Object(mut obj)) => obj
            .remove(RIMZ_WRAPPED_KEY)
            .and_then(non_recursive_status_line_value),
        _ => None,
    };
    if let Some(original) = original {
        ui_object_mut(root).insert("statusLine".to_owned(), original);
    } else if root
        .get("ui")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        root.remove("ui");
    }
    true
}

/// Classify how an install would change `ui.statusLine`, for the consent summary.
pub(super) fn classify_status_line_change(root: &Map<String, Value>) -> Option<StatusLineChange> {
    if root.get("ui").is_some_and(|ui| !ui.is_object()) {
        return None;
    }
    match status_line(root) {
        None => Some(StatusLineChange::Added),
        Some(Value::Object(obj)) if is_rimz_managed_object(obj) => {
            Some(StatusLineChange::Unchanged)
        }
        Some(Value::Object(obj)) if obj.get("type").and_then(Value::as_str) == Some("command") => {
            Some(StatusLineChange::Wrapping {
                original: status_line_display(&Value::Object(obj.clone())),
            })
        }
        Some(_) => None,
    }
}

fn status_line_display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(obj) => obj
            .get("command")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    }
}

pub(super) fn wrapped_status_line_command_from(root: &Map<String, Value>) -> Option<String> {
    let Some(Value::Object(obj)) = status_line(root) else {
        return None;
    };
    if !is_rimz_managed_object(obj) {
        return None;
    }
    extract_status_line_command(obj.get(RIMZ_WRAPPED_KEY)?)
}

fn extract_status_line_command(value: &Value) -> Option<String> {
    status_line_command(value)
        .filter(|command| !is_rimz_status_line_command(command))
        .map(ToOwned::to_owned)
}

fn non_recursive_status_line_value(value: Value) -> Option<Value> {
    if status_line_command(&value).is_some_and(is_rimz_status_line_command) {
        None
    } else {
        Some(value)
    }
}

fn status_line_command(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Object(obj) => obj
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty()),
        _ => None,
    }
}

fn is_rimz_status_line_command(command: &str) -> bool {
    command.contains(RIMZ_STATUS_LINE_MARKER)
}
