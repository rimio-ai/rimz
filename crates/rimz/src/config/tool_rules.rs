//! Validated per-definition permission rules.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ToolRule(String);

impl std::str::FromStr for ToolRule {
    type Err = ToolRuleErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let rule = value.trim();
        let name = if let Some((name, specifier)) = rule.split_once('(') {
            if !specifier.ends_with(')') || specifier[..specifier.len() - 1].trim().is_empty() {
                return Err(ToolRuleErr(value.to_owned()));
            }
            name
        } else {
            rule
        };
        if value.chars().any(char::is_control)
            || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'*' | b'-'))
        {
            return Err(ToolRuleErr(value.to_owned()));
        }
        Ok(Self(rule.to_owned()))
    }
}

impl TryFrom<String> for ToolRule {
    type Error = ToolRuleErr;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ToolRule> for String {
    fn from(value: ToolRule) -> Self {
        value.0
    }
}

impl std::fmt::Display for ToolRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "lists allowed-tools rule {0:?}; rimz takes a tool name, optionally with a non-empty (specifier) closing the rule, such as \"Bash(git *)\" or \"Read\""
)]
pub struct ToolRuleErr(String);
