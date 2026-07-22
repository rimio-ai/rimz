//! Cursor `hooks.json` merge installer.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, Result, StatusLineChange, agent_config_path,
    read_optional_file,
    settings_json::{self, PendingWrite},
};
use crate::store::atomic;

use super::{
    CURSOR_HOOKS, RETAINED_RENDERING_KEYS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER,
    RIMZ_STATUS_LINE_COMMAND, RIMZ_STATUS_LINE_MARKER,
};

const STATUS_LINE_KEY: &str = "statusLine";
const LEGACY_MANAGED_KEY: &str = "_rimz_managed";
const LEGACY_WRAPPED_KEY: &str = "_rimz_wrapped";

pub(super) static MANAGED_INTEGRATION: CursorManagedIntegration = CursorManagedIntegration;

pub(super) struct CursorManagedIntegration;

impl ManagedIntegration for CursorManagedIntegration {
    fn install(&self) -> Result<HookInstallReport> {
        install_into(
            &cursor_hooks_path()?,
            &cursor_cli_config_path()?,
            &cursor_statusline_state_path()?,
        )
    }

    fn preview(&self) -> Result<HookInstallPreview> {
        preview_at(
            &cursor_hooks_path()?,
            &cursor_cli_config_path()?,
            &cursor_statusline_state_path()?,
        )
    }

    fn uninstall(&self) -> Result<HookUninstallReport> {
        uninstall_from(
            &cursor_hooks_path()?,
            &cursor_cli_config_path()?,
            &cursor_statusline_state_path()?,
        )
    }

    fn installed(&self) -> bool {
        let Ok(hooks_path) = cursor_hooks_path() else {
            return false;
        };
        let Ok(config_path) = cursor_cli_config_path() else {
            return false;
        };
        hooks_installed_at(&hooks_path) && statusline_installed_at(&config_path)
    }

    fn managed_artifacts_present(&self) -> bool {
        cursor_hooks_path().is_ok_and(|path| managed_artifacts_at(&path))
            || cursor_cli_config_path().is_ok_and(|path| statusline_artifact_at(&path))
            || cursor_statusline_state_path().is_ok_and(|path| path.exists())
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        wrapped_status_line_command_at(
            &cursor_cli_config_path().ok()?,
            &cursor_statusline_state_path().ok()?,
        )
    }
}

pub(super) fn cursor_hooks_path() -> Result<PathBuf> {
    agent_config_path(
        "cursor",
        "RIMZ_CURSOR_HOOKS",
        Path::new(".cursor/hooks.json"),
    )
}

pub(super) fn cursor_cli_config_path() -> Result<PathBuf> {
    agent_config_path(
        "cursor",
        "RIMZ_CURSOR_CLI_CONFIG",
        Path::new(".cursor/cli-config.json"),
    )
}

pub(super) fn cursor_statusline_state_path() -> Result<PathBuf> {
    Ok(std::env::var_os("RIMZ_CURSOR_STATUSLINE_STATE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::paths::config_home().join("rimz/cursor-statusline.json")))
}

pub(super) fn install_into(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookInstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let (hooks, events) = install_candidate(hooks_path)?;
    let (config, displaced) = statusline_install_candidate(config_path)?;
    let state_original = displaced
        .as_ref()
        .map(|_| settings_json::read_optional_bytes("cursor", state_path))
        .transpose()?
        .flatten();
    let hooks_candidate = settings_json::render_json("cursor", &hooks)?;
    let config_candidate = settings_json::render_json("cursor", &config)?;
    if let Some(original) = displaced.as_ref() {
        write_statusline_state(state_path, original)?;
    }
    settings_json::commit_pair(
        "cursor",
        PendingWrite::required(hooks_path, &hooks_candidate),
        PendingWrite::required(config_path, &config_candidate),
        hooks_original.as_deref(),
        config_original.as_deref(),
    )?;
    Ok(HookInstallReport {
        agent: "cursor",
        files: report_files(
            hooks_path,
            hooks_original.is_some(),
            config_path,
            config_original.is_some(),
            displaced
                .is_some()
                .then_some((state_path, state_original.is_some())),
        ),
        installed_events: events,
    })
}

pub(super) fn preview_at(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookInstallPreview> {
    let hooks_original = read_optional_file("cursor", hooks_path)?;
    let config_original = read_optional_file("cursor", config_path)?;
    let existing_config = read_existing_json(config_path)?;
    let status_line_change = classify_statusline(&existing_config);
    let (hooks, events) = install_candidate(hooks_path)?;
    let (config, displaced) = statusline_install_candidate(config_path)?;
    let mut files = vec![
        HookInstallFilePreview {
            path: hooks_path.to_path_buf(),
            existed: hooks_original.is_some(),
            original: hooks_original,
            candidate: settings_json::render_json("cursor", &hooks)?,
        },
        HookInstallFilePreview {
            path: config_path.to_path_buf(),
            existed: config_original.is_some(),
            original: config_original,
            candidate: settings_json::render_json("cursor", &config)?,
        },
    ];
    if let Some(original) = displaced.as_ref() {
        let state_original = read_optional_file("cursor", state_path)?;
        files.push(HookInstallFilePreview {
            path: state_path.to_path_buf(),
            existed: state_original.is_some(),
            original: state_original,
            candidate: render_statusline_state(original)?,
        });
    }
    Ok(HookInstallPreview {
        agent: "cursor",
        files,
        planned_events: events,
        status_line_change,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall_from(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookUninstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let state_existed = settings_json::read_optional_bytes("cursor", state_path)?.is_some();
    let mut hooks = read_existing_json(hooks_path)?;
    let mut config = read_existing_json(config_path)?;
    let removed_events = strip_owned(&mut hooks);
    strip_statusline(&mut config, state_path)?;
    let hooks_candidate = hooks_original
        .is_some()
        .then(|| settings_json::render_json("cursor", &hooks))
        .transpose()?;
    let config_candidate = config_original
        .is_some()
        .then(|| settings_json::render_json("cursor", &config))
        .transpose()?;
    settings_json::commit_pair(
        "cursor",
        PendingWrite::optional(hooks_path, hooks_candidate.as_deref()),
        PendingWrite::optional(config_path, config_candidate.as_deref()),
        hooks_original.as_deref(),
        config_original.as_deref(),
    )?;
    if state_existed {
        remove_statusline_state(state_path)?;
    }
    Ok(HookUninstallReport {
        agent: "cursor",
        files: report_files(
            hooks_path,
            hooks_original.is_some(),
            config_path,
            config_original.is_some(),
            state_existed.then_some((state_path, true)),
        ),
        removed_events,
    })
}

fn statusline_install_candidate(path: &Path) -> Result<(Map<String, Value>, Option<Value>)> {
    let mut root = read_existing_json(path)?;
    let existing = root.get(STATUS_LINE_KEY).cloned();
    let displaced = displaced_statusline(existing.as_ref());
    let mut statusline = Map::new();
    if let Some(Value::Object(object)) = existing.as_ref() {
        for key in RETAINED_RENDERING_KEYS {
            if let Some(value) = object.get(*key) {
                statusline.insert((*key).to_owned(), value.clone());
            }
        }
    }
    statusline.insert("type".to_owned(), Value::String("command".to_owned()));
    statusline.insert(
        "command".to_owned(),
        Value::String(RIMZ_STATUS_LINE_COMMAND.to_owned()),
    );
    root.insert(STATUS_LINE_KEY.to_owned(), Value::Object(statusline));
    Ok((root, displaced))
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_json(path) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    CURSOR_HOOKS.iter().all(|hook_record| {
        hooks
            .get(hook_record.event)
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

pub(super) fn statusline_installed_at(path: &Path) -> bool {
    read_existing_json(path).is_ok_and(|root| {
        root.get(STATUS_LINE_KEY)
            .and_then(Value::as_object)
            .and_then(|object| object.get("command"))
            .and_then(Value::as_str)
            == Some(RIMZ_STATUS_LINE_COMMAND)
    })
}

pub(super) fn statusline_artifact_at(path: &Path) -> bool {
    read_existing_json(path).is_ok_and(|root| {
        root.get(STATUS_LINE_KEY)
            .and_then(Value::as_object)
            .and_then(|object| object.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
    })
}

pub(super) fn wrapped_status_line_command_at(
    config_path: &Path,
    state_path: &Path,
) -> Option<String> {
    match read_statusline_state(state_path).ok()? {
        Some(value) => statusline_command(&value)
            .filter(|command| !command.contains(RIMZ_STATUS_LINE_MARKER))
            .map(ToOwned::to_owned),
        None => {
            let root = read_existing_json(config_path).ok()?;
            let wrapped = legacy_wrapped(root.get(STATUS_LINE_KEY)?)?;
            statusline_command(&wrapped)
                .filter(|command| !command.contains(RIMZ_STATUS_LINE_MARKER))
                .map(ToOwned::to_owned)
        }
    }
}

fn displaced_statusline(existing: Option<&Value>) -> Option<Value> {
    let existing = existing?;
    if statusline_command(existing).is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
    {
        legacy_wrapped(existing).and_then(non_recursive_statusline)
    } else {
        Some(existing.clone())
    }
}

fn classify_statusline(root: &Map<String, Value>) -> Option<StatusLineChange> {
    match root.get(STATUS_LINE_KEY) {
        None => Some(StatusLineChange::Added),
        Some(value)
            if statusline_command(value)
                .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER)) =>
        {
            Some(StatusLineChange::Unchanged)
        }
        Some(Value::Object(object))
            if object
                .get("command")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty) =>
        {
            Some(StatusLineChange::Added)
        }
        Some(value) => Some(StatusLineChange::Wrapping {
            original: display_statusline(value),
        }),
    }
}

fn strip_statusline(root: &mut Map<String, Value>, state_path: &Path) -> Result<()> {
    let Some(current) = root.get(STATUS_LINE_KEY) else {
        return Ok(());
    };
    let owned = current
        .as_object()
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER));
    if !owned {
        return Ok(());
    }
    let legacy = legacy_wrapped(current).and_then(non_recursive_statusline);
    let original = match read_statusline_state(state_path)? {
        Some(value) => Some(value),
        None => legacy.map(strip_legacy_keys),
    };
    if let Some(original) = original {
        root.insert(STATUS_LINE_KEY.to_owned(), original);
    } else {
        root.remove(STATUS_LINE_KEY);
    }
    Ok(())
}

fn legacy_wrapped(value: &Value) -> Option<Value> {
    value.as_object()?.get(LEGACY_WRAPPED_KEY).cloned()
}

fn non_recursive_statusline(value: Value) -> Option<Value> {
    if statusline_command(&value).is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER)) {
        None
    } else {
        Some(value)
    }
}

fn strip_legacy_keys(value: Value) -> Value {
    let Value::Object(mut object) = value else {
        return value;
    };
    object.remove(LEGACY_MANAGED_KEY);
    object.remove(LEGACY_WRAPPED_KEY);
    Value::Object(object)
}

fn statusline_command(value: &Value) -> Option<&str> {
    match value {
        Value::String(command) if !command.is_empty() => Some(command),
        Value::Object(object) => object
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty()),
        _ => None,
    }
}

fn display_statusline(value: &Value) -> String {
    statusline_command(value)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn read_statusline_state(path: &Path) -> Result<Option<Value>> {
    let Some(bytes) = settings_json::read_optional_bytes("cursor", path)? else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| AgentErr::InstallParse {
        agent: "cursor",
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    let Value::Object(mut root) = value else {
        return Err(AgentErr::Install {
            agent: "cursor",
            reason: format!(
                "expected {} to contain only a `statusLine` object field",
                path.display()
            ),
        });
    };
    if root.len() != 1 || !root.contains_key(STATUS_LINE_KEY) {
        return Err(AgentErr::Install {
            agent: "cursor",
            reason: format!(
                "expected {} to contain only a `statusLine` field",
                path.display()
            ),
        });
    }
    Ok(root.remove(STATUS_LINE_KEY))
}

fn render_statusline_state(original: &Value) -> Result<String> {
    let mut root = Map::new();
    root.insert(STATUS_LINE_KEY.to_owned(), original.clone());
    settings_json::render_json("cursor", &root)
}

fn write_statusline_state(path: &Path, original: &Value) -> Result<()> {
    atomic::write_bytes_atomically(path, render_statusline_state(original)?.as_bytes())?;
    Ok(())
}

fn remove_statusline_state(path: &Path) -> Result<()> {
    std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
        agent: "cursor",
        path: path.to_path_buf(),
        source,
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
    for hook_record in CURSOR_HOOKS {
        let entries = hooks
            .entry(hook_record.event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        let entries = entries.as_array_mut().ok_or_else(|| AgentErr::Install {
            agent: "cursor",
            reason: format!(
                "expected `hooks.{}` to be an array in {}",
                hook_record.event,
                path.display()
            ),
        })?;
        entries.push(json!({ "command": RIMZ_HOOK_COMMAND }));
    }
    Ok((
        root,
        CURSOR_HOOKS
            .iter()
            .map(|hook| hook.event.to_owned())
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

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    settings_json::read_json_object("cursor", path)
}

fn report_files(
    hooks_path: &Path,
    hooks_existed: bool,
    config_path: &Path,
    config_existed: bool,
    state: Option<(&Path, bool)>,
) -> Vec<HookInstallFileReport> {
    let mut files = vec![
        HookInstallFileReport {
            path: hooks_path.to_path_buf(),
            existed: hooks_existed,
        },
        HookInstallFileReport {
            path: config_path.to_path_buf(),
            existed: config_existed,
        },
    ];
    if let Some((path, existed)) = state {
        files.push(HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        });
    }
    files
}
