use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{AttentionConfig, WorktreeConfig};
use crate::run::PermissionMode;

/// Agent-launch preferences. Team entries bind role names to profiles; inline
/// launch specs still resolve through profile/command parsing in
/// [`crate::agents_spec`].
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    /// Where a launch lands: the current pane, a new pane, or a new backend
    /// tab/window.
    /// Declared before the table fields so the section serializes as valid
    /// TOML (a scalar after a sub-table would bind to the wrong table).
    pub placement: LaunchPlacement,
    pub worktree: WorktreeConfig,
    pub attention: AttentionConfig,
    #[serde(default)]
    pub profiles: ProfilesConfig,
    #[serde(default)]
    pub commands: CommandsConfig,
    #[serde(default = "default_machine_teams")]
    pub teams: TeamsConfig,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            placement: LaunchPlacement::default(),
            worktree: WorktreeConfig::default(),
            attention: AttentionConfig::default(),
            profiles: ProfilesConfig::default(),
            commands: CommandsConfig::default(),
            teams: default_machine_teams(),
        }
    }
}

fn default_machine_teams() -> TeamsConfig {
    TeamsConfig(BTreeMap::from([(
        "peer".to_owned(),
        Team {
            roles: Vec::new(),
            layout: Some("claude,codex".to_owned()),
        },
    )]))
}

/// Default launch placement for `rimz agents <spec>` launches; the per-launch
/// `--new-pane` / `--new-tab` flags override it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LaunchPlacement {
    /// A one-cell non-worktree launch runs in the current pane; a multi-cell,
    /// named-channel, or worktree launch opens a new tab.
    #[default]
    Auto,
    /// A one-cell non-worktree launch splits a new pane in the current tab; a
    /// multi-cell, named-channel, or worktree launch opens a new tab.
    Pane,
    /// Always open a new tab/window.
    Tab,
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
    /// A file whose contents append to the agent's base system prompt,
    /// preserving the native base rules while adding profile-local guidance.
    /// Resolved and existence-checked like `system_prompt_file`.
    #[serde(
        default,
        rename = "append-system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub append_system_prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct TeamsConfig(pub BTreeMap<String, Team>);

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Team {
    #[serde(default)]
    pub roles: Vec<RoleBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoleBinding {
    pub role: String,
    pub profile: String,
    #[serde(default)]
    pub mode: Option<PermissionMode>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(
        default,
        rename = "system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(
        default,
        rename = "append-system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub append_system_prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CommandsConfig(pub BTreeMap<String, String>);
