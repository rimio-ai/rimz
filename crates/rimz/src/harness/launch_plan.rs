//! Read-only launch compilation and explicit runtime artifact materialization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ProviderLogin;
use crate::agents::capabilities::SystemTextChannel;
use crate::agents::skill_links::{self, Desired, SkillLinkErr, SkillLinkOutcome, SkillLinkPlan};
use crate::config::effective::LaunchAgents;
use crate::config::{AccountsConfig, CommandsConfig, Isolation};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::sandbox::{self, ENV_SCRATCH, ENV_SHARED, SandboxPlan};

use super::launch::{self, AgentProcessStage, CompiledAgentProcess, ExecRequest};
use super::launch_reminders::{LaunchReminders, TeamReminder};
use super::prompt_compose::{
    self, MaterializedSystemPrompt, SystemPromptPlan, SystemPromptSources,
};

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
    pub ambient_env: &'a BTreeMap<String, String>,
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
    recorded_model: Option<&str>,
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
    let replay = if matches!(request.action, launch::ExecAction::Launch { .. }) {
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
    if inputs
        .effective
        .is_none_or(|effective| effective.env_reminder)
    {
        reminders.env = Some(super::launch_env::read());
    }
    let login = crate::agents::room_login(
        &inputs.state.workspace_record,
        inputs.accounts,
        &request.kind,
    )?;
    let mut extra_env = prompt.materialized.env.clone();
    extra_env.extend(login.env(&BTreeMap::new()));
    let isolation = if inputs.bwrap.is_some() {
        crate::config::Isolation::Sandbox
    } else {
        crate::config::Isolation::Host
    };
    extra_env.insert(Isolation::ENV.to_owned(), isolation.to_string());
    let scratch_dir = inputs.state.scratch_dir(request.identity.name.as_deref());
    if inputs
        .effective
        .is_none_or(|effective| effective.allow_routine_rimz)
    {
        let view = sandbox::TmpView::current(
            Some(isolation),
            request.identity.name.as_deref(),
            inputs.state,
        );
        reminders.routine_rimz = Some((
            inputs.runtime.prompt_dir(),
            [
                view.agent_path(&scratch_dir),
                view.agent_path(&inputs.state.shared_dir),
            ],
        ));
    }
    extra_env.insert(ENV_SCRATCH.to_owned(), scratch_dir.display().to_string());
    extra_env.insert(
        ENV_SHARED.to_owned(),
        inputs.state.shared_dir.display().to_string(),
    );
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
                tmp_dir: &inputs.state.tmp_dir,
                scratch_dir: &scratch_dir,
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
            let cwd = sandbox::TmpView::current(
                Some(Isolation::Sandbox),
                request.identity.name.as_deref(),
                inputs.state,
            )
            .agent_path(inputs.cwd);
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
    plan.state
        .ensure_scratch_dir(plan.request.identity.name.as_deref())?;
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
