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
    type Err = SkillListErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || matches!(value, "." | "..")
            || value.contains(['/', '\\', ':'])
            || value.chars().any(char::is_whitespace)
        {
            return Err(SkillListErr::InvalidName(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for SkillName {
    type Error = SkillListErr;

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

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SkillListErr {
    #[error(
        "invalid skill name `{0}`: list bare names (the `:mode` suffix is gone; listed skills are model-callable, unlisted ones user-only)"
    )]
    InvalidName(String),
    #[error("duplicate skill name `{0}`")]
    DuplicateName(SkillName),
}

pub fn validate_skill_list(skills: &[SkillName]) -> Result<(), SkillListErr> {
    let mut names = BTreeSet::new();
    for skill in skills {
        if !names.insert(skill) {
            return Err(SkillListErr::DuplicateName(skill.clone()));
        }
    }
    Ok(())
}

pub(crate) fn deserialize_optional_skill_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<SkillName>>, D::Error>
where
    D: Deserializer<'de>,
{
    let skills = Option::<Vec<SkillName>>::deserialize(deserializer)?;
    if let Some(skills) = &skills {
        validate_skill_list(skills).map_err(serde::de::Error::custom)?;
    }
    Ok(skills)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_names_parse_and_round_trip() {
        for input in ["merge", "review-pr", "rimz.skills"] {
            let name: SkillName = input.parse().unwrap();
            assert_eq!(name.as_str(), input);
            assert_eq!(serde_json::to_value(&name).unwrap(), input);
            assert_eq!(
                serde_json::from_str::<SkillName>(&serde_json::to_string(&name).unwrap()).unwrap(),
                name
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
            "merge:auto",
            "merge:off",
            "merge:manual",
        ] {
            let error = input.parse::<SkillName>().unwrap_err();
            assert_eq!(error, SkillListErr::InvalidName(input.to_owned()));
            assert!(error.to_string().contains("the `:mode` suffix is gone"));
        }
    }

    #[test]
    fn profile_skill_lists_reject_duplicates_and_preserve_empty() {
        assert!(
            toml::from_str::<crate::config::Profile>(
                "agent = 'claude'\nskills = ['merge', 'merge']"
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
