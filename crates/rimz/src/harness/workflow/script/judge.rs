use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::harness::headless::judge::{self as core, FailureKind};
use crate::harness::headless::{HeadlessProfile, ProcessErr};
use crate::harness::launch::ExecIdentity;

pub(super) fn evaluate(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Evaluation {
    let mut result = Evaluation::failed("died", "judge interrupted");
    if let Err(error) = run(state, paths, spec, record, &mut result) {
        result.object = Err(error);
    }
    result
}

fn run(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    result: &mut Evaluation,
) -> Result<(), String> {
    let check = &spec.checks[&record.node];
    // Agent checks were validated with a profile and a confined prompt file.
    let file = check
        .prompt_file
        .as_ref()
        .ok_or("judge needs a prompt-file")?;
    let prompt_file = spec::confined_file(&record.spec_dir, file).map_err(|e| e.to_string())?;
    let prompt = std::fs::read_to_string(prompt_file).map_err(|e| e.to_string())?;
    let digests = BTreeMap::from([(file.clone(), digest(prompt.as_bytes()))]);
    let attempt = record.attempt.as_ref().expect("prepared judge attempt");
    let output_path = paths.check_output(attempt.seq);
    let prompt = format!(
        concat!(
            "{}\n\nRIMZ_WORKFLOW_DIR is {}. output_path is {}. ",
            "Your final message must be exactly one JSON object: required string outcome; ",
            "optional string reason, RFC 3339 string recheck_at, and string-or-integer key. ",
            "Any other fields are carried to the next node as input. ",
            "Use no prose and no Markdown fence. The scope checkout is read-only; ",
            "write signs only under signs/<step>.md in RIMZ_WORKFLOW_DIR."
        ),
        prompt,
        paths.work.display(),
        output_path.display()
    );
    let config = crate::config::MachineConfig::load_lenient();
    let runtime = crate::disk::paths::RuntimePaths::for_state(state).map_err(|e| e.to_string())?;
    let profile = HeadlessProfile::resolve(
        &config,
        check.agent.as_deref().ok_or("judge needs an agent")?,
        &record.project_root,
        state,
        &runtime,
        Timestamp::now(),
    )
    .map_err(|e| e.to_string())?;
    let managed = managed_launch(&profile, record)?;
    let catalog = record.task.as_ref().map(|_| {
        crate::harness::schedule::catalog::TaskCatalog::load_lenient(Some(&record.project_root))
    });
    let task = record
        .task
        .as_deref()
        .zip(catalog.as_ref())
        .and_then(|(name, catalog)| catalog.for_run(name))
        .map(|task| task.entry());
    if let Some(reason) = budget_refusal(
        &config,
        &profile,
        &managed,
        task,
        state,
        &runtime,
        Timestamp::now(),
    ) {
        result.failure = "budget";
        return Err(reason);
    }
    let kind = profile.cell.kind.clone();
    let mut identity = ExecIdentity {
        params: profile.cell.launch.clone(),
        ..Default::default()
    };
    identity.params.workflow = Some(WorkflowAttemptRef {
        run: record.id.clone(),
        seq: attempt.seq,
        role: WorkflowAttemptRole::Judge,
    });
    identity.params.automation = Some(crate::agents::Automation::Workflow {
        workflow: record.workflow.clone(),
        step: record.node.clone(),
    });
    let schema_file = state
        .ensure_temp_unit(None)
        .map_err(|e| e.to_string())?
        .join(format!(
            "workflow-judge-{}.schema.json",
            crate::ids::RunId::new()
        ));
    // The engine bounds a check; the judge process has no deadline of its own.
    let timeout = Duration::from_secs(u32::MAX.into());
    let (prepared, warnings) = core::prepare(
        profile,
        core::Request {
            prompt,
            reminder: crate::harness::launch::ReminderSection::Workflow(
                super::super::brief::judge(spec, record),
            ),
            identity,
            dir: &record.scope.path,
            root: &record.project_root,
            schema_file,
            verdict_file: output_path,
            timeout,
        },
        &config,
        &runtime,
    )
    .map_err(|e| e.to_string())?;
    for warning in warnings {
        tracing::warn!(%warning, "workflow judge launch warning");
    }
    let attempt = prepared
        .attempt(|adapter, plan, _| {
            let admitted = crate::harness::headless::admit(adapter, plan)?;
            with_child(
                state,
                paths,
                spec,
                record,
                digests,
                |interrupted, spawned| {
                    admitted
                        .execute_hosted(timeout, interrupted, spawned)
                        .map_err(|e| e.to_string())
                },
            )
            .map_err(|error| ProcessErr::Io(std::io::Error::other(error.to_string())))
        })
        .map_err(|e| e.to_string())?;
    if let Some(output) = &attempt.output {
        result.cost_usd = attempt.cost_usd;
        result.stderr_tail = Some(crate::proc::tail_output(&output.stderr, 4096));
    }
    let failure = match attempt.answer {
        Ok(object) => {
            record_decline(spec, record, check, kind, &object, result.cost_usd);
            result.object = Ok(object);
            return Ok(());
        }
        Err(failure) => failure,
    };
    // Every failure but a limit is a dead judge here: the engine's bounds settle
    // its own interruptions, and it validates the object itself.
    if failure.kind == FailureKind::Limited {
        result.failure = "limited";
        result.limited = Some(workflow::Limited {
            reset_at: failure.reset_at,
        });
    }
    Err(failure.text)
}

/// The judge's account binding, resolved for its checkout as a loop fire resolves the agent
/// it launches.
fn managed_launch(
    profile: &HeadlessProfile,
    record: &RunRecord,
) -> Result<crate::agents::ManagedLaunchState, String> {
    let mut invocation = crate::harness::launch::ExecRequest::bare_launch(
        profile.cell.kind.clone(),
        profile.cell.args.clone(),
    );
    invocation.identity.params.model = profile.cell.launch.model.clone();
    crate::harness::launch::compile_managed_agent_process(
        &record.project_root,
        &invocation,
        &record.scope.path,
        &crate::agents::ManagedLaunchState::PendingResolution,
        None,
    )
    .map(|(_, managed)| managed)
    .map_err(|e| e.to_string())
}

/// Why the judge's account may not spend now: the fleet's or the account's daily cap is spent,
/// the account is parked, its bound provider quota is exhausted until a reset, or the window
/// lacks the surplus the run's loop `task` asks for. A refused judge never spawns, as a refused
/// agent launch never does.
pub(in crate::harness::workflow) fn budget_refusal(
    config: &crate::config::MachineConfig,
    profile: &HeadlessProfile,
    managed: &crate::agents::ManagedLaunchState,
    task: Option<&crate::config::TaskEntry>,
    state: &StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    now: Timestamp,
) -> Option<String> {
    let key = profile.login.key();
    if let Some(reason) =
        crate::harness::budget::scope_gate(runtime, state, Some(&key), config, now)
    {
        return Some(reason);
    }
    if let Some(binding) = managed.binding()
        && let Some(reason) = crate::agents::provider_budget_gate(runtime, &key, binding, now)
    {
        return Some(reason);
    }
    crate::harness::schedule::runner::surplus_refusal(task?, profile.cell.kind.as_str(), || {
        managed
            .capacity(runtime, &key)
            .and_then(|capacity| capacity.longest_window_surplus(now))
    })
}

/// A fresh answer from a loop task's judge that holds the task's act back is one assist,
/// as a loop guard's decline was; a replayed answer never reaches here.
fn record_decline(
    spec: &WorkflowSpec,
    record: &RunRecord,
    check: &spec::Check,
    kind: crate::ids::AgentKind,
    object: &serde_json::Value,
    cost_usd: Option<f64>,
) {
    let (Some(task), Some(outcome), Some(profile)) = (
        record.task.as_ref(),
        object["outcome"].as_str(),
        check.agent.as_ref(),
    ) else {
        return;
    };
    if !declines(spec, &record.node, outcome) {
        return;
    }
    crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
        at: Timestamp::now(),
        assist: crate::harness::assist_log::Assist::JudgeDecline {
            task: task.clone(),
            checkout: record.scope.path.clone(),
            profile: profile.clone(),
            kind,
            reason: object["reason"].as_str().unwrap_or_default().to_owned(),
            cost_usd,
        },
    });
}

/// Whether a judge's verdict routes away from every step and agent check while its opposite
/// verdict leads into one: the compiled decline of a loop task's guard.
pub(in crate::harness::workflow) fn declines(
    spec: &WorkflowSpec,
    node: &str,
    outcome: &str,
) -> bool {
    let opposite = match outcome {
        "pass" => "fail",
        "fail" => "pass",
        _ => return false,
    };
    let edges = spec.edges(node);
    let acts = |outcome: &str| {
        edges
            .iter()
            .find(|edge| edge.outcome == outcome)
            .map(|edge| {
                spec.steps.contains_key(&edge.target)
                    || spec.checks.get(&edge.target).is_some_and(|check| {
                        matches!(check.kind(), Some(spec::CheckKind::Agent(_)))
                    })
            })
    };
    acts(outcome) == Some(false) && acts(opposite) == Some(true)
}
