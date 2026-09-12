//! Read-only launch compilation and explicit runtime artifact materialization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ProviderLogin;
use crate::agents::capabilities::SystemTextChannel;
use crate::config::effective::LaunchAgents;
use crate::config::{AccountsConfig, CommandsConfig};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::sandbox::{self, SandboxPlan};

use super::launch::{self, AgentProcessStage, CompiledAgentProcess, ExecRequest};
use super::launch_reminders::LaunchReminders;
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
    pub warnings: Vec<LaunchPlanWarning>,
    runtime: RuntimePaths,
    state: StatePaths,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchPlanWarning {
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
    Paths(#[from] crate::disk::paths::PathErr),
    #[error("unknown agent kind `{0}`")]
    UnknownAgent(crate::ids::AgentKind),
    #[error(transparent)]
    Login(#[from] crate::agents::RoomLoginErr),
}

impl LaunchPlan {
    pub fn process(&self) -> &CompiledAgentProcess {
        match &self.stage {
            AgentProcessStage::Ready(process)
            | AgentProcessStage::LoginShellReentry { process, .. } => process,
        }
    }

    pub fn argv(&self) -> &[String] {
        match &self.stage {
            AgentProcessStage::Ready(process) => &process.argv,
            AgentProcessStage::LoginShellReentry { argv, .. } => argv,
        }
    }
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
        },
        inputs.runtime,
    )?;
    apply_materialized_system_prompt(&mut request, &prompt.materialized);
    let (mut reminders, warnings) = reminders(&request, inputs.effective, inputs.commands);
    reminders.sandbox = inputs.bwrap.is_some();
    let login = crate::agents::room_login(
        &inputs.state.workspace_record,
        inputs.accounts,
        &request.kind,
    )?;
    let mut extra_env = prompt.materialized.env.clone();
    extra_env.extend(login.env(&BTreeMap::new()));
    let mut stage = launch::compile_agent_process_stage_with_extra_env(
        inputs.project_root,
        &request,
        inputs.cwd,
        inputs.rimz_bin,
        inputs.runtime,
        &extra_env,
        &reminders,
    )?;
    let sandbox =
        if let (Some(bwrap), AgentProcessStage::Ready(process)) = (inputs.bwrap, &mut stage) {
            let mut env = inputs.ambient_env.clone();
            env.extend(process.env.clone());
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
                skills_dir: &inputs.state.skills_dir,
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
            process.argv = sandbox::bwrap_argv(bwrap, &plan.plan, inputs.cwd, &process.argv);
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
        warnings,
        runtime: inputs.runtime.clone(),
        state: inputs.state.clone(),
    })
}

pub fn apply(plan: &LaunchPlan) -> Result<(), LaunchPlanErr> {
    plan.runtime.ensure_dirs()?;
    prompt_compose::apply_system_prompt(&plan.prompt)?;
    if let AgentProcessStage::LoginShellReentry {
        prompt_artifact: Some(artifact),
        ..
    } = &plan.stage
    {
        launch::write_prompt_artifact(artifact)?;
    }
    if let Some(sandbox) = &plan.sandbox {
        plan.state.ensure_tmp_dir()?;
        sandbox::apply(sandbox)?;
    }
    Ok(())
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
        team
    });
    (
        LaunchReminders {
            model,
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
