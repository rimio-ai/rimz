//! Non-destructive Kimi `[[hooks]]` config merge.

use std::path::{Path, PathBuf};

use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    read_optional_file,
};
use crate::store::atomic;

use super::HOOK_EVENTS;

pub(super) const RIMZ_HOOK_COMMAND: &str =
    "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source kimi";
const RIMZ_COMMAND_MARKER: &str = "rimz hooks feed --source kimi";

pub(super) fn config_path() -> Result<PathBuf> {
    Ok(std::env::var_os("RIMZ_KIMI_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| super::wire::kimi_home().join("config.toml")))
}

fn read_table(path: &Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(toml::Table::new()),
        Ok(text) => toml::from_str(&text).map_err(|source| AgentErr::InstallParse {
            agent: "kimi",
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "kimi",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn candidate(path: &Path) -> Result<(toml::Table, Vec<String>)> {
    let mut root = read_table(path)?;
    strip_managed(&mut root);
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    if !hooks.is_array() {
        return Err(AgentErr::Install {
            agent: "kimi",
            reason: "`hooks` must be an array of tables".to_owned(),
        });
    }
    let array = hooks.as_array_mut().expect("array checked above");
    for event in HOOK_EVENTS {
        let mut hook = toml::Table::new();
        hook.insert("event".to_owned(), toml::Value::String((*event).to_owned()));
        if matches!(
            *event,
            "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "PermissionRequest"
                | "PermissionResult"
        ) {
            hook.insert("matcher".to_owned(), toml::Value::String(".*".to_owned()));
        }
        hook.insert(
            "command".to_owned(),
            toml::Value::String(RIMZ_HOOK_COMMAND.to_owned()),
        );
        hook.insert(
            "timeout".to_owned(),
            toml::Value::Integer(if *event == "SessionEnd" { 4 } else { 10 }),
        );
        array.push(toml::Value::Table(hook));
    }
    Ok((
        root,
        HOOK_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect(),
    ))
}

fn strip_managed(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks) = root.get_mut("hooks").and_then(toml::Value::as_array_mut) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    hooks.retain(|value| {
        let managed = value
            .as_table()
            .and_then(|hook| hook.get("command"))
            .and_then(toml::Value::as_str)
            .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER));
        if managed
            && let Some(event) = value
                .as_table()
                .and_then(|hook| hook.get("event"))
                .and_then(toml::Value::as_str)
        {
            removed.push(event.to_owned());
        }
        !managed
    });
    if hooks.is_empty() {
        root.remove("hooks");
    }
    removed
}

fn render(table: &toml::Table) -> Result<String> {
    toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "kimi",
        source: Box::new(source),
    })
}

fn write(path: &Path, table: &toml::Table) -> Result<()> {
    atomic::write_bytes_atomically(path, render(table)?.as_bytes())?;
    Ok(())
}

pub(super) fn install(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (table, installed_events) = candidate(path)?;
    write(path, &table)?;
    Ok(HookInstallReport {
        agent: "kimi",
        config_path: path.to_path_buf(),
        installed_events,
        merged: existed,
    })
}

pub(super) fn preview(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("kimi", path)?;
    let (table, planned_events) = candidate(path)?;
    Ok(HookInstallPreview {
        agent: "kimi",
        config_path: path.to_path_buf(),
        planned_events,
        original_config,
        candidate_config: render(&table)?,
        merged: existed,
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

pub(super) fn uninstall(path: &Path) -> Result<HookUninstallReport> {
    if !path.exists() {
        return Ok(HookUninstallReport {
            agent: "kimi",
            config_path: path.to_path_buf(),
            removed_events: Vec::new(),
            existed: false,
        });
    }
    let mut table = read_table(path)?;
    let mut removed_events = strip_managed(&mut table);
    removed_events.sort();
    removed_events.dedup();
    write(path, &table)?;
    Ok(HookUninstallReport {
        agent: "kimi",
        config_path: path.to_path_buf(),
        removed_events,
        existed: true,
    })
}

pub(super) fn installed(path: &Path) -> bool {
    let Ok(table) = read_table(path) else {
        return false;
    };
    let Some(hooks) = table.get("hooks").and_then(toml::Value::as_array) else {
        return false;
    };
    HOOK_EVENTS.iter().all(|event| {
        hooks.iter().any(|value| {
            let Some(hook) = value.as_table() else {
                return false;
            };
            hook.get("event").and_then(toml::Value::as_str) == Some(event)
                && hook
                    .get("command")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER))
        })
    })
}

pub(super) fn managed(path: &Path) -> bool {
    read_table(path).is_ok_and(|table| {
        table
            .get("hooks")
            .and_then(toml::Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|value| {
                    value
                        .as_table()
                        .and_then(|hook| hook.get("command"))
                        .and_then(toml::Value::as_str)
                        .is_some_and(|command| command.contains(RIMZ_COMMAND_MARKER))
                })
            })
    })
}
