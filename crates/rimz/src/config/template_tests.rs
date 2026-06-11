use std::collections::BTreeSet;

use super::*;

#[test]
fn template_defaults_deserialize_to_machine_defaults() {
    let uncommented = uncomment_default_lines(MachineConfig::template());
    let parsed: MachineConfig = toml::from_str(&uncommented).expect("template defaults parse");
    assert_eq!(parsed, MachineConfig::default());
}

#[test]
fn template_covers_serialized_default_leaves() {
    let serialized = toml::to_string(&MachineConfig::default()).expect("serialize defaults");
    let value: toml::Value = toml::from_str(&serialized).expect("parse serialized defaults");
    let mut expected = BTreeSet::new();
    collect_leaf_paths("", &value, &mut expected);

    let template = template_default_paths(MachineConfig::template());
    for path in &expected {
        assert!(
            template.contains(path),
            "template is missing commented default for {path}"
        );
    }

    let allowed_template_only = BTreeSet::from(["sidebar.provider_list".to_owned()]);
    for path in template.difference(&expected) {
        assert!(
            allowed_template_only.contains(path),
            "template default {path} is not a serialized default leaf"
        );
    }
}

#[test]
fn template_lists_optional_sidebar_theme_slots() {
    let template = MachineConfig::template();
    for setting in ["mode", "scheme"] {
        assert!(
            template.contains(&format!("## {setting} = ")),
            "template is missing optional sidebar theme setting {setting}"
        );
    }
    for slot in [
        "good",
        "warn",
        "caution",
        "alarm",
        "accent",
        "cool",
        "meta",
        "soft",
        "dim",
        "faint",
        "rule",
        "selection",
    ] {
        assert!(
            template.contains(&format!("## {slot} = ")),
            "template is missing optional sidebar theme slot {slot}"
        );
    }
}

fn uncomment_default_lines(template: &str) -> String {
    let mut out = String::new();
    for line in template.lines() {
        if commented_default_key(line).is_some() {
            let trimmed = line.trim_start();
            let indent_len = line.len() - trimmed.len();
            out.push_str(&line[..indent_len]);
            out.push_str(trimmed.strip_prefix("# ").expect("comment prefix"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn template_default_paths(template: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let mut section: Vec<String> = Vec::new();
    for line in template.lines() {
        let trimmed = line.trim();
        if let Some(raw) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = raw.split('.').map(ToOwned::to_owned).collect();
            continue;
        }
        let Some((key, value)) = commented_default_assignment(line) else {
            continue;
        };
        let mut path = section.clone();
        path.push(key.to_owned());
        let snippet = format!("value = {value}");
        let parsed = toml::from_str::<toml::Value>(&snippet).expect("template value parses");
        let value = parsed.get("value").expect("template value key");
        collect_leaf_paths(&path.join("."), value, &mut paths);
    }
    paths
}

fn commented_default_key(line: &str) -> Option<&str> {
    commented_default_assignment(line).map(|(key, _)| key)
}

fn commented_default_assignment(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().strip_prefix("# ")?;
    let (key, value) = rest.split_once(" = ")?;
    if !key
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return None;
    }
    let value = value.split_once(" #").map_or(value, |(value, _)| value);
    Some((key, value.trim_end()))
}

fn collect_leaf_paths(prefix: &str, value: &toml::Value, out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let next = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_leaf_paths(&next, value, out);
            }
        }
        _ => {
            out.insert(prefix.to_owned());
        }
    }
}
