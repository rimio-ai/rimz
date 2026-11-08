use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::harness::headless::judge::{self as core, Contract, FailureKind};
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
            contract: Contract::Workflow,
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
