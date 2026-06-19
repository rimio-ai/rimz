//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::agents_spec::{self, LayoutErr};
use crate::config::{CommandsConfig, ProfilesConfig, TeamsConfig};
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

/// Effective profiles for launch: machine profiles overlaid by trusted repo
/// profiles. Repo profiles are inert until trust is granted, and a repo profile
/// may inherit only repo profiles or built-in kinds so the hashed executable
/// surface stays closed.
pub fn effective_profiles(
    machine: &ProfilesConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<ProfilesConfig> {
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    if report.state != TrustState::Trusted {
        return Ok(machine.clone());
    }

    let Some(mut repo) = read_repo_config(&config_path)? else {
        return Ok(machine.clone());
    };
    let config_dir = config_path.parent().unwrap_or(project_root);
    agents_spec::resolve_profile_prompt_paths(&mut repo.profiles, config_dir);
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
                    if machine.0.contains_key(&base) =>
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

    let mut merged = machine.clone();
    merged.0.extend(repo.profiles.0);
    Ok(merged)
}

/// Effective teams for launch: machine teams overlaid by trusted repo teams.
/// Repo teams may bind only repo profiles, keeping shared launch shapes inside
/// the trusted executable surface.
pub fn effective_teams(
    machine: &TeamsConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<TeamsConfig> {
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    if report.state != TrustState::Trusted {
        return Ok(machine.clone());
    }

    let Some(mut repo) = read_repo_config(&config_path)? else {
        return Ok(machine.clone());
    };
    let config_dir = config_path.parent().unwrap_or(project_root);
    agents_spec::resolve_profile_prompt_paths(&mut repo.profiles, config_dir);
    agents_spec::resolve_team_prompt_paths(&mut repo.teams, config_dir);
    agents_spec::validate_config(&repo.profiles, &CommandsConfig::default(), &repo.teams).map_err(
        |source| EffectiveConfigErr::Agents {
            path: config_path.clone(),
            source,
        },
    )?;

    let mut merged = machine.clone();
    merged.0.extend(repo.teams.0);
    Ok(merged)
}

/// Return a trust error only when a requested launch spec would consume a repo
/// profile while the project is not trusted. Repo profiles are otherwise inert:
/// machine profiles, machine commands, and built-in cells keep launching in an
/// untrusted checkout even when `.rimz/config.toml` declares profiles.
pub fn block_untrusted_profile_reference(
    spec: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<()> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return Ok(());
    };
    let report = trust::status_with_roots(project_root, config_root)?;
    if report.state == TrustState::Trusted {
        return Ok(());
    }
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    let repo_profiles = repo_profile_names(&config_path)?;
    let repo_teams = repo_team_names(&config_path)?;
    if (repo_profiles.is_empty()
        || !spec_references_repo_profile(spec, &repo_profiles, profiles, commands, teams))
        && !repo_teams.contains(spec)
    {
        return Ok(());
    }
    let fix = match report.state {
        TrustState::Stale => {
            "the executable surface changed since the grant; review it and rerun `rimz trust grant`"
        }
        _ => "run `rimz trust grant` to apply them",
    };
    Err(EffectiveConfigErr::Blocked {
        path: config_path,
        state: report.state.as_str(),
        fix,
    })
}

fn read_repo_config(path: &Path) -> Result<Option<RepoConfig>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value = toml::from_str::<toml::Value>(&text).map_err(|source| {
                EffectiveConfigErr::Parse {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            repo_config_from_value(&value)
                .map(Some)
                .map_err(|source| EffectiveConfigErr::Parse {
                    path: path.to_path_buf(),
                    source,
                })
        }
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

fn repo_value(path: &Path) -> Result<Option<toml::Value>> {
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

fn repo_profile_names(path: &Path) -> Result<BTreeSet<String>> {
    let Some(value) = repo_value(path)? else {
        return Ok(BTreeSet::new());
    };
    Ok(value
        .as_table()
        .and_then(|table| table.get("profiles"))
        .and_then(toml::Value::as_table)
        .map(|profiles| profiles.keys().cloned().collect())
        .unwrap_or_default())
}

fn repo_team_names(path: &Path) -> Result<BTreeSet<String>> {
    let Some(value) = repo_value(path)? else {
        return Ok(BTreeSet::new());
    };
    Ok(value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("teams"))
        .and_then(toml::Value::as_table)
        .map(|teams| teams.keys().cloned().collect())
        .unwrap_or_default())
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
