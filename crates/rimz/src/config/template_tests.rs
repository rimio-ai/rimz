use std::collections::BTreeSet;

use super::*;

const ACTIVE_ZELLIJ_DEFAULTS: &[&str] = &[
    "mouse_click_through",
    "focus_follows_mouse",
    "session_serialization",
    "auto_layout",
];

#[test]
fn template_defaults_deserialize_to_machine_defaults() {
    let uncommented = uncomment_default_lines(MachineConfig::template());
    let parsed: MachineConfig = toml::from_str(&uncommented).expect("template defaults parse");

    // The template ships the glyph groups as active defaults (paste-to-replace),
    // so a fresh config pins them to the Unicode preset rather than leaving them
    // unset. Those values render identically to the default glyph set
    // (verified in the render crate's `template_glyph_defaults_match_unicode_preset`);
    // normalize them away before comparing the rest of the config.
    assert!(
        !parsed.sidebar.glyphs.is_unset(),
        "template ships active glyph defaults"
    );
    let mut normalized = parsed.clone();
    normalized.sidebar.glyphs = super::SidebarGlyphsConfig::default();
    assert_eq!(normalized, MachineConfig::default());
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
        "body",
        "muted",
        "faint",
        "rule",
        "selection",
        "selection_bg",
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
        let assignment = commented_default_assignment(line).or_else(|| {
            if section.len() == 1 && section[0] == "zellij" {
                active_default_assignment(line)
                    .filter(|(key, _)| ACTIVE_ZELLIJ_DEFAULTS.contains(key))
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
