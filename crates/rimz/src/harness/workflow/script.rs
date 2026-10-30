use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{WorkflowSpec, spec, state::Transition};
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::harness::schedule::runner::{self, CheckEcho, WatchDeadline};
use crate::store::workflow::{self, RunRecord, RunState};

mod condition;
mod judge;

#[derive(Debug, thiserror::Error)]
pub(super) enum ChildError {
    #[error("evaluation interrupted")]
    Interrupted,
    #[error("{0}")]
    Failed(String),
}

pub(super) struct Evaluation {
    pub object: Result<Value, String>,
    pub stderr_tail: Option<String>,
    pub failure: &'static str,
    pub limited: Option<workflow::Limited>,
    pub cost_usd: Option<f64>,
}

impl Evaluation {
    fn failed(failure: &'static str, reason: &str) -> Self {
        Self {
            object: Err(reason.into()),
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

fn exit_failure(label: &str, status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        return format!("{label} exited {code}");
    }
    if let Some(raw) = status.signal() {
        let signal = nix::sys::signal::Signal::try_from(raw)
            .map_or_else(|_| format!("signal {raw}"), |signal| signal.to_string());
        return format!("{label} killed by {signal}");
    }
    format!("{label} {status}")
}

fn command_words(
    record: &RunRecord,
    run: &str,
    digests: &mut BTreeMap<String, String>,
) -> Result<Vec<std::ffi::OsString>, String> {
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
    let run = match spec.checks[&record.node].kind() {
        Some(spec::CheckKind::Agent(_)) => return judge::evaluate(state, paths, spec, record),
        Some(spec::CheckKind::Shell(shell)) => {
            return evaluate_shell(state, paths, spec, record, shell);
        }
        Some(spec::CheckKind::Condition) => {
            let mut result = Evaluation::failed("invalid", "condition evaluation interrupted");
            result.object = condition::evaluate(state, &spec.checks[&record.node], record);
            return result;
        }
        Some(spec::CheckKind::Run(run)) => run,
        None => return Evaluation::failed("invalid", "check needs exactly one runner"),
    };
    let mut result = Evaluation::failed("invalid", "evaluation interrupted");
    let mut digests = BTreeMap::new();
    let argv = match command_words(record, run, &mut digests) {
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
    let output = with_child(
        state,
        paths,
        spec,
        record,
        digests,
        |interrupted, spawned| {
            crate::proc::run_bounded_output_hosted(
                &mut command,
                Duration::from_secs(u32::MAX.into()),
                interrupted,
                spawned,
            )
            .map_err(|error| error.to_string())
        },
    );
    match output {
        Ok(output) => {
            result.stderr_tail = Some(
                String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4096)..])
                    .into_owned(),
            );
            result.object = if output.timed_out {
                Err("evaluation timed out".into())
            } else if !output.status.success() {
                // command_words resolved this exact, nonempty argv successfully.
                let words = shlex::split(run).expect("resolved workflow argv");
                let label = words
                    .iter()
                    .find(|word| word.contains('/'))
                    .unwrap_or(&words[0]);
                Err(exit_failure(label, output.status))
            } else {
                serde_json::from_slice(&output.stdout)
                    .map_err(|e| format!("invalid check stdout: {e}"))
            };
        }
        Err(error) => {
            result.object = Err(error.to_string());
        }
    }
    result
}

fn evaluate_shell(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    shell: &str,
) -> Evaluation {
    let mut result = Evaluation::failed("invalid", "shell evaluation interrupted");
    result.object = (|| {
        // Evaluation follows a recorded live attempt.
        let attempt = record
            .attempt
            .as_ref()
            .expect("prepared hosted shell check");
        let log = paths.check_log(attempt.seq);
        let file = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        let mut env = crate::workspace::pin_env(&state.workspace_id, &record.project_root);
        env.insert("RIMZ_WORKFLOW_DIR".into(), paths.work.display().to_string());
        env.insert("RIMZ_WORKFLOW_RUN".into(), record.id.to_string());
        let output = with_child(
            state,
            paths,
            spec,
            record,
            BTreeMap::new(),
            |interrupted, spawned| {
                runner::run_command(
                    &record.scope.path,
                    shell,
                    WatchDeadline::Hosted {
                        argv: None,
                        interrupted,
                        spawned,
                    },
                    CheckEcho::Tee { file },
                    &env,
                    |_, _| {},
                    &mut None,
                )
                .map_err(|error| format!("{error:#}"))
            },
        )
        .map_err(|error| error.to_string())?;
        if output.interrupted() {
            return Err("shell evaluation interrupted".into());
        }
        let mut object = json!({
            "outcome": if output.passed() { "pass" } else { "fail" },
            "code": output.code(),
            "log": log,
        });
        if !output.passed() {
            object["reason"] = json!(output.code().map_or_else(
                || "killed by signal".into(),
                |code| format!("exited {code}"),
            ));
        }
        Ok(object)
    })();
    result
}

pub(super) fn execute_step(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    on_spawn: impl Fn(),
) -> Result<runner::CheckOutcome, ChildError> {
    let mut digests = BTreeMap::new();
    // Only validated command steps enter this hosting path.
    let (command, argv) = match spec.steps[&record.node]
        .kind()
        .expect("command step runner")
    {
        spec::StepKind::Run(run) => (
            run,
            Some(command_words(record, run, &mut digests).map_err(ChildError::Failed)?),
        ),
        spec::StepKind::Shell(shell) => (shell, None),
        spec::StepKind::Agent(_) => unreachable!("agent steps use the attempt host"),
    };
    // prepare publishes the live attempt before dispatching the command.
    let seq = record.attempt.as_ref().expect("prepared command step").seq;
    let log = paths.step_log(seq);
    let file = std::fs::File::create(&log)
        .map_err(|error| ChildError::Failed(format!("{}: {error}", log.display())))?;
    let mut env = crate::workspace::pin_env(&state.workspace_id, &record.project_root);
    env.insert("RIMZ_WORKFLOW_DIR".into(), paths.work.display().to_string());
    env.insert("RIMZ_WORKFLOW_RUN".into(), record.id.to_string());
    with_child(
        state,
        paths,
        spec,
        record,
        digests,
        |interrupted, spawned| {
            let mut publish = |pid| {
                spawned(pid)?;
                on_spawn();
                Ok(())
            };
            runner::run_command(
                &record.scope.path,
                command,
                WatchDeadline::Hosted {
                    argv: argv.as_deref(),
                    interrupted,
                    spawned: &mut publish,
                },
                CheckEcho::Tee { file },
                &env,
                |_, _| {},
                &mut None,
            )
            .map_err(|error| format!("{error:#}"))
        },
    )
}

fn with_child<T>(
    state: &StatePaths,
    paths: &WorkflowPaths,
    spec: &WorkflowSpec,
    record: &RunRecord,
    digests: BTreeMap<String, String>,
    execute: impl FnOnce(
        &dyn Fn() -> bool,
        &mut dyn FnMut(u32) -> std::io::Result<()>,
    ) -> Result<T, String>,
) -> Result<T, ChildError> {
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
        return Err(ChildError::Interrupted);
    }
    let mut guard = match crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock) {
        Ok(guard) => Some(guard),
        Err(error) => {
            return Err(ChildError::Failed(error.to_string()));
        }
    };
    // Admission, spawn, and child publication share the lock with cancel and bounds.
    if interrupted() {
        return Err(ChildError::Interrupted);
    }
    execute(&interrupted, &mut |pid| {
        let mut current = workflow::load(paths).map_err(std::io::Error::other)?;
        // The lock prevents another transition between admission and publication.
        let attempt = current.attempt.as_mut().expect("admitted live attempt");
        attempt.child = Some(workflow::HostChild {
            pid,
            start: crate::proc::process_start_token(pid),
        });
        attempt.digests = digests.clone();
        workflow::write(paths, &current, guard.as_ref().expect("spawn lock held"))
            .map_err(std::io::Error::other)?;
        guard.take();
        Ok(())
    })
    .map_err(ChildError::Failed)
}
