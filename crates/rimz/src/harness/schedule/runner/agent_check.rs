//! A loop judge's complete launch path, bounded process edge, and verdict record.

use std::cell::{Cell, RefCell};

use super::*;
use crate::agents::HeadlessRequest;
use crate::config::Judge;
use crate::harness::headless::HeadlessProfile;
pub(super) use crate::harness::headless::ProcessErr;
use crate::harness::headless::judge::{self as core, Contract, FailureKind, LoopOutcome};
use crate::harness::launch::ExecIdentity;
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
    let stop_after_exit = Cell::new(false);
    let poll = || {
        // Drain stops before reading liveness, so a judge that exited before
        // the stop read keeps its verdict. The host reaps after pipe drain,
        // so this exit read must not reap.
        let pending: Vec<_> = signals.borrow_mut().pending().collect();
        let judge_exited = !terminated.get()
            && group
                .get()
                .is_some_and(|pid| !crate::proc::process_is_live(pid, None));
        for _ in pending {
            // A judge that exited keeps its verdict, but a child of it
            // still holding the output pipes gets the stop, so the drain ends.
            if judge_exited {
                stop_after_exit.set(true);
            } else {
                terminated.set(true);
            }
            if let Some(pid) = group.get() {
                let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
        }
        if judge_exited {
            return false;
        }
        // The bounded driver kills on interruption. After SIGTERM, let the
        // check exit itself, bounded only by its original timeout.
        interrupted() || (terminated.get() && group.get().is_none())
    };
    let started = Instant::now();
    let admitted = crate::harness::headless::admit(adapter, plan)?;
    // Admission counts against the deadline: with nothing left, no judge starts.
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Ok(crate::proc::BoundedOutput {
            status: std::os::unix::process::ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdin: None,
            timed_out: true,
        });
    }
    let output = admitted.execute_hosted(remaining, &poll, |pid| {
        group.set(Some(pid));
        Ok(())
    })?;
    if stop_after_exit.get()
        && let Some(hold) = record_hold.as_mut()
    {
        hold.stop_read = true;
    }
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

pub(in crate::harness::schedule) fn prompt_path(path: &Path, task: &LoadedTask) -> Result<PathBuf> {
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
    tier: usize,
    judge: &Judge,
    dir: &Path,
    root: &Path,
) -> Result<ControlFlow<(LoopRunResult, String), (CheckOutcome, JudgeRecord)>> {
    let started = Instant::now();
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
    identity.params.automation = Some(crate::agents::Automation::Loop);
    let unit = state.ensure_temp_unit(None)?;
    let path = unit.join(format!("loop-judge-{}", RunId::new()));
    std::fs::create_dir(&path)?;
    let files = CheckFiles(path);
    let (prepared, warnings) = core::prepare(
        profile,
        core::Request {
            prompt: fire.augment_guard_prompt(prompt),
            reminder: crate::harness::launch::ReminderSection::Judge(reminder::compose_check(
                &fire.name,
                &fire.task,
                &action_prompt,
            )),
            identity,
            dir,
            root: &workspace.project_root,
            contract: Contract::Loop,
            schema_file: files.0.join("schema.json"),
            verdict_file: files.0.join("verdict.json"),
            timeout,
        },
        &fire.config,
        &runtime,
    )?;
    // One deadline bounds both attempts: the retry runs on what is left of it.
    let deadline = Instant::now() + timeout;
    let record = |judged: JudgeTry, retry: Option<String>| JudgeStage {
        judged,
        retry,
        profile: judge.agent.clone(),
        kind: kind.clone(),
        model: prepared.model().map(str::to_owned),
        warnings: warnings.clone(),
        duration_ms: Some(elapsed_millis(started)),
    };
    let mut judged = attempt(fire, &prepared, timeout)?;
    if let Err(failure) = &mut judged.answer
        && fire.record_hold.as_ref().is_some_and(|hold| hold.stop_read)
    {
        // A stop read as the judge exited ends the tier like one read while it ran.
        failure.kind = FailureKind::Canceled;
    }
    let mut retry = None;
    if let Err(failure) = &judged.answer
        && failure.kind.retry_owed(false)
        && !fire.check_interrupted()
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let timed_out = core::Failure::timed_out(timeout);
            judged.answer = Err(core::Failure {
                text: format!("{}\n{}", timed_out.text, failure.text),
                ..timed_out
            });
        } else {
            // The retry must read only its own answer.
            let _ = std::fs::remove_file(prepared.verdict_file());
            let first_error = failure.text.clone();
            let second = match attempt(fire, &prepared, remaining) {
                Ok(second) => second,
                Err(refusal) => {
                    // The refusal stops the fire; the first attempt's spend stays in history.
                    fire.fired_guard.push(super::FiredCheck::Judge {
                        tier,
                        record: record(judged, None).into_parts().1,
                    });
                    return Err(refusal);
                }
            };
            retry = Some(first_error);
            judged.usage.add(&second.usage);
            judged = JudgeTry {
                usage: judged.usage,
                ..second
            };
        }
    }
    Ok(ControlFlow::Continue(record(judged, retry).into_parts()))
}

/// A finished judge tier, before it becomes the chain's outcome and history record.
struct JudgeStage {
    judged: JudgeTry,
    retry: Option<String>,
    profile: String,
    kind: crate::ids::AgentKind,
    model: Option<String>,
    warnings: Vec<String>,
    duration_ms: Option<u64>,
}

impl JudgeStage {
    fn into_parts(self) -> (CheckOutcome, JudgeRecord) {
        let JudgeTry {
            answer,
            usage,
            tail,
        } = self.judged;
        // An interrupt that skipped the retry is read again by the caller's chain.
        let interrupted = matches!(&answer, Err(failure) if failure.kind == FailureKind::Canceled);
        let timed_out = matches!(&answer, Err(failure) if failure.kind == FailureKind::Timeout);
        let (passed, output, verdict, reason, error) = match answer {
            Ok(answer) => {
                let passed = answer.outcome == LoopOutcome::Pass;
                (
                    passed,
                    answer.reason.clone(),
                    Some(passed),
                    Some(answer.reason),
                    None,
                )
            }
            Err(failure) => {
                let output = format!("{}\n{tail}", failure.text);
                (false, output.clone(), None, None, Some(output))
            }
        };
        let record = JudgeRecord {
            profile: self.profile,
            kind: self.kind,
            model: self.model,
            verdict,
            reason,
            error,
            cost_usd: usage.cost_usd,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            duration_ms: self.duration_ms,
            reused: false,
            warnings: self.warnings,
            retry: self.retry,
        };
        let outcome = CheckOutcome {
            passed,
            code: None,
            timed_out,
            interrupted,
            terminated: false,
            output,
            stderr_line: String::new(),
        };
        (outcome, record)
    }
}

#[derive(Default)]
struct Usage {
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl Usage {
    fn add(&mut self, other: &Self) {
        fn sum<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            }
        }
        self.cost_usd = sum(self.cost_usd, other.cost_usd);
        self.input_tokens = sum(self.input_tokens, other.input_tokens);
        self.output_tokens = sum(self.output_tokens, other.output_tokens);
    }
}

/// One loop judge attempt, its answer decoded under the loop contract.
struct JudgeTry {
    answer: std::result::Result<core::LoopAnswer, core::Failure>,
    usage: Usage,
    tail: String,
}

fn attempt(
    fire: &mut TaskFire<'_>,
    prepared: &core::Prepared,
    timeout: Duration,
) -> Result<JudgeTry> {
    let interrupts = &fire.check_interrupts;
    let check_process = fire.check_process;
    let record_hold = &mut fire.record_hold;
    let attempt = prepared.attempt(|adapter, plan, request| {
        check_process(
            adapter,
            plan,
            request,
            timeout,
            &|| {
                interrupts
                    .as_ref()
                    .is_some_and(throttle::Interrupts::raised)
            },
            record_hold,
        )
    });
    let attempt = match attempt {
        Ok(attempt) => attempt,
        Err(ProcessErr::Directory(error @ crate::agents::LaunchDirRefusal::Home { .. })) => {
            anyhow::bail!("{error}; re-add the loop task from a project directory")
        }
        Err(ProcessErr::Directory(error)) => return Err(error.into()),
        Err(ProcessErr::Version(error)) => return Err(error.into()),
        Err(ProcessErr::BindGuard(error)) => return Err(error.into()),
        // The core turns every I/O end into a failed attempt; only refusals reach here.
        Err(ProcessErr::Io(error)) => return Err(error.into()),
    };
    let tail = attempt.output.as_ref().map_or_else(String::new, |output| {
        format!(
            "{}\n{}",
            crate::proc::tail_output(&output.stdout, CHECK_OUTPUT_CAP / 2),
            crate::proc::tail_output(&output.stderr, CHECK_OUTPUT_CAP / 2)
        )
    });
    Ok(JudgeTry {
        answer: attempt.answer.and_then(core::LoopAnswer::from_value),
        usage: Usage {
            cost_usd: attempt.cost_usd,
            input_tokens: attempt.input_tokens,
            output_tokens: attempt.output_tokens,
        },
        tail,
    })
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
        retry: None,
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
