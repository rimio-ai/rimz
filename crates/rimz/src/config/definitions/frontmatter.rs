//! Strict YAML frontmatter and the fields inherited along definition chains.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, de::IgnoredAny};

use crate::agents::PermissionMode;

use super::DefinitionErr;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct AgentFrontmatter {
    #[serde(rename = "name")]
    _name: Option<String>,
    pub agent: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub mode: Option<PermissionMode>,
    pub effort: Option<String>,
    #[serde(default, deserialize_with = "token_count")]
    pub auto_compact: Option<String>,
    #[serde(default, deserialize_with = "number_text")]
    pub budget: Option<String>,
    pub model_reminder: Option<bool>,
    #[serde(default, deserialize_with = "list")]
    pub traits: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub subagents: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub skills: Option<Vec<String>>,
}

impl AgentFrontmatter {
    pub(super) fn inherit(&mut self, parent: &Self) {
        macro_rules! inherit {
            ($($field:ident),* $(,)?) => { $(
                if self.$field.is_none() {
                    self.$field = parent.$field.clone();
                }
            )* };
        }
        inherit!(
            model,
            mode,
            effort,
            auto_compact,
            budget,
            tools,
            skills,
            subagents,
            model_reminder
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BaseFrontmatter {
    pub description: Option<String>,
}

fn list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<String>>, D::Error> {
    Ok(Some(
        Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default(),
    ))
}

fn token_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Count {
        Text(String),
        Integer(u64),
    }
    Ok(Some(match Count::deserialize(deserializer)? {
        Count::Text(text) => text,
        Count::Integer(count) => count.to_string(),
    }))
}

fn number_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Text(String),
        Integer(i64),
        Float(f64),
    }
    Ok(Some(match Number::deserialize(deserializer)? {
        Number::Text(text) => text,
        Number::Integer(number) => number.to_string(),
        Number::Float(number) => number.to_string(),
    }))
}

pub(super) fn split<'a>(path: &Path, text: &'a str) -> Result<(&'a str, &'a str), DefinitionErr> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Err(DefinitionErr::new(path, "is missing YAML frontmatter"));
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((&text[first.len()..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(DefinitionErr::new(
        path,
        "never closes its frontmatter with `---`",
    ))
}

pub(super) fn parse<T: serde::de::DeserializeOwned>(
    path: &Path,
    yaml: &str,
) -> Result<T, DefinitionErr> {
    let keys: BTreeMap<String, IgnoredAny> = serde_saphyr::from_str(yaml).map_err(|error| {
        DefinitionErr::new(
            path,
            format!("frontmatter is not a mapping or is malformed: {error}"),
        )
    })?;
    for (key, message) in [
        (
            "soul",
            "still selects `soul:`; put its prompt body in the definition",
        ),
        (
            "meka",
            "still sets `meka:`; name the runtime base in `agent:`",
        ),
        (
            "signals",
            "sets `signals:`, which rimz reads on a team role; declare the binding on the role that receives the event",
        ),
        (
            "flip-compact",
            "sets `flip-compact:`, which rimz reads on a team role; a solo seat flips no stage",
        ),
    ] {
        if keys.contains_key(key) {
            return Err(DefinitionErr::new(path, message));
        }
    }
    serde_saphyr::from_str(yaml)
        .map_err(|error| DefinitionErr::new(path, format!("malformed frontmatter: {error}")))
}

pub(super) fn description(path: &Path, text: Option<&str>) -> Result<String, DefinitionErr> {
    let text = text.unwrap_or("").trim();
    if text.is_empty() {
        return Err(DefinitionErr::new(path, "has no non-empty `description:`"));
    }
    if text.contains(['\n', '\r']) {
        return Err(DefinitionErr::new(
            path,
            "has a multi-line `description:`; keep it to one line",
        ));
    }
    Ok(text.to_owned())
}
