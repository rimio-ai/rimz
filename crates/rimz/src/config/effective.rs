//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{
    AgentSpecSources, CommandsConfig, ConfigFileDiagnosis, MachineConfig, ProfilesConfig,
    TaskEntry, Tasks, TeamsConfig,
};
use crate::harness::schedule::{self, ScheduleErr};
use crate::harness::spec::{self as agents_spec, LayoutErr};
use crate::trust::{self, TrustState};

const PROJECT_CONFIG_REL: &str = ".rimz/config.toml";

#[derive(Debug, thiserror::Error)]
pub enum EffectiveConfigErr {
    #[error("{0}")]
    FailedDefinition(String),
    #[error("project config cannot set lsp.{0}; move it to ~/.rimz/config.toml")]
    ProjectLspPolicy(String),
    #[error(transparent)]
    Trust(#[from] trust::TrustErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot load {path} — the file has a TOML error")]
    Parse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
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

impl EffectiveConfigErr {
    /// The classified TOML failure carried directly or through project trust.
    pub fn diagnosis(&self) -> Option<&ConfigFileDiagnosis> {
        match self {
            Self::Trust(source) => source.diagnosis(),
            Self::Parse { diagnosis, .. } => Some(diagnosis),
            _ => None,
        }
    }
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
    #[error("task `{task}` needs a trigger; set `every`, `cron`, or `signal` for project tasks")]
    MustRepeat { task: String },
    #[error(transparent)]
    Budget(#[from] super::loop_::TaskBudgetError),
    #[error(transparent)]
    Schedule(#[from] ScheduleErr),
}

#[derive(Default)]
struct RepoConfig {
    env_reminder: Option<bool>,
    lsp_servers: BTreeMap<String, super::LspServerConfig>,
    profiles: ProfilesConfig,
    subagent_profiles: ProfilesConfig,
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
    pub env_reminder: bool,
    pub lsp_servers: BTreeMap<String, super::LspServerConfig>,
    pub untrusted_lsp_servers: Vec<String>,
    pub profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    failed_definitions: BTreeMap<String, String>,
    set_failure: Option<String>,
    repo_sources: AgentSpecSources,
    state: TrustState,
    config_path: PathBuf,
}

/// Read `agents` and `subagents.profiles` from the machine snapshot without reloading it.
pub fn load(machine: &MachineConfig, project_root: &Path) -> Result<LaunchAgents> {
    load_with_roots(machine, project_root, &crate::disk::paths::rimz_home())
}

/// Effective teams for background policy, falling back to the machine snapshot.
pub fn teams(machine: &MachineConfig, project_root: Option<&Path>) -> TeamsConfig {
    teams_with_roots(machine, project_root, &crate::disk::paths::rimz_home())
}

fn teams_with_roots(
    machine: &MachineConfig,
    project_root: Option<&Path>,
    config_root: &Path,
) -> TeamsConfig {
    project_root
        .and_then(|root| load_with_roots(machine, root, config_root).ok())
        .map(|agents| agents.teams)
        .unwrap_or_else(|| machine.agents.teams.clone())
}

/// `trust::status_with_roots` parses `.rimz/config.toml` before anything here reads it, so a malformed project file fails at trust status whatever the trust state.
pub fn load_with_roots(
    machine: &MachineConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<LaunchAgents> {
    let failed_definitions = machine
        .notices
        .failed_definitions
        .keys()
        .filter_map(|name| {
            machine
                .definition_failure_for(name)
                .map(|detail| (name.clone(), detail))
        })
        .collect();
    let set_failure = machine.unattributed_definition_failure();
    let machine_subagent_profiles = &machine.subagents.profiles;
    let mut lsp_servers = machine.lsp.servers.clone();
    let machine = &machine.agents;
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    let repo_value = read_repo_value(&config_path)?;
    if let Some(key) = repo_value.as_ref().and_then(project_lsp_policy_key) {
        return Err(EffectiveConfigErr::ProjectLspPolicy(key.to_owned()));
    }
    if report.state != TrustState::Trusted {
        let untrusted_lsp_servers = repo_value
            .as_ref()
            .and_then(|value| {
                value
                    .get("lsp")?
                    .get("servers")?
                    .as_table()
                    .map(|servers| servers.keys().cloned().collect())
            })
            .unwrap_or_default();
        return Ok(LaunchAgents {
            env_reminder: machine.env_reminder,
            lsp_servers,
            untrusted_lsp_servers,
            profiles: machine.profiles.clone(),
            subagent_profiles: machine_subagent_profiles.clone(),
            teams: machine.teams.clone(),
            failed_definitions,
            set_failure,
            repo_sources: AgentSpecSources::default(),
            state: report.state,
            config_path,
        });
    }

    let Some(repo_value) = repo_value else {
        return Ok(LaunchAgents {
            env_reminder: machine.env_reminder,
            lsp_servers,
            untrusted_lsp_servers: Vec::new(),
            profiles: machine.profiles.clone(),
            subagent_profiles: machine_subagent_profiles.clone(),
            teams: machine.teams.clone(),
            failed_definitions,
            set_failure,
            repo_sources: AgentSpecSources::default(),
            state: report.state,
            config_path,
        });
    };
    let mut repo =
        repo_config_from_value(&repo_value).map_err(|source| EffectiveConfigErr::Parse {
            path: config_path.clone(),
            diagnosis: Box::new(ConfigFileDiagnosis::spanless(
                &config_path,
                source.message(),
            )),
        })?;
    lsp_servers.extend(std::mem::take(&mut repo.lsp_servers));
    let config_dir = config_path.parent().unwrap_or(project_root);
    agents_spec::resolve_prompt_paths(&mut repo.profiles, &mut repo.teams, config_dir);
    agents_spec::resolve_profile_prompt_paths(&mut repo.subagent_profiles, config_dir);
    for name in repo.profiles.0.keys() {
        if repo.profiles.0[name].isolation.is_some() {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileSetsIsolation {
                    profile: name.clone(),
                },
            });
        }
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
    for name in repo.subagent_profiles.0.keys() {
        if repo.subagent_profiles.0[name].isolation.is_some() {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileSetsIsolation {
                    profile: name.clone(),
                },
            });
        }
        agents_spec::resolve_profile(name, &repo.subagent_profiles).map_err(|source| {
            let source = match source {
                LayoutErr::UnknownProfileBase { profile, base }
                    if machine_subagent_profiles.0.contains_key(&base) =>
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
    agents_spec::validate_subagent_profile_namespace(
        &repo.subagent_profiles,
        &CommandsConfig::default(),
        &TeamsConfig::default(),
    )
    .map_err(|source| EffectiveConfigErr::Agents {
        path: config_path.clone(),
        source,
    })?;
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

    let repo_sources = AgentSpecSources {
        teams: Default::default(),
        agent_profiles: repo
            .profiles
            .0
            .keys()
            .map(|name| (name.clone(), config_path.clone()))
            .collect(),
        subagent_profiles: repo
            .subagent_profiles
            .0
            .keys()
            .map(|name| (name.clone(), config_path.clone()))
            .collect(),
        commands: Default::default(),
    };

    let mut profiles = machine.profiles.clone();
    profiles.0.extend(repo.profiles.0);
    let mut subagent_profiles = machine_subagent_profiles.clone();
    subagent_profiles.0.extend(repo.subagent_profiles.0);
    let mut teams = machine.teams.clone();
    teams.0.extend(repo.teams.0);
    agents_spec::validate_subagent_allowlists(&profiles, &subagent_profiles, &machine.commands)
        .map_err(|source| EffectiveConfigErr::Agents {
            path: config_path.clone(),
            source,
        })?;
    Ok(LaunchAgents {
        env_reminder: repo.env_reminder.unwrap_or(machine.env_reminder),
        lsp_servers,
        untrusted_lsp_servers: Vec::new(),
        profiles,
        subagent_profiles,
        teams,
        failed_definitions,
        set_failure,
        repo_sources,
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
    if report.state == TrustState::NoConfig {
        return Ok(None);
    }
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
                diagnosis: Box::new(ConfigFileDiagnosis::spanless(config_path, source.message())),
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
        if entry.every.is_none() && entry.cron.is_none() && entry.signal.is_none() {
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
        schedule::parse_trigger(name, entry).map_err(|source| EffectiveConfigErr::Tasks {
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
    /// Refuse every launch while a definition failure no single name owns stands.
    pub fn block_set_failure(&self) -> Result<()> {
        match &self.set_failure {
            Some(detail) => Err(EffectiveConfigErr::FailedDefinition(detail.clone())),
            None => Ok(()),
        }
    }

    /// Refuse unresolved names whose Markdown definitions failed to load.
    pub fn block_failed_reference(
        &self,
        spec: Option<&str>,
        agent_override: Option<&str>,
    ) -> Result<()> {
        let spec = spec.map(str::trim);
        for name in [
            spec,
            agent_override.map(str::trim),
            spec.and_then(|spec| spec.split_once('.').map(|(team, _)| team)),
        ]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        {
            if let Some(detail) = self.failed_definitions.get(name) {
                return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
            }
        }
        Ok(())
    }

    /// Overlay trusted project profile provenance onto machine catalog sources.
    pub fn overlay_profile_sources(&self, sources: &mut AgentSpecSources) {
        sources
            .agent_profiles
            .extend(self.repo_sources.agent_profiles.clone());
        sources
            .subagent_profiles
            .extend(self.repo_sources.subagent_profiles.clone());
    }

    pub fn profiles_for(&self, scope: ProfileScope) -> &ProfilesConfig {
        match scope {
            ProfileScope::Agents => &self.profiles,
            ProfileScope::Subagents => &self.subagent_profiles,
        }
    }

    /// Return a trust error only when a requested launch spec would consume a
    /// repo profile or team while the project is not trusted. Repo entries are
    /// otherwise inert: machine profiles, machine commands, and built-in cells
    /// keep launching in an untrusted checkout even when `.rimz/config.toml`
    /// declares profiles.
    pub fn block_untrusted_reference(
        &self,
        scope: ProfileScope,
        spec: Option<&str>,
        commands: &CommandsConfig,
    ) -> Result<()> {
        let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
            return Ok(());
        };
        if matches!(self.state, TrustState::Trusted | TrustState::NoConfig) {
            return Ok(());
        }
        let Some(repo_value) = read_repo_value(&self.config_path)? else {
            return Ok(());
        };
        let repo_profiles = profile_names(&repo_value, scope);
        let repo_teams = team_names(&repo_value);
        let team_spec = spec.split_once('.').map_or(spec, |(team, _)| team);
        if (repo_profiles.is_empty()
            || !spec_references_repo_profile(
                spec,
                &repo_profiles,
                self.profiles_for(scope),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileScope {
    Agents,
    Subagents,
}

fn read_repo_value(path: &Path) -> Result<Option<toml::Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<toml::Value>(&text)
            .map(Some)
            .map_err(|source| EffectiveConfigErr::Parse {
                path: path.to_path_buf(),
                diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, &text, &source)),
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
        for field in [
            "root",
            "dir",
            "wait",
            "wait-meta",
            "deadline",
            "watch",
            "once",
        ] {
            if table.contains_key(field) {
                return Err(ProjectTasksErr::UnsupportedField {
                    task: task.clone(),
                    field,
                    fix: match field {
                        "root" => "project tasks run at the project root; remove `root`",
                        "dir" => "project tasks run at the project root; remove `dir`",
                        "wait" => "project tasks cannot pin a machine-local session; use `agent`",
                        "wait-meta" => "wait provenance is machine state; arm it with `rimz wait`",
                        "deadline" => {
                            "poll-until deadlines are machine state; create them with `rimz loop add --until`"
                        }
                        "watch" => "watched commands are machine state; use `rimz wait`",
                        "once" => "one-shot subscriptions are machine state; remove `once`",
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

fn project_lsp_policy_key(value: &toml::Value) -> Option<&'static str> {
    let lsp = value.get("lsp")?.as_table()?;
    [
        "reserve-percent",
        "reserve-min",
        "kill-floor-percent",
        "idle-timeout",
    ]
    .into_iter()
    .find(|key| lsp.contains_key(*key))
}

fn repo_config_from_value(value: &toml::Value) -> std::result::Result<RepoConfig, toml::de::Error> {
    let env_reminder = value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("env-reminder"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?;
    if let Some(key) = project_lsp_policy_key(value) {
        return Err(serde::de::Error::custom(
            EffectiveConfigErr::ProjectLspPolicy(key.to_owned()),
        ));
    }
    let lsp_servers = value
        .get("lsp")
        .and_then(|lsp| lsp.get("servers"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let profiles = value
        .get("profiles")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let subagent_profiles = value
        .get("subagents")
        .and_then(toml::Value::as_table)
        .and_then(|subagents| subagents.get("profiles"))
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
    Ok(RepoConfig {
        env_reminder,
        lsp_servers,
        profiles,
        subagent_profiles,
        teams,
    })
}

fn profile_names(value: &toml::Value, scope: ProfileScope) -> BTreeSet<String> {
    let profiles = match scope {
        ProfileScope::Agents => value.get("profiles"),
        ProfileScope::Subagents => value
            .get("subagents")
            .and_then(toml::Value::as_table)
            .and_then(|subagents| subagents.get("profiles")),
    };
    profiles
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
