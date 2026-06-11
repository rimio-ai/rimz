use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::run::PermissionMode;

/// Tab-launch preferences. Layout entries are shape strings whose cells resolve
/// through keyword parsing in [`crate::tab_layout`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TabConfig {
    pub keywords: KeywordsConfig,
    pub layouts: LayoutsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct KeywordsConfig(pub BTreeMap<String, Keyword>);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Keyword {
    Command(String),
    CommandTable {
        command: String,
    },
    Agent {
        agent: String,
        #[serde(default)]
        mode: Option<PermissionMode>,
        #[serde(default)]
        args: Option<String>,
    },
}

impl<'de> Deserialize<'de> for Keyword {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match KeywordInput::deserialize(deserializer)? {
            KeywordInput::Command(command) => Ok(Self::Command(command)),
            KeywordInput::CommandTable(command) => Ok(Self::CommandTable {
                command: command.command,
            }),
            KeywordInput::Agent(agent) => Ok(Self::Agent {
                agent: agent.agent,
                mode: agent.mode,
                args: agent.args,
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum KeywordInput {
    Command(String),
    CommandTable(CommandKeyword),
    Agent(AgentKeyword),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandKeyword {
    command: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentKeyword {
    agent: String,
    #[serde(default)]
    mode: Option<PermissionMode>,
    #[serde(default)]
    args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, String>);
