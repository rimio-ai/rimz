use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{WorkflowSpec, spec, state::Transition};
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::store::workflow::{self, AttemptOutcome, RunRecord, RunState};

pub(super) struct Evaluation {
    pub object: Result<Value, String>,
    pub digests: BTreeMap<String, String>,
    pub stderr_tail: Option<String>,
    pub step_outcome: AttemptOutcome,
}

pub(super) fn step_stub() -> Option<String> {
    #[cfg(feature = "testkit")]
    {
        std::env::var("RIMZ_TEST_WORKFLOW_STEP").ok()
    }
    #[cfg(not(feature = "testkit"))]
    {
        None
    }
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn command_words(
    record: &RunRecord,
    spec: &WorkflowSpec,
    digests: &mut BTreeMap<String, String>,
) -> Result<Vec<std::ffi::OsString>, String> {
    if let Some(check) = spec.checks.get(&record.node) {
        let run = check
            .run
            .as_deref()
            .ok_or("agent checks are not supported yet")?;
        let argv = spec::resolve_argv(&record.spec_dir, run).map_err(|e| e.to_string())?;
        // resolve_argv has parsed this exact command successfully.
        let words = shlex::split(run).expect("resolved workflow argv");
        for (word, path) in words.iter().zip(&argv) {
            if word.contains('/') {
                let bytes =
                    std::fs::read(path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
                digests.insert(word.clone(), digest(&bytes));
            }
        }
        return Ok(argv);
    }
    let stub = step_stub().ok_or("this build does not run steps yet")?;
    let words = shlex::split(&stub)
        .filter(|w| !w.is_empty())
        .ok_or("invalid test step argv")?;
    let step = &spec.steps[&record.node];
    let prompt =
        spec::confined_file(&record.spec_dir, &step.prompt_file).map_err(|e| e.to_string())?;
    digests.insert(
        step.prompt_file.clone(),
        digest(&std::fs::read(prompt).map_err(|e| e.to_string())?),
    );
    Ok(words.into_iter().map(Into::into).collect())
}

pub(super) fn evaluate(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Evaluation {
    let mut result = Evaluation {
        object: Err("evaluation interrupted".into()),
        digests: BTreeMap::new(),
        stderr_tail: None,
        step_outcome: AttemptOutcome::Died,
    };
    let argv = match command_words(record, spec, &mut result.digests) {
        Ok(argv) => argv,
        Err(err) => {
            result.object = Err(err);
            return result;
        }
    };
    // The engine calls evaluate only after prepare has published this attempt.
    let attempt = record
        .attempt
        .as_ref()
        .expect("evaluation follows a recorded live attempt");
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&record.scope.path)
        .stdin(Stdio::null())
        .env("RIMZ_WORKFLOW_DIR", &paths.work)
        .envs(crate::workspace::pin_env(
            &state.workspace_id,
            &record.project_root,
        ));
    let interrupted = || {
        let Ok(current) = workflow::load(paths) else {
            return true;
        };
        if current.state != RunState::Active
            || !current
                .attempt
                .as_ref()
                .is_some_and(|a| a.seq == attempt.seq)
        {
            return true;
        }
        let t = Transition {
            record: current,
            spec,
            paths,
            now: Timestamp::now(),
            entries: Vec::new(),
        };
        t.timeout_at()
            .map_or(true, |at| at.is_some_and(|at| at <= t.now))
            || (!t.record.deadline_fired && t.record.deadline.is_some_and(|at| at <= t.now))
    };
    // Re-read after path resolution and context publication, immediately before spawn.
    if interrupted() {
        return result;
    }
    match crate::proc::run_bounded_output_interruptible(
        &mut command,
        Duration::from_secs(u32::MAX.into()),
        interrupted,
    ) {
        Ok(output) => {
            result.stderr_tail = Some(
                String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4096)..])
                    .into_owned(),
            );
            result.step_outcome = if output.timed_out {
                AttemptOutcome::Timeout
            } else if output.status.success() {
                AttemptOutcome::Ended
            } else {
                AttemptOutcome::Died
            };
            result.object = if output.timed_out {
                Err("evaluation timed out".into())
            } else if !output.status.success() {
                Err(format!("script exited {}", output.status))
            } else {
                serde_json::from_slice(&output.stdout)
                    .map_err(|e| format!("invalid check stdout: {e}"))
            };
        }
        Err(err) => {
            result.object = Err(err.to_string());
        }
    }
    result
}
