//! A loop judge's complete launch path, bounded process edge, and verdict record.

use super::*;
use crate::agents::{CHECK_VERDICT_SCHEMA, HeadlessRequest, PermissionMode};
use crate::config::{Judge, effective::ProfileScope};
use crate::harness::launch::{ExecAction, ExecIdentity, ExecRequest, decode_exec_envelope};
use crate::harness::launch_plan::{self, LaunchPlan};
use crate::harness::plan::{LaunchAvailability, LaunchFinalizeOptions, PermissionModeChoice};
use crate::harness::schedule::run_log::JudgeRecord;

#[derive(Debug, thiserror::Error)]
pub(super) enum ProcessErr {
    #[error(transparent)]
    Directory(#[from] crate::agents::LaunchDirRefusal),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

struct CheckFiles(PathBuf);

impl Drop for CheckFiles {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            tracing::debug!(%error, "headless judge scratch cleanup failed");
        }
    }
}

pub(super) fn prompt_path(path: &Path, task: &LoadedTask) -> Result<PathBuf> {
    if !matches!(task.source(), catalog::TaskSource::Project { .. }) {
        return resolve_config_path(path);
    }
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let config = catalog::project_config_path(&task.entry().root);
    Ok(config
        .parent()
        .context("project config has no parent")?
        .join(expanded))
}

pub(super) fn run(
    fire: &TaskFire<'_>,
    judge: &Judge,
    dir: &Path,
    root: &Path,
) -> Result<ControlFlow<(LoopRunResult, String), (CheckOutcome, JudgeRecord)>> {
    if judge.recheck.is_some() && !fire.entry.stay {
        bail!("judge recheck requires a resident task");
    }
    if let Some(recheck) = &judge.recheck
        && recheck.trim() != "0"
    {
        parse_task_timeout(recheck).map_err(anyhow::Error::msg)?;
    }
    let timeout = judge
        .timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(anyhow::Error::msg)?
        .unwrap_or(CHECK_DEFAULT_TIMEOUT);
    if timeout.is_zero() {
        bail!("judge timeout must be greater than zero");
    }
    let prompt = match (&judge.prompt, &judge.prompt_file) {
        (Some(prompt), None) => prompt.clone(),
        (None, Some(path)) => {
            let path = prompt_path(path, &fire.task)?;
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading judge prompt-file `{}`", path.display()))?
        }
        _ => bail!("judge requires exactly one of prompt and prompt-file"),
    };
    if prompt.trim().is_empty() {
        bail!("judge prompt is empty");
    }
    let action_prompt = resolve_task_prompt(&fire.name, &fire.entry)?;
    let workspace = WorkspaceResolver::resolve(dir, Some(root.to_path_buf()))?;
    let state = StatePaths::for_project_root(&workspace.project_root)?;
    let runtime = RuntimePaths::for_state(&state)?;
    let mut effective = crate::config::effective::load(&fire.config, &workspace.project_root)?;
    let availability = LaunchAvailability::read(&runtime, &state, &fire.config, fire.now);
    effective.route(
        &fire.config.tiers,
        ProfileScope::Agents,
        Some(&judge.agent),
        None,
        None,
        None,
        |kind, model| availability.unavailable(kind, model),
    )?;
    let mut resolved = crate::harness::plan::resolve_launch(
        &effective,
        ProfileScope::Agents,
        &fire.config.agents.commands,
        Some(&judge.agent),
        None,
    )?;
    let cells: usize = resolved
        .layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum();
    if cells != 1 || resolved.layout.agent_cells().count() != 1 {
        bail!("judge profile `{}` must resolve to one agent", judge.agent);
    }
    let kind = resolved
        .layout
        .agent_cells()
        .next()
        .context("single judge cell missing")?
        .kind
        .clone();
    let adapter = find_definition(&kind).with_context(|| format!("unknown judge kind `{kind}`"))?;
    let form = adapter.spec().launch.headless.with_context(|| {
        format!(
            "judge profile `{}` resolves to `{kind}`; only claude and codex run headless judges",
            judge.agent
        )
    })?;
    let mut warnings: Vec<String> = crate::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            agent_base: Some(&judge.agent),
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
    let cell = resolved
        .layout
        .agent_cells()
        .next()
        .context("single judge cell missing")?;
    let accounts = crate::agents::room_accounts(&state.workspace_record, None, &fire.config)?;
    let login = LaunchLogin::RoomDefault.resolve(&kind, &accounts, &fire.config.accounts)?;
    let mut scope = FireScope::new(
        kind.clone(),
        runtime.clone(),
        state.clone(),
        None,
        Some(login.clone()),
    );
    scope.managed_launch = resolve_managed_spawn_state(
        &fire.entry,
        &workspace,
        &ResolvedSingleAgentLaunch {
            kind: kind.as_str().to_owned(),
            args: cell.args.clone(),
            model: cell.launch.model.clone(),
        },
    )?;
    if let Some(refusal) = fire.scope_refusal(&scope) {
        return Ok(ControlFlow::Break(refusal));
    }
    let mut identity = ExecIdentity {
        params: cell.launch.clone(),
        ..Default::default()
    };
    identity.params.login = Some(login.key().name);
    identity.params.loop_task = Some(fire.name.clone());
    let mut request = ExecRequest::fresh(cell, identity, None, false);
    request.action = ExecAction::Launch {
        prompt: Some(fire.augment_guard_prompt(prompt)),
        extra_args: cell.args.clone(),
    };
    request.loop_reminder = Some(reminder::compose_check(
        &fire.name,
        &fire.task,
        &action_prompt,
    ));
    let unit = state.ensure_temp_unit(None)?;
    let path = unit.join(format!("loop-judge-{}", RunId::new()));
    std::fs::create_dir(&path)?;
    let files = CheckFiles(path);
    let host_request = HeadlessRequest {
        schema: CHECK_VERDICT_SCHEMA.into(),
        schema_file: files.0.join("schema.json"),
        verdict_file: files.0.join("verdict.json"),
    };
    std::fs::write(&host_request.schema_file, CHECK_VERDICT_SCHEMA)?;
    request.headless = Some(host_request.clone());
    let envelope = decode_exec_envelope(kind.as_str(), None, &serde_json::to_string(&request)?)?;
    let prepared = launch_plan::prepare_exec(
        envelope,
        dir,
        &workspace.project_root,
        &crate::proc::rimz_exe(),
        &fire.config,
        Some(&effective),
        &|| Err("no room".into()),
        |_, login| {
            let shared = crate::agents::account_links::reconcile(login, &ambient_env(), &|| {
                crate::room::other_live_agents_on(&login.key(), &[]).ok()
            })?;
            Ok(shared.iter().flat_map(|shared| shared.warnings()).collect())
        },
    );
    warnings.extend(prepared.link_warnings);
    warnings.extend(prepared.model_warnings);
    let (plan, _) = prepared.outcome?;
    warnings.extend(plan.warnings.iter().map(ToString::to_string));
    launch_plan::apply(&plan)?;
    let output = (fire.check_process)(adapter, &plan, &host_request, timeout, &|| {
        fire.check_interrupts
            .as_ref()
            .is_some_and(throttle::Interrupts::raised)
    });
    let prices = crate::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    let (mut result, timed_out, interrupted, tail) = match output {
        Ok(output) => {
            let mut result = form.read_result(
                &output.stdout,
                &output.stderr,
                &host_request,
                plan.request.identity.params.model.as_deref(),
                plan.request.identity.params.fast,
                &prices,
            );
            if output.timed_out {
                result.verdict = None;
                result.error = Some(format!(
                    "judge timed out after {}",
                    super::super::arm::duration_label(timeout)
                ));
            }
            if result.verdict.is_none() && result.error.is_none() {
                result.error = Some(format!("judge returned no verdict ({})", output.status));
            }
            let tail = format!(
                "{}\n{}",
                crate::proc::tail_output(&output.stdout, CHECK_OUTPUT_CAP / 2),
                crate::proc::tail_output(&output.stderr, CHECK_OUTPUT_CAP / 2)
            );
            (result, output.timed_out, false, tail)
        }
        Err(ProcessErr::Directory(error @ crate::agents::LaunchDirRefusal::Home { .. })) => {
            anyhow::bail!("{error}; re-add the loop task from a project directory")
        }
        Err(ProcessErr::Directory(error)) => return Err(error.into()),
        Err(ProcessErr::Io(error)) => (
            crate::agents::HeadlessResult {
                error: Some(format!("running judge: {error}")),
                ..Default::default()
            },
            false,
            error.kind() == std::io::ErrorKind::Interrupted,
            String::new(),
        ),
    };
    let passed = result.verdict.as_ref().is_some_and(|verdict| verdict.pass);
    let output = result.verdict.as_ref().map_or_else(
        || {
            format!(
                "{}\n{tail}",
                result
                    .error
                    .as_deref()
                    .unwrap_or("judge returned no verdict")
            )
        },
        |verdict| verdict.reason.clone(),
    );
    let error = if result.verdict.is_none() {
        Some(output.clone())
    } else {
        result.error
    };
    let record = JudgeRecord {
        profile: judge.agent.clone(),
        kind,
        model: plan.request.identity.params.model.clone(),
        verdict: result.verdict.as_ref().map(|verdict| verdict.pass),
        reason: result.verdict.take().map(|verdict| verdict.reason),
        error,
        cost_usd: result.cost_usd,
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        duration_ms: None,
        reused: false,
        warnings,
    };
    Ok(ControlFlow::Continue((
        CheckOutcome {
            passed,
            code: None,
            timed_out,
            interrupted,
            output,
        },
        record,
    )))
}

pub(super) fn replay(
    fire: &TaskFire<'_>,
    decline: &super::super::launch_ledger::DeclineRecord,
    kind: &crate::ids::AgentKind,
) -> JudgeRecord {
    JudgeRecord {
        profile: decline.profile.clone(),
        kind: kind.clone(),
        model: decline.model.clone(),
        verdict: Some(fire.entry.on == Some(CheckOn::Fail)),
        reason: Some(decline.reason.clone()),
        error: None,
        cost_usd: Some(0.0),
        input_tokens: Some(0),
        output_tokens: Some(0),
        duration_ms: Some(0),
        reused: true,
        warnings: Vec::new(),
    }
}

pub(super) type ProcessRunner = fn(
    &crate::agents::AgentDefinition,
    &LaunchPlan,
    &HeadlessRequest,
    Duration,
    &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, ProcessErr>;

pub(super) fn execute(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    _request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> std::result::Result<crate::proc::BoundedOutput, ProcessErr> {
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
    Ok(crate::proc::run_bounded_output_interruptible(
        &mut command,
        timeout,
        interrupted,
    )?)
}

#[cfg(test)]
mod tests;
