use std::collections::BTreeMap;

use serde_path_to_error::Segment;

use super::{Issue, SpecErr, WorkflowSpec, issue};

mod clean;

pub(super) fn parse(mut value: toml::Value) -> Result<WorkflowSpec, SpecErr> {
    let mut ignored = Vec::new();
    let mut indices = BTreeMap::new();
    let mut invalid_routes = Vec::new();
    loop {
        let mut unknown = Vec::new();
        let mut callback = |path: serde_ignored::Path<'_>| unknown.push(ignored_path(&path));
        let deserializer = serde_ignored::Deserializer::new(value.clone(), &mut callback);
        let error =
            match serde_path_to_error::deserialize::<_, WorkflowSpec>(deserializer) {
                Ok(mut spec) => {
                    ignored.extend(unknown.into_iter().map(|path| {
                        ignored_issue(&original_path(&path, &indices), "unknown field")
                    }));
                    spec.ignored = ignored;
                    clean::apply(&mut spec, &indices);
                    if let Some((path, source)) = invalid_routes
                        .into_iter()
                        .find(|(path, _)| !spec.ignored.iter().any(|issue| issue.path == *path))
                    {
                        return Err(SpecErr::Toml { path, source });
                    }
                    return Ok(spec);
                }
                Err(error) => error,
            };
        let mut path = error
            .path()
            .iter()
            .filter_map(|segment| match segment {
                Segment::Map { key } => Some(key.clone()),
                Segment::Seq { index } => Some(index.to_string()),
                Segment::Enum { variant } => Some(variant.clone()),
                Segment::Unknown => None,
            })
            .collect::<Vec<_>>();
        let source = error.into_inner();
        if path.len() > 4 && path[0] == "checks" && path[2] == "on" && path[4] == "to" {
            path.truncate(4);
        }
        if path.len() >= 4
            && path[0] == "checks"
            && path[2] == "wake"
            && let Some(table) = at(&value, &path[..4]).and_then(toml::Value::as_table)
        {
            let signal = table.get("signal").map_or_else(
                || Err(serde::de::Error::missing_field("signal")),
                |signal| signal.clone().try_into::<String>(),
            );
            if let Err(source) = signal {
                path.truncate(4);
                path.push("signal".into());
                return Err(SpecErr::Toml {
                    path: original_path(&path, &indices),
                    source,
                });
            }
        }
        if let Some(field) = missing_required(&value, &path) {
            path.push(field.into());
            return Err(SpecErr::Toml {
                path: original_path(&path, &indices),
                source,
            });
        }
        if !optional(&value, &path) {
            return Err(SpecErr::Toml {
                path: original_path(&path, &indices),
                source,
            });
        }
        if path.len() == 4
            && matches!(path[0].as_str(), "checks" | "steps")
            && path[2] == "on"
            && let Some(target) = at_mut(&mut value, &path)
        {
            // A typed placeholder lets cleanup classify dead routes without dropping live ones.
            *target = toml::Value::String("@user".into());
            invalid_routes.push((original_path(&path, &indices), source));
            continue;
        }
        if path.len() > 4 && path[0] == "checks" && path[2] == "wake" {
            path.truncate(4);
        }
        if path.len() > 3 && path[0] == "steps" && path[2] == "max-visits-per-outcome" {
            path.truncate(3);
        }
        let dotted = original_path(&path, &indices);
        if !remove(&mut value, &path, &mut indices) {
            return Err(SpecErr::Toml {
                path: dotted,
                source,
            });
        }
        ignored.push(ignored_issue(&dotted, source.message()));
    }
}

fn ignored_path(path: &serde_ignored::Path<'_>) -> Vec<String> {
    use serde_ignored::Path;
    match path {
        Path::Root => Vec::new(),
        Path::Map { parent, key } => {
            let mut path = ignored_path(parent);
            path.push(key.clone());
            path
        }
        Path::Seq { parent, index } => {
            let mut path = ignored_path(parent);
            path.push(index.to_string());
            path
        }
        Path::Some { parent }
        | Path::NewtypeStruct { parent }
        | Path::NewtypeVariant { parent } => ignored_path(parent),
    }
}

fn original_path(path: &[String], indices: &BTreeMap<String, Vec<usize>>) -> String {
    let mut parts = path.to_vec();
    if parts.len() >= 4
        && parts[0] == "checks"
        && parts[2] == "wake"
        && let Some(index) = indices
            .get(&parts[..3].join("."))
            .and_then(|indices| indices.get(parts[3].parse::<usize>().ok()?))
    {
        parts[3] = index.to_string();
    }
    if parts.is_empty() {
        "workflow".into()
    } else {
        parts.join(".")
    }
}

fn at<'a>(value: &'a toml::Value, path: &[String]) -> Option<&'a toml::Value> {
    path.iter().try_fold(value, |value, key| match value {
        toml::Value::Table(table) => table.get(key),
        toml::Value::Array(array) => array.get(key.parse::<usize>().ok()?),
        _ => None,
    })
}

fn at_mut<'a>(value: &'a mut toml::Value, path: &[String]) -> Option<&'a mut toml::Value> {
    path.iter().try_fold(value, |value, key| match value {
        toml::Value::Table(table) => table.get_mut(key),
        toml::Value::Array(array) => array.get_mut(key.parse::<usize>().ok()?),
        _ => None,
    })
}

fn missing_required(value: &toml::Value, path: &[String]) -> Option<&'static str> {
    let fields: &[&str] = match path
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => &["workflow"],
        ["workflow"] => &["scope", "start"],
        ["checks", _, "cases", _] => &["when", "outcome"],
        ["checks", _, "wake", _] => &["signal"],
        _ => &[],
    };
    let table = at(value, path)?.as_table()?;
    fields
        .iter()
        .copied()
        .find(|field| !table.contains_key(*field))
}

fn optional(value: &toml::Value, path: &[String]) -> bool {
    let parts = path.iter().map(String::as_str).collect::<Vec<_>>();
    match parts.as_slice() {
        [] | ["workflow"] | ["workflow", "scope" | "start", ..] => false,
        ["workflow", "on", ..] | ["checks" | "steps", _, "on"] => false,
        ["checks", _, "cases", _, "for", ..] => true,
        ["checks", _, "cases", ..] => false,
        ["checks", _, "sleep", ..] => false,
        ["checks", _, "when", ..] => at(value, &path[..2])
            .and_then(toml::Value::as_table)
            .is_none_or(|check| !check.contains_key("sleep")),
        ["checks", _, "wake", _, "signal", ..] => false,
        _ => true,
    }
}

fn remove(
    value: &mut toml::Value,
    path: &[String],
    indices: &mut BTreeMap<String, Vec<usize>>,
) -> bool {
    let Some((key, parent)) = path.split_last() else {
        return false;
    };
    let Some(value) = at_mut(value, parent) else {
        return false;
    };
    match value {
        toml::Value::Table(table) => table.remove(key).is_some(),
        toml::Value::Array(array) => {
            let Ok(index) = key.parse::<usize>() else {
                return false;
            };
            if index >= array.len() {
                return false;
            }
            indices
                .entry(parent.join("."))
                .or_insert_with(|| (0..array.len()).collect())
                .remove(index);
            array.remove(index);
            true
        }
        _ => false,
    }
}

fn ignored_issue(path: &str, reason: impl std::fmt::Display) -> Issue {
    issue(path, format!("ignored field: {reason}"))
}
