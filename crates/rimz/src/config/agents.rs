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
    #[serde(default)]
    pub args: Option<String>,
}

/// Locate either retired prompt-fragment key in machine or project config.
/// These paths execute commands, so silently ignoring a removed key would
/// launch a posture the user did not declare.
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
        if let Some((name, key)) = profiles.iter().find_map(|(name, profile)| {
            let profile = profile.as_table()?;
            ["append-system-prompt-file", "append-system-prompt-files"]
                .into_iter()
                .find(|key| profile.contains_key(*key))
                .map(|key| (name, key))
        }) {
            return Some(format!(
                "profile `{name}` field `{key}` was removed; use the agent's native context files for additive guidance or `system-prompt-file` for full replacement"
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
            if let Some(key) = ["append-system-prompt-file", "append-system-prompt-files"]
                .into_iter()
                .find(|key| role.contains_key(*key))
            {
                let role_name = role
                    .get("role")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("<unnamed>");
                return Some(format!(
                    "team `{team_name}` role `{role_name}` field `{key}` was removed; use the agent's native context files for additive guidance or `system-prompt-file` for full replacement"
                ));
            }
        }
    }
    None
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CommandsConfig(pub BTreeMap<String, String>);
