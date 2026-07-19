//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::{AgentsConfig, CommandsConfig, ProfilesConfig, TaskEntry, Tasks, TeamsConfig};
use crate::harness::schedule::{self, ScheduleErr};
use crate::harness::spec::{self as agents_spec, LayoutErr};
use crate::trust::{self, TrustState};

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

#[derive(Debug, thiserror::Error)]
pub enum EffectiveConfigErr {
    #[error(transparent)]
    Trust(#[from] trust::TrustErr),
    #[error("cannot access {path}: {source}")]
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
    #[error("invalid project tasks config at {path}: {source}")]
    Tasks {
        path: PathBuf,
        #[source]
        source: ProjectTasksErr,
    },
    #[error("profiles are configured in {path} but the project is {state}; {fix}")]
    Blocked {
        path: PathBuf,
        state: &'static str,
        fix: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, EffectiveConfigErr>;

#[derive(Debug, thiserror::Error)]
pub enum ProjectTasksErr {
    #[error("task `{task}` sets `{field}`; {fix}")]
    UnsupportedField {
        task: String,
        field: &'static str,
        fix: &'static str,
    },
    #[error("task `{task}` has no prompt; set `prompt` or `prompt-file`")]
    MissingPrompt { task: String },
    #[error("task `{task}` must repeat; set `every` or `cron` for project tasks")]
    MustRepeat { task: String },
    #[error(transparent)]
    Budget(#[from] crate::config::TaskBudgetError),
    #[error(transparent)]
    Schedule(#[from] ScheduleErr),
}

#[derive(Default)]
struct RepoConfig {
    profiles: ProfilesConfig,
    teams: TeamsConfig,
}

#[derive(Debug)]
pub struct ProjectTasks {
    pub tasks: Tasks,
    pub state: TrustState,
    pub config_path: PathBuf,
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
    agents_spec::resolve_prompt_paths(&mut repo.profiles, &mut repo.teams, config_dir);
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
    validate_repo_team_profile_closure(&repo).map_err(|source| EffectiveConfigErr::Agents {
        path: config_path.clone(),
        source,
    })?;
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

fn validate_repo_team_profile_closure(repo: &RepoConfig) -> agents_spec::Result<()> {
    for (team_name, team) in &repo.teams.0 {
        for binding in &team.roles {
            if !repo.profiles.0.contains_key(&binding.profile) {
                return Err(LayoutErr::UnknownRoleProfile {
                    team: team_name.clone(),
                    role: binding.role.clone(),
                    profile: binding.profile.clone(),
                });
            }
        }
    }
    Ok(())
}

pub fn project_tasks(project_root: &Path, config_root: &Path) -> Result<Option<ProjectTasks>> {
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    let Some(repo_value) = read_repo_value(&config_path)? else {
        return Ok(None);
    };
    project_tasks_from_value(project_root, &config_path, report.state, &repo_value)
}

pub fn project_tasks_from_value(
    project_root: &Path,
    config_path: &Path,
    state: TrustState,
    value: &toml::Value,
) -> Result<Option<ProjectTasks>> {
    let Some(tasks_value) = value.get("tasks") else {
        return Ok(None);
    };
    reject_project_task_state_fields(tasks_value).map_err(|source| EffectiveConfigErr::Tasks {
        path: config_path.to_path_buf(),
        source,
    })?;
    let mut tasks: Tasks =
        tasks_value
            .clone()
            .try_into()
            .map_err(|source| EffectiveConfigErr::Parse {
                path: config_path.to_path_buf(),
                source,
            })?;
    let config_dir = config_path.parent().unwrap_or(project_root);
    for (name, entry) in &mut tasks.0 {
        schedule::validate_name(name).map_err(|source| EffectiveConfigErr::Tasks {
            path: config_path.to_path_buf(),
            source: source.into(),
        })?;
        entry.root = project_root.to_path_buf();
        if entry.agent.is_some() && !task_has_prompt(entry) {
            return Err(EffectiveConfigErr::Tasks {
                path: config_path.to_path_buf(),
                source: ProjectTasksErr::MissingPrompt { task: name.clone() },
            });
        }
        if entry.every.is_none() && entry.cron.is_none() {
            return Err(EffectiveConfigErr::Tasks {
                path: config_path.to_path_buf(),
                source: ProjectTasksErr::MustRepeat { task: name.clone() },
            });
        }
        entry
            .validate_budget(name)
            .map_err(|source| EffectiveConfigErr::Tasks {
                path: config_path.to_path_buf(),
                source: source.into(),
            })?;
        resolve_task_prompt_paths(entry, config_dir);
        schedule::parse_schedule(name, entry).map_err(|source| EffectiveConfigErr::Tasks {
            path: config_path.to_path_buf(),
            source: source.into(),
        })?;
    }
    Ok(Some(ProjectTasks {
        tasks,
        state,
        config_path: config_path.to_path_buf(),
    }))
}

fn task_has_prompt(entry: &TaskEntry) -> bool {
    entry
        .prompt
        .as_deref()
        .is_some_and(|prompt| !prompt.trim().is_empty())
        || entry
            .prompt_file
            .as_deref()
            .is_some_and(|path| !path.as_os_str().is_empty())
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
        Err(EffectiveConfigErr::Blocked {
            path: self.config_path.clone(),
            state: self.state.as_str(),
            fix: trust::blocked_fix(self.state),
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

fn reject_project_task_state_fields(
    tasks_value: &toml::Value,
) -> std::result::Result<(), ProjectTasksErr> {
    let Some(tasks) = tasks_value.as_table() else {
        return Ok(());
    };
    for (task, value) in tasks {
        let Some(table) = value.as_table() else {
            continue;
        };
        for field in ["root", "wake", "deadline"] {
            if table.contains_key(field) {
                return Err(ProjectTasksErr::UnsupportedField {
                    task: task.clone(),
                    field,
                    fix: match field {
                        "root" => "project tasks run at the project root; remove `root`",
                        "wake" => "project tasks cannot pin a machine-local session; use `agent`",
                        "deadline" => {
                            "poll-until deadlines are machine state; create them with `rimz loop add --until`"
                        }
                        _ => unreachable!("field list is fixed"),
                    },
                });
            }
        }
    }
    Ok(())
}

fn resolve_task_prompt_paths(entry: &mut TaskEntry, config_dir: &Path) {
    if let Some(path) = entry.prompt_file.as_mut() {
        *path = resolve_project_prompt_path(path, config_dir);
    }
    if let Some(path) = entry.system_prompt_file.as_mut() {
        *path = resolve_project_prompt_path(path, config_dir);
    }
}

fn resolve_project_prompt_path(path: &Path, config_dir: &Path) -> PathBuf {
    let expanded = crate::agents::transcript_fs::expand_tilde(&path.to_string_lossy());
    if expanded.is_absolute() {
        expanded
    } else {
        config_dir.join(expanded)
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
            layout_cells(layout).any(|token| {
                let is_declared_role = team.roles.iter().any(|binding| binding.role == token);
                !is_declared_role
                    && repo_profiles.contains(token)
                    && !machine_cell_word(token, profiles, commands)
            })
        });
        return role_refs_repo_profile || layout_refs_repo_profile;
    }
    layout_cells(spec).any(|token| {
        let (cell, _) = agents_spec::split_inline_role(token, profiles, commands);
        repo_profiles.contains(cell) && !machine_cell_word(cell, profiles, commands)
    })
}

fn layout_cells(raw: &str) -> impl Iterator<Item = &str> {
    agents_spec::parse_layout_structure(raw)
        .ok()
        .into_iter()
        .flat_map(|layout| layout.cells().collect::<Vec<_>>())
}

fn machine_cell_word(token: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    profiles.0.contains_key(token)
        || commands.0.contains_key(token)
        || agents_spec::parse_layout_spec(token, profiles, commands).is_ok()
}

#[cfg(test)]
mod tests;
