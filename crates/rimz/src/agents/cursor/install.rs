//! Cursor `hooks.json` merge installer.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, agent_config_path, read_optional_file,
    settings_json::{self, PendingWrite},
};

use super::{
    RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER, RIMZ_STATUS_LINE_MARKER, STATUS_LINE, WIRED_EVENTS,
};

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

pub(super) fn install_into(hooks_path: &Path, config_path: &Path) -> Result<HookInstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let (hooks, events) = install_candidate(hooks_path)?;
    let config = statusline_install_candidate(config_path)?;
    let hooks_candidate = settings_json::render_json("cursor", &hooks)?;
    let config_candidate = settings_json::render_json("cursor", &config)?;
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
        ),
        installed_events: events,
    })
}

pub(super) fn preview_at(hooks_path: &Path, config_path: &Path) -> Result<HookInstallPreview> {
    let hooks_original = read_optional_file("cursor", hooks_path)?;
    let config_original = read_optional_file("cursor", config_path)?;
    let existing_config = read_existing_json(config_path)?;
    let status_line_change =
        super::super::managed_statusline::classify(&existing_config, &STATUS_LINE);
    let (hooks, events) = install_candidate(hooks_path)?;
    let config = statusline_install_candidate(config_path)?;
    Ok(HookInstallPreview {
        agent: "cursor",
        files: vec![
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
        ],
        planned_events: events,
        status_line_change,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall_from(hooks_path: &Path, config_path: &Path) -> Result<HookUninstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let mut hooks = read_existing_json(hooks_path)?;
    let mut config = read_existing_json(config_path)?;
    let removed_events = strip_owned(&mut hooks);
    super::super::managed_statusline::strip(&mut config, &STATUS_LINE);
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
    Ok(HookUninstallReport {
        agent: "cursor",
        files: report_files(
            hooks_path,
            hooks_original.is_some(),
            config_path,
            config_original.is_some(),
        ),
        removed_events,
    })
}

fn statusline_install_candidate(path: &Path) -> Result<Map<String, Value>> {
    let mut root = read_existing_json(path)?;
    super::super::managed_statusline::upsert(&mut root, &STATUS_LINE);
    Ok(root)
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

pub(super) fn statusline_installed_at(path: &Path) -> bool {
    read_existing_json(path).is_ok_and(|root| {
        super::super::managed_statusline::is_managed(&root, &STATUS_LINE)
            && root
                .get(STATUS_LINE.key_path[0])
                .and_then(Value::as_object)
                .and_then(|object| object.get("command"))
                .and_then(Value::as_str)
                == Some(STATUS_LINE.command)
    })
}

pub(super) fn statusline_artifact_at(path: &Path) -> bool {
    read_existing_json(path).is_ok_and(|root| {
        super::super::managed_statusline::is_managed(&root, &STATUS_LINE)
            || root
                .get(STATUS_LINE.key_path[0])
                .and_then(Value::as_object)
                .and_then(|object| object.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
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

pub(super) fn read_existing_json(path: &Path) -> Result<Map<String, Value>> {
    settings_json::read_json_object("cursor", path)
}

fn report_files(
    hooks_path: &Path,
    hooks_existed: bool,
    config_path: &Path,
    config_existed: bool,
) -> Vec<HookInstallFileReport> {
    vec![
        HookInstallFileReport {
            path: hooks_path.to_path_buf(),
            existed: hooks_existed,
        },
        HookInstallFileReport {
            path: config_path.to_path_buf(),
            existed: config_existed,
        },
    ]
}
