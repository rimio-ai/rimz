use std::collections::BTreeSet;

use super::*;

const ACTIVE_ZELLIJ_DEFAULTS: &[&str] = &[
    "mouse_click_through",
    "focus_follows_mouse",
    "session_serialization",
    "auto_layout",
];

const ACTIVE_TMUX_DEFAULTS: &[&str] = &[
    "mouse",
    "focus_events",
    "history_limit",
    "allow_passthrough",
    "set_clipboard",
    "extended_keys",
    "extended_keys_format",
    "escape_time_ms",
    "renumber_windows",
    "aggressive_resize",
    "pane_border_status",
    "pane_border_lines",
];

#[test]
fn template_defaults_deserialize_to_machine_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("config.toml"),
        uncomment_default_lines(MachineConfig::template_core()),
    )
    .expect("write config template");
    std::fs::write(
        dir.path().join("theme.toml"),
        uncomment_default_lines(MachineConfig::template_theme()),
    )
    .expect("write theme template");
    std::fs::write(
        dir.path().join("agents.toml"),
        uncomment_default_lines(MachineConfig::template_agents()),
    )
    .expect("write agents template");
    let parsed =
        MachineConfig::load_from(&dir.path().join("config.toml")).expect("template defaults parse");

    assert_eq!(parsed, MachineConfig::default());
}

#[test]
fn template_covers_serialized_default_leaves() {
    let serialized = toml::to_string(&MachineConfig::default()).expect("serialize defaults");
    let value: toml::Value = toml::from_str(&serialized).expect("parse serialized defaults");
    let mut expected = BTreeSet::new();
    collect_leaf_paths("", &value, &mut expected);

    let template = all_template_default_paths();
    for path in &expected {
        assert!(
            template.contains(path),
            "template is missing commented default for {path}"
        );
    }

    let allowed_template_only = BTreeSet::<String>::new();
    for path in template.difference(&expected) {
        assert!(
            allowed_template_only.contains(path),
            "template default {path} is not a serialized default leaf"
        );
    }
}

#[test]
fn template_lists_optional_sidebar_theme_slots() {
    let template = MachineConfig::template_theme();
    for setting in ["mode", "scheme"] {
        assert!(
            template.contains(&format!("## {setting} = ")),
            "template is missing optional theme setting {setting}"
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
        "body",
        "muted",
        "faint",
        "rule",
        "selection",
        "selection_bg",
    ] {
        assert!(
            template.contains(&format!("## {slot} = ")),
            "template is missing optional theme slot {slot}"
        );
    }
}

fn all_template_default_paths() -> BTreeSet<String> {
    let mut out = template_default_paths(MachineConfig::template_core());
    out.extend(
        template_default_paths(MachineConfig::template_theme())
            .into_iter()
            .map(|path| {
                path.strip_prefix("colors.")
                    .map(|rest| format!("theme.colors.{rest}"))
                    .unwrap_or(path)
            }),
    );
    out.extend(template_default_paths(MachineConfig::template_agents()));
    out
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
        let assignment = commented_default_assignment(line).or_else(|| {
            if section.len() == 1 && section[0] == "zellij" {
                active_default_assignment(line)
                    .filter(|(key, _)| ACTIVE_ZELLIJ_DEFAULTS.contains(key))
            } else if section.len() == 1 && section[0] == "tmux" {
                active_default_assignment(line)
                    .filter(|(key, _)| ACTIVE_TMUX_DEFAULTS.contains(key))
            } else {
                None
            }
        });
        let Some((key, value)) = assignment else {
            continue;
        };
        let mut path = section.clone();
        path.push(key.to_owned());
        let snippet = format!("value = {value}");
        let parsed = toml::from_str::<toml::Value>(&snippet).expect("template value parses");
        let value = parsed.get("value").expect("template value key");
        collect_leaf_paths(&path.join("."), value, &mut paths);
    }
    if template.contains("[agents.teams.peer]") && template.contains("layout = \"claude,codex\"") {
        paths.insert("agents.teams.peer.layout".to_owned());
        paths.insert("agents.teams.peer.roles".to_owned());
    }
    paths
}

fn commented_default_key(line: &str) -> Option<&str> {
    commented_default_assignment(line).map(|(key, _)| key)
}

fn commented_default_assignment(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start().strip_prefix("# ")?;
    default_assignment(rest)
}

fn active_default_assignment(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim_start();
    if rest.starts_with('#') {
        return None;
    }
    default_assignment(rest)
}

fn default_assignment(rest: &str) -> Option<(&str, &str)> {
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
