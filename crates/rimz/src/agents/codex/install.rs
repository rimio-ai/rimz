//! Codex hook installer for `config.toml`.
//!
//! This module owns the non-destructive TOML merge/uninstall path, Rimz hook command detection, and Codex trust-state reporting for managed hooks.

use std::path::{Path, PathBuf};

use crate::agents::{
    AgentErr, HookInstallPreview, HookInstallReport, HookUninstallReport, Result,
    agent_config_path, read_optional_file,
};
use crate::store::atomic;

use super::{
    CODEX_HOOK_TIMEOUT_SECS, HOOKS_TABLE, INSTALLED_EVENTS, RIMZ_BLOCK, RIMZ_HOOK_COMMAND,
    RIMZ_HOOK_MARKER,
};

pub(super) fn codex_config_path() -> Result<PathBuf> {
    // Honour an explicit override (`RIMZ_CODEX_CONFIG`) so tests and tooling
    // can point the installer at a tempdir without touching real config.
    agent_config_path(
        "codex",
        "RIMZ_CODEX_CONFIG",
        Path::new(".codex/config.toml"),
    )
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path)?;
    write_table(path, &root)?;

    Ok(HookInstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        installed_events: installed,
        merged: existed,
        additional_config_paths: Vec::new(),
    })
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("codex", path)?;
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "codex",
        config_path: path.to_path_buf(),
        planned_events: installed,
        original_config,
        candidate_config: render_table(&root)?,
        merged: existed,
        // Codex has no statusline; it inherits the no-op `wrapped_status_line_command`.
        status_line_change: None,
        subagent_status_line_change: None,
        additional_configs: Vec::new(),
    })
}

fn install_candidate(path: &Path) -> Result<(toml::Table, Vec<String>)> {
    let mut root = read_existing_table(path)?;

    // Strip any prior Rimz-managed hooks (and the legacy block) before writing
    // the fresh set — installer constants are the single source of truth.
    strip_rimz_hook_commands(&mut root);
    remove_rimz_block(&mut root);

    let mut installed = Vec::new();
    for &(event, matcher) in INSTALLED_EVENTS {
        insert_rimz_hook_group(&mut root, event, matcher);
        installed.push(event.to_owned());
    }

    Ok((root, installed))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    if !existed {
        return Ok(HookUninstallReport {
            agent: "codex",
            config_path: path.to_path_buf(),
            removed_events: Vec::new(),
            existed: false,
            additional_config_paths: Vec::new(),
        });
    }

    let mut root = read_existing_table(path)?;
    let mut removed = strip_rimz_hook_commands(&mut root);
    removed.extend(remove_rimz_block(&mut root));
    removed.sort();
    removed.dedup();
    write_table(path, &root)?;

    Ok(HookUninstallReport {
        agent: "codex",
        config_path: path.to_path_buf(),
        removed_events: removed,
        existed: true,
        additional_config_paths: Vec::new(),
    })
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_table(path) else {
        return false;
    };
    INSTALLED_EVENTS
        .iter()
        .all(|(event, _)| has_rimz_hook_command(&root, event))
}

pub(super) fn managed_artifacts_at(path: &Path) -> bool {
    read_existing_table(path).is_ok_and(|root| {
        has_any_rimz_hook_command(&root)
            || root
                .get(HOOKS_TABLE)
                .and_then(toml::Value::as_table)
                .is_some_and(|hooks| hooks.contains_key(RIMZ_BLOCK))
    })
}

/// Rimz-installed hook events Codex has not yet trusted. Codex records trust
/// per hook-definition hash under `[hooks.state]`
/// (`"<config-path>:<event_token>:<i>:<j>"` keys) and **silently skips** an
/// untrusted hook, so an installed-but-untrusted event is a dead channel only
/// the user can open (`/hooks` inside Codex). Presence-only by token: a hash
/// mismatch is Codex's to re-flag, and mirroring its hash algorithm would
/// couple Rimz to an upstream internal.
pub(super) fn untrusted_hook_events_at(path: &Path) -> Vec<String> {
    let Ok(root) = read_existing_table(path) else {
        return Vec::new();
    };
    let state = root
        .get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table);
    INSTALLED_EVENTS
        .iter()
        .filter(|(event, _)| has_rimz_hook_command(&root, event))
        .filter(|(event, _)| {
            let needle = format!(":{}:", snake_event_token(event));
            !state.is_some_and(|state| state.keys().any(|key| key.contains(&needle)))
        })
        .map(|(event, _)| (*event).to_owned())
        .collect()
}

/// Codex's `[hooks.state]` event token: the hook event name in lower_snake
/// (`PermissionRequest` → `permission_request`), as Codex writes it.
pub(super) fn snake_event_token(event: &str) -> String {
    let mut out = String::with_capacity(event.len() + 4);
    for (i, c) in event.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn read_existing_table(path: &Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(toml::Table::new()),
        Ok(text) => toml::from_str::<toml::Table>(&text).map_err(|source| AgentErr::InstallParse {
            agent: "codex",
            path: path.to_path_buf(),
            source: Box::new(source),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(source) => Err(AgentErr::InstallIo {
            agent: "codex",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_table(path: &Path, table: &toml::Table) -> Result<()> {
    let text = render_table(table)?;
    atomic::write_bytes_atomically(path, text.as_bytes())?;
    Ok(())
}

fn render_table(table: &toml::Table) -> Result<String> {
    toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "codex",
        source: Box::new(source),
    })
}

fn insert_rimz_hook_group(root: &mut toml::Table, event: &str, matcher: Option<&str>) {
    let hooks = root
        .entry(HOOKS_TABLE.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let hooks_table = match hooks {
        toml::Value::Table(table) => table,
        _ => {
            *hooks = toml::Value::Table(toml::Table::new());
            hooks.as_table_mut().expect("just inserted table")
        }
    };

    let groups = hooks_table
        .entry(event.to_owned())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let groups_array = match groups {
        toml::Value::Array(array) => array,
        _ => {
            *groups = toml::Value::Array(Vec::new());
            groups.as_array_mut().expect("just inserted array")
        }
    };

    let mut handler = toml::Table::new();
    handler.insert("type".to_owned(), toml::Value::String("command".to_owned()));
    handler.insert(
        "command".to_owned(),
        toml::Value::String(RIMZ_HOOK_COMMAND.to_owned()),
    );
    handler.insert(
        "timeout".to_owned(),
        toml::Value::Integer(CODEX_HOOK_TIMEOUT_SECS),
    );
    handler.insert(
        "statusMessage".to_owned(),
        toml::Value::String(format!("Routing {event} through Rimz")),
    );

    let mut group = toml::Table::new();
    if let Some(matcher) = matcher {
        group.insert(
            "matcher".to_owned(),
            toml::Value::String(matcher.to_owned()),
        );
    }
    group.insert(
        "hooks".to_owned(),
        toml::Value::Array(vec![toml::Value::Table(handler)]),
    );
    groups_array.push(toml::Value::Table(group));
}

fn strip_rimz_hook_commands(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks_table) = root
        .get_mut(HOOKS_TABLE)
        .and_then(toml::Value::as_table_mut)
    else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    let event_names = hooks_table.keys().cloned().collect::<Vec<_>>();
    for event in event_names {
        let Some(groups) = hooks_table
            .get_mut(&event)
            .and_then(toml::Value::as_array_mut)
        else {
            continue;
        };

        for group in groups.iter_mut() {
            let Some(group_table) = group.as_table_mut() else {
                continue;
            };
            let Some(handlers) = group_table
                .get_mut("hooks")
                .and_then(toml::Value::as_array_mut)
            else {
                continue;
            };
            let before = handlers.len();
            handlers.retain(|handler| !is_rimz_hook_handler(handler));
            if handlers.len() != before {
                removed.push(event.clone());
            }
        }

        groups.retain(|group| {
            group
                .as_table()
                .and_then(|table| table.get("hooks"))
                .and_then(toml::Value::as_array)
                .is_none_or(|handlers| !handlers.is_empty())
        });
        if groups.is_empty() {
            hooks_table.remove(&event);
        }
    }
    if hooks_table.is_empty() {
        root.remove(HOOKS_TABLE);
    }
    removed
}

pub(super) fn has_rimz_hook_command(root: &toml::Table, event: &str) -> bool {
    root.get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get(event))
        .and_then(toml::Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .as_table()
                    .and_then(|table| table.get("hooks"))
                    .and_then(toml::Value::as_array)
                    .is_some_and(|handlers| handlers.iter().any(is_current_rimz_hook_handler))
            })
        })
}

fn has_any_rimz_hook_command(root: &toml::Table) -> bool {
    root.get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .is_some_and(|hooks| {
            hooks.values().any(|value| {
                value.as_array().is_some_and(|groups| {
                    groups.iter().any(|group| {
                        group
                            .as_table()
                            .and_then(|table| table.get("hooks"))
                            .and_then(toml::Value::as_array)
                            .is_some_and(|handlers| handlers.iter().any(is_rimz_hook_handler))
                    })
                })
            })
        })
}

fn handler_command(handler: &toml::Value) -> Option<&str> {
    handler
        .as_table()
        .and_then(|table| table.get("command"))
        .and_then(toml::Value::as_str)
}

/// Whether a handler is the current rimz command exactly — drives "already
/// installed correctly?" detection, so an old `--event` form reads as needing
/// reinstall.
fn is_current_rimz_hook_handler(handler: &toml::Value) -> bool {
    handler_command(handler).is_some_and(|command| command == RIMZ_HOOK_COMMAND)
}

/// Whether a handler is rimz-owned in any historical form (with `--event`,
/// without `exec`). Drives strip on install/uninstall, so duplicates never
/// accumulate across version drift.
fn is_rimz_hook_handler(handler: &toml::Value) -> bool {
    handler_command(handler).is_some_and(|command| command.contains(RIMZ_HOOK_MARKER))
}

fn remove_rimz_block(root: &mut toml::Table) -> Vec<String> {
    let Some(hooks_value) = root.get_mut(HOOKS_TABLE) else {
        return Vec::new();
    };
    let Some(hooks_table) = hooks_value.as_table_mut() else {
        return Vec::new();
    };
    let removed_value = hooks_table.remove(RIMZ_BLOCK);
    let removed_events = removed_value
        .as_ref()
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("events"))
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if hooks_table.is_empty() {
        root.remove(HOOKS_TABLE);
    }
    removed_events
}
