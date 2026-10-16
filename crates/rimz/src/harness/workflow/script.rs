use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{WorkflowSpec, spec, state::Transition};
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::store::workflow::{self, RunRecord, RunState};

mod judge;

pub(super) struct Evaluation {
    pub object: Result<Value, String>,
    pub digests: BTreeMap<String, String>,
    pub stderr_tail: Option<String>,
    pub failure: &'static str,
    pub limited: Option<workflow::Limited>,
    pub cost_usd: Option<f64>,
}

impl Evaluation {
    fn failed(failure: &'static str, reason: &str) -> Self {
        Self {
            object: Err(reason.into()),
            digests: BTreeMap::new(),
            stderr_tail: None,
            failure,
            limited: None,
            cost_usd: None,
        }
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
    let check = &spec.checks[&record.node];
    let run = check.run.as_deref().ok_or("script check needs run")?;
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
    Ok(argv)
}

pub(super) fn evaluate(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
) -> Evaluation {
    if spec.checks[&record.node].agent.is_some() {
        return judge::evaluate(state, paths, spec, record);
    }
    let mut result = Evaluation::failed("invalid", "evaluation interrupted");
    let argv = match command_words(record, spec, &mut result.digests) {
        Ok(argv) => argv,
        Err(err) => {
            result.object = Err(err);
            return result;
        }
    };
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&record.scope.path)
        .stdin(Stdio::null())
        .env("RIMZ_WORKFLOW_DIR", &paths.work)
        .env("RIMZ_WORKFLOW_RUN", record.id.as_str())
        .envs(crate::workspace::pin_env(
            &state.workspace_id,
            &record.project_root,
        ));
    let output = with_child(state, paths, spec, record, |interrupted, spawned| {
        crate::proc::run_bounded_output_hosted(
            &mut command,
            Duration::from_secs(u32::MAX.into()),
            interrupted,
            spawned,
        )
        .map_err(|error| error.to_string())
    });
    match output {
        Ok(output) => {
            result.stderr_tail = Some(
                String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4096)..])
                    .into_owned(),
            );
            result.object = if output.timed_out {
                Err("evaluation timed out".into())
            } else if !output.status.success() {
                Err(format!("script exited {}", output.status))
            } else {
                serde_json::from_slice(&output.stdout)
                    .map_err(|e| format!("invalid check stdout: {e}"))
            };
        }
        Err(error) => {
            result.object = Err(error);
        }
    }
    result
}

fn with_child(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    execute: impl FnOnce(
        &dyn Fn() -> bool,
        &mut dyn FnMut(u32) -> std::io::Result<()>,
    ) -> Result<crate::proc::BoundedOutput, String>,
) -> Result<crate::proc::BoundedOutput, String> {
    // Evaluation follows a recorded live attempt.
    let attempt = record.attempt.as_ref().expect("prepared hosted evaluation");
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
        return Err("evaluation interrupted".into());
    }
    let mut guard = match crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock) {
        Ok(guard) => Some(guard),
        Err(error) => {
            return Err(error.to_string());
        }
    };
    // Admission, spawn, and child publication share the lock with cancel and bounds.
    if interrupted() {
        return Err("evaluation interrupted".into());
    }
    execute(&interrupted, &mut |pid| {
        let mut current = workflow::load(paths).map_err(std::io::Error::other)?;
        // The lock prevents another transition between admission and publication.
        current
            .attempt
            .as_mut()
            .expect("admitted live attempt")
            .child = Some(workflow::HostChild {
            pid,
            start: crate::proc::process_start_token(pid),
        });
        workflow::write(paths, &current, guard.as_ref().expect("spawn lock held"))
            .map_err(std::io::Error::other)?;
        guard.take();
        Ok(())
    })
}
