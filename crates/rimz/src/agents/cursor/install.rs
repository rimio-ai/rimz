//! Cursor `hooks.json` merge installer.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, agent_config_path, read_optional_file,
};
use crate::store::atomic;

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
    let hooks_original = read_optional_bytes(hooks_path)?;
    let config_original = read_optional_bytes(config_path)?;
    let (hooks, events) = install_candidate(hooks_path)?;
    let config = statusline_install_candidate(config_path)?;
    let hooks_candidate = render_json(&hooks)?;
    let config_candidate = render_json(&config)?;
    commit_pair(
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
                candidate: render_json(&hooks)?,
            },
            HookInstallFilePreview {
                path: config_path.to_path_buf(),
                existed: config_original.is_some(),
                original: config_original,
                candidate: render_json(&config)?,
            },
        ],
        planned_events: events,
        status_line_change,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall_from(hooks_path: &Path, config_path: &Path) -> Result<HookUninstallReport> {
    let hooks_original = read_optional_bytes(hooks_path)?;
    let config_original = read_optional_bytes(config_path)?;
    let mut hooks = read_existing_json(hooks_path)?;
    let mut config = read_existing_json(config_path)?;
    let removed_events = strip_owned(&mut hooks);
    super::super::managed_statusline::strip(&mut config, &STATUS_LINE);
    let hooks_candidate = hooks_original
        .is_some()
        .then(|| render_json(&hooks))
        .transpose()?;
    let config_candidate = config_original
        .is_some()
        .then(|| render_json(&config))
        .transpose()?;
    commit_pair(
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

fn render_json(root: &Map<String, Value>) -> Result<String> {
    let text = serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
        AgentErr::InstallSerialize {
            agent: "cursor",
            source: Box::new(source),
        }
    })?;
    Ok(format!("{text}\n"))
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "cursor",
            path: path.to_path_buf(),
            source,
        }),
    }
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

struct PendingWrite<'a> {
    path: &'a Path,
    candidate: Option<&'a [u8]>,
}

impl<'a> PendingWrite<'a> {
    fn required(path: &'a Path, candidate: &'a str) -> Self {
        Self {
            path,
            candidate: Some(candidate.as_bytes()),
        }
    }

    fn optional(path: &'a Path, candidate: Option<&'a str>) -> Self {
        Self {
            path,
            candidate: candidate.map(str::as_bytes),
        }
    }
}

fn commit_pair(
    first: PendingWrite<'_>,
    second: PendingWrite<'_>,
    first_original: Option<&[u8]>,
    second_original: Option<&[u8]>,
) -> Result<()> {
    commit_pair_with(
        first,
        second,
        first_original,
        second_original,
        atomic::write_bytes_atomically,
    )
}

fn commit_pair_with(
    first: PendingWrite<'_>,
    second: PendingWrite<'_>,
    first_original: Option<&[u8]>,
    second_original: Option<&[u8]>,
    mut write: impl FnMut(&Path, &[u8]) -> atomic::Result<()>,
) -> Result<()> {
    let first_written = if let Some(candidate) = first.candidate {
        write(first.path, candidate)?;
        true
    } else {
        false
    };
    if let Some(candidate) = second.candidate
        && let Err(error) = write(second.path, candidate)
    {
        let second_rollback = restore(second.path, second_original, &mut write);
        let first_rollback = first_written.then(|| restore(first.path, first_original, &mut write));
        let first_rollback_error = match first_rollback {
            Some(Err(error)) => Some(error),
            Some(Ok(())) | None => None,
        };
        let rollback_errors = [second_rollback.err(), first_rollback_error]
            .into_iter()
            .flatten()
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        return if rollback_errors.is_empty() {
            Err(error.into())
        } else {
            Err(AgentErr::Install {
                agent: "cursor",
                reason: format!(
                    "writing {} failed ({error}); rollback also failed ({})",
                    second.path.display(),
                    rollback_errors.join("; "),
                ),
            })
        };
    }
    Ok(())
}

fn restore(
    path: &Path,
    original: Option<&[u8]>,
    write: &mut impl FnMut(&Path, &[u8]) -> atomic::Result<()>,
) -> atomic::Result<()> {
    if let Some(original) = original {
        write(path, original)
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(atomic::AtomicErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod transaction_tests {
    use super::*;

    #[test]
    fn second_write_failure_restores_first_file_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("hooks.json");
        let second_path = dir.path().join("cli-config.json");
        let original = b"{  \"user\": true }\n";
        std::fs::write(&first_path, original).unwrap();
        let mut writes = 0;
        let error = commit_pair_with(
            PendingWrite {
                path: &first_path,
                candidate: Some(b"first candidate"),
            },
            PendingWrite {
                path: &second_path,
                candidate: Some(b"second candidate"),
            },
            Some(original),
            None,
            |path, bytes| {
                writes += 1;
                if writes == 2 {
                    return Err(atomic::AtomicErr::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::other("injected second-write failure"),
                    });
                }
                atomic::write_bytes_atomically(path, bytes)
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected second-write failure"));
        assert_eq!(std::fs::read(first_path).unwrap(), original);
        assert!(!second_path.exists());
    }

    #[test]
    fn uninstall_second_write_failure_restores_both_files_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("hooks.json");
        let second_path = dir.path().join("cli-config.json");
        let first_original = b"{  \"hooks\": {\"user\": []} }\n";
        let second_original = b"{\n  \"statusLine\": \"user status\"\n}\n";
        std::fs::write(&first_path, first_original).unwrap();
        std::fs::write(&second_path, second_original).unwrap();
        let mut writes = 0;
        let error = commit_pair_with(
            PendingWrite {
                path: &first_path,
                candidate: Some(b"uninstalled hooks"),
            },
            PendingWrite {
                path: &second_path,
                candidate: Some(b"restored statusline"),
            },
            Some(first_original),
            Some(second_original),
            |path, bytes| {
                writes += 1;
                if writes == 2 {
                    return Err(atomic::AtomicErr::Io {
                        path: path.to_path_buf(),
                        source: std::io::Error::other("injected uninstall failure"),
                    });
                }
                atomic::write_bytes_atomically(path, bytes)
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected uninstall failure"));
        assert_eq!(std::fs::read(first_path).unwrap(), first_original);
        assert_eq!(std::fs::read(second_path).unwrap(), second_original);
    }
}
