use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{AttentionConfig, WorktreeConfig};
use crate::harness::run::PermissionMode;

/// Agent-launch preferences. Machine-team entries bind role names to profiles
/// or registered agent kinds; inline launch specs resolve through the same
/// profile/command rules in [`crate::harness::spec`].
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AgentsConfig {
    /// Where a launch lands: the current pane, a new pane, or a new backend
    /// tab/window.
    /// Declared before the table fields so the section serializes as valid
    /// TOML (a scalar after a sub-table would bind to the wrong table).
    pub placement: LaunchPlacement,
    /// Maximum number of agent-launched descendant layers.
    #[serde(default = "default_max_launch_depth", rename = "max-launch-depth")]
    pub max_launch_depth: u8,
    pub worktree: WorktreeConfig,
    pub attention: AttentionConfig,
    #[serde(default)]
    pub subagents: SubagentsConfig,
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
            max_launch_depth: default_max_launch_depth(),
            worktree: WorktreeConfig::default(),
            attention: AttentionConfig::default(),
            subagents: SubagentsConfig::default(),
            profiles: ProfilesConfig::default(),
            commands: CommandsConfig::default(),
            teams: default_machine_teams(),
        }
    }
}

const fn default_max_launch_depth() -> u8 {
    1
}

/// Defaults applied by the agent-only `rimz subagents` launch doorway.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SubagentsConfig {
    /// Supervised-run deadline in the CLI duration syntax (`30m`, `2h`).
    pub timeout: String,
    /// Optional per-child spend cap (`5` or `20/day`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        Self {
            timeout: "30m".to_owned(),
            budget: None,
        }
    }
}

fn default_machine_teams() -> TeamsConfig {
    TeamsConfig(BTreeMap::from([(
        "peer".to_owned(),
        Team {
            roles: Vec::new(),
            leader: None,
            layout: Some("claude,codex".to_owned()),
            scratch_files: Vec::new(),
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
    #[serde(default)]
    pub budget: Option<String>,
    /// A file whose contents replace the agent's base system prompt, giving the
    /// profile its own voice. Resolved relative to the config file and
    /// existence-checked when the profile launches.
    #[serde(
        default,
        rename = "system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt_file: Option<PathBuf>,
    /// Prompt fragments composed in order after `system_prompt_file`.
    #[serde(
        default,
        rename = "append-system-prompt-files",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub append_system_prompt_files: Vec<PathBuf>,
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
    pub leader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(
        default,
        rename = "scratch-files",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub scratch_files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoleBinding {
    pub role: String,
    /// A named profile or registered agent kind. A same-named machine profile
    /// overrides the kind's implicit base.
    pub profile: String,
    #[serde(default)]
    pub mode: Option<PermissionMode>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub budget: Option<String>,
    /// A replacement system prompt. Relative paths use the declaring file's
    /// directory, so a role in `~/.agents/teams/<name>/team.toml` can name a
    /// prompt shipped beside that fragment.
    #[serde(
        default,
        rename = "system-prompt-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt_file: Option<PathBuf>,
    /// Ordered prompt fragments appended after the resolved profile chain.
    #[serde(
        default,
        rename = "append-system-prompt-files",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub append_system_prompt_files: Vec<PathBuf>,
    #[serde(default)]
    pub args: Option<String>,
}

/// Locate the retired singular prompt-fragment key in machine or project config.
pub(crate) fn retired_append_prompt_key(doc: &toml::Table) -> Option<String> {
    let agents = doc.get("agents").and_then(toml::Value::as_table);
    for profiles in [
        agents
            .and_then(|agents| agents.get("profiles"))
            .and_then(toml::Value::as_table),
        doc.get("profiles").and_then(toml::Value::as_table),
    ]
    .into_iter()
    .flatten()
    {
        if let Some((name, _)) = profiles.iter().find(|(_, profile)| {
            profile
                .as_table()
                .is_some_and(|profile| profile.contains_key("append-system-prompt-file"))
        }) {
            return Some(format!(
                "profile `{name}` key `append-system-prompt-file` was renamed to `append-system-prompt-files`; use an array of paths"
            ));
        }
    }

    let teams = agents
        .and_then(|agents| agents.get("teams"))
        .and_then(toml::Value::as_table)?;
    for (team_name, team) in teams {
        let roles = team
            .as_table()
            .and_then(|team| team.get("roles"))
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten();
        for role in roles {
            let Some(role) = role.as_table() else {
                continue;
            };
            if role.contains_key("append-system-prompt-file") {
                let role_name = role
                    .get("role")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("<unnamed>");
                return Some(format!(
                    "team `{team_name}` role `{role_name}` key `append-system-prompt-file` was renamed to `append-system-prompt-files`; use an array of paths"
                ));
            }
        }
    }
    None
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CommandsConfig(pub BTreeMap<String, String>);
