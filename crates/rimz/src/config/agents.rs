use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::run::PermissionMode;

/// Agent-launch preferences. Layout entries are shape strings whose cells
/// resolve through profile/command parsing in [`crate::agents_spec`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    /// Where a launch lands: a new backend tab/window or the current view.
    /// Declared before the table fields so the section serializes as valid
    /// TOML (a scalar after a sub-table would bind to the wrong table).
    pub tab: TabPlacement,
    #[serde(default)]
    pub profiles: ProfilesConfig,
    #[serde(default)]
    pub commands: CommandsConfig,
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
pub struct ProfilesConfig(pub BTreeMap<String, Profile>);

/// A named agent profile. `agent` is a base reference: either a built-in agent
/// kind or another profile that resolves to one.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub agent: String,
    #[serde(default)]
    pub mode: Option<PermissionMode>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// A file whose contents replace the agent's base system prompt, giving the
    /// profile its own voice. Resolved relative to the config file and
    /// existence-checked when the profile launches.
    #[serde(
        default,
        rename = "system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LayoutsConfig(pub BTreeMap<String, String>);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CommandsConfig(pub BTreeMap<String, String>);
