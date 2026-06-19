//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents_spec::{self, LayoutErr};
use crate::config::{CommandsConfig, LayoutsConfig, ProfilesConfig};
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
    #[error("invalid project profiles at {path}: {source}")]
    Profiles {
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

#[derive(Default, Deserialize)]
#[serde(default)]
struct RepoConfig {
    profiles: ProfilesConfig,
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
        &LayoutsConfig::default(),
    )
    .map_err(|source| EffectiveConfigErr::Profiles {
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
            EffectiveConfigErr::Profiles {
                path: config_path.clone(),
                source,
            }
        })?;
    }

    let mut merged = machine.clone();
    merged.0.extend(repo.profiles.0);
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
    layouts: &LayoutsConfig,
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
    if repo_profiles.is_empty()
        || !spec_references_repo_profile(spec, &repo_profiles, profiles, commands, layouts)
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
        Ok(text) => toml::from_str::<RepoConfig>(&text)
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
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value = toml::from_str::<toml::Value>(&text).map_err(|source| {
                EffectiveConfigErr::Parse {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            Ok(value
                .as_table()
                .and_then(|table| table.get("profiles"))
                .and_then(toml::Value::as_table)
                .map(|profiles| profiles.keys().cloned().collect())
                .unwrap_or_default())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(source) => Err(EffectiveConfigErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn spec_references_repo_profile(
    spec: &str,
    repo_profiles: &BTreeSet<String>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    layouts: &LayoutsConfig,
) -> bool {
    let shape = if !machine_cell_word(spec, profiles, commands) {
        layouts.0.get(spec).map(String::as_str).unwrap_or(spec)
    } else {
        spec
    };
    layout_tokens(shape)
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
