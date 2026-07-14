//! Strict JSON-object settings I/O plus managed `hooks.<Event>` merge semantics.

use std::path::Path;

use serde_json::{Map, Value};

use crate::store::atomic;

use super::hook_types::HookRecord;
use super::managed_statusline::{self, ManagedStatusLineSpec};
use super::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, read_optional_file,
};

const HOOKS_KEY: &str = "hooks";
const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
const RIMZ_SYNC_KEY: &str = "_rimz_sync";

#[derive(Clone, Copy)]
pub(crate) enum SyncEncoding {
    EntryMarker,
    HandlerAsync,
    None,
}

pub(crate) struct ManagedJsonHookSpec {
    pub agent: &'static str,
    pub catalog: &'static [HookRecord],
    pub command: &'static str,
    pub legacy_command_marker: &'static str,
    pub timeout: u64,
    pub sync: SyncEncoding,
    pub status_lines: &'static [&'static ManagedStatusLineSpec],
}

impl ManagedJsonHookSpec {
    pub fn install_into(&self, path: &Path) -> Result<HookInstallReport> {
        let existed = path.exists();
        let (root, installed_events) = self.candidate(path)?;
        self.write_json(path, &root)?;
        Ok(HookInstallReport {
            agent: self.agent,
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed,
            }],
            installed_events,
        })
    }

    pub fn preview_at(&self, path: &Path) -> Result<HookInstallPreview> {
        let existed = path.exists();
        let original = read_optional_file(self.agent, path)?;
        let existing = self.read_json(path)?;
        let status_line_change = self
            .status_lines
            .first()
            .and_then(|spec| managed_statusline::classify(&existing, spec));
        let subagent_status_line_change = self
            .status_lines
            .get(1)
            .and_then(|spec| managed_statusline::classify(&existing, spec));
        let (candidate, planned_events) = self.candidate(path)?;
        Ok(HookInstallPreview {
            agent: self.agent,
            files: vec![HookInstallFilePreview {
                path: path.to_path_buf(),
                existed,
                original,
                candidate: self.render_json(&candidate)?,
            }],
            planned_events,
            status_line_change,
            subagent_status_line_change,
        })
    }

    pub fn uninstall_from(&self, path: &Path) -> Result<HookUninstallReport> {
        let existed = path.exists();
        if !existed {
            return Ok(HookUninstallReport {
                agent: self.agent,
                files: vec![HookInstallFileReport {
                    path: path.to_path_buf(),
                    existed,
                }],
                removed_events: Vec::new(),
            });
        }
        let mut root = self.read_json(path)?;
        let removed_events = self.strip_owned(&mut root);
        for spec in self.status_lines {
            managed_statusline::strip(&mut root, spec);
        }
        self.write_json(path, &root)?;
        Ok(HookUninstallReport {
            agent: self.agent,
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed,
            }],
            removed_events,
        })
    }

    pub fn installed_at(&self, path: &Path) -> bool {
        let Ok(root) = self.read_json(path) else {
            return false;
        };
        let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
            return false;
        };
        self.catalog.iter().all(|hook| {
            hooks
                .get(hook.event)
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry
                            .as_object()
                            .is_some_and(|entry| self.canonical_entry_is_installed(entry, hook))
                    })
                })
        }) && self
            .status_lines
            .iter()
            .all(|spec| managed_statusline::install_satisfied(&root, spec))
    }

    pub fn managed_artifacts_at(&self, path: &Path) -> bool {
        let Ok(root) = self.read_json(path) else {
            return false;
        };
        root.get(HOOKS_KEY)
            .and_then(Value::as_object)
            .is_some_and(|hooks| {
                hooks.values().any(|entries| {
                    entries.as_array().is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry
                                .as_object()
                                .is_some_and(|entry| self.entry_is_owned(entry))
                        })
                    })
                })
            })
            || self
                .status_lines
                .iter()
                .any(|spec| managed_statusline::is_managed(&root, spec))
    }

    pub fn read_json(&self, path: &Path) -> Result<Map<String, Value>> {
        match std::fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => Ok(Map::new()),
            Ok(text) => {
                let value: Value =
                    serde_json::from_str(&text).map_err(|source| AgentErr::InstallParse {
                        agent: self.agent,
                        path: path.to_path_buf(),
                        source: Box::new(source),
                    })?;
                match value {
                    Value::Object(root) => Ok(root),
                    other => Err(AgentErr::Install {
                        agent: self.agent,
                        reason: format!(
                            "expected JSON object at the top level of {}; found {}",
                            path.display(),
                            json_type_name(&other)
                        ),
                    }),
                }
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(source) => Err(AgentErr::InstallIo {
                agent: self.agent,
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn render_json(&self, root: &Map<String, Value>) -> Result<String> {
        let text =
            serde_json::to_string_pretty(&Value::Object(root.clone())).map_err(|source| {
                AgentErr::InstallSerialize {
                    agent: self.agent,
                    source: Box::new(source),
                }
            })?;
        Ok(format!("{text}\n"))
    }

    fn candidate(&self, path: &Path) -> Result<(Map<String, Value>, Vec<String>)> {
        let mut root = self.read_json(path)?;
        self.reject_async_blocking(&root)?;
        self.strip_owned(&mut root);
        for hook in self.catalog {
            self.upsert(&mut root, hook);
        }
        for spec in self.status_lines {
            managed_statusline::upsert(&mut root, spec);
        }
        Ok((
            root,
            self.catalog
                .iter()
                .map(|hook| event_label(hook.event, hook.matcher))
                .collect(),
        ))
    }

    fn write_json(&self, path: &Path, root: &Map<String, Value>) -> Result<()> {
        atomic::write_bytes_atomically(path, self.render_json(root)?.as_bytes())?;
        Ok(())
    }

    fn reject_async_blocking(&self, root: &Map<String, Value>) -> Result<()> {
        let Some(hooks) = root.get(HOOKS_KEY).and_then(Value::as_object) else {
            return Ok(());
        };
        for hook in self.catalog.iter().filter(|hook| hook.synchronous) {
            let Some(entries) = hooks.get(hook.event).and_then(Value::as_array) else {
                continue;
            };
            for entry in entries {
                let Some(entry) = entry.as_object().filter(|entry| is_marked(entry)) else {
                    continue;
                };
                let actual_matcher = entry.get("matcher").and_then(Value::as_str);
                let broad_qwen_pretool = matches!(self.sync, SyncEncoding::HandlerAsync)
                    && hook.event == "PreToolUse"
                    && actual_matcher.is_none();
                if !matcher_matches(hook.matcher, actual_matcher) && !broad_qwen_pretool {
                    continue;
                }
                let invalid = match self.sync {
                    SyncEncoding::EntryMarker => {
                        entry.get(RIMZ_SYNC_KEY).and_then(Value::as_bool) != Some(true)
                    }
                    SyncEncoding::HandlerAsync => entry
                        .get(HOOKS_KEY)
                        .and_then(Value::as_array)
                        .is_some_and(|handlers| {
                            handlers.iter().any(|handler| {
                                handler.get("async").and_then(Value::as_bool) == Some(true)
                            })
                        }),
                    SyncEncoding::None => false,
                };
                if invalid {
                    return Err(AgentErr::Install {
                        agent: self.agent,
                        reason: format!(
                            "existing config marks blocking hook `{}` as async; refusing to install",
                            event_label(hook.event, hook.matcher)
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn upsert(&self, root: &mut Map<String, Value>, hook: &HookRecord) {
        let hooks = root
            .entry(HOOKS_KEY.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !hooks.is_object() {
            *hooks = Value::Object(Map::new());
        }
        let hooks = hooks.as_object_mut().expect("hooks shape established");
        let entries = hooks
            .entry(hook.event.to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !entries.is_array() {
            *entries = Value::Array(Vec::new());
        }
        entries
            .as_array_mut()
            .expect("event collection shape established")
            .push(self.build_entry(hook));
    }

    fn build_entry(&self, hook: &HookRecord) -> Value {
        let mut entry = Map::new();
        if let Some(matcher) = hook.matcher {
            entry.insert("matcher".to_owned(), Value::String(matcher.to_owned()));
        }
        entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
        if matches!(self.sync, SyncEncoding::EntryMarker) {
            entry.insert(RIMZ_SYNC_KEY.to_owned(), Value::Bool(hook.synchronous));
        }
        let mut handler = Map::new();
        handler.insert("type".to_owned(), Value::String("command".to_owned()));
        handler.insert("command".to_owned(), Value::String(self.command.to_owned()));
        handler.insert("timeout".to_owned(), Value::Number(self.timeout.into()));
        entry.insert(
            HOOKS_KEY.to_owned(),
            Value::Array(vec![Value::Object(handler)]),
        );
        Value::Object(entry)
    }

    fn canonical_entry_is_installed(&self, entry: &Map<String, Value>, hook: &HookRecord) -> bool {
        if !matcher_matches(hook.matcher, entry.get("matcher").and_then(Value::as_str))
            || !self.entry_is_owned(entry)
        {
            return false;
        }
        let sync_ok = match self.sync {
            SyncEncoding::EntryMarker if hook.synchronous => {
                match entry.get(RIMZ_SYNC_KEY).and_then(Value::as_bool) {
                    Some(value) => value,
                    None => !is_marked(entry),
                }
            }
            SyncEncoding::HandlerAsync if hook.synchronous => entry
                .get(HOOKS_KEY)
                .and_then(Value::as_array)
                .is_some_and(|handlers| {
                    handlers
                        .iter()
                        .all(|handler| handler.get("async").and_then(Value::as_bool) != Some(true))
                }),
            _ => true,
        };
        sync_ok
            && (!matches!(self.sync, SyncEncoding::None)
                || self.entry_has_exact_handler(entry, hook))
    }

    fn entry_has_exact_handler(&self, entry: &Map<String, Value>, hook: &HookRecord) -> bool {
        if hook.matcher.is_none() && entry.contains_key("matcher") {
            return false;
        }
        let Some(handlers) = entry.get(HOOKS_KEY).and_then(Value::as_array) else {
            return false;
        };
        handlers.len() == 1
            && handlers[0].as_object().is_some_and(|handler| {
                handler.get("type").and_then(Value::as_str) == Some("command")
                    && handler.get("command").and_then(Value::as_str) == Some(self.command)
                    && handler.get("timeout").and_then(Value::as_u64) == Some(self.timeout)
            })
    }

    fn entry_is_owned(&self, entry: &Map<String, Value>) -> bool {
        if is_marked(entry) {
            return true;
        }
        let Some(handlers) = entry.get(HOOKS_KEY).and_then(Value::as_array) else {
            return false;
        };
        !handlers.is_empty()
            && handlers.iter().all(|handler| {
                handler
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(self.legacy_command_marker))
            })
    }

    fn strip_owned(&self, root: &mut Map<String, Value>) -> Vec<String> {
        let mut removed = Vec::new();
        let Some(hooks) = root.get_mut(HOOKS_KEY).and_then(Value::as_object_mut) else {
            return removed;
        };
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            let Some(entries) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
                continue;
            };
            entries.retain(|entry| {
                let Some(entry) = entry.as_object() else {
                    return true;
                };
                if self.entry_is_owned(entry) {
                    removed.push(event_label(
                        &event,
                        entry.get("matcher").and_then(Value::as_str),
                    ));
                    false
                } else {
                    true
                }
            });
            if entries.is_empty() {
                hooks.remove(&event);
            }
        }
        if hooks.is_empty() {
            root.remove(HOOKS_KEY);
        }
        removed
    }
}

fn matcher_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (expected, actual) {
        (None, None | Some("")) => true,
        (Some(expected), Some(actual)) => expected == actual,
        _ => false,
    }
}

fn event_label(event: &str, matcher: Option<&str>) -> String {
    matcher
        .filter(|matcher| !matcher.is_empty())
        .map_or_else(|| event.to_owned(), |matcher| format!("{event}:{matcher}"))
}

fn is_marked(entry: &Map<String, Value>) -> bool {
    entry
        .get(RIMZ_MANAGED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
