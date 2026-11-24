//! The `capabilities:` profile key's RimZ entries: which of RimZ's own capabilities a profile
//! grants, whether the launch reaches each as MCP tools or as a skill.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::mcp::catalog::{Capability, LoadMode};

/// One RimZ capability a `capabilities:` list grants: `rimz-<name>`, a lowercase name of
/// letters, digits, and hyphens that starts with a letter, with an optional load mark
/// (`rimz-lsp:deferred`), or `rimz`, which carries the mark of a grant that names every
/// capability. A loaded definition grants only a capability of RimZ's MCP server
/// (`Capability::ALL`); the text form takes any `rimz-` name so an exec request written by an
/// older build still decodes, and a name outside `Capability::ALL` there grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RimzEntry {
    name: String,
    mode: Option<LoadMode>,
}

impl RimzEntry {
    pub(crate) fn new(capability: Capability, mode: Option<LoadMode>) -> Self {
        Self {
            name: capability.name().to_owned(),
            mode,
        }
    }

    /// The `rimz` entry, whose mark applies to each capability with none of its own.
    pub(crate) fn server(mode: Option<LoadMode>) -> Self {
        Self {
            name: crate::mcp::catalog::SERVER_NAME.to_owned(),
            mode,
        }
    }

    pub(crate) fn is_server(&self) -> bool {
        self.name == crate::mcp::catalog::SERVER_NAME
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn mode(&self) -> Option<LoadMode> {
        self.mode
    }

    /// The capability of RimZ's MCP server the entry names; none for a name an older build
    /// granted that is a skill alone.
    pub(crate) fn capability(&self) -> Option<Capability> {
        Capability::from_name(&self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RimzEntryErr {
    #[error(
        "invalid RimZ capability `{0}`: write `rimz` or `rimz-<name>`, a lowercase name of letters, digits, and hyphens that starts with a letter, with an optional `:always` or `:deferred`"
    )]
    Invalid(String),
}

impl FromStr for RimzEntry {
    type Err = RimzEntryErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || RimzEntryErr::Invalid(value.to_owned());
        let entry = value.trim();
        let (name, mode) = match entry.rsplit_once(':') {
            Some((name, mark)) => (
                name.trim_end(),
                Some(LoadMode::from_name(mark.trim_start()).ok_or_else(invalid)?),
            ),
            None => (entry, None),
        };
        if name == crate::mcp::catalog::SERVER_NAME {
            return Ok(Self::server(mode));
        }
        let mut rest = name.strip_prefix("rimz-").ok_or_else(invalid)?.chars();
        if !rest.next().is_some_and(|first| first.is_ascii_lowercase())
            || !rest.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(invalid());
        }
        Ok(Self {
            name: name.to_owned(),
            mode,
        })
    }
}

impl TryFrom<String> for RimzEntry {
    type Error = RimzEntryErr;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<RimzEntry> for String {
    fn from(value: RimzEntry) -> Self {
        value.to_string()
    }
}

impl fmt::Display for RimzEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        match self.mode {
            Some(mode) => write!(f, ":{}", mode.name()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_parse_a_rimz_name_and_an_optional_mark_and_round_trip_as_text() {
        for (input, name, mode) in [
            ("rimz-lsp", "rimz-lsp", None),
            (" rimz-reflect ", "rimz-reflect", None),
            ("rimz-lsp:deferred", "rimz-lsp", Some(LoadMode::Deferred)),
            ("rimz-wait : always", "rimz-wait", Some(LoadMode::Always)),
            ("rimz:deferred", "rimz", Some(LoadMode::Deferred)),
        ] {
            let entry: RimzEntry = input.parse().unwrap();
            assert_eq!((entry.name(), entry.mode()), (name, mode), "{input}");
            let text = serde_json::to_string(&entry).unwrap();
            assert_eq!(serde_json::from_str::<RimzEntry>(&text).unwrap(), entry);
        }
        assert_eq!(
            "rimz-lsp:deferred"
                .parse::<RimzEntry>()
                .unwrap()
                .capability(),
            Some(Capability::Lsp)
        );
        assert_eq!(
            "rimz-reflect".parse::<RimzEntry>().unwrap().capability(),
            None
        );
        for input in [
            "",
            "rimz-",
            "rimz-Lsp",
            "rimz-1x",
            "github",
            "rimz-lsp:eager",
            "+rimz-lsp",
        ] {
            assert_eq!(
                input.parse::<RimzEntry>().unwrap_err(),
                RimzEntryErr::Invalid(input.to_owned()),
                "{input}"
            );
        }
    }
}
