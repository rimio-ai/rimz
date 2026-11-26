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

/// Which prompt shorthand form a [`PromptReference`] matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptReferenceKind {
    Rimz,
    Skill,
    Subagent,
}

/// One shorthand reference (`rimz(x)`, `skill(x)`, or `subagent(x)`) located in
/// a prompt text, with the name inside the parentheses and its full byte range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptReference<'a> {
    pub kind: PromptReferenceKind,
    pub name: &'a str,
    pub range: std::ops::Range<usize>,
}

const PROMPT_REFERENCE_KEYWORDS: [(&str, PromptReferenceKind); 3] = [
    ("rimz", PromptReferenceKind::Rimz),
    ("skill", PromptReferenceKind::Skill),
    ("subagent", PromptReferenceKind::Subagent),
];

/// Scan `text` in byte order for `rimz(x)`, `skill(x)`, and `subagent(x)`
/// references. Each keyword is case-sensitive and must follow no identifier
/// character; code spans count like other text.
pub fn prompt_references(text: &str) -> Vec<PromptReference<'_>> {
    let mut found = Vec::new();
    for (open, _) in text.match_indices('(') {
        let Some((keyword, kind)) = PROMPT_REFERENCE_KEYWORDS
            .into_iter()
            .find(|(keyword, _)| text[..open].ends_with(keyword))
        else {
            continue;
        };
        let start = open - keyword.len();
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_alphanumeric() || before == '_')
        {
            continue;
        }
        let Some((name, _)) = text[open + 1..].split_once(')') else {
            continue;
        };
        let valid = match kind {
            PromptReferenceKind::Rimz => {
                let mut chars = name.chars();
                chars.next().is_some_and(|first| first.is_ascii_lowercase())
                    && chars.all(|rest| {
                        rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-'
                    })
            }
            PromptReferenceKind::Skill => !name.contains('(') && name.parse::<SkillName>().is_ok(),
            PromptReferenceKind::Subagent => {
                !name.is_empty()
                    && !name
                        .chars()
                        .any(|ch| ch.is_whitespace() || matches!(ch, ',' | '+' | '/' | '(' | ')'))
            }
        };
        if valid {
            found.push(PromptReference {
                kind,
                name,
                range: start..open + 1 + name.len() + 1,
            });
        }
    }
    found
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
    fn prompt_references_scan_all_three_forms_with_ranges_and_grammars() {
        let rimz_text = "Use rimz(shell), `rimz(lsp)`, and (rimz(workflow-write)); rimz(shell) again.\n\
                         rimz(notes2) and x-rimz(control) count. spawn_detached_rimz(foo), \
                         rimz2(bar), Rimz(baz), RIMZ(qux), rimz(Wait), rimz( wait), rimz(), \
                         rimz(1x), rimz(-x), rimz(a_b), rimz(a b), Skill(rimz-message), rimz(open";
        let rimz_found: Vec<_> = prompt_references(rimz_text)
            .into_iter()
            .map(|r| (r.kind, r.name, &rimz_text[r.range]))
            .collect();
        assert_eq!(
            rimz_found,
            [
                (PromptReferenceKind::Rimz, "shell", "rimz(shell)"),
                (PromptReferenceKind::Rimz, "lsp", "rimz(lsp)"),
                (
                    PromptReferenceKind::Rimz,
                    "workflow-write",
                    "rimz(workflow-write)"
                ),
                (PromptReferenceKind::Rimz, "shell", "rimz(shell)"),
                (PromptReferenceKind::Rimz, "notes2", "rimz(notes2)"),
                (PromptReferenceKind::Rimz, "control", "rimz(control)"),
            ]
        );
        let lone = "rimz(shell)";
        assert_eq!(
            prompt_references(lone),
            [PromptReference {
                kind: PromptReferenceKind::Rimz,
                name: "shell",
                range: 0..lone.len(),
            }]
        );

        let text = "Use rimz(lsp), `skill(rimz-lsp)`, skill(_shared), skill(rimz.skills), \
                    skill(s), subagent(explorer), subagent(opencode-mini), subagent(forge.lint), \
                    subagent(My_Profile), and subagent(s).\n\
                    Plain: Skill(rimz-lsp), Agent(explorer), Subagent(explorer), Rimz(lsp), \
                    my_skill(commit), my_subagent(explorer), spawn_detached_rimz(shell), \
                    skill(), skill(.), skill(..), skill(+commit), skill(a/b), skill(a\\b), \
                    skill(a:b), skill(a b), skill(a(b)), subagent(), subagent(a b), \
                    subagent(a,b), subagent(a+b), subagent(a/b), subagent(a(b)), \
                    skill(open, subagent(open.";
        let found = prompt_references(text);
        let summary: Vec<_> = found
            .iter()
            .map(|r| (r.kind, r.name, &text[r.range.clone()]))
            .collect();
        assert_eq!(
            summary,
            [
                (PromptReferenceKind::Rimz, "lsp", "rimz(lsp)"),
                (PromptReferenceKind::Skill, "rimz-lsp", "skill(rimz-lsp)"),
                (PromptReferenceKind::Skill, "_shared", "skill(_shared)"),
                (
                    PromptReferenceKind::Skill,
                    "rimz.skills",
                    "skill(rimz.skills)"
                ),
                (PromptReferenceKind::Skill, "s", "skill(s)"),
                (
                    PromptReferenceKind::Subagent,
                    "explorer",
                    "subagent(explorer)"
                ),
                (
                    PromptReferenceKind::Subagent,
                    "opencode-mini",
                    "subagent(opencode-mini)"
                ),
                (
                    PromptReferenceKind::Subagent,
                    "forge.lint",
                    "subagent(forge.lint)"
                ),
                (
                    PromptReferenceKind::Subagent,
                    "My_Profile",
                    "subagent(My_Profile)"
                ),
                (PromptReferenceKind::Subagent, "s", "subagent(s)"),
            ]
        );
        // Nested parentheses reject the outer form and still scan a valid inner form.
        let nested = "skill(rimz(shell))";
        let inner: Vec<_> = prompt_references(nested)
            .into_iter()
            .map(|r| (r.kind, r.name, &nested[r.range]))
            .collect();
        assert_eq!(inner, [(PromptReferenceKind::Rimz, "shell", "rimz(shell)")]);
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
