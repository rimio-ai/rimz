//! Signal emission, loop watchers, and in-process firing; the vocabulary they carry lives in
//! `harness::signal`.

mod team;
pub mod workflow;
pub(crate) use team::team_lifecycle_signals;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek};

use anyhow::Context;
use serde_json::{Map, Value};

use super::arming;
use super::catalog::TaskCatalog;
use super::run_log::check_record;
use crate::RuntimePaths;
use crate::config::{TaskEntry, WatchSpec};
use crate::disk::lease;
use crate::disk::paths::StatePaths;
use crate::harness::check_run::{CheckEcho, WAIT_TAIL_CAP, WatchDeadline, run_command};
use crate::harness::schedule::run_control::RunLockInfo;
use crate::harness::signal::{Signal, SignalResolution, WatchOutcome, WatchVerdict};
use crate::store::Store;
use crate::store::event::SignalSource;
use crate::workspace::ResolvedWorkspace;
use std::path::{Path, PathBuf};

/// Host path of wait `name`'s output, in its arming agent's `out/` directory.
pub(super) fn wait_output_path(paths: &StatePaths, name: &str, entry: &TaskEntry) -> PathBuf {
    let reader = entry
        .wait_meta
        .as_ref()
        .and_then(|meta| meta.reader.as_deref());
    paths.out_reader_dir(reader).join(format!("{name}.output"))
}

pub(super) fn read_wait_tail(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(WAIT_TAIL_CAP as u64);
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(WAIT_TAIL_CAP);
    file.take(WAIT_TAIL_CAP as u64).read_to_end(&mut bytes)?;
    let output = String::from_utf8_lossy(&bytes);
    let mut start = output.len().saturating_sub(WAIT_TAIL_CAP);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    Ok(output[start..].to_owned())
}

pub(crate) fn lifecycle_signal(event: &crate::agents::LifecycleEvent) -> Option<Signal> {
    if !matches!(
        event.signal,
        crate::agents::LifecycleSignal::Ended | crate::agents::LifecycleSignal::Lost
    ) && matches!(
        event.transition,
        crate::agents::LifecycleTransition::Ignored { .. }
    ) {
        return None;
    }
    let (name, errored) = match &event.signal {
        crate::agents::LifecycleSignal::Registered
        | crate::agents::LifecycleSignal::SubagentStarted => ("agent.started", false),
        crate::agents::LifecycleSignal::TurnEnded { errored, .. } if *errored => {
            ("agent.failed", true)
        }
        crate::agents::LifecycleSignal::TurnEnded { .. } => ("agent.idle", false),
        crate::agents::LifecycleSignal::AwaitingInput { .. } => ("agent.waiting", false),
        crate::agents::LifecycleSignal::Ended
        | crate::agents::LifecycleSignal::Lost
        | crate::agents::LifecycleSignal::SubagentStopped { .. } => ("agent.ended", false),
        _ => return None,
    };
    let mut payload = Map::from_iter([
        (
            "kind".to_owned(),
            Value::String(event.kind.as_str().to_owned()),
        ),
        (
            "session".to_owned(),
            Value::String(event.agent_id.as_str().to_owned()),
        ),
        (
            "status".to_owned(),
            Value::String(event.status.as_str().to_owned()),
        ),
        ("errored".to_owned(), Value::Bool(errored)),
    ]);
    if let Some(agent_name) = &event.agent_name {
        payload.insert("handle".to_owned(), Value::String(format!("@{agent_name}")));
    }
    if let Some(parent) = &event.parent_agent_id {
        payload.insert(
            "parent".to_owned(),
            Value::String(parent.as_str().to_owned()),
        );
    }
    // These literals are covered by the signal-name grammar test.
    let name = name.parse().expect("static lifecycle signal name is valid");
    Some(Signal {
        name,
        payload,
        source: SignalSource::Lifecycle,
        watch: None,
    })
}

pub fn run_watcher(store: &Store, workspace: &ResolvedWorkspace, name: &str) -> anyhow::Result<()> {
    let Some(_guard) =
        acquire_watch_lock(store.runtime_paths(), name).context("locking wait watcher")?
    else {
        return Ok(());
    };
    let catalog = TaskCatalog::load(Some(&workspace.project_root))?;
    let Some(task) = catalog.for_run(name) else {
        anyhow::bail!("no wait named {name} in the catalog");
    };
    if task.entry().resolved_root() != workspace.project_root {
        anyhow::bail!(
            "wait {name} belongs to {}, watcher started for {}",
            task.entry().resolved_root().display(),
            workspace.project_root.display()
        );
    }
    let Some(WatchSpec::Command(command)) = &task.entry().watch else {
        anyhow::bail!("wait {name} has no watch");
    };
    let timeout = task_timeout(task.entry())?;
    let output_path = wait_output_path(store.paths(), name, task.entry());
    let file = OpenOptions::new()
        .append(true)
        .open(&output_path)
        .with_context(|| format!("opening wait output {}", output_path.display()))?;
    let started = std::time::Instant::now();
    let emit = |verdict, output| {
        let signal = Signal {
            name: format!("shell.{name}")
                .parse()
                .expect("generated shell signal name is valid"),
            payload: Map::new(),
            source: SignalSource::Watch,
            watch: Some(WatchOutcome::measured(verdict, output, &output_path)),
        };
        if let Err(err) = store.append_signal(&workspace.session_name, (&signal).into()) {
            tracing::warn!(task = name, error = %err, "appending wait signal");
        }
        if let Err(err) = fire_signal_with_wait(
            store.runtime_paths(),
            &workspace.project_root,
            &signal,
            true,
        ) {
            tracing::warn!(task = name, error = %err, "firing watched wait");
        }
    };
    let outcome = run_command(
        &task.entry().run_dir(),
        command,
        WatchDeadline::Watch(timeout),
        CheckEcho::Tee { file },
        &std::collections::BTreeMap::new(),
        |elapsed_ms, output| emit(WatchVerdict::Running { elapsed_ms }, output),
        &mut None,
    )?;
    let check = check_record(&outcome);
    emit(
        WatchVerdict::Exited {
            code: check.code,
            elapsed_ms: elapsed_millis(started),
        },
        check.output,
    );
    Ok(())
}

fn task_timeout(entry: &TaskEntry) -> anyhow::Result<Option<std::time::Duration>> {
    entry
        .timeout
        .as_deref()
        .map(crate::utils::time::parse_compact_duration)
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn elapsed_millis(started: std::time::Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// The durable agent rows of `project_root`, or none when they cannot be read:
/// retirement acts on positive evidence of an end, never on a failed read.
///
/// The audit scope is load-bearing. A runtime-scoped snapshot expels every row
/// carrying `ended_at` (`store::runtime`'s visibility rule), which is precisely
/// the evidence the retirement predicate needs, so it would see an ended session
/// as merely absent and leave its rows standing.
fn audit_agents(project_root: &Path) -> crate::store::runtime::AuditRows {
    let read = || -> anyhow::Result<crate::store::runtime::AuditRows> {
        let paths = crate::disk::paths::StatePaths::for_project_root(project_root)?;
        Ok(crate::store::runtime::audit_rows(&paths)?)
    };
    read().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "loop: failed to read agent state before firing");
        crate::store::runtime::AuditRows::default()
    })
}

/// Record and fire a best-effort signal after its originating operation succeeded.
pub fn emit_in_process(store: &Store, session_name: &str, project_root: &Path, signal: &Signal) {
    if let Err(error) = store.append_signal(session_name, signal.into()) {
        tracing::warn!(%error, name = %signal.name, "could not append signal");
        return;
    }
    if let Err(error) = fire_signal(store.runtime_paths(), project_root, signal) {
        tracing::warn!(%error, name = %signal.name, "could not fire signal tasks");
    }
}

/// Fire matching tasks in the emitter process. Signal events are never replayed.
pub fn fire_signal(
    runtime: &RuntimePaths,
    project_root: &Path,
    signal: &Signal,
) -> Result<SignalFire, serde_json::Error> {
    fire_signal_with_wait(runtime, project_root, signal, false)
}

#[derive(Debug, Default)]
pub struct SignalFire {
    pub started: Vec<String>,
    pub not_started: Vec<String>,
    /// Tasks subscribed to the signal's name or family whose match filter excluded its payload;
    /// a task on a sibling name is not listed.
    pub excluded: Vec<String>,
}

fn fire_signal_with_wait(
    runtime: &RuntimePaths,
    project_root: &Path,
    signal: &Signal,
    wait: bool,
) -> Result<SignalFire, serde_json::Error> {
    let fired_at = jiff::Timestamp::now();
    wake_workflows(project_root, signal);
    // A row pinned to a session that is durably ended must never be selected,
    // whoever ended it: the sibling-skip branch below returns before the fire
    // path's only liveness gate, so this is the decision point.
    if let Err(err) = super::arm::retire_ended_sessions(project_root, || audit_agents(project_root))
    {
        tracing::warn!(error = %err, "loop: failed to retire ended sessions before firing");
    }
    let tasks = super::fire::workspace_runnable_tasks(runtime, Some(project_root));
    let mut fired = SignalFire::default();
    let arming_entries = match arming::load() {
        Ok(arming) => arming,
        Err(error) => {
            tracing::warn!(%error, "loop: the task record is unreadable; no task fires");
            return Ok(fired);
        }
    };
    for (name, task) in &tasks {
        let Ok(parsed) = task.trigger() else { continue };
        let resolution = parsed.trigger.resolve(name, signal);
        if resolution == SignalResolution::Ignore {
            continue;
        }
        if resolution == SignalResolution::Excluded {
            fired.excluded.push(name.clone());
            continue;
        }
        let key = task.key(name);
        if super::fire::is_untrusted(task)
            || arming::ArmState::resolve(
                arming_entries.get(&key),
                task.source(),
                jiff::Timestamp::now(),
            ) != arming::ArmState::Live
        {
            if resolution == SignalResolution::Deliver {
                fired.not_started.push(name.clone());
            }
            continue;
        }
        if resolution == SignalResolution::Skip {
            use super::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult, SignalRecord};
            let mut record = LoopRunRecord::new(
                name,
                LoopRunResult::SignalSkipped,
                LoopRunMode::Scheduled,
                0,
            );
            record.signal = Some(SignalRecord {
                name: signal.name.clone(),
                payload: signal.payload.clone(),
            });
            super::run_log::record_transition(task, &record);
            continue;
        }
        let outcome = if wait && matches!(parsed.trigger, super::Trigger::Watch(_)) {
            super::fire::wait_loop_run(runtime, task, Some(project_root), name, signal, fired_at)
        } else {
            super::fire::spawn_loop_run(
                runtime,
                task,
                Some(project_root),
                name,
                Some((signal, fired_at)),
                None,
                None,
                super::fire::LoopRunHost::Detached,
            )
        };
        if outcome == super::fire::LaunchOutcome::Started {
            fired.started.push(name.clone());
        } else {
            fired.not_started.push(name.clone());
        }
    }
    Ok(fired)
}

fn workflow_wake_args(project_root: &Path, signal: &Signal) -> Vec<std::ffi::OsString> {
    crate::harness::workflow::advance_args(project_root, None, Some(signal), None)
}

fn wake_workflows(project_root: &Path, signal: &Signal) {
    let args = workflow_wake_args(project_root, signal);
    let launch = || -> Result<(), Box<dyn std::error::Error>> {
        let paths = StatePaths::for_project_root(project_root)?;
        let heard = if signal.source == SignalSource::Lifecycle {
            crate::harness::workflow::hears_lifecycle(&paths, signal, &|run| {
                crate::harness::run::load(&paths, &run.run_id)
                    .ok()
                    .and_then(|run| run.agent_id)
            })?
        } else {
            crate::store::workflow::live_runs(&paths)?
                .iter()
                .any(|r| r.state == crate::store::workflow::RunState::Active)
        };
        if !heard {
            return Ok(());
        }
        crate::child_process::spawn_detached_reaped(
            crate::proc::rimz_exe(),
            |command| {
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;

                    // The advance must survive closing the pane that emitted the signal.
                    command.process_group(0);
                }
                command.args(&args);
                Ok(())
            },
            "workflow signal wake",
        )?;
        Ok(())
    };
    if let Err(error) = launch() {
        tracing::warn!(%error, "could not wake workflows");
    }
}

fn watch_lock_path(runtime: &RuntimePaths, name: &str) -> std::path::PathBuf {
    runtime.lock_path(format!("loop-watch-{name}.lock"))
}

pub(super) fn acquire_watch_lock(
    runtime: &RuntimePaths,
    name: &str,
) -> std::io::Result<Option<lease::Lease>> {
    let path = watch_lock_path(runtime, name);
    match lease::claim(&path, |file| {
        serde_json::to_writer(
            file,
            &RunLockInfo {
                pid: std::process::id(),
                started_at: jiff::Timestamp::now(),
            },
        )
        .map_err(std::io::Error::other)
    })? {
        lease::LeaseAttempt::Acquired(guard) => Ok(Some(guard)),
        lease::LeaseAttempt::Held(_) => Ok(None),
    }
}

pub fn watcher_info(runtime: &RuntimePaths, name: &str) -> std::io::Result<Option<RunLockInfo>> {
    let path = watch_lock_path(runtime, name);
    match lease::probe(&path)? {
        lease::LeaseState::Free => Ok(None),
        lease::LeaseState::Held(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
    }
}

pub fn stop_watcher(runtime: &RuntimePaths, name: &str) -> std::io::Result<bool> {
    let Some(info) = watcher_info(runtime, name)? else {
        return Ok(false);
    };
    // `watcher_info` opens its own descriptor, so flock reports the lock held
    // even to the process holding it and hands back that holder's own pid. A
    // watcher that retires its own row mid-fire — the reconcile at the top of
    // `fire_signal_with_wait` reaches here through `arm::retire_session` — would
    // otherwise `killpg` its own group and take the fire, its delivery, and the
    // watched command with it.
    if info.pid == std::process::id() {
        return Ok(false);
    }
    let Ok(pid) = i32::try_from(info.pid) else {
        return Ok(false);
    };
    if pid <= 0 {
        return Ok(false);
    }
    match nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(true),
        Err(err) => Err(std::io::Error::other(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{
        AgentStatus, LifecycleEvent, LifecycleSignal, LifecycleTransition, TurnPhase,
    };
    use crate::ids::{AgentKind, AgentSessionId, EventId, WorkspaceId};

    #[test]
    fn watcher_probes_never_defeat_a_starting_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        std::fs::create_dir_all(&runtime.locks_dir).unwrap();
        drop(acquire_watch_lock(&runtime, "race").unwrap().unwrap());
        let path = watch_lock_path(&runtime, "race");
        let info = lease::assert_waits_for_gate(&path, || watcher_info(&runtime, "race")).unwrap();
        assert!(info.is_none());
        let claim =
            lease::assert_waits_for_gate(&path, || acquire_watch_lock(&runtime, "race")).unwrap();
        assert!(claim.is_some());
    }

    #[test]
    fn workflow_wakes_carry_the_whole_signal_from_every_source() {
        for source in [
            SignalSource::Cli,
            SignalSource::Watch,
            SignalSource::Forge,
            SignalSource::Git,
            SignalSource::Worktree,
            SignalSource::Team,
            SignalSource::Lifecycle,
        ] {
            let signal = Signal {
                name: "review.ready".parse().unwrap(),
                source,
                payload: serde_json::from_value(serde_json::json!({
                    "path": "/a checkout/with spaces", "note": "quote \" and\nnewline"
                }))
                .unwrap(),
                watch: None,
            };
            let args = workflow_wake_args(Path::new("/project"), &signal);
            assert_eq!(
                args[..5],
                ["--root", "/project", "workflow", "advance", "--wake-signal"]
                    .map(std::ffi::OsString::from)
            );
            assert_eq!(
                serde_json::from_str::<Signal>(args[5].to_str().unwrap()).ok(),
                Some(signal)
            );
        }
    }

    #[test]
    fn wait_tail_bounds_lossy_utf8_and_keeps_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wait.log");
        let mut bytes = vec![0xff; WAIT_TAIL_CAP * 2];
        bytes.extend_from_slice(b"final diagnostic");
        std::fs::write(&path, bytes).unwrap();
        let tail = read_wait_tail(&path).unwrap();
        assert!(tail.len() <= WAIT_TAIL_CAP);
        assert!(tail.ends_with("final diagnostic"));
        assert!(tail.contains('�'));
        std::fs::write(&path, "short output").unwrap();
        assert_eq!(read_wait_tail(&path).unwrap(), "short output");
    }

    fn lifecycle_event(signal: LifecycleSignal) -> LifecycleEvent {
        LifecycleEvent {
            v: 1,
            event_id: EventId::parse("evt_018f47a2c00070008000000000000000").unwrap(),
            at: "2026-06-01T12:00:00Z".parse().unwrap(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("session-1"),
            agent_name: Some("coder".to_owned()),
            parent_agent_id: Some(AgentSessionId::from("session-parent")),
            signal,
            prior_status: Some(AgentStatus::Running),
            status: AgentStatus::Success,
            phase: TurnPhase::Idle,
            transition: LifecycleTransition::Normal,
            compaction_closed: false,
            waiting_cleared: false,
        }
    }

    #[test]
    fn lifecycle_events_derive_the_public_signal_table() {
        for (input, expected, errored) in [
            (LifecycleSignal::Registered, "agent.started", false),
            (LifecycleSignal::SubagentStarted, "agent.started", false),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
                "agent.idle",
                false,
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                    turn_id: None,
                },
                "agent.failed",
                true,
            ),
            (
                LifecycleSignal::AwaitingInput {
                    extra_questions: 0,
                    kind: crate::agents::AskKind::Question,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                "agent.waiting",
                false,
            ),
            (LifecycleSignal::Ended, "agent.ended", false),
            (LifecycleSignal::Lost, "agent.ended", false),
            (
                LifecycleSignal::SubagentStopped { errored: true },
                "agent.ended",
                false,
            ),
        ] {
            let signal = lifecycle_signal(&lifecycle_event(input)).expect("derived signal");
            assert_eq!(signal.name.as_str(), expected);
            assert_eq!(signal.source, SignalSource::Lifecycle);
            assert_eq!(signal.payload["handle"], "@coder");
            assert_eq!(signal.payload["session"], "session-1");
            assert_eq!(signal.payload["parent"], "session-parent");
            assert_eq!(signal.payload["errored"], errored);
        }

        let mut ignored = lifecycle_event(LifecycleSignal::Registered);
        ignored.transition = LifecycleTransition::Ignored {
            reason: "duplicate".to_owned(),
        };
        assert_eq!(lifecycle_signal(&ignored), None);
        for terminal in [LifecycleSignal::Ended, LifecycleSignal::Lost] {
            ignored.signal = terminal;
            assert_eq!(
                lifecycle_signal(&ignored).unwrap().name.as_str(),
                "agent.ended"
            );
        }
        assert_eq!(
            lifecycle_signal(&lifecycle_event(LifecycleSignal::TurnStarted {
                turn_id: None
            })),
            None
        );
    }
}
