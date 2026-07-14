//! Managed JSON command wrappers for provider statusline callbacks.

use serde_json::{Map, Value};

use super::StatusLineChange;

pub(crate) const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
pub(crate) const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";

#[derive(Clone, Copy)]
pub(crate) enum RenderingOptions {
    All,
    Only(&'static [&'static str]),
}

pub(crate) struct ManagedStatusLineSpec {
    pub key: &'static str,
    pub command: &'static str,
    pub command_marker: &'static str,
    pub rendering_options: RenderingOptions,
}

pub(crate) fn is_managed(root: &Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    matches!(
        root.get(spec.key),
        Some(Value::Object(object)) if object_is_managed(object)
    )
}

pub(crate) fn upsert(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) {
    let existing = root.remove(spec.key);
    let original = match &existing {
        Some(Value::Object(object)) if object_is_managed(object) => object
            .get(RIMZ_WRAPPED_KEY)
            .cloned()
            .and_then(|value| non_recursive_value(value, spec)),
        Some(other) => non_recursive_value(other.clone(), spec),
        None => None,
    };
    let mut entry = Map::new();
    if let Some(Value::Object(source)) = existing.as_ref().or(original.as_ref()) {
        for (key, value) in source {
            if key != "command"
                && key != RIMZ_MANAGED_KEY
                && key != RIMZ_WRAPPED_KEY
                && retains_rendering_option(spec.rendering_options, key)
            {
                entry.insert(key.clone(), value.clone());
            }
        }
    }
    entry.insert("type".to_owned(), Value::String("command".to_owned()));
    entry.insert("command".to_owned(), Value::String(spec.command.to_owned()));
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        entry.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    }
    root.insert(spec.key.to_owned(), Value::Object(entry));
}

pub(crate) fn strip(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    if !is_managed(root, spec) {
        return false;
    }
    let original = match root.remove(spec.key) {
        Some(Value::Object(mut object)) => object
            .remove(RIMZ_WRAPPED_KEY)
            .and_then(|value| non_recursive_value(value, spec)),
        _ => None,
    };
    if let Some(original) = original {
        root.insert(spec.key.to_owned(), original);
    }
    true
}

pub(crate) fn classify(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> StatusLineChange {
    match root.get(spec.key) {
        None => StatusLineChange::Added,
        Some(Value::Object(object)) if object_is_managed(object) => StatusLineChange::Unchanged,
        Some(other) => StatusLineChange::Wrapping {
            original: display(other),
        },
    }
}

pub(crate) fn wrapped_command(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> Option<String> {
    let Some(Value::Object(object)) = root.get(spec.key) else {
        return None;
    };
    if !object_is_managed(object) {
        return None;
    }
    command(object.get(RIMZ_WRAPPED_KEY)?)
        .filter(|command| !command.contains(spec.command_marker))
        .map(ToOwned::to_owned)
}

fn retains_rendering_option(policy: RenderingOptions, key: &str) -> bool {
    match policy {
        RenderingOptions::All => true,
        RenderingOptions::Only(keys) => keys.contains(&key),
    }
}

fn object_is_managed(object: &Map<String, Value>) -> bool {
    object
        .get(RIMZ_MANAGED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn non_recursive_value(value: Value, spec: &ManagedStatusLineSpec) -> Option<Value> {
    if command(&value).is_some_and(|command| command.contains(spec.command_marker)) {
        None
    } else {
        Some(value)
    }
}

fn command(value: &Value) -> Option<&str> {
    match value {
        Value::String(command) if !command.is_empty() => Some(command),
        Value::Object(object) => object
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.is_empty()),
        _ => None,
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::String(command) => command.clone(),
        Value::Object(object) => object
            .get("command")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    }
}
