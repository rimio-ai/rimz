//! Managed JSON command wrappers for provider statusline callbacks.

use std::sync::LazyLock;

use serde_json::{Map, Value};

use super::StatusLineChange;

pub(crate) const RIMZ_MANAGED_KEY: &str = "_rimz_managed";
pub(crate) const RIMZ_WRAPPED_KEY: &str = "_rimz_wrapped";

#[derive(Clone, Copy)]
pub(crate) enum RenderingOptions {
    All,
    Only(&'static [&'static str]),
}

#[derive(Clone, Copy)]
pub(crate) enum WrapPolicy {
    Any,
    CommandMode,
    ObjectOnly,
}

pub(crate) struct ManagedStatusLineSpec {
    pub key_path: &'static [&'static str],
    pub command: &'static str,
    pub command_marker: &'static str,
    pub rendering_options: RenderingOptions,
    pub wrap_policy: WrapPolicy,
    pub required_for_install: bool,
}

pub(crate) fn is_managed(root: &Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    matches!(value_at(root, spec.key_path), Some(Value::Object(object)) if object_is_managed(object))
}

pub(crate) fn install_satisfied(root: &Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    if !spec.required_for_install {
        return true;
    }
    let Some(parent) = parent_at(root, spec.key_path) else {
        return parent_path_exists(root, spec.key_path);
    };
    let key = spec.key_path.last().copied().unwrap_or_default();
    match parent.get(key) {
        Some(Value::Object(object)) if object_is_managed(object) => object
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(spec.command_marker)),
        Some(Value::Object(object)) if matches!(spec.wrap_policy, WrapPolicy::CommandMode) => {
            object.get("type").and_then(Value::as_str) != Some("command")
        }
        Some(_) if matches!(spec.wrap_policy, WrapPolicy::CommandMode) => true,
        Some(_) => false,
        None => false,
    }
}

pub(crate) fn upsert(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) {
    let Some(parent) = parent_mut(root, spec.key_path, true) else {
        return;
    };
    let key = spec.key_path.last().copied().unwrap_or_default();
    let existing = parent.get(key).cloned();
    if !wrap_allowed(existing.as_ref(), spec.wrap_policy) {
        return;
    }
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
        for (name, value) in source {
            if name != "command"
                && name != RIMZ_MANAGED_KEY
                && name != RIMZ_WRAPPED_KEY
                && retains_rendering_option(spec.rendering_options, name)
            {
                entry.insert(name.clone(), value.clone());
            }
        }
    }
    entry.insert("type".to_owned(), Value::String("command".to_owned()));
    entry.insert("command".to_owned(), Value::String(spec.command.to_owned()));
    entry.insert(RIMZ_MANAGED_KEY.to_owned(), Value::Bool(true));
    if let Some(original) = original {
        entry.insert(RIMZ_WRAPPED_KEY.to_owned(), original);
    }
    parent.insert(key.to_owned(), Value::Object(entry));
}

pub(crate) fn strip(root: &mut Map<String, Value>, spec: &ManagedStatusLineSpec) -> bool {
    if !is_managed(root, spec) {
        return false;
    }
    let key = spec.key_path.last().copied().unwrap_or_default();
    let original = parent_mut(root, spec.key_path, false)
        .and_then(|parent| parent.remove(key))
        .and_then(|value| match value {
            Value::Object(mut object) => object
                .remove(RIMZ_WRAPPED_KEY)
                .and_then(|value| non_recursive_value(value, spec)),
            _ => None,
        });
    if let Some(original) = original {
        if let Some(parent) = parent_mut(root, spec.key_path, false) {
            parent.insert(key.to_owned(), original);
        }
    } else {
        remove_empty_parents(root, spec.key_path);
    }
    true
}

pub(crate) fn classify(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> Option<StatusLineChange> {
    let parent = parent_at(root, spec.key_path)?;
    let key = spec.key_path.last().copied().unwrap_or_default();
    match parent.get(key) {
        None => Some(StatusLineChange::Added),
        Some(Value::Object(object)) if object_is_managed(object) => {
            Some(StatusLineChange::Unchanged)
        }
        Some(Value::Object(object))
            if matches!(spec.wrap_policy, WrapPolicy::ObjectOnly)
                && object
                    .get("command")
                    .and_then(Value::as_str)
                    .is_none_or(|command| command.trim().is_empty()) =>
        {
            Some(StatusLineChange::Added)
        }
        Some(value) if wrap_allowed(Some(value), spec.wrap_policy) => {
            Some(StatusLineChange::Wrapping {
                original: display(value),
            })
        }
        Some(_) => None,
    }
}

pub(crate) fn wrapped_command(
    root: &Map<String, Value>,
    spec: &ManagedStatusLineSpec,
) -> Option<String> {
    let Value::Object(object) = value_at(root, spec.key_path)? else {
        return None;
    };
    if !object_is_managed(object) {
        return None;
    }
    command(object.get(RIMZ_WRAPPED_KEY)?)
        .filter(|command| !command.contains(spec.command_marker))
        .map(ToOwned::to_owned)
}

fn wrap_allowed(value: Option<&Value>, policy: WrapPolicy) -> bool {
    match policy {
        WrapPolicy::Any => true,
        WrapPolicy::ObjectOnly => value.is_none_or(Value::is_object),
        WrapPolicy::CommandMode => match value {
            None => true,
            Some(Value::Object(object)) if object_is_managed(object) => true,
            Some(Value::Object(object)) => {
                object.get("type").and_then(Value::as_str) == Some("command")
            }
            Some(_) => false,
        },
    }
}

fn value_at<'a>(root: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (key, parents) = path.split_last()?;
    let mut object = root;
    for parent in parents {
        object = object.get(*parent)?.as_object()?;
    }
    object.get(*key)
}

fn parent_at<'a>(root: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Map<String, Value>> {
    let (_, parents) = path.split_last()?;
    let mut object = root;
    for parent in parents {
        match object.get(*parent) {
            None => return Some(&EMPTY_OBJECT),
            Some(Value::Object(next)) => object = next,
            Some(_) => return None,
        }
    }
    Some(object)
}

static EMPTY_OBJECT: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);

fn parent_path_exists(root: &Map<String, Value>, path: &[&str]) -> bool {
    let Some((_, parents)) = path.split_last() else {
        return false;
    };
    let mut object = root;
    for parent in parents {
        match object.get(*parent) {
            None => return false,
            Some(Value::Object(next)) => object = next,
            Some(_) => return true,
        }
    }
    false
}

fn parent_mut<'a>(
    root: &'a mut Map<String, Value>,
    path: &[&str],
    create: bool,
) -> Option<&'a mut Map<String, Value>> {
    let (_, parents) = path.split_last()?;
    let mut object = root;
    for parent in parents {
        if !object.contains_key(*parent) {
            if !create {
                return None;
            }
            object.insert((*parent).to_owned(), Value::Object(Map::new()));
        }
        object = object.get_mut(*parent)?.as_object_mut()?;
    }
    Some(object)
}

fn remove_empty_parents(root: &mut Map<String, Value>, path: &[&str]) {
    if path.len() != 2 {
        return;
    }
    let parent = path[0];
    if root
        .get(parent)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        root.remove(parent);
    }
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
