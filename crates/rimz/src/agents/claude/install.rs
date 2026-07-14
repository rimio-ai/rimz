//! Claude settings installer for `settings.json`.
//!
//! This module owns managed hook merge/uninstall, hook ownership predicates, blocking-hook sync validation, and session/subagent statusline wrapping.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, StatusLineChange, agent_config_path, read_optional_file,
};
use crate::store::atomic;

use super::{
    BLOCKING_EVENTS, CLAUDE_HOOK_TIMEOUT_SECS, HOOKS_KEY, INSTALLED_EVENTS, RIMZ_HOOK_COMMAND,
    RIMZ_HOOK_MARKER, RIMZ_MANAGED_KEY, RIMZ_SYNC_KEY, STATUS_LINE, SUBAGENT_STATUS_LINE,
};
use crate::agents::managed_statusline::ManagedStatusLineSpec;

pub(super) fn claude_settings_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_CLAUDE_SETTINGS`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    agent_config_path(
        "claude",
        "RIMZ_CLAUDE_SETTINGS",
        Path::new(".claude/settings.json"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path)?;
    write_json(path, &root)?;

    Ok(HookInstallReport {
        agent: "claude",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        installed_events: installed,
    })
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("claude", path)?;
    let existing = read_existing_json(path)?;
    let status_line_change = classify_status_line_change(&existing, &STATUS_LINE);
    let subagent_status_line_change = classify_status_line_change(&existing, &SUBAGENT_STATUS_LINE);
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "claude",
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            original: original_config,
            candidate: render_json(&root)?,
            existed,
        }],
        planned_events: installed,
        status_line_change: Some(status_line_change),
        subagent_status_line_change: Some(subagent_status_line_change),
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

    // Wrap both render commands so Rimz captures Claude's rich per-render JSON —
    // the session `statusLine` and the per-child `subagentStatusLine`. Idempotent
    // by construction: a prior Rimz-managed value carries the user's original
    // under `_rimz_wrapped`, which the upsert reads back rather than re-wrapping.
    upsert_rimz_status_line(&mut root, &STATUS_LINE);
    upsert_rimz_status_line(&mut root, &SUBAGENT_STATUS_LINE);

    Ok((root, installed))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    if !existed {
        return Ok(HookUninstallReport {
            agent: "claude",
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed: false,
            }],
            removed_events: Vec::new(),
        });
    }
    let mut root = read_existing_json(path)?;
    let removed = strip_rimz_matchers(&mut root);
    // Restore both render commands (or drop the field if Rimz added it).
    strip_rimz_status_line(&mut root, &STATUS_LINE);
    strip_rimz_status_line(&mut root, &SUBAGENT_STATUS_LINE);
    write_json(path, &root)?;
    Ok(HookUninstallReport {
        agent: "claude",
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
    INSTALLED_EVENTS.iter().all(|(event, matcher)| {
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
    })
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
    has_hook_artifact
        || status_line_is_rimz_managed(&root, &STATUS_LINE)
        || status_line_is_rimz_managed(&root, &SUBAGENT_STATUS_LINE)
}

fn canonical_entry_is_installed(
    obj: &Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
) -> bool {
    let actual_matcher = obj.get("matcher").and_then(Value::as_str);
    matcher_matches(matcher, actual_matcher)
        && entry_is_rimz_owned(obj)
        && blocking_sync_marker_is_usable(obj, event, matcher)
}

fn blocking_sync_marker_is_usable(
    obj: &Map<String, Value>,
    event: &str,
    matcher: Option<&str>,
) -> bool {
    let blocking = BLOCKING_EVENTS
        .iter()
        .any(|&(e, m)| e == event && matcher_matches(m, matcher));
    if !blocking {
        return true;
    }
    match obj.get(RIMZ_SYNC_KEY).and_then(Value::as_bool) {
        Some(true) => true,
        Some(false) => false,
        None => !is_rimz_managed_object(obj),
    }
}

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => {
            let value: Value =
                serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                    agent: "claude",
                    path: path.to_path_buf(),
                    source: Box::new(source),
                })?;
            match value {
                Value::Object(map) => Ok(map),
                other => Err(AgentErr::Install {
                    agent: "claude",
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
            agent: "claude",
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
            agent: "claude",
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
/// `_rimz_managed = true` that targets a blocking event but is marked
/// `_rimz_sync = false`. The set of "must block" events is owned by
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
            if matcher_matches(expected_matcher, actual_matcher)
                && !obj
                    .get(RIMZ_SYNC_KEY)
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return Err(AgentErr::Install {
                    agent: "claude",
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

fn build_matcher_entry(event: &str, matcher: Option<&str>) -> Value {
    let blocking = BLOCKING_EVENTS
        .iter()
        .any(|&(e, m)| e == event && matcher_matches(m, matcher));
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".to_owned(), Value::String(m.to_owned()));
    }
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    entry.insert(RIMZ_SYNC_KEY.to_owned(), Value::Bool(blocking));
    // No `--event`: the helper reads `hook_event_name` from the hook's stdin
    // payload, so every installed command is identical (`RIMZ_HOOK_COMMAND`).
    let command = RIMZ_HOOK_COMMAND.to_owned();
    let mut hook = Map::new();
    hook.insert("type".to_owned(), Value::String("command".to_owned()));
    hook.insert("command".to_owned(), Value::String(command));
    hook.insert(
        "timeout".to_owned(),
        Value::Number(CLAUDE_HOOK_TIMEOUT_SECS.into()),
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

fn status_line_is_rimz_managed(root: &Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    crate::agents::managed_statusline::is_managed(root, spec)
}

/// Insert or refresh Rimz's `statusLine` wrapper. Idempotent: a prior
/// Rimz-managed statusline has the user's original under `_rimz_wrapped`, which
/// is read back (never double-wrapped); a user's prior statusline of any shape
/// (command object, bare string, or other type) is captured whole; no prior
/// statusline leaves `_rimz_wrapped` absent.
///
/// When the original is a command object, its sibling rendering keys
/// (`padding`, `refreshInterval`, …) are carried onto the managed object so the
/// wrap stays visually faithful while installed — Claude reads them off the
/// top-level object, which would otherwise lose them until uninstall. The whole
/// original is still stored under `_rimz_wrapped` for exact restoration.
pub(super) fn upsert_rimz_status_line(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) {
    crate::agents::managed_statusline::upsert(root, spec);
}

/// Restore the user's original command under `spec.key`. When the current one is
/// Rimz-managed, replace it with the captured `_rimz_wrapped` value, or remove
/// the key entirely when nothing was wrapped. A non-Rimz value is left
/// untouched. Returns whether a Rimz-managed value was found.
fn strip_rimz_status_line(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    crate::agents::managed_statusline::strip(root, spec)
}

/// Classify how an install would change `spec.key`, for the consent summary.
pub(super) fn classify_status_line_change(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> StatusLineChange {
    crate::agents::managed_statusline::classify(root, spec)
}

/// The user's original command that a Rimz-managed value under `spec.key`
/// currently wraps, if any — read from `_rimz_wrapped` (handling both the
/// `{type,command}` object form and a bare command string). `None` when the key
/// is absent, not Rimz-managed, or wraps nothing runnable.
pub(super) fn wrapped_status_line_command_from(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> Option<String> {
    crate::agents::managed_statusline::wrapped_command(root, spec)
}
