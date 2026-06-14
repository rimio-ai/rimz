use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::run::PermissionMode;

/// Agent-launch preferences. Layout entries are shape strings whose cells
/// resolve through alias parsing in [`crate::agents_spec`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    /// Where a launch lands: a new backend tab/window or the current view.
    /// Declared before the table fields so the section serializes as valid
    /// TOML (a scalar after a sub-table would bind to the wrong table).
    pub tab: TabPlacement,
    #[serde(default)]
    pub aliases: AliasesConfig,
    pub layouts: LayoutsConfig,
}

/// Default tab placement for `rimz agents <spec>` launches; the per-launch
/// `--same-tab` / `--new-tab` flags override it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabPlacement {
    /// A worktree or multi-cell layout opens a new tab; a single non-worktree
    /// agent splits the current view.
    #[default]
    Auto,
    /// Always open a new tab/window.
    New,
    /// Split the current view when a launch is a single agent cell, falling
    /// back to a new tab when it cannot (a multi-cell layout, or no launching
    /// pane).
    Same,
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
        /// A file whose contents replace the agent's base system prompt, giving
        /// the role its own voice. Resolved relative to the config file and
        /// existence-checked when the alias launches.
        #[serde(
            default,
            rename = "system-prompt-file",
            skip_serializing_if = "Option::is_none"
        )]
        system_prompt_file: Option<PathBuf>,
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
                system_prompt_file: agent.system_prompt_file,
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
    #[serde(default, rename = "system-prompt-file")]
    system_prompt_file: Option<PathBuf>,
    #[serde(default)]
    args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, String>);
