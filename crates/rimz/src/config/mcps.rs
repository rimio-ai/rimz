//! Profile `mcps` selections: the RimZ MCP capabilities a launch serves and
//! the user's own MCP servers it keeps.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::mcp::catalog::Capability;

/// One `mcps` entry: a RimZ capability (`rimz-lsp`) or a user server's name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct McpName(String);

impl McpName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn capability(&self) -> Option<Capability> {
        Capability::from_name(&self.0)
    }
}

/// The RimZ capabilities an `mcps` selection serves: every one when absent.
pub fn mcp_capabilities(mcps: Option<&[McpName]>) -> Vec<Capability> {
    match mcps {
        None => Capability::ALL.to_vec(),
        Some(names) => Capability::ALL
            .into_iter()
            .filter(|capability| {
                names
                    .iter()
                    .any(|name| name.capability() == Some(*capability))
            })
            .collect(),
    }
}

impl FromStr for McpName {
    type Err = McpListErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let name = value.trim();
        let reserved = name == crate::mcp::catalog::SERVER_NAME
            || (name.starts_with("rimz-") && Capability::from_name(name).is_none());
        if name.is_empty() || reserved {
            return Err(McpListErr::InvalidName(value.to_owned()));
        }
        Ok(Self(name.to_owned()))
    }
}

impl TryFrom<String> for McpName {
    type Error = McpListErr;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<McpName> for String {
    fn from(value: McpName) -> Self {
        value.0
    }
}

impl fmt::Display for McpName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpListErr {
    #[error(
        "invalid mcps entry `{0}`: name a RimZ capability (rimz-lsp, rimz-wait, rimz-control, rimz-notes) or one of your own MCP servers"
    )]
    InvalidName(String),
    #[error("duplicate mcps entry `{0}`")]
    DuplicateName(McpName),
}

pub(crate) fn parse_mcp_list(names: &[String]) -> Result<Vec<McpName>, McpListErr> {
    let mut parsed: Vec<McpName> = Vec::new();
    for name in names {
        let name: McpName = name.parse()?;
        if parsed.contains(&name) {
            return Err(McpListErr::DuplicateName(name));
        }
        parsed.push(name);
    }
    Ok(parsed)
}

pub(crate) fn deserialize_optional_mcp_list<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<McpName>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Vec<String>>::deserialize(deserializer)?
        .map(|names| parse_mcp_list(&names))
        .transpose()
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_names_refuse_reserved_and_empty_entries() {
        for input in ["rimz-lsp", "rimz-notes", "github", "my.server", "rimzy"] {
            assert_eq!(input.parse::<McpName>().unwrap().as_str(), input);
        }
        for input in ["", " ", "rimz", "rimz-message", "rimz-"] {
            let error = input.parse::<McpName>().unwrap_err();
            assert_eq!(error, McpListErr::InvalidName(input.to_owned()));
            assert!(
                error
                    .to_string()
                    .contains("rimz-lsp, rimz-wait, rimz-control, rimz-notes")
            );
        }
        assert_eq!(
            parse_mcp_list(&["github".to_owned(), "github".to_owned()]).unwrap_err(),
            McpListErr::DuplicateName("github".parse().unwrap())
        );
    }

    #[test]
    fn toml_profiles_keep_an_empty_list_and_refuse_reserved_names() {
        let profile: crate::config::Profile =
            toml::from_str("agent = 'claude'\nmcps = []").unwrap();
        assert_eq!(profile.mcps, Some(Vec::new()));
        let profile: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
        assert_eq!(profile.mcps, None);
        let error = toml::from_str::<crate::config::Profile>("agent = 'claude'\nmcps = ['rimz']")
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid mcps entry `rimz`"), "{error}");
        let profile: crate::config::Profile =
            toml::from_str("agent = 'claude'\nmcps = [' rimz-lsp ']").unwrap();
        let names = profile.mcps.unwrap();
        assert_eq!(names[0].capability(), Some(Capability::Lsp));
        assert_eq!(names[0].as_str(), "rimz-lsp");
    }

    #[test]
    fn capabilities_follow_the_list() {
        assert_eq!(mcp_capabilities(None), Capability::ALL.to_vec());
        assert_eq!(mcp_capabilities(Some(&[])), Vec::new());
        let names =
            parse_mcp_list(&["github".into(), "rimz-notes".into(), "rimz-lsp".into()]).unwrap();
        assert_eq!(
            mcp_capabilities(Some(&names)),
            vec![Capability::Lsp, Capability::Notes]
        );
    }
}
