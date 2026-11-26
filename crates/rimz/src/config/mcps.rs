//! Profile `mcps` selections: the RimZ MCP capabilities a launch serves and
//! the user's own MCP servers it keeps.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::mcp::catalog::{Capability, LoadMode};

/// One `mcps` entry: `*`, `rimz`, a RimZ capability (`rimz-lsp`), or a
/// user server's name, with an optional load mode (`github:deferred`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct McpName {
    name: String,
    mode: Option<LoadMode>,
}

/// The entry that admits every server and capability.
const ALL: &str = "*";

impl McpName {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn mode(&self) -> Option<LoadMode> {
        self.mode
    }

    /// The `*` entry.
    pub(crate) fn all() -> Self {
        Self {
            name: ALL.to_owned(),
            mode: None,
        }
    }

    /// `*`, which keeps every one of the user's servers and every capability.
    pub fn is_all(&self) -> bool {
        self.name == ALL
    }

    pub fn capability(&self) -> Option<Capability> {
        Capability::from_name(&self.name)
    }

    /// The user's own server the entry names: not `*`, `rimz`, or a capability.
    pub fn user_server(&self) -> Option<&str> {
        let rimz = self.is_all()
            || self.name == crate::mcp::catalog::SERVER_NAME
            || self.capability().is_some();
        (!rimz).then_some(self.name())
    }
}

/// Whether an `mcps` selection limits what a launch loads: a list without
/// `*` does, an absent one does not.
pub fn mcps_limit(mcps: Option<&[McpName]>) -> bool {
    mcps.is_some_and(|entries| !entries.iter().any(McpName::is_all))
}

/// The RimZ capabilities an `mcps` selection serves: every one when absent
/// or when it lists `*` or `rimz`.
pub fn mcp_capabilities(mcps: Option<&[McpName]>) -> Vec<Capability> {
    let Some(entries) = mcps else {
        return Capability::ALL.to_vec();
    };
    if entries
        .iter()
        .any(|entry| entry.is_all() || entry.name == crate::mcp::catalog::SERVER_NAME)
    {
        return Capability::ALL.to_vec();
    }
    Capability::ALL
        .into_iter()
        .filter(|capability| {
            entries
                .iter()
                .any(|entry| entry.capability() == Some(*capability))
        })
        .collect()
}

/// The load mark on the `rimz` entry of an `mcps` selection.
pub fn server_mark(mcps: Option<&[McpName]>) -> Option<LoadMode> {
    mcps.unwrap_or_default()
        .iter()
        .find(|entry| entry.name == crate::mcp::catalog::SERVER_NAME)
        .and_then(McpName::mode)
}

/// Each capability an `mcps` selection serves, with its load mark: its own entry's, else the
/// `rimz` entry's, which marks every served capability that has none of its own.
pub fn served_marks(mcps: Option<&[McpName]>) -> Vec<(Capability, Option<LoadMode>)> {
    let entries = mcps.unwrap_or_default();
    let server_mark = server_mark(mcps);
    mcp_capabilities(mcps)
        .into_iter()
        .map(|capability| {
            let own = entries
                .iter()
                .find(|entry| entry.capability() == Some(capability))
                .and_then(McpName::mode);
            (capability, own.or(server_mark))
        })
        .collect()
}

impl FromStr for McpName {
    type Err = McpListErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || McpListErr::InvalidName(value.to_owned());
        let entry = value.trim();
        if entry.starts_with('+') {
            return Err(McpListErr::PlusEntry(value.to_owned()));
        }
        let (name, mode) = match entry.rsplit_once(':') {
            Some((name, mark)) => (
                name.trim_end(),
                Some(LoadMode::from_name(mark.trim_start()).ok_or_else(invalid)?),
            ),
            None => (entry, None),
        };
        let reserved = name.starts_with("rimz-") && Capability::from_name(name).is_none();
        if name.is_empty() || reserved || (name == ALL && mode.is_some()) {
            return Err(invalid());
        }
        Ok(Self {
            name: name.to_owned(),
            mode,
        })
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
        value.to_string()
    }
}

impl fmt::Display for McpName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        match self.mode {
            Some(mode) => write!(f, ":{}", mode.name()),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpListErr {
    #[error(
        "invalid mcps entry `{0}`: list `*`, `rimz`, a RimZ capability (rimz-lsp, rimz-wait, \
         rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, \
         rimz-control, rimz-notes), or one of your own MCP servers, each name with an optional \
         `:always` or `:deferred`"
    )]
    InvalidName(String),
    #[error(
        "invalid mcps entry `{0}`: `+` entries add to an inherited list in Markdown definitions only; list the bare name"
    )]
    PlusEntry(String),
    #[error("invalid mcps entry `{entry}`: `{earlier}` already gives `{}` another mark", entry.name())]
    ConflictingMark { entry: McpName, earlier: McpName },
    #[error("duplicate mcps entry `{0}`")]
    DuplicateName(McpName),
}

/// Adds `entry` to `entries`; `false` when the same entry is already there,
/// and an error when the same name is there with another mark.
pub(crate) fn push_mcp_entry(
    entries: &mut Vec<McpName>,
    entry: McpName,
) -> Result<bool, McpListErr> {
    match entries.iter().find(|earlier| earlier.name == entry.name) {
        Some(earlier) if *earlier == entry => Ok(false),
        Some(earlier) => Err(McpListErr::ConflictingMark {
            earlier: earlier.clone(),
            entry,
        }),
        None => {
            entries.push(entry);
            Ok(true)
        }
    }
}

pub(crate) fn parse_mcp_list(names: &[String]) -> Result<Vec<McpName>, McpListErr> {
    let mut parsed: Vec<McpName> = Vec::new();
    for name in names {
        let entry: McpName = name.parse()?;
        if !push_mcp_entry(&mut parsed, entry.clone())? {
            return Err(McpListErr::DuplicateName(entry));
        }
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

    fn list(names: &[&str]) -> Vec<McpName> {
        parse_mcp_list(
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn entries_parse_a_name_and_an_optional_mark() {
        for (input, name, mode) in [
            ("rimz-lsp", "rimz-lsp", None),
            ("github", "github", None),
            ("my.server", "my.server", None),
            ("rimzy", "rimzy", None),
            ("rimz", "rimz", None),
            ("*", "*", None),
            ("github:always", "github", Some(LoadMode::Always)),
            ("rimz:deferred", "rimz", Some(LoadMode::Deferred)),
            (" rimz-wait:always ", "rimz-wait", Some(LoadMode::Always)),
        ] {
            let entry: McpName = input.parse().unwrap();
            assert_eq!((entry.name(), entry.mode()), (name, mode), "{input}");
            assert_eq!(entry.to_string(), input.trim());
        }
        let all: McpName = "*".parse().unwrap();
        assert!(all.is_all());
        assert_eq!(all.user_server(), None);
        let rimz: McpName = "rimz:always".parse().unwrap();
        assert!(!rimz.is_all());
        assert_eq!((rimz.capability(), rimz.user_server()), (None, None));
        let lsp: McpName = "rimz-lsp:deferred".parse().unwrap();
        assert_eq!(
            (lsp.capability(), lsp.user_server()),
            (Some(Capability::Lsp), None)
        );
        let github: McpName = "github:always".parse().unwrap();
        assert_eq!(github.user_server(), Some("github"));
        let spaced: McpName = "github : always".parse().unwrap();
        assert_eq!(spaced, github);
    }

    #[test]
    fn entries_refuse_empty_names_bad_marks_and_reserved_names() {
        for input in [
            "",
            " ",
            ":always",
            "rimz-chat",
            "rimz-",
            "*:always",
            "* :always",
            "github:eager",
            "github:",
        ] {
            let error = input.parse::<McpName>().unwrap_err();
            assert_eq!(error, McpListErr::InvalidName(input.to_owned()), "{input}");
        }
        assert_eq!(
            "github:eager".parse::<McpName>().unwrap_err().to_string(),
            "invalid mcps entry `github:eager`: list `*`, `rimz`, a RimZ capability \
             (rimz-lsp, rimz-wait, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, \
             rimz-loop, rimz-message, rimz-control, rimz-notes), or one of your own MCP servers, \
             each name with an optional `:always` or `:deferred`"
        );
        assert_eq!(
            parse_mcp_list(&["github".to_owned(), "github".to_owned()]).unwrap_err(),
            McpListErr::DuplicateName("github".parse().unwrap())
        );
        let error = parse_mcp_list(&["github:always".to_owned(), "github".to_owned()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid mcps entry `github`: `github:always` already gives `github` another mark"
        );
    }

    #[test]
    fn plus_entries_are_refused_outside_a_markdown_list() {
        for input in ["+github", " +rimz-lsp:deferred"] {
            assert_eq!(
                input.parse::<McpName>().unwrap_err(),
                McpListErr::PlusEntry(input.to_owned()),
                "{input}"
            );
        }
        let error =
            toml::from_str::<crate::config::Profile>("agent = 'claude'\nmcps = ['+github']")
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("mcps entry `+github`")
                && error.contains("in Markdown definitions only"),
            "{error}"
        );
    }

    #[test]
    fn toml_profiles_keep_an_empty_list_and_read_marks() {
        let profile: crate::config::Profile =
            toml::from_str("agent = 'claude'\nmcps = []").unwrap();
        assert_eq!(profile.mcps, Some(Vec::new()));
        let profile: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
        assert_eq!(profile.mcps, None);
        let error =
            toml::from_str::<crate::config::Profile>("agent = 'claude'\nmcps = ['*:always']")
                .unwrap_err()
                .to_string();
        assert!(error.contains("invalid mcps entry `*:always`"), "{error}");
        let profile: crate::config::Profile =
            toml::from_str("agent = 'claude'\nmcps = [' rimz-lsp ', '*', 'rimz:deferred']")
                .unwrap();
        let names = profile.mcps.unwrap();
        assert_eq!(names[0].capability(), Some(Capability::Lsp));
        assert_eq!(names[0].name(), "rimz-lsp");
        assert!(names[1].is_all());
        assert_eq!(names[2].mode(), Some(LoadMode::Deferred));
    }

    #[test]
    fn capabilities_and_limits_follow_the_list() {
        assert_eq!(mcp_capabilities(None), Capability::ALL.to_vec());
        assert_eq!(mcp_capabilities(Some(&[])), Vec::new());
        assert_eq!(
            mcp_capabilities(Some(&list(&["github", "rimz-notes", "rimz-lsp"]))),
            vec![Capability::Lsp, Capability::Notes]
        );
        for all in [
            list(&["*"]),
            list(&["rimz"]),
            list(&["github", "rimz:always"]),
        ] {
            assert_eq!(
                mcp_capabilities(Some(&all)),
                Capability::ALL.to_vec(),
                "{all:?}"
            );
        }

        assert!(!mcps_limit(None));
        assert!(!mcps_limit(Some(&list(&["*"]))));
        assert!(!mcps_limit(Some(&list(&["github:always", "*"]))));
        assert!(mcps_limit(Some(&[])));
        assert!(mcps_limit(Some(&list(&["rimz"]))));
    }
}
