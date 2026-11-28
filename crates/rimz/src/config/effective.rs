//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::project_file::{ProjectAgents, ProjectFile};
use crate::config::{
    AgentSpecSources, CommandsConfig, ConfigFileDiagnosis, DefinitionOrigin, MachineConfig,
    Profile, ProfileSource, ProfilesConfig, Tasks, TeamsConfig,
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
    #[error("project config cannot set [tiers]; move it to ~/.rimz/config.toml")]
    ProjectTiers,
    #[error("project config cannot set [models]; move it to ~/.rimz/config.toml")]
    ProjectModels,
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
    #[error("invalid project agents config at {path}: {message}")]
    TeamConsensus { path: PathBuf, message: String },
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
    #[error("task `{task}` must repeat; set `cron` for project tasks")]
    MustRepeat { task: String },
    #[error(transparent)]
    Schedule(#[from] ScheduleErr),
}

#[derive(Default)]
struct RepoConfig {
    env_reminder: Option<bool>,
    lsp_servers: BTreeMap<String, super::LspServerConfig>,
    profiles: ProfilesConfig,
    subagent_profiles: ProfilesConfig,
    profile_unknown_keys: BTreeMap<String, Vec<String>>,
    subagent_unknown_keys: BTreeMap<String, Vec<String>>,
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
#[derive(Clone)]
pub struct LaunchAgents {
    pub env_reminder: bool,
    /// Volatile Environment facts arrive at prompt submit where the adapter can carry them.
    pub runtime_env: bool,
    pub allow_routine_rimz: bool,
    pub lsp_servers: BTreeMap<String, super::LspServerConfig>,
    pub untrusted_lsp_servers: Vec<String>,
    pub profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    failed_definitions: BTreeMap<String, String>,
    failed_trees: BTreeMap<String, ProfileScope>,
    failed_teams: BTreeMap<String, String>,
    project_definition_errors: Vec<String>,
    set_failure: Option<String>,
    repo_sources: AgentSpecSources,
    state: TrustState,
    config_path: PathBuf,
}

/// Read `agents` and `subagents.profiles` from the machine snapshot without reloading it.
pub fn load(machine: &MachineConfig, project_root: &Path) -> Result<LaunchAgents> {
    load_with_roots(machine, project_root, &crate::disk::paths::rimz_home())
}

/// Seat policy uses trusted effective tables, or the supplied machine snapshot on failure.
pub(crate) fn load_seat_tables(
    machine: &MachineConfig,
    project_root: Option<&Path>,
) -> (TeamsConfig, ProfilesConfig, ProfilesConfig) {
    match project_root.and_then(|root| load(machine, root).ok()) {
        Some(launch) => (launch.teams, launch.profiles, launch.subagent_profiles),
        None => (
            machine.agents.teams.clone(),
            machine.agents.profiles.clone(),
            machine.subagents.profiles.clone(),
        ),
    }
}

/// `trust::status_with_roots` parses `.rimz/config.toml` before anything here reads it, so a malformed project file fails at trust status whatever the trust state.
pub fn load_with_roots(
    machine: &MachineConfig,
    project_root: &Path,
    config_root: &Path,
) -> Result<LaunchAgents> {
    let machine_config = machine;
    let failed_definitions = machine.failed_definition_details();
    // A machine team file that failed owns its name ahead of every reading but a command.
    let machine_failed_teams: BTreeMap<String, String> = machine
        .notices
        .failed_teams
        .iter()
        .filter_map(|name| Some((name.clone(), failed_definitions.get(name)?.clone())))
        .collect();
    let failed_trees = machine.notices.failed_trees.clone();
    let set_failure = machine.unattributed_definition_failure();
    let machine_subagent_profiles = &machine.subagents.profiles;
    let mut lsp_servers = machine.lsp.servers.clone();
    let auto_compact_default = machine.harness.auto_compact_default();
    let consensus_pieces = &machine.consensus_pieces;
    let machine = &machine.agents;
    let report = trust::status_with_roots(project_root, config_root)?;
    let config_path = project_root.join(PROJECT_CONFIG_REL);
    let repo_value = if report.state == TrustState::NoConfig {
        None
    } else {
        read_repo_value(&config_path)?
    };
    if let Some(key) = repo_value.as_ref().and_then(project_lsp_policy_key) {
        return Err(EffectiveConfigErr::ProjectLspPolicy(key.to_owned()));
    }
    if repo_value
        .as_ref()
        .is_some_and(|value| value.get("tiers").is_some())
    {
        return Err(EffectiveConfigErr::ProjectTiers);
    }
    if repo_value
        .as_ref()
        .is_some_and(|value| value.get("models").is_some())
    {
        return Err(EffectiveConfigErr::ProjectModels);
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
            runtime_env: machine.runtime_env,
            allow_routine_rimz: machine.allow_routine_rimz,
            lsp_servers,
            untrusted_lsp_servers,
            profiles: machine.profiles.clone(),
            subagent_profiles: machine_subagent_profiles.clone(),
            teams: machine.teams.clone(),
            failed_definitions,
            failed_trees,
            failed_teams: machine_failed_teams,
            project_definition_errors: Vec::new(),
            set_failure,
            repo_sources: AgentSpecSources::default(),
            state: report.state,
            config_path,
        });
    }

    let Some(repo_value) = repo_value else {
        return Ok(LaunchAgents {
            env_reminder: machine.env_reminder,
            runtime_env: machine.runtime_env,
            allow_routine_rimz: machine.allow_routine_rimz,
            lsp_servers,
            untrusted_lsp_servers: Vec::new(),
            profiles: machine.profiles.clone(),
            subagent_profiles: machine_subagent_profiles.clone(),
            teams: machine.teams.clone(),
            failed_definitions,
            failed_trees,
            failed_teams: machine_failed_teams,
            project_definition_errors: Vec::new(),
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
    for (name, team) in &mut repo.teams.0 {
        consensus_pieces.compose(name, team).map_err(|message| {
            EffectiveConfigErr::TeamConsensus {
                path: config_path.clone(),
                message,
            }
        })?;
    }
    agents_spec::resolve_profile_prompt_paths(&mut repo.subagent_profiles, config_dir);
    let is_machine_profile = |base: &str| {
        machine
            .profiles
            .0
            .get(base)
            .is_some_and(|row| !row.kind_base)
            || machine_subagent_profiles
                .0
                .get(base)
                .is_some_and(|row| !row.kind_base)
    };
    for (name, profile) in &repo.profiles.0 {
        if profile.isolation.is_some() {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileSetsIsolation {
                    profile: name.clone(),
                },
            });
        }
        let base = profile.agent.as_str();
        if crate::agents::find_runtime(base).is_none()
            && !repo.profiles.0.contains_key(base)
            && is_machine_profile(base)
        {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileEscapesTrust {
                    profile: name.clone(),
                    base: base.to_owned(),
                },
            });
        }
    }
    for (name, profile) in &repo.subagent_profiles.0 {
        if profile.isolation.is_some() {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileSetsIsolation {
                    profile: name.clone(),
                },
            });
        }
        let base = profile.agent.as_str();
        if crate::agents::find_runtime(base).is_none()
            && !repo.subagent_profiles.0.contains_key(base)
            && !repo.profiles.0.contains_key(base)
            && is_machine_profile(base)
        {
            return Err(EffectiveConfigErr::Agents {
                path: config_path.clone(),
                source: LayoutErr::RepoProfileEscapesTrust {
                    profile: name.clone(),
                    base: base.to_owned(),
                },
            });
        }
    }
    validate_repo_team_profile_closure(&repo).map_err(|source| EffectiveConfigErr::Agents {
        path: config_path.clone(),
        source,
    })?;
    for (team_name, team) in &repo.teams.0 {
        agents_spec::validate_team_structure(
            team_name,
            team,
            &repo.profiles,
            &CommandsConfig::default(),
        )
        .map_err(|source| EffectiveConfigErr::Agents {
            path: config_path.clone(),
            source,
        })?;
    }

    let mut profile_unknown_keys = std::mem::take(&mut repo.profile_unknown_keys);
    let mut subagent_unknown_keys = std::mem::take(&mut repo.subagent_unknown_keys);
    let agents_input = repo
        .profiles
        .0
        .into_iter()
        .map(|(name, profile)| {
            let unknown = profile_unknown_keys.remove(&name).unwrap_or_default();
            (name, (profile, unknown))
        })
        .collect();
    let subagents_input = repo
        .subagent_profiles
        .0
        .into_iter()
        .map(|(name, profile)| {
            let unknown = subagent_unknown_keys.remove(&name).unwrap_or_default();
            (name, (profile, unknown))
        })
        .collect();
    let resolved = super::definitions::resolve_project_profiles(
        super::definitions::ProjectProfilesInput {
            agents_home: config_root,
            config_path: &config_path,
            contexts: machine_config.definition_contexts.as_ref(),
            tiers: &machine_config.tiers,
            auto_compact_default,
            commands: &machine.commands,
            machine_subagent_profiles,
            machine_agents: &machine.profiles,
            agents: agents_input,
            subagents: subagents_input,
        },
        |name| failed_definitions.get(name).cloned(),
    );

    let project_source = ProfileSource {
        path: config_path.clone(),
        origin: DefinitionOrigin::Project,
    };
    let repo_sources = AgentSpecSources {
        teams: Default::default(),
        agent_profiles: resolved
            .agents
            .keys()
            .map(|name| (name.clone(), project_source.clone()))
            .collect(),
        subagent_profiles: resolved
            .subagents
            .keys()
            .map(|name| (name.clone(), project_source.clone()))
            .collect(),
        commands: Default::default(),
    };

    let mut loaded_repo_profiles = ProfilesConfig::from(resolved.agents);
    loaded_repo_profiles.set_failed_rows(resolved.failed_agents.clone());
    let mut profiles = machine.profiles.clone();
    profiles.0.extend(resolved.machine_agents);
    profiles.0.extend(loaded_repo_profiles.0.clone());
    let mut failed_agents = resolved.failed_agents;
    failed_agents.extend(resolved.failed_machine_agents);
    // A failed kind base keeps its row for the definitions that replace its list, and refuses
    // its own name through the failure record.
    for failed_name in failed_agents.keys() {
        if profiles
            .0
            .get(failed_name)
            .is_some_and(|row| !row.kind_base)
        {
            profiles.0.remove(failed_name);
        }
    }
    profiles.set_failed_rows(failed_agents);
    profiles.add_failed_bases(resolved.failed_machine_bases);
    let mut subagent_profiles = machine_subagent_profiles.clone();
    subagent_profiles.0.extend(resolved.subagents);
    for failed_name in resolved.failed_subagents.keys() {
        if subagent_profiles
            .0
            .get(failed_name)
            .is_some_and(|row| !row.kind_base)
        {
            subagent_profiles.0.remove(failed_name);
        }
    }
    subagent_profiles.set_failed_rows(resolved.failed_subagents);

    let mut teams = machine.teams.clone();
    let mut failed_teams = machine_failed_teams;
    // A machine team with a cell, role or layout, that rests on a row failed in this project
    // fails on its own name here, as a project team does below. The machine load accepted the
    // team, so any other error keeps it.
    teams.0.retain(|team_name, team| {
        let Err(LayoutErr::FailedProfile(detail)) =
            agents_spec::validate_team(team_name, team, &profiles, &machine.commands)
        else {
            return true;
        };
        failed_teams.insert(team_name.clone(), detail);
        false
    });
    let mut project_definition_errors: Vec<String> =
        resolved.errors.iter().map(ToString::to_string).collect();
    for (team_name, team) in repo.teams.0 {
        match agents_spec::validate_team(
            &team_name,
            &team,
            &loaded_repo_profiles,
            &CommandsConfig::default(),
        ) {
            Ok(()) => {
                failed_teams.remove(&team_name);
                teams.0.insert(team_name, team);
            }
            // A seat that does not build under the engine its role chose fails its team alone.
            Err(
                error @ (LayoutErr::FailedProfile(_)
                | LayoutErr::CannotRun { .. }
                | LayoutErr::Engine(_)),
            ) => {
                teams.0.remove(&team_name);
                failed_teams.insert(team_name, error.to_string());
            }
            // A team without a leader fails alone, and room entry warns about it as it does
            // about a project row that did not load.
            Err(error @ LayoutErr::MissingLeader { .. }) => {
                let detail = format!("{}: {error}", config_path.display());
                teams.0.remove(&team_name);
                project_definition_errors.push(detail.clone());
                failed_teams.insert(team_name, detail);
            }
            Err(source) => {
                return Err(EffectiveConfigErr::Agents {
                    path: config_path.clone(),
                    source,
                });
            }
        }
    }

    Ok(LaunchAgents {
        env_reminder: repo.env_reminder.unwrap_or(machine.env_reminder),
        runtime_env: machine.runtime_env,
        allow_routine_rimz: machine.allow_routine_rimz,
        lsp_servers,
        untrusted_lsp_servers: Vec::new(),
        profiles,
        subagent_profiles,
        teams,
        failed_definitions,
        failed_trees,
        failed_teams,
        project_definition_errors,
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
    for (name, entry) in &mut tasks.0 {
        crate::harness::name::validate_name(name).map_err(|source| EffectiveConfigErr::Tasks {
            path: config_path.to_path_buf(),
            source: ScheduleErr::from(source).into(),
        })?;
        entry.root = project_root.to_path_buf();
        if entry.refused.is_some() {
            continue;
        }
        if !schedule::repeats(entry) {
            return Err(EffectiveConfigErr::Tasks {
                path: config_path.to_path_buf(),
                source: ProjectTasksErr::MustRepeat { task: name.clone() },
            });
        }
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

impl LaunchAgents {
    /// Formatted failure detail for a team that failed in this project (a project team, or a
    /// machine team with a seat or layout cell that failed here) or whose machine file failed, a
    /// row that failed in this project, or a machine definition.
    pub fn definition_failure_for(&self, name: &str) -> Option<String> {
        self.failed_teams
            .get(name)
            .or_else(|| self.profiles.failed_rows().get(name))
            .or_else(|| self.subagent_profiles.failed_rows().get(name))
            .or_else(|| self.failed_definitions.get(name))
            .cloned()
    }

    /// Ordered failure messages for trusted project definition rows that did not load.
    pub fn project_definition_errors(&self) -> &[String] {
        &self.project_definition_errors
    }

    /// Combined machine and project definition failures, with this project's failures winning
    /// collisions.
    pub(crate) fn all_failed_definitions(&self) -> BTreeMap<String, String> {
        let mut map = self.failed_definitions.clone();
        map.extend(self.subagent_profiles.failed_rows().clone());
        map.extend(self.profiles.failed_rows().clone());
        map.extend(self.failed_teams.clone());
        map
    }

    /// Refuse every launch while a definition failure no single name owns stands.
    pub fn block_set_failure(&self) -> Result<()> {
        match &self.set_failure {
            Some(detail) => Err(EffectiveConfigErr::FailedDefinition(detail.clone())),
            None => Ok(()),
        }
    }

    /// Refuse a bare team or `team.role` spec naming a team that failed in this project, or whose
    /// machine file failed, ahead of any profile, the built-in team, or an executable on PATH.
    pub(crate) fn block_failed_team(&self, spec: &str) -> Result<()> {
        let spec = spec.trim();
        if let Some(detail) = self.failed_teams.get(spec).or_else(|| {
            spec.split_once('.')
                .and_then(|(team, _)| self.failed_teams.get(team))
        }) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
        }
        Ok(())
    }

    /// Refuse a name, team seat prefix, or unshadowed terminal kind when its definition failed,
    /// reading `profiles` as the table of `scope`.
    pub(crate) fn block_failed_profile(
        &self,
        name: &str,
        scope: ProfileScope,
        profiles: &ProfilesConfig,
    ) -> Result<()> {
        let name = name.trim();
        if let Some(detail) = profiles.failed(name) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
        }
        self.block_failed_team(name)?;
        if !profiles.0.contains_key(name) {
            if let Some(detail) = self.machine_failure(scope, name).or_else(|| {
                name.split_once('.')
                    .and_then(|(team, _)| self.failed_definitions.get(team))
            }) {
                return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
            }
            if let Some(kind) = crate::agents::find_runtime(name)
                .map(crate::agents::Runtime::kind)
                .filter(|kind| !profiles.0.contains_key(*kind))
                && let Some(detail) = self.failed_definitions.get(kind)
            {
                return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
            }
            return Ok(());
        }
        if let Ok(profile) = agents_spec::resolve_profile(name, profiles)
            && !profiles.0.contains_key(profile.kind.as_str())
            && let Some(detail) = self.failed_definitions.get(profile.kind.as_str())
        {
            return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
        }
        Ok(())
    }

    /// Refuse failed definitions or kind bases referenced by `name`, checking `own`, the table of
    /// `scope`, then `fallback`, the agents table.
    pub(crate) fn block_failed_override(
        &self,
        name: &str,
        scope: ProfileScope,
        own: &ProfilesConfig,
        fallback: Option<&ProfilesConfig>,
    ) -> Result<()> {
        let name = name.trim();
        if let Some(detail) = own.failed(name) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
        }
        let (scope, table) = if own.0.contains_key(own.key(name)) {
            (scope, own)
        } else if let Some(fb) = fallback {
            if let Some(detail) = fb.failed(name) {
                return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
            }
            if fb.0.contains_key(fb.key(name)) {
                (ProfileScope::Agents, fb)
            } else {
                (scope, own)
            }
        } else {
            (scope, own)
        };
        self.block_failed_profile(name, scope, table)
    }

    /// Refuse failed definitions consumed by a launch in `scope`, leaving legacy bare rows alone:
    /// the profile is read from the table `scope` resolves it from, and the recorded `--agent`
    /// from that table then, on a subagent launch, the agents table.
    pub fn block_failed_launch(
        &self,
        kind: &crate::ids::AgentKind,
        profile: Option<&str>,
        record: Option<&crate::agents::LaunchRecord>,
        scope: ProfileScope,
    ) -> Result<()> {
        let (table_scope, profiles) =
            scope.resolve_launch(profile, &self.profiles, &self.subagent_profiles);
        let fallback = (scope == ProfileScope::Subagents).then_some(&self.profiles);
        if let Some(name) = profile.or_else(|| record.map(|_| kind.as_str())) {
            self.block_failed_profile(name, table_scope, profiles)?;
        }
        if let Some(agent) = record.and_then(|record| record.agent.as_deref()) {
            self.block_failed_override(agent, table_scope, profiles, fallback)?;
        }
        Ok(())
    }

    /// Refuse names whose definitions failed to load, asking first the tables `scope` resolves
    /// them from: a spec's teams and `scope`'s table, and an `--agent` name's table then the
    /// agents table on a subagent launch. A healthy row there ends the lookup ahead of the machine
    /// failure record; a name they lack refuses with the other table's failure by that name. A
    /// launch asks this only for a name it does not resolve.
    pub fn block_failed_reference(
        &self,
        scope: ProfileScope,
        spec: Option<&str>,
        agent_override: Option<&str>,
    ) -> Result<()> {
        let fallback = (scope == ProfileScope::Subagents).then_some(ProfileScope::Agents);
        let spec = spec.map(str::trim).filter(|spec| !spec.is_empty());
        let spec_cell = spec.and_then(|spec| spec.split_once(':').map(|(cell, _)| cell));
        let spec_fast = spec_cell
            .or(spec)
            .and_then(|name| name.strip_suffix("-fast"))
            .filter(|base| !base.is_empty());
        if let Some(spec) = spec {
            self.block_failed_team(spec)?;
        }
        let other = scope.other();
        for name in [spec, spec_cell, spec_fast].into_iter().flatten() {
            self.block_failed_row(name, &[scope], Some(other))?;
        }
        if let Some(name) = agent_override
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            // A subagent launch's `--agent` already reads the agents table as its fallback.
            let unread = (scope == ProfileScope::Agents).then_some(other);
            let scopes: Vec<_> = std::iter::once(scope).chain(fallback).collect();
            self.block_failed_row(name, &scopes, unread)?;
        }
        if let Some(team) = spec.and_then(|spec| spec.split_once('.').map(|(team, _)| team))
            && let Some(detail) = self.failed_definitions.get(team)
        {
            return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
        }
        Ok(())
    }

    /// Refuse `name` when the first table of `scopes` that holds it holds a failure, or when none
    /// holds it and a machine definition of theirs by that name failed, or, short of both, when
    /// the table of `other` (one the launch does not resolve from) holds a failure by that name,
    /// its failed row over its machine definition, and neither a healthy row there nor a team
    /// takes it: the launch names the wrong doorway, and that failure is what the user has to fix.
    /// A healthy row there shadows its machine definition, so the caller's own wrong-doorway hint
    /// answers instead.
    fn block_failed_row(
        &self,
        name: &str,
        scopes: &[ProfileScope],
        other: Option<ProfileScope>,
    ) -> Result<()> {
        for table in scopes.iter().map(|scope| self.profiles_for(*scope)) {
            if let Some(detail) = table.failed(name) {
                return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
            }
            if table.0.contains_key(table.key(name)) {
                return Ok(());
            }
        }
        let wrong_doorway = other
            .filter(|_| !self.teams.0.contains_key(name) && self.teams.role_spec(name).is_none())
            .and_then(|other| {
                let table = self.profiles_for(other);
                if table.0.contains_key(table.key(name)) {
                    return None;
                }
                table
                    .failed_rows()
                    .get(name)
                    .or_else(|| self.machine_failure(other, name))
            });
        match scopes
            .iter()
            .find_map(|scope| self.machine_failure(*scope, name))
            .or(wrong_doorway)
        {
            Some(detail) => Err(EffectiveConfigErr::FailedDefinition(detail.clone())),
            None => Ok(()),
        }
    }

    /// The machine failure the table of `scope` answers for `name`: none for a definition only
    /// the other scope's tree declared, which fails in that table alone.
    fn machine_failure(&self, scope: ProfileScope, name: &str) -> Option<&String> {
        self.failed_definitions.get(name).filter(|_| {
            self.failed_trees
                .get(name)
                .is_none_or(|tree| *tree == scope)
        })
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

    /// Resolve a profile through its launch doorway, including the team-role exception.
    pub fn profiles_for_launch(
        &self,
        scope: ProfileScope,
        profile: Option<&str>,
    ) -> &ProfilesConfig {
        scope
            .resolve_launch(profile, &self.profiles, &self.subagent_profiles)
            .1
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileScope {
    Agents,
    Subagents,
}

/// Resolve a profile-level setting in launch chain order (L -> X -> final kind base) from
/// in-memory tables without reading disk. An `agent_override` that no longer resolves reads
/// as absent. When `direct_when_unchanged` is set and neither `--agent` nor a kind switch
/// moved the profile, reads the profile row directly without walking parents or kind bases.
#[expect(
    clippy::too_many_arguments,
    reason = "caller identity, launch scope, both profile tables, and unchanged-read mode are passed explicitly"
)]
pub(crate) fn resolve_seat_setting<T>(
    profile: Option<&str>,
    agent_override: Option<&str>,
    target_kind: Option<&str>,
    scope: ProfileScope,
    profiles: &ProfilesConfig,
    subagent_profiles: &ProfilesConfig,
    direct_when_unchanged: bool,
    pick: impl Fn(&Profile) -> Option<T>,
) -> Option<T> {
    let (resolved_scope, own) = scope.resolve_launch(profile, profiles, subagent_profiles);
    let subagent_scope = resolved_scope == ProfileScope::Subagents;
    let fallback = subagent_scope.then_some(profiles);
    let x = agents_spec::resolve_agent_override(agent_override, own, fallback)
        .ok()
        .flatten();
    agents_spec::pick_in_launch_chain(
        profile,
        own,
        fallback,
        x.as_ref(),
        target_kind,
        subagent_scope,
        direct_when_unchanged,
        pick,
    )
}

/// A seat's declared keep-warm rule, before TTL admission and live stage evaluation.
pub(crate) struct KeepWarmSeat<'a> {
    pub team: Option<&'a str>,
    pub role: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub scope: ProfileScope,
    pub launched_child: bool,
}

impl<'a> From<&'a crate::agents::AgentState> for KeepWarmSeat<'a> {
    fn from(agent: &'a crate::agents::AgentState) -> Self {
        Self {
            team: agent.team.as_deref(),
            role: agent.role.as_deref(),
            profile: agent.profile.as_deref(),
            agent: agent
                .record
                .as_ref()
                .and_then(|record| record.agent.as_deref()),
            kind: Some(agent.kind.as_str()),
            scope: ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child()),
            launched_child: agent.is_launched_child(),
        }
    }
}

impl KeepWarmSeat<'_> {
    pub fn declared_horizon(
        &self,
        teams: &TeamsConfig,
        profiles: &ProfilesConfig,
        subagent_profiles: &ProfilesConfig,
        harness: &super::HarnessConfig,
    ) -> Option<(std::time::Duration, Option<String>)> {
        if self.launched_child {
            return None;
        }
        let setting = match self.team {
            Some(team) => teams
                .0
                .get(team)?
                .keep_warm_with_default(self.role, harness.keep_warm)?,
            None => super::KeepWarmFields {
                max: Some(
                    resolve_seat_setting(
                        self.profile,
                        self.agent,
                        self.kind,
                        self.scope,
                        profiles,
                        subagent_profiles,
                        false,
                        |profile| {
                            profile
                                .keep_warm
                                .as_ref()
                                .and_then(|policy| policy.fields().max)
                        },
                    )
                    .unwrap_or(harness.keep_warm),
                ),
                while_clause: None,
            },
        };
        let horizon = match setting.max {
            Some(super::KeepWarm::For(max)) => Some(max),
            _ => setting
                .while_clause
                .as_ref()
                .and(harness.cache_keepalive_max),
        }?;
        Some((horizon, setting.while_clause))
    }
}

/// A seat's idle-compaction floor, also resolvable before a launch has a row.
pub(crate) struct IdleCompactSeat<'a> {
    pub team: Option<&'a str>,
    pub role: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub scope: ProfileScope,
    pub launched_child: bool,
}

impl<'a> From<&'a crate::agents::AgentState> for IdleCompactSeat<'a> {
    fn from(agent: &'a crate::agents::AgentState) -> Self {
        Self {
            team: agent.team.as_deref(),
            role: agent.role.as_deref(),
            profile: agent.profile.as_deref(),
            agent: agent
                .record
                .as_ref()
                .and_then(|record| record.agent.as_deref()),
            kind: Some(agent.kind.as_str()),
            scope: ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child()),
            launched_child: agent.is_launched_child(),
        }
    }
}

impl IdleCompactSeat<'_> {
    pub fn resolve(
        &self,
        teams: &TeamsConfig,
        profiles: &ProfilesConfig,
        subagent_profiles: &ProfilesConfig,
        harness: &super::HarnessConfig,
    ) -> Option<u64> {
        if self.launched_child {
            return None;
        }
        let role = self.team.and_then(|team| {
            let role = self.role?;
            teams
                .0
                .get(team)?
                .roles
                .iter()
                .find(|binding| binding.role == role)?
                .idle_compact
        });
        let setting = role
            .or_else(|| {
                resolve_seat_setting(
                    self.profile,
                    self.agent,
                    self.kind,
                    self.scope,
                    profiles,
                    subagent_profiles,
                    false,
                    |profile| profile.idle_compact,
                )
            })
            .unwrap_or(harness.idle_compact);
        match setting {
            super::IdleCompact::Off => None,
            super::IdleCompact::Tokens(floor) => Some(floor),
        }
    }
}

/// A seat's live configuration identity, also constructible before a launch has a row.
pub(crate) struct SmartCompactSeat<'a> {
    pub team: Option<&'a str>,
    pub role: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub scope: ProfileScope,
}

impl<'a> From<&'a crate::agents::AgentState> for SmartCompactSeat<'a> {
    fn from(agent: &'a crate::agents::AgentState) -> Self {
        Self {
            team: agent.team.as_deref(),
            role: agent.role.as_deref(),
            profile: agent.profile.as_deref(),
            agent: agent
                .record
                .as_ref()
                .and_then(|record| record.agent.as_deref()),
            kind: Some(agent.kind.as_str()),
            scope: ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child()),
        }
    }
}

impl SmartCompactSeat<'_> {
    pub fn explicit(
        &self,
        teams: &TeamsConfig,
        profiles: &ProfilesConfig,
        subagent_profiles: &ProfilesConfig,
    ) -> Option<super::FlipCompact> {
        let role = self.team.and_then(|team| {
            let role = self.role?;
            teams
                .0
                .get(team)?
                .roles
                .iter()
                .find(|binding| binding.role == role)?
                .smart_compact
        });
        role.or_else(|| {
            resolve_seat_setting(
                self.profile,
                self.agent,
                self.kind,
                self.scope,
                profiles,
                subagent_profiles,
                false,
                |profile| profile.smart_compact,
            )
        })
    }

    pub fn resolve(
        &self,
        teams: &TeamsConfig,
        profiles: &ProfilesConfig,
        subagent_profiles: &ProfilesConfig,
        harness: &super::HarnessConfig,
    ) -> Option<crate::store::message::AutoCompact> {
        match self.explicit(teams, profiles, subagent_profiles) {
            Some(super::FlipCompact::Off) => None,
            Some(super::FlipCompact::Threshold(line)) => Some(line),
            None => harness.smart_compact,
        }
    }
}

impl ProfileScope {
    /// The scope whose table a launch in this one does not resolve from.
    fn other(self) -> Self {
        match self {
            Self::Agents => Self::Subagents,
            Self::Subagents => Self::Agents,
        }
    }

    /// A recorded doorway wins over the one implied by child launch treatment.
    pub fn for_launch(recorded: Option<Self>, child: bool) -> Self {
        recorded.unwrap_or(if child { Self::Subagents } else { Self::Agents })
    }

    /// Select the scope and table, keeping agents-only team-role profiles on their table without
    /// letting a failed project subagent row fall through to a same-named agents profile.
    pub fn resolve_launch<'a>(
        self,
        profile: Option<&str>,
        profiles: &'a ProfilesConfig,
        subagent_profiles: &'a ProfilesConfig,
    ) -> (Self, &'a ProfilesConfig) {
        let agents_only = profile.is_some_and(|name| {
            subagent_profiles.failed(name).is_none()
                && !subagent_profiles.0.contains_key(name)
                && profiles.0.contains_key(name)
        });
        if self == Self::Subagents && !agents_only {
            (Self::Subagents, subagent_profiles)
        } else {
            (Self::Agents, profiles)
        }
    }
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
            "when",
            "for",
            "each-worktree",
            "loop-task",
            "booked-by",
        ] {
            if table.contains_key(field) {
                return Err(ProjectTasksErr::UnsupportedField {
                    task: task.clone(),
                    field,
                    fix: match field {
                        "root" => "project tasks run at the project root; remove `root`",
                        "dir" => "project tasks run at the project root; remove `dir`",
                        "wait" => "project tasks cannot pin a machine-local session; use `command`",
                        "wait-meta" => {
                            "wait provenance is machine state; arm it with `rimz shell` or `rimz watch`"
                        }
                        "deadline" => "poll-until deadlines are machine state; remove `deadline`",
                        "watch" => "watched commands are machine state; use `rimz shell`",
                        "when" | "for" => {
                            "project tasks cannot use conditions yet; add it with `rimz loop add` without `--project`"
                        }
                        "each-worktree" => {
                            "per-worktree fan-out is machine state; add it with `rimz loop add` without `--project`"
                        }
                        "loop-task" => {
                            "resident subscription provenance is machine state; remove `loop-task`"
                        }
                        "booked-by" => "booking provenance is machine state; remove `booked-by`",
                        _ => unreachable!("field list is fixed"),
                    },
                });
            }
        }
    }
    Ok(())
}

fn project_lsp_policy_key(value: &toml::Value) -> Option<&'static str> {
    let lsp = value.get("lsp")?.as_table()?;
    [
        "reserve-percent",
        "reserve-min",
        "idle-timeout",
        "recycle-factor",
        "recycle-min",
    ]
    .into_iter()
    .find(|key| lsp.contains_key(*key))
}

fn repo_config_from_value(value: &toml::Value) -> std::result::Result<RepoConfig, toml::de::Error> {
    if let Some(key) = project_lsp_policy_key(value) {
        return Err(serde::de::Error::custom(
            EffectiveConfigErr::ProjectLspPolicy(key.to_owned()),
        ));
    }
    let file: ProjectFile = value.clone().try_into()?;
    let subagent_profiles = file.subagents.and_then(|subagents| subagents.profiles);
    for (name, profile) in [&file.profiles, &subagent_profiles]
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flatten()
    {
        reject_project_tier_key(profile, name)?;
    }
    // A role's `tier` is a key `RoleBinding` does not declare, so it is read from the file.
    if let Some(teams) = value
        .get("agents")
        .and_then(|agents| agents.get("teams"))
        .and_then(toml::Value::as_table)
    {
        for (name, team) in teams {
            for role in team
                .get("roles")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                reject_project_tier_key(role, name)?;
            }
        }
    }
    let (teams, env_reminder) = match file.agents {
        ProjectAgents::Table(agents) => (agents.teams, agents.env_reminder),
        ProjectAgents::Absent | ProjectAgents::Entries => (TeamsConfig::default(), None),
    };
    let (profiles, profile_unknown_keys) = parse_repo_profiles(file.profiles.as_ref())?;
    let (subagent_profiles, subagent_unknown_keys) =
        parse_repo_profiles(subagent_profiles.as_ref())?;
    let config = RepoConfig {
        env_reminder,
        lsp_servers: file.lsp.map(|lsp| lsp.servers).unwrap_or_default(),
        profiles,
        subagent_profiles,
        profile_unknown_keys,
        subagent_unknown_keys,
        teams,
    };
    for (scope, profiles) in [
        ("profiles", &config.profiles),
        ("subagents.profiles", &config.subagent_profiles),
    ] {
        for (name, profile) in &profiles.0 {
            reject_project_tier(&format!("{scope}.{name}.model"), profile.model.as_deref())?;
        }
    }
    for (name, team) in &config.teams.0 {
        for role in &team.roles {
            reject_project_tier(
                &format!("agents.teams.{name}.roles.{}.model", role.role),
                role.model.as_deref(),
            )?;
        }
    }
    Ok(config)
}

fn parse_repo_profiles(
    value: Option<&toml::Value>,
) -> std::result::Result<(ProfilesConfig, BTreeMap<String, Vec<String>>), toml::de::Error> {
    let Some(value) = value else {
        return Ok((ProfilesConfig::default(), BTreeMap::new()));
    };
    let Some(table) = value.as_table() else {
        let profiles: ProfilesConfig = value.clone().try_into()?;
        return Ok((profiles, BTreeMap::new()));
    };
    let mut profiles = BTreeMap::new();
    let mut unknown_keys = BTreeMap::new();
    for (name, row_value) in table {
        let mut ignored = Vec::new();
        let profile: Profile = serde_ignored::deserialize(row_value.clone(), |path| {
            let key = path.to_string();
            if !ignored.contains(&key) {
                ignored.push(key);
            }
        })?;
        if !ignored.is_empty() {
            unknown_keys.insert(name.clone(), ignored);
        }
        profiles.insert(name.clone(), profile);
    }
    Ok((ProfilesConfig::from(profiles), unknown_keys))
}

fn reject_project_tier(key: &str, model: Option<&str>) -> std::result::Result<(), toml::de::Error> {
    if let Some(tier) = model.and_then(super::tiers::ModelTier::from_model) {
        return Err(serde::de::Error::custom(format!(
            "project {key} = '{tier}': model tiers resolve in Markdown definitions; name a concrete model"
        )));
    }
    Ok(())
}

fn reject_project_tier_key(
    value: &toml::Value,
    name: &str,
) -> std::result::Result<(), toml::de::Error> {
    if value.get("tier").is_some() {
        return Err(serde::de::Error::custom(format!(
            "project {name} sets tier: model tiers resolve in Markdown definitions; name a concrete model"
        )));
    }
    Ok(())
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
