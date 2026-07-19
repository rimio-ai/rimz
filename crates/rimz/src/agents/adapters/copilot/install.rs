//! Copilot whole-file hook and reversible statusline installer.

use std::path::Path;

use serde_json::{Map, Value};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, Result, read_optional_file,
    settings_json::{self, PendingWrite},
};

use super::{COPILOT_HOOKS, COPILOT_MANAGED_SOURCE, RIMZ_STATUS_LINE_MARKER, STATUS_LINE};

const AGENT: &str = "copilot";

pub(super) static MANAGED_INTEGRATION: CopilotManagedIntegration = CopilotManagedIntegration;

pub(super) struct CopilotManagedIntegration;

impl ManagedIntegration for CopilotManagedIntegration {
    fn install(&self) -> Result<HookInstallReport> {
        install(
            &super::paths::hooks_path()?,
            &super::paths::settings_path()?,
        )
    }

    fn preview(&self) -> Result<HookInstallPreview> {
        preview(
            &super::paths::hooks_path()?,
            &super::paths::settings_path()?,
        )
    }

    fn uninstall(&self) -> Result<HookUninstallReport> {
        uninstall(
            &super::paths::hooks_path()?,
            &super::paths::settings_path()?,
        )
    }

    fn installed(&self) -> bool {
        super::paths::hooks_path()
            .and_then(|hooks| Ok((hooks, super::paths::settings_path()?)))
            .is_ok_and(|(hooks, settings)| installed(&hooks, &settings))
    }

    fn managed_artifacts_present(&self) -> bool {
        super::paths::hooks_path()
            .and_then(|hooks| Ok((hooks, super::paths::settings_path()?)))
            .is_ok_and(|(hooks, settings)| managed(&hooks, &settings))
    }

    fn wiring_input_paths(&self, _descriptor: &super::super::AgentSpec) -> Vec<std::path::PathBuf> {
        [super::paths::hooks_path(), super::paths::settings_path()]
            .into_iter()
            .flatten()
            .collect()
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        wrapped_statusline_command(&super::paths::settings_path().ok()?)
    }
}

pub(super) fn install(hooks_path: &Path, settings_path: &Path) -> Result<HookInstallReport> {
    let hooks_original = settings_json::read_optional_bytes(AGENT, hooks_path)?;
    let settings_original = settings_json::read_optional_bytes(AGENT, settings_path)?;
    let hook_preview = COPILOT_MANAGED_SOURCE.preview_at(hooks_path)?;
    let Some(hook_candidate) = hook_preview
        .files
        .first()
        .map(|file| file.candidate.as_str())
    else {
        return Err(AgentErr::Install {
            agent: AGENT,
            reason: "managed hook preview produced no candidate".to_owned(),
        });
    };
    let (settings, _) = statusline_candidate(settings_path)?;
    let settings_candidate = settings_json::render_json(AGENT, &settings)?;
    settings_json::commit_pair(
        AGENT,
        PendingWrite::required(settings_path, &settings_candidate),
        PendingWrite::required(hooks_path, hook_candidate),
        settings_original.as_deref(),
        hooks_original.as_deref(),
    )?;
    Ok(HookInstallReport {
        agent: AGENT,
        files: report_files(
            hooks_path,
            hooks_original.is_some(),
            settings_path,
            settings_original.is_some(),
        ),
        installed_events: event_names(),
    })
}

pub(super) fn preview(hooks_path: &Path, settings_path: &Path) -> Result<HookInstallPreview> {
    let hook_preview = COPILOT_MANAGED_SOURCE.preview_at(hooks_path)?;
    let settings_original = read_optional_file(AGENT, settings_path)?;
    let (settings, status_line_change) = statusline_candidate(settings_path)?;
    let mut files = hook_preview.files;
    files.push(HookInstallFilePreview {
        path: settings_path.to_path_buf(),
        existed: settings_original.is_some(),
        original: settings_original,
        candidate: settings_json::render_json(AGENT, &settings)?,
    });
    Ok(HookInstallPreview {
        agent: AGENT,
        files,
        planned_events: event_names(),
        status_line_change: Some(status_line_change),
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall(hooks_path: &Path, settings_path: &Path) -> Result<HookUninstallReport> {
    uninstall_with(hooks_path, settings_path, |path| {
        COPILOT_MANAGED_SOURCE.uninstall_from(path)
    })
}

pub(super) fn uninstall_with(
    hooks_path: &Path,
    settings_path: &Path,
    remove_hook: impl FnOnce(&Path) -> Result<HookUninstallReport>,
) -> Result<HookUninstallReport> {
    let hooks_original = settings_json::read_optional_bytes(AGENT, hooks_path)?;
    let settings_original = settings_json::read_optional_bytes(AGENT, settings_path)?;
    let mut settings = settings_json::read_json_object(AGENT, settings_path)?;
    let stripped = super::super::managed_statusline::strip(&mut settings, &STATUS_LINE);
    let settings_candidate = stripped
        .then(|| settings_json::render_json(AGENT, &settings))
        .transpose()?;
    settings_json::commit_pair(
        AGENT,
        PendingWrite::optional(settings_path, settings_candidate.as_deref()),
        PendingWrite::optional(hooks_path, None),
        settings_original.as_deref(),
        hooks_original.as_deref(),
    )?;

    let hook_report = match remove_hook(hooks_path) {
        Ok(report) => report,
        Err(primary) => {
            let rollback = stripped.then(|| restore_settings(settings_path, &settings_original));
            return match rollback {
                Some(Err(rollback)) => Err(AgentErr::Install {
                    agent: AGENT,
                    reason: format!(
                        "removing {} failed ({primary}); restoring {} also failed ({rollback})",
                        hooks_path.display(),
                        settings_path.display(),
                    ),
                }),
                Some(Ok(())) | None => Err(primary),
            };
        }
    };
    Ok(HookUninstallReport {
        agent: AGENT,
        files: report_files(
            hooks_path,
            hooks_original.is_some(),
            settings_path,
            settings_original.is_some(),
        ),
        removed_events: hook_report.removed_events,
    })
}

fn restore_settings(path: &Path, original: &Option<Vec<u8>>) -> Result<()> {
    let Some(original) = original else {
        return Ok(());
    };
    let original = std::str::from_utf8(original).map_err(|_| AgentErr::Install {
        agent: AGENT,
        reason: format!("cannot restore non-UTF-8 settings at {}", path.display()),
    })?;
    settings_json::commit_pair(
        AGENT,
        PendingWrite::required(path, original),
        PendingWrite::optional(path, None),
        Some(original.as_bytes()),
        Some(original.as_bytes()),
    )
}

fn statusline_candidate(
    path: &Path,
) -> Result<(Map<String, Value>, super::super::StatusLineChange)> {
    let mut root = settings_json::read_json_object(AGENT, path)?;
    if let Some(value) = root.get(STATUS_LINE.key_path[0]) {
        let compatible = value.as_object().is_some_and(|statusline| {
            super::super::managed_statusline::is_managed(&root, &STATUS_LINE)
                || (statusline.get("type").and_then(Value::as_str) == Some("command")
                    && statusline
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|command| !command.trim().is_empty()))
        });
        if !compatible {
            return Err(incompatible_statusline(path));
        }
    }
    let change = super::super::managed_statusline::classify(&root, &STATUS_LINE)
        .ok_or_else(|| incompatible_statusline(path))?;
    super::super::managed_statusline::upsert(&mut root, &STATUS_LINE);
    Ok((root, change))
}

fn incompatible_statusline(path: &Path) -> AgentErr {
    AgentErr::Install {
        agent: AGENT,
        reason: format!(
            "expected `statusLine` in {} to be a command-mode object (`{{\"type\":\"command\",\"command\":...}}`); move the incompatible value aside before installing RimZ",
            path.display()
        ),
    }
}

pub(super) fn installed(hooks_path: &Path, settings_path: &Path) -> bool {
    COPILOT_MANAGED_SOURCE.installed_at(hooks_path) && statusline_installed(settings_path)
}

pub(super) fn managed(hooks_path: &Path, settings_path: &Path) -> bool {
    COPILOT_MANAGED_SOURCE.managed_artifacts_at(hooks_path) || statusline_artifact(settings_path)
}

pub(super) fn statusline_installed(path: &Path) -> bool {
    read_jsonc_object(path).is_some_and(|root| {
        super::super::managed_statusline::is_managed(&root, &STATUS_LINE)
            && root
                .get(STATUS_LINE.key_path[0])
                .and_then(Value::as_object)
                .is_some_and(|statusline| {
                    statusline.get("type").and_then(Value::as_str) == Some("command")
                        && statusline.get("command").and_then(Value::as_str)
                            == Some(STATUS_LINE.command)
                })
    })
}

fn statusline_artifact(path: &Path) -> bool {
    read_jsonc_object(path).is_some_and(|root| {
        super::super::managed_statusline::is_managed(&root, &STATUS_LINE)
            || root
                .get(STATUS_LINE.key_path[0])
                .and_then(Value::as_object)
                .and_then(|statusline| statusline.get("command"))
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(RIMZ_STATUS_LINE_MARKER))
    })
}

pub(super) fn wrapped_statusline_command(path: &Path) -> Option<String> {
    let root = read_jsonc_object(path)?;
    let current = root
        .get(STATUS_LINE.key_path[0])
        .and_then(Value::as_object)
        .and_then(|statusline| statusline.get("command"))
        .and_then(Value::as_str)?;
    current.contains(RIMZ_STATUS_LINE_MARKER).then_some(())?;
    super::super::managed_statusline::wrapped_command(&root, &STATUS_LINE)
}

fn read_jsonc_object(path: &Path) -> Option<Map<String, Value>> {
    let value = super::super::jsonc::from_slice::<Value>(&std::fs::read(path).ok()?).ok()?;
    value.as_object().cloned()
}

fn event_names() -> Vec<String> {
    COPILOT_HOOKS
        .iter()
        .map(|hook| hook.event.to_owned())
        .collect()
}

fn report_files(
    hooks_path: &Path,
    hooks_existed: bool,
    settings_path: &Path,
    settings_existed: bool,
) -> Vec<HookInstallFileReport> {
    vec![
        HookInstallFileReport {
            path: hooks_path.to_path_buf(),
            existed: hooks_existed,
        },
        HookInstallFileReport {
            path: settings_path.to_path_buf(),
            existed: settings_existed,
        },
    ]
}
