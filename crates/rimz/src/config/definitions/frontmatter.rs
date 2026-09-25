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
    pub isolation: Option<crate::config::Isolation>,
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
            isolation,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TeamFrontmatter {
    pub name: Option<String>,
    #[serde(rename = "description")]
    _description: Option<String>,
    pub layout: Option<String>,
    pub leader: Option<String>,
    #[serde(default, deserialize_with = "list")]
    pub stages: Option<Vec<String>>,
    #[serde(default, deserialize_with = "list")]
    pub traits: Option<Vec<String>>,
    #[serde(default)]
    pub roles: Vec<RoleFrontmatter>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(super) struct RoleFrontmatter {
    pub agent: String,
    pub role: Option<String>,
    #[serde(default, deserialize_with = "list")]
    pub owns: Option<Vec<String>>,
    pub model: Option<String>,
    pub mode: Option<PermissionMode>,
    pub effort: Option<String>,
    #[serde(default, deserialize_with = "token_count")]
    pub auto_compact: Option<String>,
    #[serde(default, deserialize_with = "flip_compact")]
    pub flip_compact: Option<String>,
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
    #[serde(default, deserialize_with = "list")]
    pub signals: Option<Vec<SignalFrontmatter>>,
    #[serde(rename = "meka", default, deserialize_with = "retired_role_meka")]
    _meka: (),
}

impl RoleFrontmatter {
    pub(super) fn overlay(&self) -> AgentFrontmatter {
        AgentFrontmatter {
            model: self.model.clone(),
            mode: self.mode,
            effort: self.effort.clone(),
            auto_compact: self.auto_compact.clone(),
            budget: self.budget.clone(),
            model_reminder: self.model_reminder,
            tools: self.tools.clone(),
            subagents: self.subagents.clone(),
            skills: self.skills.clone(),
            ..AgentFrontmatter::default()
        }
    }
}

fn retired_role_meka<'de, D: Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom(
        "role still sets `meka:`; its model decides the runtime",
    ))
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum SignalFrontmatter {
    Selector(String),
    Binding(SignalBindingFrontmatter),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignalBindingFrontmatter {
    pub signal: String,
    #[serde(rename = "match")]
    pub matches: Option<BTreeMap<String, String>>,
    pub prompt: Option<String>,
}

fn list<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Ok(Some(
        Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default(),
    ))
}

fn flip_compact<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Count {
        Boolean(bool),
        Text(String),
        Integer(u64),
    }
    let invalid = || {
        serde::de::Error::custom(
            "flip-compact takes a token count such as '120k', a percentage such as '70%', or 'off'",
        )
    };
    Ok(Some(
        match Count::deserialize(deserializer).map_err(|_| invalid())? {
            Count::Boolean(false) => "off".to_owned(),
            Count::Boolean(true) => return Err(invalid()),
            Count::Text(text) => text,
            Count::Integer(count) => count.to_string(),
        },
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

pub(crate) fn split<'a>(path: &Path, text: &'a str) -> Result<(&'a str, &'a str), DefinitionErr> {
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
