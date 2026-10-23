//! A loop judge's complete launch path, bounded process edge, and verdict record.

use std::cell::{Cell, RefCell};

use super::*;
use crate::agents::{CHECK_VERDICT_SCHEMA, HeadlessRequest};
use crate::config::Judge;
use crate::harness::headless::HeadlessProfile;
pub(super) use crate::harness::headless::ProcessErr;
use crate::harness::launch::{ExecAction, ExecIdentity, ExecRequest};
use crate::harness::launch_plan::LaunchPlan;
use crate::harness::schedule::run_log::JudgeRecord;

pub(super) fn execute(
    adapter: &crate::agents::AgentDefinition,
    plan: &LaunchPlan,
    _request: &HeadlessRequest,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
    record_hold: &mut Option<CheckSignalGuard>,
) -> Result<crate::proc::BoundedOutput, ProcessErr> {
    let signals = RefCell::new(CheckSignals::new(
        &[signal_hook::consts::SIGTERM],
        record_hold,
    )?);
    let group = Cell::new(None);
    let terminated = Cell::new(false);
    let poll = || {
        // The host owns reaping; a non-reaping liveness check lets an exited
        // judge keep its verdict without releasing its PID before pipe drain.
        if !terminated.get()
            && group
                .get()
                .is_some_and(|pid| !crate::proc::process_is_live(pid, None))
        {
            return false;
        }
        for _ in signals.borrow_mut().pending() {
            terminated.set(true);
            if let Some(pid) = group.get() {
                let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
        }
        // The bounded driver kills on interruption. After SIGTERM, let the
        // check exit itself, bounded only by its original timeout.
        interrupted() || (terminated.get() && group.get().is_none())
    };
    let output =
        crate::harness::headless::admit(adapter, plan)?.execute_hosted(timeout, &poll, |pid| {
            group.set(Some(pid));
            Ok(())
        })?;
    if terminated.get() {
        return Err(ProcessErr::Io(std::io::ErrorKind::Interrupted.into()));
    }
    Ok(output)
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
    fire: &mut TaskFire<'_>,
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
    let profile = HeadlessProfile::resolve(
        &fire.config,
        &judge.agent,
        &workspace.project_root,
        &state,
        &runtime,
        fire.now,
    )?;
    let cell = &profile.cell;
    let kind = cell.kind.clone();
    let adapter = profile.adapter;
    // HeadlessProfile admitted this adapter's headless form.
    let form = adapter
        .spec()
        .launch
        .headless
        .expect("resolved headless profile");
    let login = &profile.login;
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
        schema_open: false,
        schema: CHECK_VERDICT_SCHEMA.into(),
        schema_file: files.0.join("schema.json"),
        verdict_file: files.0.join("verdict.json"),
    };
    std::fs::write(&host_request.schema_file, CHECK_VERDICT_SCHEMA)?;
    request.headless = Some(host_request.clone());
    let (plan, warnings) = profile.prepare(request, &fire.config, dir, &workspace.project_root)?;
    let interrupts = &fire.check_interrupts;
    let output = (fire.check_process)(
        adapter,
        &plan,
        &host_request,
        timeout,
        &|| {
            interrupts
                .as_ref()
                .is_some_and(throttle::Interrupts::raised)
        },
        &mut fire.record_hold,
    );
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
                result.error = Some(
                    format!(
                        "judge timed out after {}",
                        super::super::arm::duration_label(timeout)
                    )
                    .into(),
                );
            }
            if result.verdict.is_none() && result.error.is_none() {
                result.error =
                    Some(format!("judge returned no verdict ({})", output.status).into());
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
        Err(ProcessErr::Version(error)) => return Err(error.into()),
        Err(ProcessErr::Io(error)) => (
            crate::agents::HeadlessResult {
                error: Some(format!("running judge: {error}").into()),
                ..Default::default()
            },
            false,
            error.kind() == std::io::ErrorKind::Interrupted,
            String::new(),
        ),
    };
    let verdict = result.verdict.take().and_then(|value| {
        match serde_json::from_value::<crate::agents::HeadlessVerdict>(value) {
            Ok(verdict) => Some(verdict),
            Err(error) => {
                result.error = Some(format!("invalid judge verdict: {error}").into());
                None
            }
        }
    });
    let passed = verdict.as_ref().is_some_and(|verdict| verdict.pass);
    let output = verdict.as_ref().map_or_else(
        || {
            format!(
                "{}\n{tail}",
                result
                    .error
                    .as_ref()
                    .map(|error| error.text.as_str())
                    .unwrap_or("judge returned no verdict")
            )
        },
        |verdict| verdict.reason.clone(),
    );
    let error = if verdict.is_none() {
        Some(output.clone())
    } else {
        result.error.map(|error| error.text)
    };
    let record = JudgeRecord {
        profile: judge.agent.clone(),
        kind,
        model: plan.request.identity.params.model.clone(),
        verdict: verdict.as_ref().map(|verdict| verdict.pass),
        reason: verdict.map(|verdict| verdict.reason),
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
            terminated: false,
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
    &mut Option<CheckSignalGuard>,
) -> std::result::Result<crate::proc::BoundedOutput, ProcessErr>;

#[cfg(test)]
mod tests;
