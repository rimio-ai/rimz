use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Definition, Invocation, Kind, Node, SpecErr, spec};
use crate::disk::paths::{StatePaths, WorkflowPaths};
use crate::harness::check_run::{self, CheckEcho, WatchDeadline};
use crate::store::workflow::{self, RunRecord, RunState};

mod condition;

pub use condition::{CaseMark, CaseMarks, case_marks, unread_reasons};

#[derive(Debug, thiserror::Error)]
pub(super) enum ChildError {
    #[error("evaluation interrupted")]
    Interrupted,
    #[error("{0}")]
    Failed(String),
}

/// Why a hosted child's poll interrupted it, decided once where the child is killed, so that
/// settlement classifies the interruption by the reading that caused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Interrupt {
    /// The run's deadline passed.
    Deadline,
    /// The node's timeout, as the definition read then had it, passed.
    Timeout,
    /// The run or its attempt moved on, or a read failed.
    Ended,
}

/// How often a hosted child's poll reads its run's definition again for its timeout; the
/// polls between reuse the last reading, since a read costs a parse and an admission.
const LIVE_READ_EVERY: Duration = Duration::from_secs(1);

/// The definition a hosted child's poll reads its timeout from: the one the attempt started
/// under, read again every [`LIVE_READ_EVERY`] and before the timeout is applied, and the
/// started one again when a read fails.
pub(super) struct Live<'a> {
    started: &'a Definition,
    read: &'a dyn Fn() -> Option<Definition>,
    last: RefCell<(Instant, Option<Definition>)>,
}

impl<'a> Live<'a> {
    pub(super) fn new(started: &'a Definition, read: &'a dyn Fn() -> Option<Definition>) -> Self {
        Self {
            started,
            read,
            last: RefCell::new((Instant::now(), None)),
        }
    }

    /// Whether `record`'s timeout has passed at `now`. A reading that finds it passed is
    /// taken again first, so a timeout is never applied from a stale reading.
    fn timed_out(
        &self,
        record: &RunRecord,
        now: Timestamp,
    ) -> Result<bool, crate::utils::time::TimeInputErr> {
        let passed = |reading: &Option<Definition>| {
            super::timeout_at(reading.as_ref().unwrap_or(self.started), record)
                .map(|at| at.is_some_and(|at| at <= now))
        };
        let mut last = self.last.borrow_mut();
        if last.0.elapsed() < LIVE_READ_EVERY && !passed(&last.1)? {
            return Ok(false);
        }
        *last = (Instant::now(), (self.read)());
        passed(&last.1)
    }
}

pub(super) struct Evaluation {
    pub object: Result<Value, String>,
    pub stderr_tail: Option<String>,
    pub failure: &'static str,
    /// The failure's detail: a dead script's exit code, when it had one, and its log.
    pub detail: Value,
    /// Why the poll interrupted the child, when it did.
    pub interrupt: Option<Interrupt>,
}

impl Evaluation {
    fn failed(failure: &'static str, reason: &str) -> Self {
        Self {
            object: Err(reason.into()),
            stderr_tail: None,
            failure,
            detail: json!({}),
            interrupt: None,
        }
    }
}

/// A check's nonzero exit in words: its exit code, else its signal.
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

pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn command_words(
    dir: &Path,
    run: &super::definition::Command,
    digests: &mut BTreeMap<String, String>,
) -> Result<Vec<std::ffi::OsString>, String> {
    let words = run
        .words()
        .ok_or(SpecErr::Command)
        .map_err(|e| e.to_string())?;
    let argv = spec::resolve_words(dir, words).map_err(|e| e.to_string())?;
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
    live: &Live<'_>,
    record: &RunRecord,
) -> Evaluation {
    let spec = live.started;
    let Some(check) = spec.check(&record.node) else {
        return Evaluation::failed("invalid", "check needs exactly one runner");
    };
    let run = match check.kind() {
        Some(Kind::Test(invocation)) => {
            return evaluate_test(state, paths, live, record, invocation);
        }
        Some(Kind::Condition(_)) => {
            let mut result = Evaluation::failed("invalid", "condition evaluation interrupted");
            result.object = condition::evaluate(state, check, record);
            return result;
        }
        Some(Kind::Sleep(_)) => {
            let mut result = Evaluation::failed("invalid", "sleep evaluation interrupted");
            result.object =
                condition_now(state, check, record, record.entered_at).map(|(object, _)| object);
            return result;
        }
        Some(Kind::Script(invocation)) => invocation,
        // A judge is an agent the engine launches, never a hosted evaluation.
        Some(Kind::Judge(_) | Kind::Command(_) | Kind::Agent(_) | Kind::Message(_)) | None => {
            return Evaluation::failed("invalid", "check needs exactly one runner");
        }
    };
    let mut result = Evaluation::failed("died", "evaluation interrupted");
    // Evaluation follows a recorded live attempt.
    let attempt = record.attempt.as_ref().expect("prepared hosted evaluation");
    let log = paths.check_log(attempt.seq);
    let mut stderr = match std::fs::File::create(&log) {
        Ok(file) => file,
        Err(error) => {
            return Evaluation::failed("died", &format!("{}: {error}", log.display()));
        }
    };
    result.detail = json!({"log": log});
    let mut digests = BTreeMap::new();
    let (argv, label) = match run {
        Invocation::Run(run) => {
            match command_words(&record.definition_dir(paths), run, &mut digests) {
                // command_words resolved this exact, nonempty argv successfully.
                Ok(argv) => (
                    argv,
                    script_label(run.words().expect("resolved workflow argv")),
                ),
                Err(err) => {
                    result.object = Err(err);
                    return result;
                }
            }
        }
        Invocation::Shell(shell) => (vec!["sh".into(), "-c".into(), shell.into()], "sh"),
    };
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&record.scope.path)
        .stdin(Stdio::null())
        .env("RIMZ_WORKFLOW_DIR", &paths.work)
        .env("RIMZ_WORKFLOW_DEFINITION", record.definition_dir(paths))
        .env("RIMZ_WORKFLOW_RUN", record.id.as_str())
        .envs(crate::workspace::pin_env(
            &state.workspace_id,
            &record.project_root,
        ))
        .envs(loop_task_env(record));
    let (output, interrupt) = with_child(
        state,
        paths,
        live,
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
    result.interrupt = interrupt;
    match output {
        Ok(output) => {
            result.stderr_tail = Some(
                String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4096)..])
                    .into_owned(),
            );
            if let Some(code) = output.status.code() {
                result.detail["code"] = json!(code);
            }
            result.object = if let Err(error) = stderr.write_all(&output.stderr) {
                Err(format!("{}: {error}", log.display()))
            } else if output.timed_out {
                Err("evaluation timed out".into())
            } else if !output.status.success() {
                Err(exit_failure(label, output.status))
            } else {
                result.failure = "invalid";
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

/// Read a condition check or sleep without spawning a check, writing, or refreshing probes.
/// A sleep runs from `slept_from`. Returns the answer and the forge probe time, omitted when no
/// forge key is read.
pub fn condition_now(
    state: &StatePaths,
    check: &Node,
    record: &RunRecord,
    slept_from: Timestamp,
) -> Result<(Value, Option<Timestamp>), String> {
    if let Some(Kind::Sleep(sleep)) = check.kind() {
        let duration = sleep.duration();
        let end = slept_from
            .checked_add(*duration.value().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        if Timestamp::now() >= end {
            return Ok((
                json!({"outcome":"then", "reason":format!("slept {}", duration.text())}),
                None,
            ));
        }
        if check.cases().is_empty() {
            return Ok((json!({"outcome":"sleeping", "recheck_at":end}), None));
        }
    }
    condition::reading(state, check, record)
}

/// The word a failed script names: its first path word, else its program.
fn script_label(words: &[String]) -> &str {
    words
        .iter()
        .find(|word| word.contains('/'))
        .unwrap_or(&words[0])
}

/// The command text and, for `run`, the argv resolved against the definition directory `dir`,
/// recording a digest for each path word.
fn invoked<'a>(
    dir: &Path,
    invocation: &'a Invocation,
    digests: &mut BTreeMap<String, String>,
) -> Result<(&'a str, Option<Vec<std::ffi::OsString>>), String> {
    Ok(match invocation {
        Invocation::Run(run) => (run.text(), Some(command_words(dir, run, digests)?)),
        Invocation::Shell(shell) => (shell.as_str(), None),
    })
}

fn evaluate_test(
    state: &StatePaths,
    paths: &WorkflowPaths,
    live: &Live<'_>,
    record: &RunRecord,
    invocation: &Invocation,
) -> Evaluation {
    let mut result = Evaluation::failed("invalid", "test evaluation interrupted");
    let mut logged = None;
    result.object = (|| {
        let mut digests = BTreeMap::new();
        let (command, argv) = invoked(&record.definition_dir(paths), invocation, &mut digests)?;
        // Evaluation follows a recorded live attempt.
        let attempt = record
            .attempt
            .as_ref()
            .expect("prepared hosted shell check");
        let log = paths.check_log(attempt.seq);
        let file = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        logged = Some(log.clone());
        let env = child_env(state, paths, record);
        let (output, interrupt) = with_child(
            state,
            paths,
            live,
            record,
            digests,
            |interrupted, spawned| {
                check_run::run_command(
                    &record.scope.path,
                    command,
                    WatchDeadline::Hosted {
                        argv: argv.as_deref(),
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
        );
        result.interrupt = interrupt;
        let output = output.map_err(|error| error.to_string())?;
        if output.interrupted() {
            return Err("test evaluation interrupted".into());
        }
        // A test that never exited (a signal killed it) answered nothing: it died.
        let code = output.code().ok_or("killed by signal")?;
        let mut object = json!({
            "outcome": if output.passed() { "pass" } else { "fail" },
            "code": code,
            "log": log,
        });
        if !output.passed() {
            object["reason"] = json!(format!("exited {code}"));
        }
        Ok(object)
    })();
    if let Some(log) = logged {
        result.detail = json!({"log": log});
        if let Some(code) = result
            .object
            .as_ref()
            .ok()
            .and_then(|object| object.get("code"))
        {
            result.detail["code"] = code.clone();
        }
    }
    result
}

/// Run a command step's child: its outcome, and why the poll interrupted it, when it did.
pub(super) fn execute_step(
    state: &StatePaths,
    paths: &WorkflowPaths,
    live: &Live<'_>,
    record: &RunRecord,
    on_spawn: impl Fn(),
) -> (
    Result<check_run::CheckOutcome, ChildError>,
    Option<Interrupt>,
) {
    let spec = live.started;
    let mut digests = BTreeMap::new();
    // Only validated command steps enter this hosting path.
    let Kind::Command(invocation) = spec.step_kind(&record.node).expect("command step runner")
    else {
        unreachable!("advance sends agent and message steps to the attempt host")
    };
    let (command, argv) = match invoked(&record.definition_dir(paths), invocation, &mut digests) {
        Ok(invoked) => invoked,
        Err(error) => return (Err(ChildError::Failed(error)), None),
    };
    // prepare publishes the live attempt before dispatching the command.
    let seq = record.attempt.as_ref().expect("prepared command step").seq;
    let log = paths.step_log(seq);
    let file = match std::fs::File::create(&log) {
        Ok(file) => file,
        Err(error) => {
            return (
                Err(ChildError::Failed(format!("{}: {error}", log.display()))),
                None,
            );
        }
    };
    let env = child_env(state, paths, record);
    with_child(
        state,
        paths,
        live,
        record,
        digests,
        |interrupted, spawned| {
            let mut publish = |pid| {
                spawned(pid)?;
                on_spawn();
                Ok(())
            };
            check_run::run_command(
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

/// Run a hosted child under the run's bounds, polled against the record and `live`: its result,
/// and why the poll interrupted it, when it did.
fn with_child<T>(
    state: &StatePaths,
    paths: &WorkflowPaths,
    live: &Live<'_>,
    record: &RunRecord,
    digests: BTreeMap<String, String>,
    execute: impl FnOnce(
        &dyn Fn() -> bool,
        &mut dyn FnMut(u32) -> std::io::Result<()>,
    ) -> Result<T, String>,
) -> (Result<T, ChildError>, Option<Interrupt>) {
    // Evaluation follows a recorded live attempt.
    let attempt = record.attempt.as_ref().expect("prepared hosted evaluation");
    let decided = Cell::new(None);
    let reading = || {
        let Ok(current) = workflow::load(paths) else {
            return Some(Interrupt::Ended);
        };
        if current.state != RunState::Active
            || !current
                .attempt
                .as_ref()
                .is_some_and(|a| a.seq == attempt.seq)
        {
            return Some(Interrupt::Ended);
        }
        let now = Timestamp::now();
        if !current.deadline_fired && current.deadline.is_some_and(|at| at <= now) {
            return Some(Interrupt::Deadline);
        }
        match live.timed_out(&current, now) {
            Ok(true) => Some(Interrupt::Timeout),
            Ok(false) => None,
            Err(_) => Some(Interrupt::Ended),
        }
    };
    let interrupted = || {
        if decided.get().is_none() {
            decided.set(reading());
        }
        decided.get().is_some()
    };
    // Re-read after path resolution and context publication, immediately before spawn.
    if interrupted() {
        return (Err(ChildError::Interrupted), decided.get());
    }
    let mut guard = match crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock) {
        Ok(guard) => Some(guard),
        Err(error) => {
            return (Err(ChildError::Failed(error.to_string())), None);
        }
    };
    // Admission, spawn, and child publication share the lock with cancel and bounds.
    if interrupted() {
        return (Err(ChildError::Interrupted), decided.get());
    }
    let result = execute(&interrupted, &mut |pid| {
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
    .map_err(ChildError::Failed);
    (result, decided.get())
}

/// The environment of a shell check or command step: the room pin, the run's directories and
/// id, and the loop task that started the run, as a loop guard's shell had it.
fn child_env(
    state: &StatePaths,
    paths: &WorkflowPaths,
    record: &RunRecord,
) -> BTreeMap<String, String> {
    let mut env = crate::workspace::pin_env(&state.workspace_id, &record.project_root);
    env.insert("RIMZ_WORKFLOW_DIR".into(), paths.work.display().to_string());
    env.insert(
        "RIMZ_WORKFLOW_DEFINITION".into(),
        record.definition_dir(paths).display().to_string(),
    );
    env.insert("RIMZ_WORKFLOW_RUN".into(), record.id.to_string());
    env.extend(loop_task_env(record));
    env
}

/// A run carrying a loop fire hands its scripts the fire id, so a launch a script makes is the
/// fire's. A task-started run's checks also name their task and, as a loop guard's did, their
/// directory, and carry the booking chain its fire froze, for an agent a check launches.
fn loop_task_env(record: &RunRecord) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(fire) = &record.loop_fire {
        env.push((
            crate::harness::loop_task::LOOP_FIRE_ENV.to_owned(),
            fire.to_string(),
        ));
    }
    let Some(task) = record.task.clone() else {
        return env;
    };
    env.push((crate::harness::loop_task::LOOP_TASK_ENV.to_owned(), task));
    env.push((
        crate::workspace::ENV_WORKTREE_PATH.to_owned(),
        record.scope.path.display().to_string(),
    ));
    if let Some(trigger) = record.prompt.clone().filter(|text| !text.is_empty()) {
        env.push((
            crate::harness::loop_task::LOOP_TRIGGER_ENV.to_owned(),
            trigger,
        ));
    }
    if let Some(fire) = record.fire.as_ref()
        && !fire.booked_by().is_empty()
        && let Ok(chain) = serde_json::to_string(fire.booked_by())
    {
        env.push((
            crate::harness::loop_task::LOOP_BOOKED_BY_ENV.to_owned(),
            chain,
        ));
    }
    env
}
