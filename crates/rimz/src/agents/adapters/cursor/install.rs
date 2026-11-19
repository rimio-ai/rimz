//! Cursor `hooks.json` merge installer.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, Result, agent_config_path, read_optional_file,
    settings_json::{self, PendingWrite},
};

use super::super::install_report::report_files;
use super::super::owned_statusline::{OwnedStatusLine, StateChange};
use super::{
    CURSOR_HOOKS, RETAINED_RENDERING_KEYS, RIMZ_HOOK_COMMAND, RIMZ_HOOK_MARKER,
    RIMZ_STATUS_LINE_COMMAND, RIMZ_STATUS_LINE_MARKER,
};

const STATUS_LINE: OwnedStatusLine = OwnedStatusLine {
    agent: "cursor",
    command: RIMZ_STATUS_LINE_COMMAND,
    marker: RIMZ_STATUS_LINE_MARKER,
    retained_keys: RETAINED_RENDERING_KEYS,
};

pub(super) static MANAGED_INTEGRATION: CursorManagedIntegration = CursorManagedIntegration;

pub(super) struct CursorManagedIntegration;

pub(super) fn outdated_files_at(hooks: &Path, configs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let (Ok(root), Ok((candidate, _))) = (read_existing_json(hooks), install_candidate(hooks)) {
        let owned = owned_hooks(&root);
        if !owned.is_empty()
            && (owned != owned_hooks(&candidate) || root.get("version") != candidate.get("version"))
        {
            files.push(hooks.to_path_buf());
        }
    }
    files.extend(
        configs
            .iter()
            .filter(|path| STATUS_LINE.outdated_at(path))
            .cloned(),
    );
    files
}

fn owned_hooks(root: &Map<String, Value>) -> Map<String, Value> {
    root.get("hooks")
        .and_then(Value::as_object)
        .map_or_else(Map::new, |hooks| {
            super::super::managed_json_hooks::owned_hook_entries(hooks, |entry| {
                entry_is_owned(entry).then(|| entry.clone())
            })
        })
}

impl ManagedIntegration for CursorManagedIntegration {
    fn outdated_files(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Vec<PathBuf> {
        cursor_hooks_path()
            .and_then(|hooks| {
                Ok(outdated_files_at(
                    &hooks,
                    &cursor_cli_config_paths(login_env)?,
                ))
            })
            .unwrap_or_default()
    }

    fn install(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallReport> {
        install_all(
            &cursor_hooks_path()?,
            &cursor_cli_config_paths(login_env)?,
            &cursor_statusline_state_path()?,
        )
    }

    fn preview(
        &self,
        login_env: &BTreeMap<String, String>,
        _options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        preview_all(
            &cursor_hooks_path()?,
            &cursor_cli_config_paths(login_env)?,
            &cursor_statusline_state_path()?,
        )
    }

    fn uninstall(&self, login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        uninstall_all(
            &cursor_hooks_path()?,
            &cursor_cli_config_paths(login_env)?,
            &cursor_statusline_state_path()?,
        )
    }

    fn installed(&self, login_env: &BTreeMap<String, String>) -> bool {
        let Ok(hooks_path) = cursor_hooks_path() else {
            return false;
        };
        let Ok(config_paths) = cursor_cli_config_paths(login_env) else {
            return false;
        };
        hooks_installed_at(&hooks_path)
            && config_paths
                .iter()
                .all(|path| statusline_installed_at(path))
    }

    fn managed_artifacts_present(&self, login_env: &BTreeMap<String, String>) -> bool {
        cursor_hooks_path().is_ok_and(|path| managed_artifacts_at(&path))
            || cursor_cli_config_paths(login_env)
                .is_ok_and(|paths| paths.iter().any(|path| statusline_artifact_at(path)))
            || cursor_statusline_state_path().is_ok_and(|path| path.exists())
    }

    fn wrapped_status_line_command(&self, login_env: &BTreeMap<String, String>) -> Option<String> {
        wrapped_status_line_command_at(
            cursor_cli_config_paths(login_env).ok()?.first()?,
            &cursor_statusline_state_path().ok()?,
        )
    }
}

pub(super) fn install_all(
    hooks_path: &Path,
    config_paths: &[PathBuf],
    state_path: &Path,
) -> Result<HookInstallReport> {
    let state = record_change(config_paths, state_path)?;
    let state_existed = state_path.exists();
    STATUS_LINE.apply_state(state_path, &state)?;
    let mut files = Vec::new();
    let mut installed_events = Vec::new();
    for config_path in config_paths.iter().rev() {
        let report = install_pair(hooks_path, config_path)?;
        files.extend(report.files);
        installed_events = report.installed_events;
    }
    if !matches!(state, StateChange::Keep) {
        files.extend(report_files([(state_path, state_existed)]));
    }
    Ok(HookInstallReport {
        plugin_dir: None,
        agent: "cursor",
        files: first_report_per_path(files),
        installed_events,
    })
}

/// The one record change an install over `config_paths` makes. Passes run in
/// reverse, so the last displacement in that order wins and the primary (the
/// first path) goes last. With no displacement, only the primary, whose
/// original the forwarder reads, can remove the record.
fn record_change(config_paths: &[PathBuf], state_path: &Path) -> Result<StateChange> {
    let Some(primary) = config_paths.first() else {
        return Ok(StateChange::Keep);
    };
    let mut displaced = None;
    for config_path in config_paths.iter().rev() {
        if let (_, Some(original)) = STATUS_LINE.install_candidate(config_path)? {
            displaced = Some(original);
        }
    }
    STATUS_LINE.state_change(primary, state_path, displaced.as_ref())
}

pub(super) fn preview_all(
    hooks_path: &Path,
    config_paths: &[PathBuf],
    state_path: &Path,
) -> Result<HookInstallPreview> {
    let (primary, others) = config_paths.split_first().ok_or_else(no_config_dir)?;
    let mut preview = preview_pair(hooks_path, primary)?;
    let state = record_change(config_paths, state_path)?;
    preview
        .files
        .extend(STATUS_LINE.state_preview(state_path, &state)?);
    for config_path in others {
        let extra = preview_pair(hooks_path, config_path)?;
        preview.files.extend(
            extra
                .files
                .into_iter()
                .filter(|file| file.path == *config_path),
        );
    }
    Ok(preview)
}

pub(super) fn uninstall_all(
    hooks_path: &Path,
    config_paths: &[PathBuf],
    state_path: &Path,
) -> Result<HookUninstallReport> {
    let mut files = Vec::new();
    let mut removed_events = Vec::new();
    for (index, config_path) in config_paths.iter().enumerate() {
        let last = index + 1 == config_paths.len();
        let report = uninstall_pair(hooks_path, config_path, state_path, last)?;
        files.extend(report.files);
        removed_events.extend(report.removed_events);
    }
    Ok(HookUninstallReport {
        agent: "cursor",
        files: first_report_per_path(files),
        removed_events,
    })
}

/// Every fan-out step reports the shared hooks and state files again; the
/// first report saw them before any write.
fn first_report_per_path(mut files: Vec<HookInstallFileReport>) -> Vec<HookInstallFileReport> {
    let mut seen = HashSet::new();
    files.retain(|file| seen.insert(file.path.clone()));
    files
}

pub(super) fn cursor_hooks_path() -> Result<PathBuf> {
    agent_config_path(
        "cursor",
        "RIMZ_CURSOR_HOOKS",
        Path::new(".cursor/hooks.json"),
    )
}

/// The `cli-config.json` in every config dir Cursor resolves for this user,
/// primary first; see [`super::config_dirs`].
pub(super) fn cursor_cli_config_paths(
    login_env: &BTreeMap<String, String>,
) -> Result<Vec<PathBuf>> {
    if let Some(raw) = std::env::var_os("RIMZ_CURSOR_CLI_CONFIG").filter(|value| !value.is_empty())
    {
        return Ok(vec![PathBuf::from(raw)]);
    }
    let paths: Vec<PathBuf> = super::config_dirs(login_env)
        .into_iter()
        .map(|home| home.join("cli-config.json"))
        .collect();
    if paths.is_empty() {
        return Err(no_config_dir());
    }
    Ok(paths)
}

fn no_config_dir() -> AgentErr {
    AgentErr::Install {
        agent: "cursor",
        reason: "$CURSOR_CONFIG_DIR, $XDG_CONFIG_HOME, and $HOME are not set; cannot resolve Cursor cli-config.json"
            .to_owned(),
    }
}

pub(super) fn cursor_statusline_state_path() -> Result<PathBuf> {
    Ok(std::env::var_os("RIMZ_CURSOR_STATUSLINE_STATE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::disk::paths::rimz_home().join("cursor-statusline.json")))
}

#[cfg(test)]
pub(super) fn install_into(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookInstallReport> {
    install_all(hooks_path, &[config_path.to_path_buf()], state_path)
}

/// One config's hook and statusline pair, written as one transaction.
fn install_pair(hooks_path: &Path, config_path: &Path) -> Result<HookInstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let (hooks, events) = install_candidate(hooks_path)?;
    let (config, _) = STATUS_LINE.install_candidate(config_path)?;
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
        plugin_dir: None,
        agent: "cursor",
        files: report_files([
            (hooks_path, hooks_original.is_some()),
            (config_path, config_original.is_some()),
        ]),
        installed_events: events,
    })
}

#[cfg(test)]
pub(super) fn preview_at(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookInstallPreview> {
    preview_all(hooks_path, &[config_path.to_path_buf()], state_path)
}

fn preview_pair(hooks_path: &Path, config_path: &Path) -> Result<HookInstallPreview> {
    let hooks_original = read_optional_file("cursor", hooks_path)?;
    let config_original = read_optional_file("cursor", config_path)?;
    let existing_config = read_existing_json(config_path)?;
    let status_line_change = STATUS_LINE.classify(&existing_config);
    let (hooks, events) = install_candidate(hooks_path)?;
    let (config, _) = STATUS_LINE.install_candidate(config_path)?;
    let files = vec![
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
    Ok(HookInstallPreview {
        agent: "cursor",
        files,
        planned_events: events,
        status_line_change,
        subagent_status_line_change: None,
        plugin_dir: None,
    })
}

#[cfg(test)]
pub(super) fn uninstall_from(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
) -> Result<HookUninstallReport> {
    uninstall_pair(hooks_path, config_path, state_path, true)
}

/// `remove_state` stays false until the last config file has restored the
/// displaced statusline from the shared state record.
fn uninstall_pair(
    hooks_path: &Path,
    config_path: &Path,
    state_path: &Path,
    remove_state: bool,
) -> Result<HookUninstallReport> {
    let hooks_original = settings_json::read_optional_bytes("cursor", hooks_path)?;
    let config_original = settings_json::read_optional_bytes("cursor", config_path)?;
    let state_existed = settings_json::read_optional_bytes("cursor", state_path)?.is_some();
    let mut hooks = read_existing_json(hooks_path)?;
    let mut config = read_existing_json(config_path)?;
    let removed_events = strip_owned(&mut hooks);
    STATUS_LINE.strip(&mut config, state_path)?;
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
    if state_existed && remove_state {
        STATUS_LINE.remove_state(state_path)?;
    }
    Ok(HookUninstallReport {
        agent: "cursor",
        files: report_files(
            [
                (hooks_path, hooks_original.is_some()),
                (config_path, config_original.is_some()),
            ]
            .into_iter()
            .chain((state_existed && remove_state).then_some((state_path, true))),
        ),
        removed_events,
    })
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
    STATUS_LINE.installed_at(path)
}

pub(super) fn statusline_artifact_at(path: &Path) -> bool {
    STATUS_LINE.artifact_at(path)
}

pub(super) fn wrapped_status_line_command_at(
    config_path: &Path,
    state_path: &Path,
) -> Option<String> {
    STATUS_LINE.wrapped_command_at(config_path, state_path)
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
