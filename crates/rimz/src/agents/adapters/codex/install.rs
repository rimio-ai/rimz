//! Codex hook and routine-command rules installer.
//!
//! This module owns the non-destructive TOML merge/uninstall path, RimZ hook command detection, and Codex trust-state reporting for managed hooks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use sha2::{Digest, Sha256};

use crate::agents::capabilities::LaunchCapability;
use crate::agents::managed_source::{ManagedSource, RIMZ_MANAGED_MARKER};
use crate::agents::provider_file;
use crate::agents::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, ManagedIntegration, Result, read_optional_file,
};

use super::super::hook_types::HookEventSpec;
use super::{
    CODEX_HOOK_TIMEOUT_SECS, CODEX_HOOKS, HOOKS_TABLE, RIMZ_BLOCK, RIMZ_HOOK_COMMAND,
    RIMZ_HOOK_MARKER,
};

pub(super) static MANAGED_INTEGRATION: CodexManagedIntegration = CodexManagedIntegration;

pub(super) struct CodexManagedIntegration;

static RULES_TEXT: LazyLock<String> = LazyLock::new(|| {
    let mut rules = format!(
        "# {RIMZ_MANAGED_MARKER}\n# RimZ rewrites this file; controlled by [agents] allow-routine-rimz.\n"
    );
    for prefix in crate::agents::ROUTINE_RIMZ_PREFIXES {
        let words = prefix
            .split_whitespace()
            .map(|word| serde_json::Value::String(word.to_owned()).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        rules.push_str(&format!(
            "prefix_rule(pattern=[{words}], decision=\"allow\")\n"
        ));
    }
    rules
});

static RULES_SOURCE: LazyLock<ManagedSource> = LazyLock::new(|| {
    ManagedSource::new(
        "codex",
        &RULES_TEXT,
        &[],
        "rules file",
        codex_rules_path,
        true,
    )
});

fn codex_rules_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    super::CodexAdapter
        .config_home(login_env)
        .map(|home| home.join("rules/rimz.rules"))
        .ok_or_else(|| AgentErr::Install {
            agent: "codex",
            reason: "$CODEX_HOME and $HOME are not set; cannot resolve Codex rules".to_owned(),
        })
}

impl ManagedIntegration for CodexManagedIntegration {
    fn install(
        &self,
        login_env: &BTreeMap<String, String>,
        options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallReport> {
        let path = codex_rules_path(login_env)?;
        let managed = RULES_SOURCE.installed_at(&path);
        let files = if options.allow_routine_rimz && (!path.exists() || managed) {
            RULES_SOURCE.install_into(&path)?.files
        } else if !options.allow_routine_rimz && managed {
            RULES_SOURCE.uninstall_from(&path)?.files
        } else {
            Vec::new()
        };
        let mut report = install_into(&codex_config_path(login_env)?)?;
        report.files.extend(files);
        Ok(report)
    }

    fn preview(
        &self,
        login_env: &BTreeMap<String, String>,
        options: crate::agents::HookInstallOptions,
    ) -> Result<HookInstallPreview> {
        let mut preview = preview_install_at(&codex_config_path(login_env)?)?;
        let path = codex_rules_path(login_env)?;
        let managed = RULES_SOURCE.installed_at(&path);
        if options.allow_routine_rimz && (!path.exists() || managed) {
            preview.files.extend(RULES_SOURCE.preview_at(&path)?.files);
        } else if !options.allow_routine_rimz && managed {
            preview.files.push(HookInstallFilePreview {
                original: read_optional_file("codex", &path)?,
                path,
                candidate: String::new(),
                existed: true,
            });
        }
        Ok(preview)
    }

    fn uninstall(&self, login_env: &BTreeMap<String, String>) -> Result<HookUninstallReport> {
        let mut report = uninstall_from(&codex_config_path(login_env)?)?;
        let path = codex_rules_path(login_env)?;
        if RULES_SOURCE.installed_at(&path) {
            report
                .files
                .extend(RULES_SOURCE.uninstall_from(&path)?.files);
        }
        Ok(report)
    }

    fn installed(&self, login_env: &BTreeMap<String, String>) -> bool {
        codex_config_path(login_env).is_ok_and(|path| hooks_installed_at(&path))
    }

    fn managed_artifacts_present(&self, login_env: &BTreeMap<String, String>) -> bool {
        codex_config_path(login_env).is_ok_and(|path| managed_artifacts_at(&path))
            || RULES_SOURCE.installed(login_env)
    }

    fn upgrade_available(
        &self,
        login_env: &BTreeMap<String, String>,
        options: crate::agents::HookInstallOptions,
    ) -> bool {
        if codex_config_path(login_env).is_ok_and(|path| hook_definition_drift_at(&path)) {
            return true;
        }
        let Ok(path) = codex_rules_path(login_env) else {
            return false;
        };
        if options.allow_routine_rimz {
            !path.exists() || RULES_SOURCE.upgrade_available_at(&path)
        } else {
            RULES_SOURCE.installed_at(&path)
        }
    }

    fn wiring_input_paths(
        &self,
        _descriptor: &super::super::AgentSpec,
        login_env: &BTreeMap<String, String>,
    ) -> Vec<PathBuf> {
        codex_config_path(login_env).into_iter().collect()
    }

    fn untrusted_installed_hooks(&self, login_env: &BTreeMap<String, String>) -> Vec<String> {
        codex_config_path(login_env)
            .map(|path| untrusted_hook_events_at(&path))
            .unwrap_or_default()
    }

    fn untrusted_preflight_hooks(&self, login_env: &BTreeMap<String, String>) -> Vec<String> {
        codex_config_path(login_env)
            .map(|path| untrusted_preflight_hook_events_at(&path))
            .unwrap_or_default()
    }
}

pub(super) fn codex_config_path(login_env: &BTreeMap<String, String>) -> Result<PathBuf> {
    // Mirror Codex's home lookup, with a config-only override for tests/tooling.
    if let Some(raw) = login_env.get("RIMZ_CODEX_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(raw));
    }
    super::CodexAdapter
        .config_home(login_env)
        .map(|home| home.join("config.toml"))
        .ok_or_else(|| AgentErr::Install {
            agent: "codex",
            reason: "$CODEX_HOME and $HOME are not set; cannot resolve Codex config".to_owned(),
        })
}

pub(super) fn install_into(path: &Path) -> Result<HookInstallReport> {
    let existed = path.exists();
    let (root, installed) = install_candidate(path)?;
    write_table(path, &root)?;

    Ok(HookInstallReport {
        agent: "codex",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        installed_events: installed,
    })
}

pub(super) fn preview_install_at(path: &Path) -> Result<HookInstallPreview> {
    let existed = path.exists();
    let original_config = read_optional_file("codex", path)?;
    let (root, installed) = install_candidate(path)?;
    Ok(HookInstallPreview {
        agent: "codex",
        files: vec![HookInstallFilePreview {
            path: path.to_path_buf(),
            original: original_config,
            candidate: render_table(&root)?,
            existed,
        }],
        planned_events: installed,
        // Codex has no statusline; it inherits the no-op `wrapped_status_line_command`.
        status_line_change: None,
        subagent_status_line_change: None,
    })
}

fn install_candidate(path: &Path) -> Result<(toml::Table, Vec<String>)> {
    let mut root = read_existing_table(path)?;

    // Strip any prior RimZ-managed hooks (and the legacy block) before writing
    // the fresh set — installer constants are the single source of truth.
    strip_rimz_hook_commands(&mut root);
    remove_rimz_block(&mut root);

    let mut installed = Vec::new();
    for hook in CODEX_HOOKS {
        insert_rimz_hook_group(&mut root, hook);
        installed.push(hook.event.to_owned());
    }

    Ok((root, installed))
}

pub(super) fn uninstall_from(path: &Path) -> Result<HookUninstallReport> {
    let existed = path.exists();
    let removed_events = if existed {
        let mut root = read_existing_table(path)?;
        let mut removed = strip_rimz_hook_commands(&mut root);
        removed.extend(remove_rimz_block(&mut root));
        removed.sort();
        removed.dedup();
        write_table(path, &root)?;
        removed
    } else {
        Vec::new()
    };

    Ok(HookUninstallReport {
        agent: "codex",
        files: vec![HookInstallFileReport {
            path: path.to_path_buf(),
            existed,
        }],
        removed_events,
    })
}

pub(super) fn hooks_installed_at(path: &Path) -> bool {
    let Ok(root) = read_existing_table(path) else {
        return false;
    };
    CODEX_HOOKS
        .iter()
        .all(|hook| has_rimz_hook_command(&root, hook.event))
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

/// RimZ-installed hook events Codex has not trusted for their current definition. Codex records trust per hook-definition hash under `[hooks.state]` (`"<config-path>:<event_token>:<i>:<j>"` keys) and silently skips an untrusted hook, so only the user can open that channel (`/hooks` inside Codex).
///
/// Trust requires an exact raw group/handler slot for a current RimZ command, with a string `trusted_hash` matching the on-disk handler's normalized hash and `enabled` not false. Unsupported handler fields yield no hash and therefore no trust.
pub(super) fn untrusted_hook_events_at(path: &Path) -> Vec<String> {
    untrusted_hook_events(path, |_| true)
}

pub(super) fn untrusted_preflight_hook_events_at(path: &Path) -> Vec<String> {
    untrusted_hook_events(path, |hook| hook.required_for_preflight)
}

fn untrusted_hook_events(path: &Path, include: impl Fn(&HookEventSpec) -> bool) -> Vec<String> {
    let Ok(root) = read_existing_table(path) else {
        return Vec::new();
    };
    let state = root
        .get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table);
    let canonical = path
        .parent()
        .and_then(|home| home.canonicalize().ok())
        .map(|home| home.join("config.toml"));
    CODEX_HOOKS
        .iter()
        .filter(|hook| include(hook))
        .filter(|hook| {
            let slots = rimz_hook_slots(&root, hook.event);
            let token = snake_event_token(hook.event);
            let is_trusted = |slot: &HookSlot<'_>| {
                let Some(hash) = hook_hash(hook.event, slot.group, slot.handler) else {
                    return false;
                };
                [Some(path), canonical.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|prefix| {
                        let key = format!(
                            "{}:{token}:{}:{}",
                            prefix.display(),
                            slot.group_index,
                            slot.handler_index
                        );
                        state
                            .and_then(|state| state.get(&key))
                            .and_then(toml::Value::as_table)
                            .is_some_and(|entry| {
                                entry.get("trusted_hash").and_then(toml::Value::as_str)
                                    == Some(hash.as_str())
                                    && entry.get("enabled").and_then(toml::Value::as_bool)
                                        != Some(false)
                            })
                    })
            };
            !slots.is_empty() && !slots.iter().any(is_trusted)
        })
        .map(|hook| hook.event.to_owned())
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

/// Codex 0.163.0's command-handler hash, conservatively rejecting unsupported fields.
pub(super) fn hook_hash(event: &str, group: &toml::Value, handler: &toml::Value) -> Option<String> {
    let handler = handler.as_table()?;
    if handler.get("type")?.as_str()? != "command" || handler.contains_key("additionalContextLimit")
    {
        return None;
    }
    let command = handler.get("command")?.as_str()?;
    let timeout = match handler.get("timeout") {
        Some(value) => u64::try_from(value.as_integer()?).ok()?,
        None if event == "Interrupt" => 1,
        None => 600,
    };
    let timeout = if event == "Interrupt" {
        timeout.clamp(1, 3)
    } else {
        timeout.max(1)
    };
    let run_async = match handler.get("async") {
        Some(value) => value.as_bool()?,
        None => false,
    };
    let mut normalized = BTreeMap::from([
        ("type", serde_json::Value::from("command")),
        ("command", command.into()),
        ("timeout", timeout.into()),
        ("async", run_async.into()),
    ]);
    if let Some(status) = handler.get("statusMessage") {
        normalized.insert("statusMessage", status.as_str()?.into());
    }
    let matcher = match group.as_table()?.get("matcher") {
        Some(value) => Some(value.as_str()?),
        None => None,
    };
    let mut identity = BTreeMap::from([
        (
            "event_name",
            serde_json::Value::from(snake_event_token(event)),
        ),
        ("hooks", serde_json::to_value([normalized]).ok()?),
    ]);
    if !matches!(event, "UserPromptSubmit" | "Stop" | "Interrupt")
        && let Some(matcher) = matcher
    {
        identity.insert("matcher", matcher.into());
    }
    let json = serde_json::to_vec(&identity).ok()?;
    Some(format!("sha256:{}", hex::encode(Sha256::digest(json))))
}

fn hook_definition_drift_at(path: &Path) -> bool {
    let Ok(root) = read_existing_table(path) else {
        return false;
    };
    CODEX_HOOKS.iter().any(|hook| {
        let frozen = frozen_hook_group(hook);
        let expected = hook_hash(hook.event, &frozen, &frozen["hooks"][0]);
        rimz_hook_slots(&root, hook.event).iter().any(|slot| {
            hook_hash(hook.event, slot.group, slot.handler)
                .is_none_or(|hash| Some(hash) != expected)
        })
    })
}

#[cfg(any(test, feature = "testkit"))]
pub(in crate::agents) fn trust_hook_config_for_test(
    path: &Path,
    excluded_events: &[&str],
) -> Result<()> {
    let fixture_err = |reason: String| AgentErr::Install {
        agent: "codex",
        reason,
    };
    if let Some(unknown) = excluded_events
        .iter()
        .find(|event| !CODEX_HOOKS.iter().any(|hook| hook.event == **event))
    {
        return Err(fixture_err(format!(
            "excluded event {unknown} is not a Codex hook"
        )));
    }
    let mut root = read_existing_table(path)?;
    let mut entries = toml::Table::new();
    for hook in CODEX_HOOKS
        .iter()
        .filter(|hook| !excluded_events.contains(&hook.event))
    {
        let slots = rimz_hook_slots(&root, hook.event);
        if slots.is_empty() {
            return Err(fixture_err(format!(
                "test hook {} is not installed; install before trusting",
                hook.event
            )));
        }
        for slot in slots {
            let hash = hook_hash(hook.event, slot.group, slot.handler)
                .ok_or_else(|| fixture_err(format!("test hook {} cannot be hashed", hook.event)))?;
            let key = format!(
                "{}:{}:{}:{}",
                path.display(),
                snake_event_token(hook.event),
                slot.group_index,
                slot.handler_index
            );
            entries.insert(
                key,
                toml::Value::Table(toml::Table::from_iter([(
                    "trusted_hash".into(),
                    hash.into(),
                )])),
            );
        }
    }
    if entries.is_empty() {
        return Ok(());
    }
    let state = root
        .get_mut(HOOKS_TABLE)
        .and_then(toml::Value::as_table_mut)
        .expect("entries came from an installed hooks table")
        .entry("state")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("test trust state must be a table");
    state.extend(entries);
    write_table(path, &root)
}

pub(super) fn read_existing_table(path: &Path) -> Result<toml::Table> {
    match read_optional_file("codex", path)? {
        None => Ok(toml::Table::new()),
        Some(text) if text.trim().is_empty() => Ok(toml::Table::new()),
        Some(text) => {
            toml::from_str::<toml::Table>(&text).map_err(|source| AgentErr::InstallParse {
                agent: "codex",
                path: path.to_path_buf(),
                source: Box::new(source),
            })
        }
    }
}

pub(super) fn adopt_config(existing: &Path, target: &Path) -> Result<Option<String>> {
    let original = read_existing_table(existing)?;
    let Some(state) = original
        .get(HOOKS_TABLE)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml::Value::as_table)
        .filter(|state| !state.is_empty())
    else {
        return Ok(None);
    };
    let mut root = read_existing_table(target)?;
    let invalid = || AgentErr::Install {
        agent: "codex",
        reason: format!("hooks.state in {} must be a table", target.display()),
    };
    let target_state = root
        .entry(HOOKS_TABLE)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(invalid)?
        .entry("state")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(invalid)?;
    let mut carried = 0;
    for (key, value) in state {
        if !target_state.contains_key(key) {
            target_state.insert(key.clone(), value.clone());
            carried += 1;
        }
    }
    if carried == 0 {
        return Ok(None);
    }
    write_table(target, &root)?;
    let entries = if carried == 1 { "entry" } else { "entries" };
    Ok(Some(format!(
        "carried {carried} hook trust {entries} from {} into {}",
        existing.display(),
        target.display()
    )))
}

fn write_table(path: &Path, table: &toml::Table) -> Result<()> {
    let text = render_table(table)?;
    provider_file::write_bytes(path, text.as_bytes())?;
    Ok(())
}

fn render_table(table: &toml::Table) -> Result<String> {
    toml::to_string_pretty(table).map_err(|source| AgentErr::InstallSerialize {
        agent: "codex",
        source: Box::new(source),
    })
}

fn insert_rimz_hook_group(root: &mut toml::Table, hook: &HookEventSpec) {
    let event = hook.event;
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

    groups_array.push(frozen_hook_group(hook));
}

fn frozen_hook_group(hook: &HookEventSpec) -> toml::Value {
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

    let mut group = toml::Table::new();
    if let Some(matcher) = hook.matcher {
        group.insert(
            "matcher".to_owned(),
            toml::Value::String(matcher.to_owned()),
        );
    }
    group.insert(
        "hooks".to_owned(),
        toml::Value::Array(vec![toml::Value::Table(handler)]),
    );
    toml::Value::Table(group)
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
    !rimz_hook_slots(root, event).is_empty()
}

struct HookSlot<'a> {
    group_index: usize,
    handler_index: usize,
    group: &'a toml::Value,
    handler: &'a toml::Value,
}

/// Raw zero-based positions and definitions of current RimZ handlers under
/// `hooks.<event>`, counted over every array element as Codex keys trust state.
fn rimz_hook_slots<'a>(root: &'a toml::Table, event: &str) -> Vec<HookSlot<'a>> {
    let Some(groups) = root
        .get(HOOKS_TABLE)
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get(event))
        .and_then(toml::Value::as_array)
    else {
        return Vec::new();
    };
    groups
        .iter()
        .enumerate()
        .filter_map(|(group_index, value)| {
            let handlers = value.as_table()?.get("hooks")?.as_array()?;
            Some((group_index, value, handlers))
        })
        .flat_map(|(group_index, group, handlers)| {
            handlers
                .iter()
                .enumerate()
                .filter(|(_, handler)| is_current_rimz_hook_handler(handler))
                .map(move |(handler_index, handler)| HookSlot {
                    group_index,
                    handler_index,
                    group,
                    handler,
                })
        })
        .collect()
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
