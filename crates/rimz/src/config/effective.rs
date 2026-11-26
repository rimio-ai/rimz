//! Effective launch configuration that depends on both machine config and the
//! trusted project config.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    #[error(
        "task `{task}` must repeat; set `every` or `cron`, or `signal` with `repeat = true`, for project tasks"
    )]
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
    failed_project_teams: BTreeMap<String, String>,
    project_definition_errors: Vec<String>,
    set_failure: Option<String>,
    repo_sources: AgentSpecSources,
    state: TrustState,
    config_path: PathBuf,
}

fn default_tier_preference(profile: &Profile) -> super::tiers::TierPreference {
    profile
        .definition_renders
        .as_ref()
        .map(|renders| renders.preference.clone())
        .or_else(|| {
            profile
                .definition
                .as_ref()
                .map(|def| def.preference().clone())
        })
        .unwrap_or_else(|| super::tiers::TierPreference {
            model: None,
            family: Some(profile.agent.clone()),
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "routes one profile row under an optional override base and reports why no kind base stands"
)]
fn route_model(
    profile: &Profile,
    base: Option<super::definitions::BuildBase<'_>>,
    base_kind: Option<&str>,
    model_str: &str,
    role: &str,
    table: &ProfilesConfig,
    fallback: Option<&ProfilesConfig>,
    failed_definitions: &BTreeMap<String, String>,
) -> std::result::Result<Profile, super::tiers::TierError> {
    let implied = crate::agents::definition_model_kind(model_str);
    if let Some(def) = &profile.definition {
        let target_kind = if let Some(kind) = base_kind {
            kind
        } else if crate::agents::keeps_model(&profile.agent, model_str) {
            profile.agent.as_str()
        } else {
            implied.unwrap_or(profile.agent.as_str())
        };
        if base.is_none()
            && let Some(error) = profile
                .definition_renders
                .as_ref()
                .and_then(|renders| renders.exclusions.get(target_kind))
        {
            return Err(super::tiers::TierError(format!(
                "{role}: model '{model_str}' ({target_kind}) cannot run this definition: {error}"
            )));
        }
        let mut selected = def
            .build(profile, base, None, Some(model_str))
            .map_err(|error| {
                super::tiers::TierError(format!(
                    "{role}: model '{model_str}' ({target_kind}) cannot run this definition: {}",
                    error.message
                ))
            })?;
        selected.model_tier = None;
        selected.tier_stamp = None;
        selected.definition_renders = profile.definition_renders.clone();
        return Ok(selected);
    }
    if profile.kind_base
        && base.is_none()
        && let Some(kind) =
            implied.filter(|_| !crate::agents::keeps_model(&profile.agent, model_str))
    {
        let renders = profile.definition_renders.as_ref();
        let base_row = renders
            .and_then(|renders| renders.renders.get(kind).cloned())
            .or_else(|| {
                agents_spec::routed_kind_base(kind, table, fallback).filter(|_| {
                    !failed_definitions.contains_key(kind) && table.failed(kind).is_none()
                })
            });
        if let Some(mut selected) = base_row {
            selected.model = Some(model_str.to_owned());
            selected.model_tier = None;
            selected.tier_stamp = None;
            return Ok(selected);
        }
        let reason = renders
            .and_then(|renders| renders.exclusions.get(kind))
            .or_else(|| failed_definitions.get(kind))
            .cloned()
            .unwrap_or_else(|| super::definitions::no_kind_base(kind));
        return Err(super::tiers::TierError(format!(
            "{role}: model '{model_str}' ({kind}) cannot run this definition: {reason}"
        )));
    }
    let mut cleared = profile.clone();
    cleared.model_tier = None;
    cleared.tier_stamp = None;
    Ok(cleared)
}

/// A failed tier walk's error, followed by why each excluded kind could not build.
fn walk_error<'a>(
    error: super::tiers::TierError,
    exclusions: impl Iterator<Item = (&'a String, &'a str)>,
) -> super::tiers::TierError {
    let reasons: Vec<String> = exclusions
        .map(|(kind, reason)| format!("{kind}: {reason}"))
        .collect();
    if reasons.is_empty() {
        return error;
    }
    super::tiers::TierError(format!("{error}; {}", reasons.join("; ")))
}

fn route_tier(
    profile: &Profile,
    base: Option<super::definitions::BuildBase<'_>>,
    requested: super::tiers::ModelTier,
    preference: super::tiers::TierPreference,
    table: &super::tiers::TierConfig,
    unavailable: &mut impl FnMut(&str, &str) -> Option<crate::agents::TierSkipReason>,
) -> std::result::Result<Option<Profile>, super::tiers::TierError> {
    let (pick, mut selected, renders) =
        if let (Some(def), Some(build_base)) = (&profile.definition, base) {
            let super::definitions::TierCandidates {
                engine,
                mut renders,
                exclusions,
            } = super::definitions::build_tier_candidates(
                table,
                requested,
                &preference,
                |kind, candidate_model| {
                    def.build(profile, Some(build_base), Some(kind), candidate_model)
                },
            );
            let pick = table
                .walk(
                    requested,
                    &preference,
                    |kind| exclusions.get(kind).map(|error| error.message.as_str()),
                    unavailable,
                )
                .map_err(|error| {
                    walk_error(
                        error,
                        exclusions
                            .iter()
                            .map(|(kind, error)| (kind, error.message.as_str())),
                    )
                })?;
            let selected = if engine {
                def.build(
                    profile,
                    Some(build_base),
                    Some(&pick.kind),
                    Some(&pick.model),
                )
                .map_err(|error| super::tiers::TierError(error.message))?
            } else {
                renders.remove(&pick.kind).ok_or_else(|| {
                    super::tiers::TierError(format!("no eligible build for {}", pick.kind))
                })?
            };
            (pick, selected, profile.definition_renders.clone())
        } else {
            let Some(mut renders) = profile.definition_renders.clone() else {
                return Ok(None);
            };
            let pick = table
                .walk(
                    requested,
                    &preference,
                    |kind| renders.exclusions.get(kind).map(String::as_str),
                    unavailable,
                )
                .map_err(|error| {
                    walk_error(
                        error,
                        renders
                            .exclusions
                            .iter()
                            .map(|(kind, reason)| (kind, reason.as_str())),
                    )
                })?;
            let mut selected = if let Some(rendered) = renders.renders.get(&pick.kind) {
                rendered.clone()
            } else if let Some(def) = &profile.definition {
                def.build(profile, None, Some(&pick.kind), Some(&pick.model))
                    .map_err(|error| super::tiers::TierError(error.message))?
            } else {
                return Err(super::tiers::TierError(format!(
                    "no eligible build for {}",
                    pick.kind
                )));
            };
            // A bare kind routes to another kind's base, whose effort is its own.
            if !selected.kind_base {
                selected.effort = renders.effort.clone();
            }
            renders.preference = preference;
            (pick, selected, Some(renders))
        };
    selected.model = Some(pick.model.clone());
    selected.model_tier = Some(super::tiers::TierProvenance {
        tier: requested,
        fell_back: pick.used_tier.is_some(),
    });
    selected.tier_stamp = Some(Box::new(crate::agents::TierStamp {
        tier: requested,
        model: pick.model,
        used_tier: pick.used_tier,
        unbuildable: pick.unbuildable,
        skipped: pick
            .skipped
            .into_iter()
            .map(|(model, reason)| crate::agents::TierSkip { model, reason })
            .collect(),
    }));
    selected.definition_renders = renders;
    Ok(Some(selected))
}

impl LaunchAgents {
    fn profiles_for_mut(&mut self, scope: ProfileScope) -> &mut ProfilesConfig {
        match scope {
            ProfileScope::Agents => &mut self.profiles,
            ProfileScope::Subagents => &mut self.subagent_profiles,
        }
    }

    /// Overlay a fresh launch without changing the memoized machine profiles.
    /// Explicit tiers and tiered profiles walk the tier table preferring `--agent`'s family when
    /// given; concrete models build the definition on the model's family unless the target kind
    /// keeps the model. `--agent` is never dropped or consumed by routing.
    #[expect(
        clippy::too_many_arguments,
        reason = "launch target and CLI routing inputs stay explicit"
    )]
    pub fn route(
        &mut self,
        table: &super::tiers::TierConfig,
        scope: ProfileScope,
        launched_profile: Option<&str>,
        tier: Option<super::tiers::ModelTier>,
        model: Option<&str>,
        family: Option<&str>,
        mut unavailable: impl FnMut(&str, &str) -> Option<crate::agents::TierSkipReason>,
    ) -> std::result::Result<(), super::tiers::TierError> {
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let family = family.map(str::trim).filter(|value| !value.is_empty());
        if tier.is_some() && model.is_some() {
            return Err(super::tiers::TierError(
                "choose one of --tier and --model".to_owned(),
            ));
        }
        if let Some(tier) = model.and_then(super::tiers::ModelTier::from_model) {
            return Err(super::tiers::TierError(format!("use `--tier {tier}`")));
        }
        let team_target = launched_profile
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .and_then(|raw| {
                let role = self.teams.role_spec(raw);
                if self.teams.0.contains_key(raw) {
                    Some((raw.to_owned(), None))
                } else {
                    role.map(|(team, role)| (team.to_owned(), Some(role.to_owned())))
                }
            });
        let updates = {
            let fallback = (scope == ProfileScope::Subagents).then_some(&self.profiles);
            let x = if let Some(family_str) = family {
                match agents_spec::resolve_agent_override(
                    Some(family_str),
                    self.profiles_for(scope),
                    fallback,
                ) {
                    Ok(ov) => ov,
                    Err(err) => {
                        if tier.is_some() {
                            if let LayoutErr::FailedProfile(detail) = err {
                                return Err(super::tiers::TierError(detail));
                            }
                            self.block_failed_override(
                                family_str,
                                self.profiles_for(scope),
                                fallback,
                            )
                            .map_err(|err| super::tiers::TierError(err.to_string()))?;
                        }
                        // Unresolved `--agent` names flow into `resolve_launch` so its failure ladder
                        // reports failed definitions, wrong doorway, and unknown overrides in order.
                        return Ok(());
                    }
                }
            } else {
                None
            };
            if let (Some(family_str), Some(ov), Some(model_str)) = (family, &x, model) {
                agents_spec::check_agent_model_family(
                    family_str,
                    ov.resolved.kind.as_str(),
                    model_str,
                )
                .map_err(super::tiers::TierError)?;
            }
            let targets = launched_profile
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|raw| {
                    let role = self.teams.role_spec(raw);
                    let team = self
                        .teams
                        .0
                        .get(raw)
                        .or_else(|| role.and_then(|(team, _)| self.teams.0.get(team)));
                    if let Some(team) = team {
                        team.roles
                            .iter()
                            .filter(|binding| role.is_none_or(|(_, role)| binding.role == role))
                            .map(|binding| {
                                (
                                    ProfileScope::Agents,
                                    binding.profile.clone(),
                                    binding.role.clone(),
                                )
                            })
                            .collect::<Vec<_>>()
                    } else {
                        agents_spec::inline_layout_profile_targets(raw, self.profiles_for(scope))
                            .into_iter()
                            .map(|(key, word)| (scope, key, word))
                            .collect()
                    }
                })
                .unwrap_or_default();
            let x_markdown = x.as_ref().and_then(|ov| {
                let profile = ov.profile.filter(|profile| profile.definition.is_some())?;
                let x_scope = if ov
                    .table
                    .is_some_and(|t| std::ptr::eq(t, &self.subagent_profiles))
                {
                    ProfileScope::Subagents
                } else {
                    ProfileScope::Agents
                };
                Some((x_scope, ov.name.as_str(), profile))
            });
            let is_bare_or_base = |target_scope: ProfileScope, name: &str| {
                self.profiles_for(target_scope).0.get(name).map_or_else(
                    || crate::agents::find_runtime(name).is_some(),
                    |profile| profile.kind_base,
                )
            };
            if tier.is_some() {
                self.block_set_failure()
                    .map_err(|err| super::tiers::TierError(err.to_string()))?;
                if let Some(raw) = launched_profile {
                    self.block_failed_team(raw)
                        .map_err(|err| super::tiers::TierError(err.to_string()))?;
                    self.block_failed_profile(raw, self.profiles_for(scope))
                        .map_err(|err| super::tiers::TierError(err.to_string()))?;
                }
                self.block_failed_reference(launched_profile, family)
                    .map_err(|err| super::tiers::TierError(err.to_string()))?;
                for (target_scope, name, _) in &targets {
                    self.block_failed_profile(name, self.profiles_for(*target_scope))
                        .map_err(|err| super::tiers::TierError(err.to_string()))?;
                }
                if let Some((team_name, None)) = &team_target
                    && let Some(team) = self.teams.0.get(team_name)
                    && let Some(layout) = team.layout.as_deref()
                {
                    for (name, _) in
                        agents_spec::inline_layout_profile_targets(layout, &self.profiles)
                    {
                        if !team.roles.iter().any(|binding| binding.role == name) {
                            self.block_failed_profile(&name, &self.profiles)
                                .map_err(|err| super::tiers::TierError(err.to_string()))?;
                        }
                    }
                }
                if targets.is_empty() {
                    return Err(super::tiers::TierError(
                        "--tier needs a profile definition; model tiers resolve in definitions"
                            .to_owned(),
                    ));
                }
                for (target_scope, name, role) in &targets {
                    let eligible =
                        self.profiles_for(*target_scope)
                            .0
                            .get(name)
                            .is_some_and(|profile| {
                                profile.definition.is_some() || profile.definition_renders.is_some()
                            })
                            || (x_markdown.is_some() && is_bare_or_base(*target_scope, name));
                    if !eligible {
                        return Err(super::tiers::TierError(format!(
                            "{role}: --tier needs a profile definition; model tiers resolve in definitions"
                        )));
                    }
                }
            }
            let x_routed_by_target = x_markdown.is_some()
                && targets
                    .iter()
                    .any(|(target_scope, name, _)| is_bare_or_base(*target_scope, name));
            let mut updates: BTreeMap<(ProfileScope, String), Profile> = BTreeMap::new();
            for (profile_scope, profiles) in [
                (ProfileScope::Agents, &self.profiles),
                (ProfileScope::Subagents, &self.subagent_profiles),
            ] {
                for (name, profile) in &profiles.0 {
                    let targeted = targets
                        .iter()
                        .any(|(ts, target, _)| *ts == profile_scope && target == name);
                    if targeted
                        || (x_routed_by_target
                            && x_markdown
                                .is_some_and(|(xs, xn, _)| xs == profile_scope && xn == name))
                    {
                        continue;
                    }
                    let Some(requested) = profile.model_tier.as_ref().map(|route| route.tier)
                    else {
                        continue;
                    };
                    if let Some(selected) = route_tier(
                        profile,
                        None,
                        requested,
                        default_tier_preference(profile),
                        table,
                        &mut unavailable,
                    )? {
                        updates.insert((profile_scope, name.clone()), selected);
                    }
                }
            }
            for (target_scope, name, role) in &targets {
                let (dest_scope, dest_name, profile, build_base, base_kind, routed_x) =
                    if is_bare_or_base(*target_scope, name)
                        && let Some((x_scope, x_name, x_profile)) = x_markdown
                    {
                        (
                            x_scope,
                            x_name,
                            x_profile,
                            None,
                            Some(x_profile.agent.as_str()),
                            true,
                        )
                    } else if let Some(profile) = self.profiles_for(*target_scope).0.get(name) {
                        (
                            *target_scope,
                            name.as_str(),
                            profile,
                            x.as_ref().map(agents_spec::AgentOverride::build_base),
                            x.as_ref().map(|ov| ov.resolved.kind.as_str()),
                            false,
                        )
                    } else {
                        continue;
                    };
                let dest_table = self.profiles_for(dest_scope);
                if let Some(model_str) = model {
                    let selected = route_model(
                        profile,
                        build_base,
                        base_kind,
                        model_str,
                        role,
                        dest_table,
                        (dest_scope == ProfileScope::Subagents).then_some(&self.profiles),
                        &self.failed_definitions,
                    )?;
                    updates.insert((dest_scope, dest_name.to_owned()), selected);
                    continue;
                }
                let family_pref = |kind: &str| super::tiers::TierPreference {
                    model: None,
                    family: Some(kind.to_owned()),
                };
                let (requested, preference) = if tier.is_some() {
                    let pref = base_kind
                        .map(family_pref)
                        .unwrap_or_else(|| default_tier_preference(profile));
                    (tier, pref)
                } else if !routed_x && let Some(ov) = &x {
                    if let Some(x_tier) = ov.written_tier() {
                        let pref = ov
                            .preference()
                            .cloned()
                            .unwrap_or_else(|| family_pref(ov.resolved.kind.as_str()));
                        (Some(x_tier), pref)
                    } else if ov.writes_engine {
                        // X writes a concrete model: drop L's tier without walking it.
                        let mut cleared = profile.clone();
                        cleared.model_tier = None;
                        cleared.tier_stamp = None;
                        updates.insert((dest_scope, dest_name.to_owned()), cleared);
                        continue;
                    } else {
                        (
                            profile.model_tier.as_ref().map(|route| route.tier),
                            family_pref(ov.resolved.kind.as_str()),
                        )
                    }
                } else {
                    (
                        profile.model_tier.as_ref().map(|route| route.tier),
                        default_tier_preference(profile),
                    )
                };
                let Some(requested) = requested else {
                    continue;
                };
                if let Some(selected) = route_tier(
                    profile,
                    build_base,
                    requested,
                    preference,
                    table,
                    &mut unavailable,
                )? {
                    updates.insert((dest_scope, dest_name.to_owned()), selected);
                }
            }
            updates
        };
        for ((profile_scope, name), profile) in updates {
            self.profiles_for_mut(profile_scope).0.insert(name, profile);
        }
        if tier.is_some()
            && let Some((team_name, role_filter)) = team_target
            && let Some(team) = self.teams.0.get_mut(&team_name)
        {
            for binding in &mut team.roles {
                if role_filter
                    .as_deref()
                    .is_none_or(|role| binding.role == role)
                {
                    binding.model = None;
                }
            }
        }
        Ok(())
    }
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
            failed_project_teams: BTreeMap::new(),
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
            failed_project_teams: BTreeMap::new(),
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
            config_path: &config_path,
            contexts: machine_config.definition_contexts.as_ref(),
            agents_home: config_root,
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
    let mut failed_project_teams = BTreeMap::new();
    for (team_name, team) in repo.teams.0 {
        match agents_spec::validate_team(
            &team_name,
            &team,
            &loaded_repo_profiles,
            &CommandsConfig::default(),
        ) {
            Ok(()) => {
                teams.0.insert(team_name, team);
            }
            Err(LayoutErr::FailedProfile(detail)) => {
                teams.0.remove(&team_name);
                failed_project_teams.insert(team_name, detail);
            }
            Err(source) => {
                return Err(EffectiveConfigErr::Agents {
                    path: config_path.clone(),
                    source,
                });
            }
        }
    }

    let project_definition_errors = resolved.errors.iter().map(ToString::to_string).collect();
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
        failed_project_teams,
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
    /// Formatted failure detail for a failed project team, a row that failed in this project, or a
    /// machine definition.
    pub fn definition_failure_for(&self, name: &str) -> Option<String> {
        self.failed_project_teams
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
        map.extend(self.failed_project_teams.clone());
        map
    }

    /// Refuse every launch while a definition failure no single name owns stands.
    pub fn block_set_failure(&self) -> Result<()> {
        match &self.set_failure {
            Some(detail) => Err(EffectiveConfigErr::FailedDefinition(detail.clone())),
            None => Ok(()),
        }
    }

    /// Refuse a bare team or `team.role` spec naming a failed project team ahead of any profile.
    pub(crate) fn block_failed_team(&self, spec: &str) -> Result<()> {
        let spec = spec.trim();
        if let Some(detail) = self.failed_project_teams.get(spec).or_else(|| {
            spec.split_once('.')
                .and_then(|(team, _)| self.failed_project_teams.get(team))
        }) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.clone()));
        }
        Ok(())
    }

    /// Refuse a name, team seat prefix, or unshadowed terminal kind when its definition failed.
    pub(crate) fn block_failed_profile(&self, name: &str, profiles: &ProfilesConfig) -> Result<()> {
        let name = name.trim();
        if let Some(detail) = profiles.failed(name) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
        }
        self.block_failed_team(name)?;
        if !profiles.0.contains_key(name) {
            if let Some(detail) = self.failed_definitions.get(name).or_else(|| {
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

    /// Refuse failed definitions or kind bases referenced by `name`, checking `own` then `fallback`.
    pub(crate) fn block_failed_override(
        &self,
        name: &str,
        own: &ProfilesConfig,
        fallback: Option<&ProfilesConfig>,
    ) -> Result<()> {
        let name = name.trim();
        if let Some(detail) = own.failed(name) {
            return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
        }
        let table = if own.0.contains_key(own.key(name)) {
            own
        } else if let Some(fb) = fallback {
            if let Some(detail) = fb.failed(name) {
                return Err(EffectiveConfigErr::FailedDefinition(detail.to_owned()));
            }
            if fb.0.contains_key(fb.key(name)) {
                fb
            } else {
                own
            }
        } else {
            own
        };
        self.block_failed_profile(name, table)
    }

    /// Refuse failed definitions consumed by a launch, leaving legacy bare rows alone.
    pub fn block_failed_launch(
        &self,
        kind: &crate::ids::AgentKind,
        profile: Option<&str>,
        record: Option<&crate::agents::LaunchRecord>,
        profiles: &ProfilesConfig,
        fallback: Option<&ProfilesConfig>,
    ) -> Result<()> {
        if let Some(name) = profile.or_else(|| record.map(|_| kind.as_str())) {
            self.block_failed_profile(name, profiles)?;
        }
        if let Some(agent) = record.and_then(|record| record.agent.as_deref()) {
            self.block_failed_override(agent, profiles, fallback)?;
        }
        Ok(())
    }

    /// Refuse unresolved names whose definitions failed to load.
    pub fn block_failed_reference(
        &self,
        spec: Option<&str>,
        agent_override: Option<&str>,
    ) -> Result<()> {
        let spec = spec.map(str::trim);
        let spec_cell = spec.and_then(|spec| spec.split_once(':').map(|(cell, _)| cell));
        let spec_fast = spec_cell
            .or(spec)
            .and_then(|name| name.strip_suffix("-fast"))
            .filter(|base| !base.is_empty());
        for name in [
            spec,
            spec_cell,
            spec_fast,
            agent_override.map(str::trim),
            spec.and_then(|spec| spec.split_once('.').map(|(team, _)| team)),
        ]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        {
            if let Some(detail) = self.definition_failure_for(name) {
                return Err(EffectiveConfigErr::FailedDefinition(detail));
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
                        "wait-meta" => "wait provenance is machine state; arm it with `rimz wait`",
                        "deadline" => "poll-until deadlines are machine state; remove `deadline`",
                        "watch" => "watched commands are machine state; use `rimz wait`",
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
    for profiles in [
        value.get("profiles"),
        value
            .get("subagents")
            .and_then(|subagents| subagents.get("profiles")),
    ]
    .into_iter()
    .flatten()
    .filter_map(toml::Value::as_table)
    {
        for (name, profile) in profiles {
            reject_project_tier_key(profile, name)?;
        }
    }
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
    let (profiles, profile_unknown_keys) = parse_repo_profiles(value.get("profiles"))?;
    let (subagent_profiles, subagent_unknown_keys) = parse_repo_profiles(
        value
            .get("subagents")
            .and_then(toml::Value::as_table)
            .and_then(|subagents| subagents.get("profiles")),
    )?;
    let teams = value
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| agents.get("teams"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let config = RepoConfig {
        env_reminder,
        lsp_servers,
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
