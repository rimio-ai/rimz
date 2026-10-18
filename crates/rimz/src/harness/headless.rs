//! Shared profile resolution, launch preparation, and execution for headless checks.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::agents::{AgentDefinition, HeadlessRequest, PermissionMode, ProviderLogin, ambient_env};
use crate::config::{
    MachineConfig,
    effective::{LaunchAgents, ProfileScope},
};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::launch::{ExecRequest, decode_exec_envelope};
use crate::harness::launch_plan::{self, LaunchPlan};
use crate::harness::plan::{self, LaunchAvailability, LaunchFinalizeOptions, PermissionModeChoice};
use crate::harness::spec::AgentCell;
use crate::store::writer::LaunchLogin;

#[derive(Debug, thiserror::Error)]
pub(crate) enum HeadlessErr {
    #[error(transparent)]
    Config(#[from] crate::config::effective::EffectiveConfigErr),
    #[error(transparent)]
    Tier(#[from] crate::config::tiers::TierError),
    #[error(transparent)]
    Resolve(#[from] plan::ResolveLaunchError),
    #[error(transparent)]
    Finalize(#[from] plan::LaunchFinalizeError),
    #[error(transparent)]
    Login(#[from] crate::agents::RoomLoginErr),
    #[error(transparent)]
    Plan(Box<launch_plan::LaunchPlanErr>),
    #[error(transparent)]
    Wire(#[from] crate::harness::launch::ExecWireErr),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Refused(String),
}

pub(crate) struct HeadlessProfile {
    pub cell: AgentCell,
    pub adapter: &'static AgentDefinition,
    pub login: ProviderLogin,
    effective: LaunchAgents,
    warnings: Vec<String>,
}

impl HeadlessProfile {
    pub(crate) fn resolve(
        config: &MachineConfig,
        profile: &str,
        root: &Path,
        state: &StatePaths,
        runtime: &RuntimePaths,
        now: jiff::Timestamp,
    ) -> Result<Self, HeadlessErr> {
        let mut effective = crate::config::effective::load(config, root)?;
        let availability = LaunchAvailability::read(runtime, state, config, now);
        effective.route(
            &config.tiers,
            ProfileScope::Agents,
            Some(profile),
            None,
            None,
            None,
            |kind, model| availability.unavailable(kind, model),
        )?;
        let mut resolved = plan::resolve_launch(
            &effective,
            ProfileScope::Agents,
            &config.agents.commands,
            Some(profile),
            None,
        )?;
        let cells: usize = resolved
            .layout
            .columns
            .iter()
            .map(|column| column.rows.len())
            .sum();
        if cells != 1 || resolved.layout.agent_cells().count() != 1 {
            return Err(HeadlessErr::Refused(format!(
                "judge profile `{profile}` must resolve to one agent"
            )));
        }
        let warnings = plan::finalize_launch_layout(
            &mut resolved.layout,
            LaunchFinalizeOptions {
                agent_base: Some(profile),
                permission_mode: Some(PermissionModeChoice::Default(PermissionMode::Auto)),
                isolation: None,
                preset: &crate::agents::LaunchPreset::default(),
                passthrough: &[],
                budget: None,
                max_turns: None,
            },
        )?
        .into_iter()
        .map(|warning| warning.to_string())
        .collect();
        // The single-agent layout was checked before finalization, which preserves its cells.
        let cell = resolved
            .layout
            .agent_cells()
            .next()
            .expect("single judge cell")
            .clone();
        let adapter = crate::agents::find_definition(&cell.kind)
            .ok_or_else(|| HeadlessErr::Refused(format!("unknown judge kind `{}`", cell.kind)))?;
        if adapter.spec().launch.headless.is_none() {
            return Err(HeadlessErr::Refused(format!(
                "judge profile `{profile}` resolves to `{}`; only claude and codex run headless judges",
                cell.kind
            )));
        }
        let accounts = crate::agents::room_accounts(&state.workspace_record, None, config)?;
        let login = LaunchLogin::RoomDefault.resolve(&cell.kind, &accounts, &config.accounts)?;
        Ok(Self {
            cell,
            adapter,
            login,
            effective,
            warnings,
        })
    }

    pub(crate) fn prepare(
        self,
        mut request: ExecRequest,
        config: &MachineConfig,
        dir: &Path,
        root: &Path,
    ) -> Result<(LaunchPlan, Vec<String>), HeadlessErr> {
        request.identity.params.login = Some(self.login.key().name);
        let envelope = decode_exec_envelope(
            self.cell.kind.as_str(),
            None,
            &serde_json::to_string(&request)?,
        )?;
        let prepared = launch_plan::prepare_exec(
            envelope,
            dir,
            root,
            &crate::proc::rimz_exe(),
            config,
            Some(&self.effective),
            &|| Err("no room".into()),
            |_, login| {
                let shared =
                    crate::agents::account_links::reconcile(login, &ambient_env(), &|| {
                        crate::room::other_live_agents_on(&login.key(), &[]).ok()
                    })?;
                Ok(shared.iter().flat_map(|shared| shared.warnings()).collect())
            },
        );
        let mut warnings = self.warnings;
        warnings.extend(prepared.link_warnings);
        warnings.extend(prepared.model_warnings);
        let (plan, _) = prepared
            .outcome
            .map_err(|error| HeadlessErr::Plan(Box::new(error)))?;
        warnings.extend(plan.warnings.iter().map(ToString::to_string));
        launch_plan::apply(&plan).map_err(|error| HeadlessErr::Plan(Box::new(error)))?;
        Ok((plan, warnings))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessErr {
    #[error(transparent)]
    Directory(#[from] crate::agents::LaunchDirRefusal),
    #[error(transparent)]
    Version(#[from] crate::agents::version::LaunchVersionErr),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct AdmittedLaunch<'a> {
    plan: &'a LaunchPlan,
}

pub(crate) fn execute(
    adapter: &AgentDefinition,
    plan: &LaunchPlan,
    _request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> Result<crate::proc::BoundedOutput, ProcessErr> {
    admit(adapter, plan)?.execute_hosted(timeout, interrupted, |_| Ok(()))
}

pub(crate) fn admit<'a>(
    adapter: &AgentDefinition,
    plan: &'a LaunchPlan,
) -> Result<AdmittedLaunch<'a>, ProcessErr> {
    let process = plan.process();
    crate::agents::preflight_launch_dir(
        adapter,
        &plan.cwd,
        process
            .env
            .get(crate::workspace::ENV_PROJECT_ROOT)
            .map(Path::new),
        &plan.login.env(&ambient_env()),
        crate::agents::LaunchDirMode::Headless,
    )?;
    if adapter.min_version().is_some()
        && let Ok(Some(path)) = process.resolve_program_after_shell_rc()
    {
        crate::agents::version::check_launch_version_floor(adapter, &path)?;
    }
    Ok(AdmittedLaunch { plan })
}

impl AdmittedLaunch<'_> {
    pub(crate) fn execute_hosted(
        self,
        timeout: Duration,
        interrupted: &dyn Fn() -> bool,
        spawned: impl FnOnce(u32) -> std::io::Result<()>,
    ) -> Result<crate::proc::BoundedOutput, ProcessErr> {
        // Admission may launch helpers; the decision must be fresh after they return.
        if interrupted() {
            return Err(ProcessErr::Io(std::io::ErrorKind::Interrupted.into()));
        }
        let plan = self.plan;
        let process = plan.process();
        let (program, args) = process
            .argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty headless command"))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&plan.cwd)
            .envs(&process.env)
            .stdin(Stdio::null());
        for key in &process.unset {
            command.env_remove(key);
        }
        Ok(crate::proc::run_bounded_output_hosted(
            &mut command,
            timeout,
            interrupted,
            spawned,
        )?)
    }
}
