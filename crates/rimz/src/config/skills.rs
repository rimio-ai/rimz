//! Profile skill selections and their string configuration grammar.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SkillName(String);

impl SkillName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// An absent list permits every skill; a present list permits only its members.
pub fn skill_is_callable(skills: Option<&[SkillName]>, name: &str) -> bool {
    skills.is_none_or(|skills| skills.iter().any(|skill| skill.as_str() == name))
}

impl FromStr for SkillName {
    type Err = SkillListErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.starts_with('+') {
            return Err(SkillListErr::PlusEntry(value.to_owned()));
        }
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
    #[error(
        "invalid skill entry `{0}`: `+` entries add to an inherited list in Markdown definitions only; list the bare name"
    )]
    PlusEntry(String),
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

/// The RimZ capabilities `text` names in the `rimz(x)` notation, each as `rimz-x`: a
/// lowercase name (`[a-z][a-z0-9-]*`) between `rimz(` and `)`, where `rimz` follows no
/// identifier character, so `spawn_detached_rimz(x)` names nothing. Any other casing or shape
/// is plain text, and a code span counts like any other text.
pub fn rimz_references(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (at, opening) in text.match_indices("rimz(") {
        if text[..at]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_alphanumeric() || before == '_')
        {
            continue;
        }
        let Some((name, _)) = text[at + opening.len()..].split_once(')') else {
            continue;
        };
        let mut chars = name.chars();
        if chars.next().is_some_and(|first| first.is_ascii_lowercase())
            && chars.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-')
        {
            names.insert(format!("rimz-{name}"));
        }
    }
    names
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
    fn plus_entries_are_refused_outside_a_markdown_list() {
        let error = "+commit".parse::<SkillName>().unwrap_err();
        assert_eq!(error, SkillListErr::PlusEntry("+commit".to_owned()));
        assert!(
            error.to_string().contains("in Markdown definitions only"),
            "{error}"
        );
        let error =
            toml::from_str::<crate::config::Profile>("agent = 'claude'\nskills = ['+commit']")
                .unwrap_err()
                .to_string();
        assert!(error.contains("skill entry `+commit`"), "{error}");
    }

    #[test]
    fn rimz_references_take_lowercase_names_after_a_non_identifier_boundary() {
        let text = "Use rimz(wait), `rimz(lsp)`, and (rimz(workflow-write)); rimz(wait) again.\n\
                    rimz(notes2) and x-rimz(control) count. spawn_detached_rimz(foo), \
                    rimz2(bar), Rimz(baz), RIMZ(qux), rimz(Wait), rimz( wait), rimz(), \
                    rimz(1x), rimz(-x), rimz(a_b), rimz(a b), Skill(rimz-message), rimz(open";
        assert_eq!(
            rimz_references(text).into_iter().collect::<Vec<_>>(),
            [
                "rimz-control",
                "rimz-lsp",
                "rimz-notes2",
                "rimz-wait",
                "rimz-workflow-write"
            ]
        );
        assert_eq!(
            rimz_references("rimz(wait)")
                .into_iter()
                .collect::<Vec<_>>(),
            ["rimz-wait"]
        );
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
        let default: crate::config::agents::AgentsConfig = toml::from_str("").unwrap();
        assert_eq!(default.isolation, crate::config::Isolation::Host);
        let sandbox: crate::config::agents::AgentsConfig =
            toml::from_str("isolation = 'scoped'").unwrap();
        assert_eq!(sandbox.isolation.to_string(), "scoped");
        assert_eq!(
            toml::from_str::<crate::config::agents::AgentsConfig>(
                &toml::to_string(&sandbox).unwrap()
            )
            .unwrap(),
            sandbox
        );
    }
}
