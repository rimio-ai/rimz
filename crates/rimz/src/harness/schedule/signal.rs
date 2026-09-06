//! Signal vocabulary shared by event ingress, lifecycle hooks, and loop firing.

mod team;
pub use team::team_lifecycle_signals;

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::str::FromStr;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::arming;
use super::catalog::TaskCatalog;
use super::run_log::LoopRunMode;
use super::runner::{
    CheckEcho, CheckOutcome, check_record, configured_timeout, effective_spawn_timeout, run_check,
    task_timeout,
};
use crate::RuntimePaths;
use crate::disk::paths::StatePaths;
use crate::harness::schedule::runner::RunLockInfo;
use crate::store::Store;
use crate::store::event::{
    MAX_SIGNAL_NAME_BYTES, SignalEventPayload, SignalName, SignalNameErr, SignalSource,
};
use crate::workspace::ResolvedWorkspace;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalSelector {
    Exact(SignalName),
    Family(String),
}

impl SignalSelector {
    pub fn family(&self) -> &str {
        match self {
            Self::Exact(name) => name.family(),
            Self::Family(family) => family,
        }
    }
}

impl fmt::Display for SignalSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(name) => name.fmt(f),
            Self::Family(family) => write!(f, "{family}.*"),
        }
    }
}

impl FromStr for SignalSelector {
    type Err = SignalNameErr;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if let Some(family) = raw.strip_suffix(".*") {
            if raw.len() > MAX_SIGNAL_NAME_BYTES || family.contains('.') {
                return Err(SignalNameErr(raw.to_owned()));
            }
            return family
                .parse::<SignalName>()
                .map(|name| Self::Family(name.as_str().to_owned()));
        }
        raw.parse().map(Self::Exact)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SignalResolution {
    Ignore,
    Skip,
    Deliver,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Signal {
    pub name: SignalName,
    #[serde(default)]
    pub payload: Map<String, Value>,
    pub source: SignalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchOutcome>,
}

impl From<&Signal> for SignalEventPayload {
    fn from(signal: &Signal) -> Self {
        Self {
            name: signal.name.clone(),
            payload: signal.payload.clone(),
            source: signal.source,
        }
    }
}

pub(super) const WAKE_TAIL_CAP: usize = 4 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WatchVerdict {
    Exited { code: Option<i32>, elapsed_ms: u64 },
    TimedOut { elapsed_ms: u64 },
    Lost { detail: String, elapsed_ms: u64 },
}

impl WatchVerdict {
    pub fn label(&self) -> String {
        let elapsed = elapsed_label(self.elapsed_ms());
        match self {
            Self::Exited {
                code: Some(code), ..
            } => format!("exit {code} after {elapsed}"),
            Self::Exited { code: None, .. } => format!("killed by signal after {elapsed}"),
            Self::TimedOut { .. } => format!("timed out after {elapsed}"),
            Self::Lost { .. } => format!(
                "watcher died after {elapsed}; the command may still be running or may have died with it"
            ),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Exited { elapsed_ms, .. }
            | Self::TimedOut { elapsed_ms }
            | Self::Lost { elapsed_ms, .. } => *elapsed_ms,
        }
    }

    fn passed(&self) -> bool {
        matches!(self, Self::Exited { code: Some(0), .. })
    }
}

pub(super) fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        crate::theme::fmt::duration_label(seconds / 60)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WatchOutcome {
    #[serde(flatten)]
    pub verdict: WatchVerdict,
    #[serde(default)]
    pub output: String,
    pub output_path: Option<PathBuf>,
}

impl WatchOutcome {
    pub(super) fn to_check_outcome(&self) -> CheckOutcome {
        let code = match self.verdict {
            WatchVerdict::Exited { code, .. } => code,
            _ => None,
        };
        CheckOutcome::new(
            self.verdict.passed(),
            matches!(self.verdict, WatchVerdict::TimedOut { .. }),
            self.output.clone(),
            code,
        )
    }
}

pub fn wake_log_path(paths: &StatePaths, name: &str) -> PathBuf {
    paths.wakes_dir.join(format!("{name}.log"))
}

pub(super) fn read_wake_tail(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(WAKE_TAIL_CAP as u64);
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(WAKE_TAIL_CAP);
    file.take(WAKE_TAIL_CAP as u64).read_to_end(&mut bytes)?;
    let output = String::from_utf8_lossy(&bytes);
    let mut start = output.len().saturating_sub(WAKE_TAIL_CAP);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    Ok(output[start..].to_owned())
}

/// Prune old wake audit output, retaining definitions and running watchers.
pub fn prune_wake_logs() -> anyhow::Result<usize> {
    let entries = match std::fs::read_dir(crate::disk::paths::workspaces_dir()) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    let catalog = match TaskCatalog::load(None) {
        Ok(catalog) => catalog,
        Err(err) => {
            tracing::warn!(error = %err, "wake log gc retained output with unreadable task state");
            return Ok(0);
        }
    };
    let retained = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.entry().watch.is_some())
        .map(|(name, _)| name.clone())
        .collect();
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "wake log gc skipped unreadable directory entry");
                continue;
            }
        };
        let Ok(id) = crate::ids::WorkspaceId::parse(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let pruned: anyhow::Result<usize> = (|| {
            let paths = StatePaths::for_workspace(id.clone())?;
            crate::workspace::record::read(&paths.workspace_record)?;
            let runtime = RuntimePaths::for_workspace(id.clone())?;
            Ok(prune_wake_logs_in(
                &paths.wakes_dir,
                &runtime,
                &retained,
                now,
            )?)
        })();
        match pruned {
            Ok(count) => removed += count,
            Err(err) => {
                tracing::warn!(workspace = %id, error = %err, "wake log gc skipped unreadable workspace")
            }
        }
    }
    Ok(removed)
}

fn prune_wake_logs_in(
    dir: &Path,
    runtime: &RuntimePaths,
    retained: &std::collections::BTreeSet<String>,
    now: std::time::SystemTime,
) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "log")
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if retained.contains(name) || watcher_info(runtime, name)?.is_some() {
            continue;
        }
        let metadata = entry.metadata()?;
        if now.duration_since(metadata.modified()?).unwrap_or_default()
            <= crate::store::event_log::DEFAULT_RETENTION
        {
            continue;
        }
        std::fs::remove_file(path)?;
        removed += 1;
    }
    Ok(removed)
}

pub(super) fn match_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

pub fn lifecycle_signal(event: &crate::agents::LifecycleEvent) -> Option<Signal> {
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
    let catalog = TaskCatalog::load(Some(&workspace.project_root))?;
    let Some(task) = catalog.for_run(name) else {
        anyhow::bail!("no wake named {name} in the catalog");
    };
    if task.entry().resolved_root() != workspace.project_root {
        anyhow::bail!(
            "wake {name} belongs to {}, watcher started for {}",
            task.entry().resolved_root().display(),
            workspace.project_root.display()
        );
    }
    let Some(command) = task.entry().watch.as_deref() else {
        anyhow::bail!("wake {name} has no watched command");
    };
    let Some(_guard) =
        acquire_watch_lock(store.runtime_paths(), name).context("locking wake watcher")?
    else {
        return Ok(());
    };
    let configured = configured_timeout(&crate::config::MachineConfig::load_lenient())?;
    // Scheduled mode always supplies the built-in timeout when neither source does.
    let timeout = effective_spawn_timeout(
        LoopRunMode::Scheduled,
        task_timeout(task.entry())?,
        configured,
    )
    .expect("scheduled mode always resolves a timeout");
    let output_path = wake_log_path(store.paths(), name);
    let file = OpenOptions::new()
        .append(true)
        .open(&output_path)
        .with_context(|| format!("opening wake output {}", output_path.display()))?;
    let started = std::time::Instant::now();
    let outcome = run_check(
        &workspace.project_root,
        command,
        timeout,
        CheckEcho::Tee { file },
    )?;
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let check = check_record(&outcome);
    let verdict = if check.timed_out {
        WatchVerdict::TimedOut { elapsed_ms }
    } else {
        WatchVerdict::Exited {
            code: check.code,
            elapsed_ms,
        }
    };
    let watch = WatchOutcome {
        verdict,
        output: check.output,
        output_path: Some(output_path),
    };
    let signal = Signal {
        name: format!("wake.{name}")
            .parse()
            .expect("generated wake signal name is valid"),
        payload: Map::new(),
        source: SignalSource::Watch,
        watch: Some(watch),
    };
    store
        .append_signal(&workspace.session_name, (&signal).into())
        .context("appending wake signal")?;
    fire_signal(store.runtime_paths(), &workspace.project_root, &signal)
        .context("firing watched wake")?;
    Ok(())
}

/// Fire matching tasks in the emitter process. Signal events are never replayed.
pub fn fire_signal(
    runtime: &RuntimePaths,
    project_root: &Path,
    signal: &Signal,
) -> Result<Vec<String>, serde_json::Error> {
    let tasks = super::fire::runnable_tasks_for(runtime, Some(project_root));
    let arming_entries = arming::load();
    let encoded = serde_json::to_string(signal)?;
    let mut fired = Vec::new();
    for (name, task) in tasks {
        let Ok(parsed) = task.trigger() else { continue };
        let resolution = parsed.trigger.resolve(&name, signal);
        if resolution == SignalResolution::Ignore {
            continue;
        }
        let key = arming::TaskKey::for_task(&name, task.source(), &task.entry().resolved_root());
        if arming::ArmState::resolve(
            arming_entries.get(&key),
            task.source(),
            jiff::Timestamp::now(),
        ) != arming::ArmState::Live
        {
            continue;
        }
        if resolution == SignalResolution::Skip {
            use super::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult, SignalRecord};
            let mut record = LoopRunRecord::new(
                &name,
                LoopRunResult::SignalSkipped,
                LoopRunMode::Scheduled,
                0,
            );
            record.signal = Some(SignalRecord {
                name: signal.name.clone(),
                payload: signal.payload.clone(),
            });
            super::run_log::record_transition(&task, &record);
            continue;
        }
        super::fire::spawn_loop_run(runtime, Some(project_root), &name, Some(&encoded), false);
        fired.push(name);
    }
    Ok(fired)
}

pub(super) struct WatchLockGuard {
    file: File,
}

impl Drop for WatchLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn watch_lock_path(runtime: &RuntimePaths, name: &str) -> std::path::PathBuf {
    runtime.root.join(format!("loop-watch-{name}.lock"))
}

pub(super) fn acquire_watch_lock(
    runtime: &RuntimePaths,
    name: &str,
) -> std::io::Result<Option<WatchLockGuard>> {
    let path = watch_lock_path(runtime, name);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            serde_json::to_writer(
                &mut file,
                &RunLockInfo {
                    pid: std::process::id(),
                    started_at: jiff::Timestamp::now(),
                },
            )
            .map_err(std::io::Error::other)?;
            file.flush()?;
            Ok(Some(WatchLockGuard { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(err) => Err(std::io::Error::from(err)),
    }
}

pub fn watcher_info(runtime: &RuntimePaths, name: &str) -> std::io::Result<Option<RunLockInfo>> {
    let path = watch_lock_path(runtime, name);
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(None)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(serde_json::from_slice(&bytes).ok())
        }
        Err(err) => Err(std::io::Error::from(err)),
    }
}

pub fn stop_watcher(runtime: &RuntimePaths, name: &str) -> std::io::Result<bool> {
    let Some(info) = watcher_info(runtime, name)? else {
        return Ok(false);
    };
    let Ok(pid) = i32::try_from(info.pid) else {
        return Ok(false);
    };
    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(false),
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
    fn watch_verdicts_share_labels_and_check_semantics() {
        for (verdict, label, passed, timed_out, code) in [
            (
                WatchVerdict::Exited {
                    code: Some(0),
                    elapsed_ms: 3_000,
                },
                "exit 0 after 3s",
                true,
                false,
                Some(0),
            ),
            (
                WatchVerdict::Exited {
                    code: Some(3),
                    elapsed_ms: 720_000,
                },
                "exit 3 after 12m",
                false,
                false,
                Some(3),
            ),
            (
                WatchVerdict::Exited {
                    code: None,
                    elapsed_ms: 3_000,
                },
                "killed by signal after 3s",
                false,
                false,
                None,
            ),
            (
                WatchVerdict::TimedOut {
                    elapsed_ms: 3_540_000,
                },
                "timed out after 59m",
                false,
                true,
                None,
            ),
            (
                WatchVerdict::Lost {
                    detail: "diagnostic".to_owned(),
                    elapsed_ms: 180_000,
                },
                "watcher died after 3m; the command may still be running or may have died with it",
                false,
                false,
                None,
            ),
        ] {
            assert_eq!(verdict.label(), label);
            assert_eq!(verdict.passed(), passed);
            let outcome = WatchOutcome {
                verdict,
                output: "actual tail".to_owned(),
                output_path: Some(PathBuf::from("/state/wakes/wake.log")),
            };
            let encoded = serde_json::to_string(&outcome).unwrap();
            assert_eq!(
                serde_json::from_str::<WatchOutcome>(&encoded).unwrap(),
                outcome
            );
            let check = check_record(&outcome.to_check_outcome());
            assert_eq!(check.output, "actual tail");
            assert_eq!(check.timed_out, timed_out);
            assert_eq!(check.code, code);
        }
    }

    #[test]
    fn wake_tail_bounds_lossy_utf8_and_keeps_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.log");
        let mut bytes = vec![0xff; WAKE_TAIL_CAP * 2];
        bytes.extend_from_slice(b"final diagnostic");
        std::fs::write(&path, bytes).unwrap();
        let tail = read_wake_tail(&path).unwrap();
        assert!(tail.len() <= WAKE_TAIL_CAP);
        assert!(tail.ends_with("final diagnostic"));
        assert!(tail.contains('�'));
        std::fs::write(&path, "short output").unwrap();
        assert_eq!(read_wake_tail(&path).unwrap(), "short output");
    }

    #[test]
    fn wake_log_gc_retains_recent_defined_and_live_output() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        std::fs::create_dir_all(&runtime.root).unwrap();
        let logs = dir.path().join("wakes");
        std::fs::create_dir(&logs).unwrap();
        let now = std::time::SystemTime::now();
        let old =
            now - crate::store::event_log::DEFAULT_RETENTION - std::time::Duration::from_secs(1);
        for name in [
            "old.log",
            "recent.log",
            "defined.log",
            "live.log",
            "unrelated.txt",
        ] {
            let file = File::create(logs.join(name)).unwrap();
            if name != "recent.log" {
                file.set_times(std::fs::FileTimes::new().set_modified(old))
                    .unwrap();
            }
        }
        let guard = acquire_watch_lock(&runtime, "live").unwrap().unwrap();
        let retained = std::collections::BTreeSet::from(["defined".to_owned()]);
        assert_eq!(
            prune_wake_logs_in(&logs, &runtime, &retained, now).unwrap(),
            1
        );
        assert!(!logs.join("old.log").exists());
        for name in ["recent.log", "defined.log", "live.log", "unrelated.txt"] {
            assert!(logs.join(name).exists(), "{name}");
        }
        drop(guard);
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
    fn signal_names_pin_the_public_grammar() {
        for valid in [
            "ci.failed",
            "ci.*",
            "deploy.finished",
            "a.b.c",
            "deploy_done",
        ] {
            let selector = valid.parse::<SignalSelector>().unwrap();
            assert_eq!(selector.to_string(), valid);
            assert_eq!(
                selector.to_string().parse::<SignalSelector>().unwrap(),
                selector
            );
        }
        for invalid in ["*", "a.b.*", "a*", "A.*", ".*", "a..*"] {
            assert!(invalid.parse::<SignalSelector>().is_err(), "{invalid}");
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
                },
                "agent.idle",
                false,
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                },
                "agent.failed",
                true,
            ),
            (
                LifecycleSignal::AwaitingInput {
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
            lifecycle_signal(&lifecycle_event(LifecycleSignal::TurnStarted)),
            None
        );
    }
}
