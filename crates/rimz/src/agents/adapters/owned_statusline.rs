//! Marker-free statusline ownership for providers that rewrite their settings.
//!
//! A provider that drops unknown keys from its `statusLine` object cannot carry
//! an ownership marker there. Installed means the command equals RimZ's, owned
//! means the command contains RimZ's marker, and the displaced original lives
//! in a RimZ-home record holding `{"statusLine": <original>}` and nothing else.
//! The legacy `_rimz_managed` and `_rimz_wrapped` keys are read as a fallback
//! and never written.

use std::path::Path;

use serde_json::{Map, Value};

use super::provider_file;
use super::{AgentErr, Result, StatusLineChange, settings_json};
use crate::agents::{HookInstallFilePreview, read_optional_file};

const STATUS_LINE_KEY: &str = "statusLine";
const LEGACY_MANAGED_KEY: &str = "_rimz_managed";
const LEGACY_WRAPPED_KEY: &str = "_rimz_wrapped";

/// One provider's statusline: whose it is, what RimZ writes, and which of the
/// displaced object's rendering keys RimZ's statusline keeps.
pub(super) struct OwnedStatusLine {
    pub(super) agent: &'static str,
    pub(super) command: &'static str,
    pub(super) marker: &'static str,
    pub(super) retained_keys: &'static [&'static str],
}

impl OwnedStatusLine {
    /// The settings root with RimZ's statusline written, and the original the
    /// write displaces, if any.
    pub(super) fn install_candidate(
        &self,
        path: &Path,
    ) -> Result<(Map<String, Value>, Option<Value>)> {
        let mut root = settings_json::read_json_object(self.agent, path)?;
        let existing = root.get(STATUS_LINE_KEY).cloned();
        let displaced = self.displaced(existing.as_ref());
        let mut statusline = Map::new();
        if let Some(Value::Object(object)) = existing.as_ref() {
            for key in self.retained_keys {
                if let Some(value) = object.get(*key) {
                    statusline.insert((*key).to_owned(), value.clone());
                }
            }
        }
        statusline.insert("type".to_owned(), Value::String("command".to_owned()));
        statusline.insert("command".to_owned(), Value::String(self.command.to_owned()));
        root.insert(STATUS_LINE_KEY.to_owned(), Value::Object(statusline));
        Ok((root, displaced))
    }

    /// An owned statusline that differs from what install would write.
    pub(super) fn outdated_at(&self, path: &Path) -> bool {
        let (Ok(root), Ok((candidate, _))) = (
            settings_json::read_json_object(self.agent, path),
            self.install_candidate(path),
        ) else {
            return false;
        };
        root.get(STATUS_LINE_KEY)
            .is_some_and(|value| self.owns(value))
            && root.get(STATUS_LINE_KEY) != candidate.get(STATUS_LINE_KEY)
    }

    pub(super) fn installed_at(&self, path: &Path) -> bool {
        self.object_command_at(path).as_deref() == Some(self.command)
    }

    pub(super) fn artifact_at(&self, path: &Path) -> bool {
        self.object_command_at(path)
            .is_some_and(|command| command.contains(self.marker))
    }

    fn object_command_at(&self, path: &Path) -> Option<String> {
        settings_json::read_json_object(self.agent, path)
            .ok()?
            .get(STATUS_LINE_KEY)?
            .as_object()?
            .get("command")?
            .as_str()
            .map(ToOwned::to_owned)
    }

    /// The displaced statusline's command, from the record or else a legacy
    /// `_rimz_wrapped`; never RimZ's own.
    pub(super) fn wrapped_command_at(
        &self,
        config_path: &Path,
        state_path: &Path,
    ) -> Option<String> {
        let original = match self.read_state(state_path).ok()? {
            Some(value) => value,
            None => {
                let root = settings_json::read_json_object(self.agent, config_path).ok()?;
                legacy_wrapped(root.get(STATUS_LINE_KEY)?)?
            }
        };
        statusline_command(&original)
            .filter(|command| !command.contains(self.marker))
            .map(ToOwned::to_owned)
    }

    fn owns(&self, value: &Value) -> bool {
        statusline_command(value).is_some_and(|command| command.contains(self.marker))
    }

    fn displaced(&self, existing: Option<&Value>) -> Option<Value> {
        let existing = existing?;
        if self.owns(existing) {
            legacy_wrapped(existing).filter(|wrapped| !self.owns(wrapped))
        } else {
            Some(existing.clone())
        }
    }

    pub(super) fn classify(&self, root: &Map<String, Value>) -> Option<StatusLineChange> {
        match root.get(STATUS_LINE_KEY) {
            None => Some(StatusLineChange::Added),
            Some(value) if self.owns(value) => Some(StatusLineChange::Unchanged),
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

    /// Restores an owned statusline from the record, else from a legacy
    /// `_rimz_wrapped`, else removes the key; a statusline RimZ does not own
    /// stays.
    pub(super) fn strip(&self, root: &mut Map<String, Value>, state_path: &Path) -> Result<()> {
        let Some(current) = root.get(STATUS_LINE_KEY) else {
            return Ok(());
        };
        let owned = current
            .as_object()
            .and_then(|object| object.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(self.marker));
        if !owned {
            return Ok(());
        }
        let legacy = legacy_wrapped(current).filter(|wrapped| !self.owns(wrapped));
        let original = match self.read_state(state_path)? {
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

    fn read_state(&self, path: &Path) -> Result<Option<Value>> {
        let Some(bytes) = settings_json::read_optional_bytes(self.agent, path)? else {
            return Ok(None);
        };
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|source| AgentErr::InstallParse {
                agent: self.agent,
                path: path.to_path_buf(),
                source: Box::new(source),
            })?;
        let Value::Object(mut root) = value else {
            return Err(AgentErr::Install {
                agent: self.agent,
                reason: format!(
                    "expected {} to contain only a `statusLine` object field",
                    path.display()
                ),
            });
        };
        if root.len() != 1 || !root.contains_key(STATUS_LINE_KEY) {
            return Err(AgentErr::Install {
                agent: self.agent,
                reason: format!(
                    "expected {} to contain only a `statusLine` field",
                    path.display()
                ),
            });
        }
        Ok(root.remove(STATUS_LINE_KEY))
    }

    /// What an install does to the record at `state_path`, given what it
    /// displaces from the settings at `config_path`.
    pub(super) fn state_change(
        &self,
        config_path: &Path,
        state_path: &Path,
        displaced: Option<&Value>,
    ) -> Result<StateChange> {
        if let Some(original) = displaced {
            return Ok(StateChange::Write(original.clone()));
        }
        let absent = !settings_json::read_json_object(self.agent, config_path)?
            .contains_key(STATUS_LINE_KEY);
        let recorded = settings_json::read_optional_bytes(self.agent, state_path)?.is_some();
        Ok(if absent && recorded {
            StateChange::Remove
        } else {
            StateChange::Keep
        })
    }

    pub(super) fn apply_state(&self, state_path: &Path, change: &StateChange) -> Result<()> {
        match change {
            StateChange::Keep => Ok(()),
            StateChange::Write(original) => self.write_state(state_path, original),
            StateChange::Remove => self.remove_state(state_path),
        }
    }

    /// The record's dry-run row; a removal is an empty candidate.
    pub(super) fn state_preview(
        &self,
        state_path: &Path,
        change: &StateChange,
    ) -> Result<Option<HookInstallFilePreview>> {
        let candidate = match change {
            StateChange::Keep => return Ok(None),
            StateChange::Write(original) => self.render_state(original)?,
            StateChange::Remove => String::new(),
        };
        let original = read_optional_file(self.agent, state_path)?;
        Ok(Some(HookInstallFilePreview {
            path: state_path.to_path_buf(),
            existed: original.is_some(),
            original,
            candidate,
        }))
    }

    fn render_state(&self, original: &Value) -> Result<String> {
        let mut root = Map::new();
        root.insert(STATUS_LINE_KEY.to_owned(), original.clone());
        settings_json::render_json(self.agent, &root)
    }

    fn write_state(&self, path: &Path, original: &Value) -> Result<()> {
        provider_file::write_bytes(path, self.render_state(original)?.as_bytes())?;
        Ok(())
    }

    pub(super) fn remove_state(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
            agent: self.agent,
            path: path.to_path_buf(),
            source,
        })
    }
}

/// What an install does to the displaced-statusline record, so that the
/// record describes exactly what that install displaced or is absent.
pub(super) enum StateChange {
    /// RimZ's own statusline, or nothing recorded: the record stays as it is.
    Keep,
    /// Anyone else's statusline: the record holds it.
    Write(Value),
    /// No statusline: a record would forward to and restore a deleted one.
    Remove,
}

fn legacy_wrapped(value: &Value) -> Option<Value> {
    value.as_object()?.get(LEGACY_WRAPPED_KEY).cloned()
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
