//! Profile skill selections and their string configuration grammar.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SkillName(String);

impl SkillName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SkillName {
    type Err = SkillSpecErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || matches!(value, "." | "..")
            || value.contains(['/', '\\', ':'])
            || value.chars().any(char::is_whitespace)
        {
            return Err(SkillSpecErr::InvalidName(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for SkillName {
    type Error = SkillSpecErr;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SkillName> for String {
    fn from(value: SkillName) -> Self {
        value.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkillMode {
    #[default]
    Auto,
    Off,
}

impl FromStr for SkillMode {
    type Err = SkillSpecErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            "manual" => Err(SkillSpecErr::ManualUnsupported),
            _ => Err(SkillSpecErr::UnknownMode(value.to_owned())),
        }
    }
}

impl fmt::Display for SkillMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Off => "off",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SkillSpec {
    pub name: SkillName,
    pub mode: SkillMode,
}

impl FromStr for SkillSpec {
    type Err = SkillSpecErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, mode) = match value.split_once(':') {
            Some((_, mode)) if mode.contains(':') => {
                return Err(SkillSpecErr::InvalidSpec(value.to_owned()));
            }
            Some((name, mode)) => (name, mode.parse()?),
            None => (value, SkillMode::Auto),
        };
        Ok(Self {
            name: name.parse()?,
            mode,
        })
    }
}

impl TryFrom<String> for SkillSpec {
    type Error = SkillSpecErr;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SkillSpec> for String {
    fn from(value: SkillSpec) -> Self {
        value.to_string()
    }
}

impl fmt::Display for SkillSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.name, self.mode)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SkillSpecErr {
    #[error("invalid skill name `{0}`: use a nonempty name without paths, colons, or whitespace")]
    InvalidName(String),
    #[error("invalid skill spec `{0}`: expected name[:mode] with at most one colon")]
    InvalidSpec(String),
    #[error(
        "skill mode `manual` is not yet supported; rewritten skill copies are planned as a follow-up; use auto or off"
    )]
    ManualUnsupported,
    #[error("unknown skill mode `{0}`: expected auto or off")]
    UnknownMode(String),
    #[error("duplicate skill name `{0}`")]
    DuplicateName(SkillName),
}

pub fn validate_skill_list(skills: &[SkillSpec]) -> Result<(), SkillSpecErr> {
    let mut names = BTreeSet::new();
    for skill in skills {
        if !names.insert(&skill.name) {
            return Err(SkillSpecErr::DuplicateName(skill.name.clone()));
        }
    }
    Ok(())
}

pub(crate) fn deserialize_skill_list<'de, D>(deserializer: D) -> Result<Vec<SkillSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    let skills = Vec::<SkillSpec>::deserialize(deserializer)?;
    validate_skill_list(&skills).map_err(serde::de::Error::custom)?;
    Ok(skills)
}

pub(super) fn deserialize_optional_skill_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<SkillSpec>>, D::Error>
where
    D: Deserializer<'de>,
{
    let skills = Option::<Vec<SkillSpec>>::deserialize(deserializer)?;
    if let Some(skills) = &skills {
        validate_skill_list(skills).map_err(serde::de::Error::custom)?;
    }
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_specs_parse_and_round_trip() {
        for (input, mode) in [
            ("merge", SkillMode::Auto),
            ("merge:auto", SkillMode::Auto),
            ("merge:off", SkillMode::Off),
        ] {
            let spec: SkillSpec = input.parse().unwrap();
            assert_eq!(spec.name.as_str(), "merge");
            assert_eq!(spec.mode, mode);
            assert_eq!(
                serde_json::from_str::<SkillSpec>(&serde_json::to_string(&spec).unwrap()).unwrap(),
                spec
            );
        }
        for input in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a b",
            "a\tb",
            "a:",
            "a:auto:off",
            "a:unknown",
        ] {
            assert!(input.parse::<SkillSpec>().is_err(), "{input:?}");
        }
        assert_eq!(
            "merge:manual".parse::<SkillSpec>(),
            Err(SkillSpecErr::ManualUnsupported)
        );
    }

    #[test]
    fn profile_skill_lists_reject_duplicates_and_preserve_empty() {
        assert!(
            toml::from_str::<crate::config::Profile>(
                "agent = 'claude'\nskills = ['merge', 'merge:off']"
            )
            .unwrap_err()
            .to_string()
            .contains("duplicate skill name `merge`")
        );
        let omitted: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
        let empty: crate::config::Profile =
            toml::from_str("agent = 'claude'\nskills = []").unwrap();
        assert_eq!(omitted.skills, None);
        assert_eq!(empty.skills, Some(Vec::new()));
    }

    #[test]
    fn isolation_defaults_to_host_and_round_trips() {
        let default: crate::config::AgentsConfig = toml::from_str("").unwrap();
        assert_eq!(default.isolation, crate::config::Isolation::Host);
        let sandbox: crate::config::AgentsConfig = toml::from_str("isolation = 'sandbox'").unwrap();
        assert_eq!(sandbox.isolation.to_string(), "sandbox");
        assert_eq!(
            toml::from_str::<crate::config::AgentsConfig>(&toml::to_string(&sandbox).unwrap())
                .unwrap(),
            sandbox
        );
    }
}
