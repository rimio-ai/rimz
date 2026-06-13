use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::run::PermissionMode;

/// Agent-launch preferences. Layout entries are shape strings whose cells
/// resolve through alias parsing in [`crate::agents_spec`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    #[serde(default)]
    pub aliases: AliasesConfig,
    pub layouts: LayoutsConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AliasesConfig(pub BTreeMap<String, Alias>);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Alias {
    Command(String),
    CommandTable {
        command: String,
    },
    Agent {
        agent: String,
        #[serde(default)]
        mode: Option<PermissionMode>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<String>,
        #[serde(default)]
        args: Option<String>,
    },
}

impl<'de> Deserialize<'de> for Alias {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match AliasInput::deserialize(deserializer)? {
            AliasInput::Command(command) => Ok(Self::Command(command)),
            AliasInput::CommandTable(command) => Ok(Self::CommandTable {
                command: command.command,
            }),
            AliasInput::Agent(agent) => Ok(Self::Agent {
                agent: agent.agent,
                mode: agent.mode,
                model: agent.model,
                effort: agent.effort,
                args: agent.args,
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AliasInput {
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
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, String>);
