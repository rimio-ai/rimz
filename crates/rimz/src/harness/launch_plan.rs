//! Read-only launch compilation and explicit runtime artifact materialization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ProviderLogin;
use crate::agents::capabilities::SystemTextChannel;
use crate::agents::skill_links::{self, Desired, SkillLinkErr, SkillLinkOutcome, SkillLinkPlan};
use crate::config::effective::LaunchAgents;
use crate::config::{AccountsConfig, CommandsConfig, Isolation};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::sandbox::{self, SandboxPlan};

use super::launch::{self, AgentProcessStage, CompiledAgentProcess, ExecRequest};
use super::launch_reminders::{LaunchReminders, TeamReminder, TempFiles};
use super::prompt_compose::{
    self, MaterializedSystemPrompt, SystemPromptPlan, SystemPromptSources,
};

/// The room's shared dir, at its host path in both isolations.
const ENV_SHARED: &str = "RIMZ_SHARED";

pub struct LaunchPlanInputs<'a> {
    pub request: &'a ExecRequest,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub rimz_bin: &'a Path,
    pub runtime: &'a RuntimePaths,
    pub state: &'a StatePaths,
    pub effective: Option<&'a LaunchAgents>,
    pub commands: &'a CommandsConfig,
    pub accounts: &'a AccountsConfig,
    pub bwrap: Option<&'a Path>,
    /// The room's agent rows, where a launched child finds its parent's
    /// handle, or why they could not be read.
    pub agents: Result<&'a [crate::agents::AgentState], &'a str>,
    pub ambient_env: &'a BTreeMap<String, String>,
    /// The machine's `[agents] shell`, read from machine config directly since
    /// `effective` is absent when the effective config fails to load.
    pub agent_shell: Option<&'a Path>,
}

pub struct LaunchPlan {
    pub request: ExecRequest,
    pub cwd: PathBuf,
    /// The provider account this launch runs under.
    pub login: ProviderLogin,
    pub prompt: SystemPromptPlan,
    pub reminder_channel: Option<SystemTextChannel>,
    pub stage: AgentProcessStage,
    pub sandbox: Option<SandboxPlan>,
    pub skill_links: Option<SkillLinkPlan>,
    pub warnings: Vec<LaunchPlanWarning>,
    temp_owner: Option<String>,
    runtime: RuntimePaths,
    state: StatePaths,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchPlanWarning {
    #[error(
        "{kind} has no per-launch skill switch; profile skills are not enforced under host isolation"
    )]
    HostSkillsUnenforced { kind: crate::ids::AgentKind },
    #[error(
        "{kind} has no per-launch permission rules; allowed-tools is not applied and the agent prompts as usual: remove allowed-tools from the definition or run it on claude"
    )]
    ToolRulesUnsupported { kind: crate::ids::AgentKind },
    #[error("launching with default RimZ launch reminders")]
    DefaultReminders,
    #[error("team `{0}` is no longer configured; launching without the team context reminder")]
    MissingTeam(String),
    #[error("parent {0} has no row in the room; this launch uses its own temp directory")]
    ParentUnitMissing(crate::ids::AgentSessionId),
    #[error(
        "could not read the room's agents to find parent {parent}'s temp directory ({error}); this launch uses its own"
    )]
    ParentUnitUnread {
        parent: crate::ids::AgentSessionId,
        error: String,
    },
}

/// The handle whose temp unit an agent uses: a launched child shares its
/// parent's, read from the parent's row; anyone else owns its own. `Err` names
/// a parent with no row, and the agent falls back to its own unit.
fn temp_owner<'a>(
    own: Option<&'a str>,
    parent: Option<(&crate::ids::AgentKind, &crate::ids::AgentSessionId)>,
    agents: &'a [crate::agents::AgentState],
) -> Result<(Option<&'a str>, bool), crate::ids::AgentSessionId> {
    let Some((kind, id)) = parent else {
        return Ok((own, false));
    };
    crate::address::launch_row(agents, kind, id)
        .map(|row| (row.name.as_deref(), true))
        .ok_or_else(|| id.clone())
}

/// The host temp unit `agent` uses, as its launch resolved it.
pub fn agent_temp_unit(
    agent: &crate::agents::AgentState,
    agents: &[crate::agents::AgentState],
    state: &StatePaths,
) -> PathBuf {
    let parent = agent
        .parent_agent_id
        .as_ref()
        .map(|id| (agent.parent_agent_kind.as_ref().unwrap_or(&agent.kind), id));
    let own = agent.name.as_deref();
    state.temp_unit_dir(temp_owner(own, parent, agents).map_or(own, |(owner, _)| owner))
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchPlanErr {
    #[error(transparent)]
    Prompt(#[from] prompt_compose::PromptComposeErr),
    #[error(transparent)]
    Process(#[from] launch::AgentProcessStageErr),
    #[error(transparent)]
    Wire(#[from] launch::ExecWireErr),
    #[error(transparent)]
    Sandbox(#[from] sandbox::SandboxErr),
    #[error(transparent)]
    SkillLinks(#[from] SkillLinkErr),
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error("unknown agent kind `{0}`")]
    UnknownAgent(crate::ids::AgentKind),
    #[error(transparent)]
    Login(#[from] crate::agents::RoomLoginErr),
    #[error(transparent)]
    Preset(#[from] crate::agents::PresetErr),
}

impl LaunchPlan {
    pub fn process(&self) -> &CompiledAgentProcess {
        match &self.stage {
            AgentProcessStage::Ready(process)
            | AgentProcessStage::LoginShellReentry { process, .. } => process,
        }
    }
}

pub fn resolve_model(
    request: &mut ExecRequest,
    machine: &crate::config::MachineConfig,
    runtime: &RuntimePaths,
    login: &ProviderLogin,
    recorded_session: Option<&crate::agents::AgentState>,
    source: Option<&mut dyn crate::agents::capabilities::ModelCatalogSource>,
    ambient_env: &BTreeMap<String, String>,
) -> Result<
    (
        Vec<String>,
        Option<crate::agents::capabilities::ModelAliasMove>,
    ),
    LaunchPlanErr,
> {
    use crate::agents::{LaunchPreset, PresetField, capabilities::ModelAliasRequest};
    let mut warnings = Vec::new();
    let mut movement = None;
    let recorded_model = recorded_session.and_then(|agent| agent.model.as_deref());
    request.identity.params.record = Some(crate::agents::LaunchRecord::replay_or_new(
        request.identity.params.record.as_deref(),
        request.identity.params.model.as_deref(),
        request.identity.params.effort.as_deref(),
    ));
    let Some(alias) = request.identity.params.model.as_deref() else {
        return Ok((warnings, movement));
    };
    let adapter = crate::agents::find_definition(request.kind.as_str())
        .ok_or_else(|| LaunchPlanErr::UnknownAgent(request.kind.clone()))?;
    let pin = machine.model_alias(&request.kind, alias);
    let is_alias = pin.is_some() || adapter.is_model_alias(alias);
    if !is_alias {
        return Ok((warnings, movement));
    }
    let replay = if matches!(request.action, launch::ExecAction::Launch { .. })
        || request.identity.resume_model_override
        || recorded_session.is_some_and(|agent| agent.record.is_some())
        || request
            .identity
            .params
            .tier
            .as_ref()
            .is_some_and(|stamp| stamp.model != alias)
    {
        None
    } else {
        let recorded_model = recorded_model.filter(|model| {
            machine.model_alias(&request.kind, model).is_none() && !adapter.is_model_alias(model)
        });
        if recorded_model.is_none() {
            warnings.push(format!(
                "{} alias {alias} has no recorded session model; resolving afresh",
                request.kind
            ));
        }
        recorded_model
    };
    let id = if let Some(model) = replay {
        model.to_owned()
    } else if let Some(pin) = pin {
        if adapter.known_catalog_model(runtime, &login.key(), pin) == Some(false) {
            warnings.push(format!("{} configured model {pin} is absent from the cached catalog; check the model pin in machine config", request.kind));
        }
        pin.to_owned()
    } else if let Some(resolved) = adapter.resolve_model_alias(
        ModelAliasRequest {
            alias,
            effort: request.identity.params.effort.as_deref(),
            login,
            login_env: &login.env(ambient_env),
            paths: runtime,
        },
        source,
    ) {
        warnings.extend(resolved.warnings);
        movement = resolved.movement;
        resolved.id
    } else {
        return Ok((warnings, movement));
    };
    let args = adapter.spec().render_preset(&LaunchPreset {
        model: Some(id.clone()),
        ..LaunchPreset::default()
    })?;
    if let Some(matcher) = adapter.spec().launch.preset_arg_matcher(PresetField::Model) {
        matcher.remove_occurrences(request.action.extra_args_mut());
    }
    request.action.extra_args_mut().extend(args);
    if replay.is_some() {
        if let Some(record) = request.identity.params.record.as_mut() {
            record.model = Some(id.clone());
        }
        if request
            .identity
            .params
            .tier
            .as_ref()
            .is_some_and(|stamp| stamp.model != id)
        {
            request.identity.params.tier = None;
        }
    }
    request.identity.params.model = Some(id);
    Ok((warnings, movement))
}

pub fn compile(inputs: LaunchPlanInputs<'_>) -> Result<LaunchPlan, LaunchPlanErr> {
    let mut request = inputs.request.clone();
    let adapter = crate::agents::find_definition(request.kind.as_str())
        .ok_or_else(|| LaunchPlanErr::UnknownAgent(request.kind.clone()))?;
    let prompt = prompt_compose::plan_system_prompt(
        &request.kind,
        &SystemPromptSources {
            system_prompt_file: request.system_prompt_file.clone(),
            append_system_prompt_files: request.append_system_prompt_files.clone(),
            team_prompt: request.team_prompt.clone(),
        },
        inputs.runtime,
    )?;
    apply_materialized_system_prompt(&mut request, &prompt.materialized);
    let (mut reminders, mut warnings) = reminders(&request, inputs.effective, inputs.commands);
    if request
        .allowed_tools
        .as_ref()
        .is_some_and(|rules| !rules.is_empty())
        && matches!(
            adapter.spec().tool_rules,
            crate::agents::skills::ToolRules::Unsupported
        )
    {
        warnings.push(LaunchPlanWarning::ToolRulesUnsupported {
            kind: request.kind.clone(),
        });
    }
    match crate::lsp::registry::live_server_names(inputs.cwd) {
        Ok(servers) => reminders.lsp_servers = servers,
        Err(error) => tracing::debug!(%error, "language-server launch reminder unavailable"),
    }
    reminders.sandbox = inputs.bwrap.is_some();
    reminders.settings = Some((inputs.runtime.prompt_dir(), inputs.ambient_env.clone()));
    reminders.env = inputs
        .effective
        .is_none_or(|effective| effective.env_reminder);
    reminders.agent_shell = inputs.agent_shell.map(Path::to_path_buf);
    let login = crate::agents::session_login(
        &request.kind,
        request.identity.params.login.as_ref(),
        inputs.accounts,
    )?;
    // The block rides the provider's prompt hook: without wired hooks the
    // launch keeps its listing, as a provider without the capability does.
    reminders.runtime_env = adapter.spec().capabilities.prompt_context
        && inputs
            .effective
            .is_none_or(|effective| effective.runtime_env)
        && crate::agents::preflight_hooks(
            adapter,
            &login.env(inputs.ambient_env),
            crate::agents::TurnLifecycleNeed::None,
        )
        .is_ok();
    let mut extra_env = prompt.materialized.env.clone();
    extra_env.extend(login.overrides(inputs.ambient_env));
    let isolation = if inputs.bwrap.is_some() {
        crate::config::Isolation::Sandbox
    } else {
        crate::config::Isolation::Host
    };
    extra_env.insert(Isolation::ENV.to_owned(), isolation.to_string());
    let params = &request.identity.params;
    let parent = params.parent_agent_id.as_ref().map(|id| {
        (
            params.parent_agent_kind.as_ref().unwrap_or(&request.kind),
            id,
        )
    });
    let own = request.identity.name.as_deref();
    let (owner, caller) = match inputs.agents {
        Ok(agents) => temp_owner(own, parent, agents).unwrap_or_else(|parent| {
            warnings.push(LaunchPlanWarning::ParentUnitMissing(parent));
            // Still a child: it has a caller and no subagents of its own.
            (own, true)
        }),
        Err(error) => {
            if let Some((_, parent)) = parent {
                warnings.push(LaunchPlanWarning::ParentUnitUnread {
                    parent: parent.clone(),
                    error: error.to_owned(),
                });
            }
            (own, parent.is_some())
        }
    };
    let temp_owner = owner.map(str::to_owned);
    let unit = inputs.state.temp_unit_dir(owner);
    let view = sandbox::TmpView::current(isolation, owner, inputs.state);
    let tmp = view.agent_path(&unit);
    let shared = inputs.state.room_shared_dir.clone();
    if inputs
        .effective
        .is_none_or(|effective| effective.allow_routine_rimz)
    {
        reminders.routine_rimz = Some((inputs.runtime.prompt_dir(), [tmp.clone(), shared.clone()]));
    }
    for key in ["TMPDIR"].iter().chain(adapter.temp_dir_env_keys()) {
        extra_env.insert((*key).to_owned(), tmp.display().to_string());
    }
    // Save the TMPDIR replaced here for the mux children the agent starts.
    // Inside an agent's tree the ambient TMPDIR is already a unit, so an
    // inherited save carries forward.
    let user_tmpdir = crate::mux::domain::USER_TMPDIR_ENV;
    let saved = inputs
        .ambient_env
        .get(user_tmpdir)
        .or_else(|| inputs.ambient_env.get("TMPDIR"))
        .cloned()
        .unwrap_or_default();
    extra_env.insert(user_tmpdir.to_owned(), saved);
    // List the temp-root keys pointed at a unit, inherited ones included, so
    // the mux restore drops them without knowing any provider.
    let keys_env = crate::mux::domain::TEMP_ROOT_KEYS_ENV;
    let temp_root_keys: std::collections::BTreeSet<&str> = inputs
        .ambient_env
        .get(keys_env)
        .into_iter()
        .flat_map(|list| list.split_whitespace())
        .chain(adapter.temp_dir_env_keys().iter().copied())
        .collect();
    extra_env.insert(
        keys_env.to_owned(),
        temp_root_keys.into_iter().collect::<Vec<_>>().join(" "),
    );
    extra_env.insert(ENV_SHARED.to_owned(), shared.display().to_string());
    // zellij derives its socket base from TMPDIR when nothing pins it; keep
    // the endpoint the wrapper's own environment resolves.
    extra_env.insert(
        "ZELLIJ_SOCKET_DIR".to_owned(),
        crate::mux::domain::ProcessDomain::zellij_socket_base(inputs.ambient_env)
            .display()
            .to_string(),
    );
    reminders.files = Some(TempFiles {
        tmp,
        shared,
        caller,
    });
    let mut stage = launch::compile_agent_process_stage_with_extra_env(
        inputs.project_root,
        &request,
        inputs.cwd,
        inputs.rimz_bin,
        inputs.runtime,
        &extra_env,
        &reminders,
    )?;
    let process = match &stage {
        AgentProcessStage::Ready(process)
        | AgentProcessStage::LoginShellReentry { process, .. } => process,
    };
    let mut env = inputs.ambient_env.clone();
    if process.host_skills == Some(crate::agents::skills::HostSkillPlan::Unenforced) {
        warnings.push(LaunchPlanWarning::HostSkillsUnenforced {
            kind: request.kind.clone(),
        });
    }
    env.extend(process.env.clone());
    let skill_links = if inputs.bwrap.is_none() {
        adapter
            .skills_home(&env)
            .zip(crate::disk::paths::skills_library_in(&env))
            .map(|(root, library)| skill_links::plan(&root, &library, Desired::Library))
            .transpose()?
            .filter(|plan| !plan.is_empty())
    } else {
        None
    };
    let sandbox =
        if let (Some(bwrap), AgentProcessStage::Ready(process)) = (inputs.bwrap, &mut stage) {
            let provider_home = adapter.config_home(&env).map(|path| sandbox::ProviderHome {
                source: path.clone(),
                target: path,
            });
            let plan = sandbox::plan(&sandbox::SandboxInputs {
                env: &env,
                cwd: inputs.cwd,
                project_root: inputs.project_root,
                worktree: request.worktree_path.as_deref(),
                tmp_dir: &unit,
                skills_dir: &inputs
                    .state
                    .agent_skills_dir(request.identity.name.as_deref()),
                provider_home,
                provider_home_env_keys: adapter.config_home_env_keys(),
                skills: sandbox::SkillInputs {
                    kind: request.kind.as_str(),
                    home: adapter.skills_home(&env),
                    manual: adapter.manual_skill(),
                    callable: request.skills.as_deref(),
                },
            })?;
            process.pin_env(plan.pins.clone());
            let cwd = view.agent_path(inputs.cwd);
            process.argv = sandbox::bwrap_argv(bwrap, &plan.plan, &cwd, &process.argv);
            Some(plan)
        } else {
            None
        };
    Ok(LaunchPlan {
        request,
        cwd: inputs.cwd.to_path_buf(),
        login,
        prompt,
        reminder_channel: adapter.append_system_text_channel(),
        stage,
        sandbox,
        skill_links,
        warnings,
        temp_owner,
        runtime: inputs.runtime.clone(),
        state: inputs.state.clone(),
    })
}

pub fn apply(plan: &LaunchPlan) -> Result<Option<SkillLinkOutcome>, LaunchPlanErr> {
    plan.runtime.ensure_dirs()?;
    if let Some((path, settings, _)) = &plan.process().settings_artifact {
        crate::disk::paths::ensure_private_runtime_dir(&plan.runtime.prompt_dir())?;
        crate::disk::atomic::write_private_temp_then_rename(path, settings)
            .map_err(launch::ExecWireErr::PromptWrite)?;
    }
    prompt_compose::apply_system_prompt(&plan.prompt)?;
    refresh_consensus_copy(plan);
    if let AgentProcessStage::LoginShellReentry {
        prompt_artifacts, ..
    } = &plan.stage
    {
        for artifact in prompt_artifacts {
            launch::write_prompt_artifact(artifact)?;
        }
    }
    plan.state.ensure_temp_unit(plan.temp_owner.as_deref())?;
    if let Some(sandbox) = &plan.sandbox {
        sandbox::apply(sandbox)?;
    }
    plan.skill_links
        .as_ref()
        .map(skill_links::apply)
        .transpose()
        .map_err(Into::into)
}

/// Keep the inspection copy current across upgrades without waiting for the
/// next `rimz setup`. The launch composes from the embedded text and never
/// reads the copy, so a failed refresh is enrichment lost, never a refusal.
fn refresh_consensus_copy(plan: &LaunchPlan) {
    let built_in = plan
        .prompt
        .sources
        .team_prompt
        .as_ref()
        .is_some_and(|layer| layer.consensus == super::team_prompt::Consensus::BuiltIn);
    if !built_in {
        return;
    }
    if let Err(error) =
        super::team_prompt::publish_consensus_copy(&crate::disk::paths::agents_home())
    {
        tracing::debug!(%error, "team consensus copy refresh skipped");
    }
}

fn reminders(
    request: &ExecRequest,
    effective: Option<&LaunchAgents>,
    commands: &CommandsConfig,
) -> (LaunchReminders, Vec<LaunchPlanWarning>) {
    let Some(effective) = effective else {
        return (
            LaunchReminders::default(),
            vec![LaunchPlanWarning::DefaultReminders],
        );
    };
    let profiles = if request.subagent {
        &effective.subagent_profiles
    } else {
        &effective.profiles
    };
    let lsp_configured = !effective.lsp_servers.is_empty();
    let model = request
        .identity
        .params
        .profile
        .as_deref()
        .and_then(|name| profiles.0.get(name))
        .and_then(|profile| profile.model_reminder)
        .unwrap_or(true);
    if request.subagent {
        return (
            LaunchReminders {
                model,
                lsp_configured,
                ..LaunchReminders::default()
            },
            Vec::new(),
        );
    }
    let subagent_catalog = Some(super::subagent_policy::catalog(
        request.identity.params.profile.as_deref(),
        &effective.profiles,
        &effective.subagent_profiles,
        commands,
    ));
    let mut warnings = Vec::new();
    let team = request.identity.params.team.as_deref().and_then(|name| {
        let team = effective.teams.0.get(name).cloned();
        if team.is_none() {
            warnings.push(LaunchPlanWarning::MissingTeam(name.to_owned()));
        }
        team.map(TeamReminder::new)
    });
    (
        LaunchReminders {
            model,
            lsp_configured,
            subagent_catalog,
            team,
            ..LaunchReminders::default()
        },
        warnings,
    )
}

fn apply_materialized_system_prompt(
    request: &mut ExecRequest,
    materialized: &MaterializedSystemPrompt,
) {
    if !materialized.args.is_empty() {
        let matcher = crate::agents::find_definition(request.kind.as_str())
            .and_then(|adapter| {
                adapter
                    .spec()
                    .launch
                    .preset_arg_matcher(crate::agents::PresetField::SystemPromptFile)
            })
            .expect("materialized prompt args require a validated prompt matcher");
        matcher.remove_occurrences(request.action.extra_args_mut());
    }
    request
        .action
        .extra_args_mut()
        .extend(materialized.args.iter().cloned());
}

#[cfg(test)]
mod tests;

#[cfg(feature = "testkit")]
pub mod testkit {
    /// Assert the consumer's compiled home with a different room default.
    pub fn assert_claude_stamped_home(
        request: &super::ExecRequest,
        root: &std::path::Path,
        expected: Option<&str>,
    ) {
        use super::*;
        let mut machine = crate::config::MachineConfig::default();
        for name in ["work", "personal", "spare"] {
            machine
                .accounts
                .named_mut(&crate::ids::AgentKind::new_unchecked("claude"))
                .unwrap()
                .insert(
                    name.parse().unwrap(),
                    crate::config::NamedAccount {
                        home: Some(root.join(name)),
                        ..Default::default()
                    },
                );
        }
        let workspace = crate::WorkspaceResolver::resolve_under(root, None, root).unwrap();
        let state = StatePaths::under(workspace.workspace_id.clone(), root).unwrap();
        state.ensure_dirs().unwrap();
        let runtime = RuntimePaths::under(workspace.workspace_id.clone(), root).unwrap();
        let mut record = crate::workspace::record::WorkspaceRecord::from_resolved(&workspace);
        record.logins = Some(
            [(
                crate::ids::AgentKind::new_unchecked("claude"),
                "spare".parse().unwrap(),
            )]
            .into(),
        );
        crate::workspace::record::write(&state, &record).unwrap();
        let effective = crate::config::effective::load_with_roots(&machine, root, root).unwrap();
        let mut request = request.clone();
        request.identity.params.isolation = Some(crate::config::Isolation::Host);
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: root,
            project_root: root,
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
        })
        .unwrap();
        assert_eq!(plan.login.name().as_str(), expected.unwrap_or("default"));
        assert_eq!(
            plan.process()
                .env
                .get("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from),
            expected.map(|name| root.join(name))
        );
    }
}
