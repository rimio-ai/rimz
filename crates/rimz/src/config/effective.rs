//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::{AgentsConfig, CommandsConfig, ProfilesConfig, TeamsConfig};
use crate::harness::spec::{self as agents_spec, LayoutErr};
use crate::trust::{self, TrustState};

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

#[derive(Debug, thiserror::Error)]
pub enum EffectiveConfigErr {
    #[error(transparent)]
    Trust(#[from] trust::TrustErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing project config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid project agents config at {path}: {source}")]
    Agents {
        path: PathBuf,
        #[source]
        source: LayoutErr,
    },
    #[error("profiles are configured in {path} but the project is {state}; {fix}")]
    Blocked {
        path: PathBuf,
        state: &'static str,
        fix: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, EffectiveConfigErr>;

#[derive(Default)]
struct RepoConfig {
    profiles: ProfilesConfig,
    teams: TeamsConfig,
}

/// Effective launch config: machine profiles/teams overlaid by trusted repo
/// profiles/teams. Repo entries are inert until trust is granted, and a repo
/// profile may inherit only repo profiles or built-in kinds so the hashed
/// executable surface stays closed.
pub struct LaunchAgents {
    pub profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    state: TrustState,
    config_path: PathBuf,
}

pub fn load(
    machine: &AgentsConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<LaunchAgents> {
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    if report.state != TrustState::Trusted {
        return Ok(LaunchAgents {
            profiles: machine.profiles.clone(),
            teams: machine.teams.clone(),
            state: report.state,
            config_path,
        });
    }

    let Some(repo_value) = read_repo_value(&config_path)? else {
        return Ok(LaunchAgents {
            profiles: machine.profiles.clone(),
            teams: machine.teams.clone(),
            state: report.state,
            config_path,
        });
    };
    let mut repo =
        repo_config_from_value(&repo_value).map_err(|source| EffectiveConfigErr::Parse {
            path: config_path.clone(),
            source,
        })?;
    let config_dir = config_path.parent().unwrap_or(project_root);
    agents_spec::resolve_profile_prompt_paths(&mut repo.profiles, config_dir);
    agents_spec::resolve_team_prompt_paths(&mut repo.teams, config_dir);
    agents_spec::validate_config(
        &repo.profiles,
        &CommandsConfig::default(),
        &TeamsConfig::default(),
    )
    .map_err(|source| EffectiveConfigErr::Agents {
        path: config_path.clone(),
        source,
    })?;
    for name in repo.profiles.0.keys() {
        agents_spec::resolve_profile(name, &repo.profiles).map_err(|source| {
            let source = match source {
                LayoutErr::UnknownProfileBase { profile, base }
                    if machine.profiles.0.contains_key(&base) =>
                {
                    LayoutErr::RepoProfileEscapesTrust { profile, base }
                }
                other => other,
            };
            EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source,
            }
        })?;
    }
    agents_spec::validate_config(&repo.profiles, &CommandsConfig::default(), &repo.teams).map_err(
        |source| EffectiveConfigErr::Agents {
            path: config_path.clone(),
            source,
        },
    )?;

    let mut profiles = machine.profiles.clone();
    profiles.0.extend(repo.profiles.0);
    let mut teams = machine.teams.clone();
    teams.0.extend(repo.teams.0);
    Ok(LaunchAgents {
        profiles,
        teams,
        state: report.state,
        config_path,
    })
}

impl LaunchAgents {
    /// Return a trust error only when a requested launch spec would consume a
    /// repo profile or team while the project is not trusted. Repo entries are
    /// otherwise inert: machine profiles, machine commands, and built-in cells
    /// keep launching in an untrusted checkout even when `.rimz/config.toml`
    /// declares profiles.
    pub fn block_untrusted_reference(
        &self,
        spec: Option<&str>,
        commands: &CommandsConfig,
    ) -> Result<()> {
        let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
            return Ok(());
        };
        if self.state == TrustState::Trusted {
            return Ok(());
        }
        let Some(repo_value) = read_repo_value(&self.config_path)? else {
            return Ok(());
        };
        let repo_profiles = profile_names(&repo_value);
        let repo_teams = team_names(&repo_value);
        let team_spec = spec.split_once('.').map_or(spec, |(team, _)| team);
        if (repo_profiles.is_empty()
            || !spec_references_repo_profile(
                spec,
                &repo_profiles,
                &self.profiles,
                commands,
                &self.teams,
            ))
            && !repo_teams.contains(team_spec)
        {
            return Ok(());
        }
        let fix = match self.state {
            TrustState::Stale => {
                "the executable surface changed since the grant; review it and rerun `rimz trust grant`"
            }
            _ => "run `rimz trust grant` to apply them",
        };
        Err(EffectiveConfigErr::Blocked {
            path: self.config_path.clone(),
            state: self.state.as_str(),
            fix,
        })
    }
}

fn read_repo_value(path: &Path) -> Result<Option<toml::Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<toml::Value>(&text)
            .map(Some)
            .map_err(|source| EffectiveConfigErr::Parse {
                path: path.to_path_buf(),
                source,
            }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EffectiveConfigErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn repo_config_from_value(value: &toml::Value) -> std::result::Result<RepoConfig, toml::de::Error> {
    let profiles = value
        .get("profiles")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let teams = value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("teams"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    Ok(RepoConfig { profiles, teams })
}

fn profile_names(value: &toml::Value) -> BTreeSet<String> {
    value
        .as_table()
        .and_then(|table| table.get("profiles"))
        .and_then(toml::Value::as_table)
        .map(|profiles| profiles.keys().cloned().collect())
        .unwrap_or_default()
}

fn team_names(value: &toml::Value) -> BTreeSet<String> {
    value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("teams"))
        .and_then(toml::Value::as_table)
        .map(|teams| teams.keys().cloned().collect())
        .unwrap_or_default()
}

fn spec_references_repo_profile(
    spec: &str,
    repo_profiles: &BTreeSet<String>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> bool {
    if let Some(team) = teams.0.get(spec) {
        let role_refs_repo_profile = team
            .roles
            .iter()
            .any(|binding| repo_profiles.contains(&binding.profile));
        let layout_refs_repo_profile = team.layout.as_deref().is_some_and(|layout| {
            layout_tokens(layout).any(|token| {
                let is_declared_role = team.roles.iter().any(|binding| binding.role == token);
                !is_declared_role
                    && repo_profiles.contains(token)
                    && !machine_cell_word(token, profiles, commands)
            })
        });
        return role_refs_repo_profile || layout_refs_repo_profile;
    }
    layout_tokens(spec)
        .any(|token| repo_profiles.contains(token) && !machine_cell_word(token, profiles, commands))
}

fn layout_tokens(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', '+'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn machine_cell_word(token: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    profiles.0.contains_key(token)
        || commands.0.contains_key(token)
        || agents_spec::parse_layout_spec(token, profiles, commands).is_ok()
}

#[cfg(test)]
mod tests;
