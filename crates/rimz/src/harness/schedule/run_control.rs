//! The run lock's holder side: claim and probe a task's lease, list held locks, find the
//! in-flight run, and the stop ladder that signals a holder and records its stopped row.
//!
//! The read-only half, the lock's file name and whether a task holds one, is `run_lock.rs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use super::{
    backlog::{self, StopCause, StopMark},
    backlog_store::LockedBacklog,
    run_lock::{self, file_name as run_lock_file_name},
    task_record::{self, TaskRecords},
};
use crate::config::TaskEntry;
use crate::disk::lease::{Lease, LeaseAttempt, LeaseState, claim, probe};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::schedule::TaskKind;
use crate::harness::schedule::run_log::{
    self, LoopRunMode, LoopRunRecord, LoopRunResult, RunTransition,
};
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::store::run::RunRecord;
use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};

const RUN_LOCK_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const HOLDER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Discard waiting triggers and stop marks when a drain cannot compile its task.
pub fn discard_backlog(name: &str, root: &Path) -> Result<()> {
    let paths = StatePaths::for_project_root(root)?;
    let ids = task_record::load()?.ids_named(name);
    for key in ids.iter().map(TaskId::as_str).chain([name]) {
        super::backlog_store::remove(&paths, key)?;
    }
    Ok(())
}

/// Newest durable supervised run still active for one loop task, started at or after its run
/// lock holder took the lock: an earlier fire's run (an `allow` task's, or one a fire is about
/// to record as overlapped) is never the holder's.
fn newest_active_run(
    paths: &StatePaths,
    name: &str,
    holder: Option<RunLockInfo>,
) -> Result<Option<RunRecord>> {
    let mut records = crate::harness::run::list(paths)?;
    records.retain(|record| {
        !record.status.is_terminal()
            && record.loop_task.as_deref() == Some(name)
            && holder.is_none_or(|holder| record.started_at >= holder.started_at)
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

/// A loop run in flight: the held run lock's holder, and its run record once written.
pub struct InFlightRun {
    pub holder: Option<RunLockInfo>,
    pub run: Option<RunRecord>,
}

/// The run of task `name` in flight under `root`. The run lock decides; the run
/// record is read only while the lock is held, so a stale record is no run.
pub fn in_flight_run(name: &str, root: &Path) -> Result<Option<InFlightRun>> {
    let Some((_, holder)) = held_run_lock(name, root)? else {
        return Ok(None);
    };
    let (_, project_root) = stop_workspace(root)?;
    let run = newest_active_run(&StatePaths::for_project_root(&project_root)?, name, holder)?;
    Ok(Some(InFlightRun { holder, run }))
}

const STOP_GRACE: Duration = Duration::from_secs(5);

pub enum StopOutcome {
    NoActiveRun,
    Stopped {
        run_id: Option<RunId>,
        signaled: bool,
        waiting: usize,
    },
}

/// Stop the run of task `name` under `root`; the task's row may already be gone.
pub fn stop_task(
    name: &str,
    root: &Path,
    cancel: impl FnOnce(Option<&ResolvedWorkspace>, StatePaths, Option<&RunRecord>) -> Result<()>,
) -> Result<(StopOutcome, RunTransition)> {
    if held_run_lock(name, root)?.is_none() {
        return Ok((StopOutcome::NoActiveRun, RunTransition::Recorded));
    }
    let (workspace, project_root) = stop_workspace(root)?;
    let paths = StatePaths::for_project_root(&project_root)?;
    let backlog_paths = StatePaths::for_project_root(root)?;
    let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
    let Some((lock, holder)) = held_run_lock(name, root)? else {
        return Ok((StopOutcome::NoActiveRun, RunTransition::Recorded));
    };
    let key = stop_key(name, Some(&lock));
    let info = holder
        .context("loop run lock holder identity unavailable; stop the holder manually and retry")?;
    let metadata = std::fs::read(&lock)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RunLockMetadata>(&bytes).ok())
        .filter(|metadata| Some(metadata.holder) == holder);
    let kind = metadata.as_ref().and_then(|metadata| metadata.kind);

    let waiting = backlog.record.tasks.entry(key.clone()).or_default();
    waiting.stopping = Some(StopMark {
        pid: info.pid,
        started_at: info.started_at,
        cause: StopCause::Stop,
    });
    let waiting = waiting.triggers.len();
    backlog.save()?;
    let run = newest_active_run(&paths, name, Some(info));
    drop(backlog);
    cancel(
        workspace.as_ref(),
        paths.clone(),
        run.as_ref().ok().and_then(Option::as_ref),
    )?;
    let run = run?;

    if wait_for_run_lock_release_path(&lock, holder, STOP_GRACE)? {
        let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
        let transition =
            append_stopped_record(&key, name, root, info, run.as_ref(), kind, &mut backlog)?;
        return Ok((
            StopOutcome::Stopped {
                run_id: run.map(|record| record.run_id),
                signaled: false,
                waiting,
            },
            transition,
        ));
    }

    // Cancellation and lock I/O can outlast a holder. Never signal its successor.
    let (signaled, signal_error) = match probe_run_lock_path(&lock)? {
        RunLockState::Held(current) if current == holder => {
            (true, signal_run_lock_holder(&info).err())
        }
        _ => (false, None),
    };
    let signal_deadline = Instant::now() + STOP_GRACE;

    if signal_error.is_none() && wait_for_run_lock_release_path(&lock, holder, STOP_GRACE)? {
        let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
        let transition =
            append_stopped_record(&key, name, root, info, run.as_ref(), kind, &mut backlog)?;
        drop(backlog);
        // A run stopped while it holds its admitted ticket dies with it. The
        // kernel frees the lock before `/proc` reports the holder dead, and the
        // sweep removes a ticket only once its owner reads dead, so wait for
        // that within the same grace. A holder still live past it keeps its
        // ticket for the next admit's sweep.
        if !wait_for_process_exit(
            info.pid,
            signal_deadline.saturating_duration_since(Instant::now()),
        ) {
            tracing::debug!(pid = info.pid, "stopped loop run holder still reads live");
        }
        crate::harness::throttle::reap();
        return Ok((
            StopOutcome::Stopped {
                run_id: run.map(|record| record.run_id),
                signaled,
                waiting,
            },
            transition,
        ));
    }

    let holder = holder
        .map(|info| format!(" (pid {})", info.pid))
        .unwrap_or_default();
    let signal = signal_error
        .map(|err| format!("; SIGTERM failed: {err:#}"))
        .unwrap_or_default();
    bail!(
        "loop `{name}` is still active{holder}; lock {}; stop the holder manually and retry{signal}",
        lock.display()
    )
}

fn stop_workspace(root: &Path) -> Result<(Option<ResolvedWorkspace>, PathBuf)> {
    let workspace = root
        .exists()
        .then(|| WorkspaceResolver::resolve(root, None))
        .transpose()
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let project_root = match &workspace {
        Some(workspace) => workspace.project_root.clone(),
        None => WorkspaceResolver::persisted_project_root(root)?,
    };
    Ok((workspace, project_root))
}

/// The backlog key a stop of task `name` marks: the task id its held run lock is spelled with
/// (or, with no lock, its live record's), else the name an older RimZ's runner keys by.
fn stop_key(name: &str, lock: Option<&Path>) -> String {
    let ids = task_record::load()
        .map(|records| records.ids_named(name))
        .unwrap_or_default();
    let file = lock
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str);
    let id = match file {
        Some(file) => ids
            .into_iter()
            .find(|id| run_lock::is_run_lock_of(file, id.as_str())),
        None => ids.into_iter().next(),
    };
    id.map_or_else(|| name.to_owned(), |id| id.to_string())
}

fn append_stopped_record(
    key: &str,
    name: &str,
    root: &Path,
    info: RunLockInfo,
    run: Option<&RunRecord>,
    kind: Option<TaskKind>,
    backlog: &mut LockedBacklog<'_>,
) -> Result<RunTransition> {
    let Some(mark) = backlog
        .record
        .tasks
        .get_mut(key)
        .and_then(|task| task.stopping.take_if(|mark| info.matches_stop(mark)))
    else {
        return Ok(RunTransition::Recorded);
    };
    let paths = StatePaths::for_project_root(root)?;
    let elapsed = Timestamp::now()
        .as_millisecond()
        .saturating_sub(info.started_at.as_millisecond());
    let duration_ms = u64::try_from(elapsed).unwrap_or(0);
    let mut record = LoopRunRecord::new(
        name,
        LoopRunResult::Canceled,
        LoopRunMode::Scheduled,
        duration_ms,
    );
    record.mode = None;
    record.task_id = TaskId::parse(key).ok();
    record.run_lock = Some(info);
    set_stopped_result(&mut record, mark.cause);
    record.run_id = run.map(|record| record.run_id.to_string());
    backlog.save()?;
    run_log::record_stopped(root, &record, kind);
    if let Err(error) = spawn_backlog_drain(&paths, root, key, name, &backlog.record) {
        tracing::warn!(task = name, %error, "failed to start loop backlog drain");
    }
    Ok(RunTransition::Recorded)
}

/// A stopped holder's row is a strike-neutral `canceled`; a `replace` mark an older RimZ left
/// keeps its reason.
pub(super) fn set_stopped_result(record: &mut LoopRunRecord, cause: StopCause) {
    record.result = LoopRunResult::Canceled;
    record.error = Some(
        match cause {
            StopCause::Replace => "replaced by a newer trigger",
            StopCause::Stop => "stopped by rimz loop stop",
        }
        .to_owned(),
    );
}

pub(super) fn spawn_backlog_drain(
    paths: &StatePaths,
    root: &Path,
    key: &str,
    name: &str,
    backlog: &backlog::Backlog,
) -> Result<()> {
    if backlog
        .tasks
        .get(key)
        .is_some_and(|task| !task.triggers.is_empty())
    {
        super::fire::spawn_loop_drain(
            &RuntimePaths::for_state(paths)?,
            root,
            name,
            super::fire::LoopRunHost::Detached,
        )?;
    }
    Ok(())
}

pub(super) struct RunLockGuard {
    _lease: Lease,
    pub(super) info: RunLockInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLockInfo {
    pub pid: u32,
    pub started_at: Timestamp,
}

impl RunLockInfo {
    pub(super) fn matches_stop(self, mark: &StopMark) -> bool {
        self.pid == mark.pid && self.started_at == mark.started_at
    }
}

#[derive(Serialize, Deserialize)]
struct RunLockMetadata {
    #[serde(flatten)]
    holder: RunLockInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<TaskKind>,
}

pub(super) enum RunLockAttempt {
    Acquired(RunLockGuard),
    Held(Option<RunLockInfo>),
}

pub enum RunLockState {
    Available,
    Held(Option<RunLockInfo>),
}

/// One listing of a root's `locks/`, with every run lock in it probed once.
pub struct RunLocks {
    files: Vec<RunLockFile>,
    /// The task records, which name the id each task's runner claims its lock under.
    records: TaskRecords,
}

struct RunLockFile {
    name: String,
    path: PathBuf,
    state: Result<RunLockState>,
}

impl RunLocks {
    /// List and probe run locks, including a cold check's runtime-only lock.
    pub fn list(root: &Path) -> Result<Self> {
        let runtime = RuntimePaths::for_state(&StatePaths::for_project_root(root)?)?;
        let locks = runtime.locks_dir;
        let listing = || format!("listing loop run locks `{}`", locks.display());
        let entries = match std::fs::read_dir(&locks) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    files: Vec::new(),
                    records: TaskRecords::default(),
                });
            }
            Err(err) => return Err(err).with_context(listing),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.with_context(listing)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if run_lock_stem(&name).is_none() {
                continue;
            }
            let path = entry.path();
            let state = probe_run_lock_path(&path);
            files.push(RunLockFile { name, path, state });
        }
        // An unreadable record leaves only the name spelling, an older RimZ's; `loop list` and
        // `rimz doctor` report the record itself.
        let records = task_record::load().unwrap_or_default();
        Ok(Self { files, records })
    }

    /// Whether task `name` is running: held when any lock file it claims is.
    pub fn state(&self, name: &str) -> Result<RunLockState> {
        Ok(match self.held(name)? {
            Some((_, holder)) => RunLockState::Held(holder),
            None => RunLockState::Available,
        })
    }

    /// The held locks none of `rows` claims, as task name and holder, by name.
    /// The name is the file's whole stem, the one `in_flight_run` and
    /// `stop_task` resolve back to that file.
    pub fn rowless(&self, rows: &[&str]) -> Vec<(String, Option<RunLockInfo>)> {
        let mut rowless = Vec::new();
        for file in &self.files {
            let Ok(RunLockState::Held(holder)) = &file.state else {
                continue;
            };
            if rows
                .iter()
                .any(|row| run_lock::claims(&file.name, row, &self.records.ids_named(row)))
            {
                continue;
            }
            if let Some(stem) = run_lock_stem(&file.name) {
                rowless.push((stem.to_owned(), *holder));
            }
        }
        rowless.sort_by(|(left, _), (right, _)| left.cmp(right));
        rowless
    }

    /// The held run lock of task `name`, with its holder. A fan-out task holds
    /// one lock per checkout; the earliest-started holder is the one reported,
    /// a holderless lock last.
    fn held(&self, name: &str) -> Result<Option<(&Path, Option<RunLockInfo>)>> {
        let ids = self.records.ids_named(name);
        let mut held = Vec::new();
        for file in &self.files {
            if !run_lock::claims(&file.name, name, &ids) {
                continue;
            }
            match &file.state {
                Ok(RunLockState::Held(holder)) => held.push((file.path.as_path(), *holder)),
                Ok(RunLockState::Available) => {}
                Err(error) => bail!("{error:#}"),
            }
        }
        Ok(held
            .into_iter()
            .min_by_key(|(_, holder)| (holder.is_none(), holder.map(|info| info.started_at))))
    }
}

pub(super) fn held_run_lock(
    name: &str,
    root: &Path,
) -> Result<Option<(PathBuf, Option<RunLockInfo>)>> {
    Ok(RunLocks::list(root)?
        .held(name)?
        .map(|(path, holder)| (path.to_owned(), holder)))
}

pub(super) fn probe_run_lock_path(path: &Path) -> Result<RunLockState> {
    match probe(path).with_context(|| format!("probing loop run lock `{}`", path.display()))? {
        LeaseState::Free => Ok(RunLockState::Available),
        LeaseState::Held(body) => Ok(RunLockState::Held(read_run_lock_info(&body))),
    }
}

pub(super) fn run_lock_path(file: &str, entry: &TaskEntry) -> Result<PathBuf> {
    let root = entry.resolved_root();
    let runtime =
        run_lock_runtime(&root)?.ok_or_else(|| crate::workspace::WorkspaceErr::NoRoom {
            location: format!("at {}", root.display()),
        })?;
    Ok(runtime.lock_path(file))
}

fn run_lock_runtime(root: &Path) -> Result<Option<RuntimePaths>> {
    let state = StatePaths::for_project_root(root).context("locating loop task state")?;
    if crate::workspace::record::read(&state.workspace_record).is_err() {
        return Ok(None);
    }
    let runtime = RuntimePaths::for_state(&state).context("locating loop task runtime")?;
    Ok(crate::store::Store::open_existing(state, runtime.clone()).map(|_| runtime))
}

/// The task name a bare run lock file carries, `run_lock_file_name`'s inverse.
fn run_lock_stem(file: &str) -> Option<&str> {
    let frame = run_lock_file_name("\0", None);
    let (prefix, suffix) = frame.split_once('\0')?;
    file.strip_prefix(prefix)?
        .strip_suffix(suffix)
        .filter(|stem| !stem.is_empty())
}

fn signal_run_lock_holder(info: &RunLockInfo) -> Result<()> {
    let pid = i32::try_from(info.pid).context("loop run lock holder pid is out of range")?;
    if pid == 0 {
        anyhow::bail!("loop run lock holder pid must be positive");
    }
    if info.pid == std::process::id() {
        anyhow::bail!("refusing to signal the current process as a loop run lock holder");
    }
    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("signaling loop run lock holder pid {pid}")),
    }
}

fn wait_for_run_lock_release_path(
    path: &Path,
    holder: Option<RunLockInfo>,
    grace: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + grace;
    loop {
        if match probe_run_lock_path(path)? {
            RunLockState::Available => true,
            RunLockState::Held(current) => holder.is_some() && current != holder,
        } {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(RUN_LOCK_RELEASE_POLL_INTERVAL.min(remaining));
    }
}

fn wait_for_process_exit(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while crate::proc::process_is_live(pid, None) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(HOLDER_EXIT_POLL_INTERVAL.min(remaining));
    }
    true
}

pub(super) fn acquire_run_lock_file(path: &Path, kind: Option<TaskKind>) -> Result<RunLockAttempt> {
    let mut holder = None;
    match claim(path, |file| {
        let info = RunLockInfo {
            pid: std::process::id(),
            started_at: Timestamp::now(),
        };
        holder = Some(info);
        serde_json::to_writer(file, &RunLockMetadata { holder: info, kind })
            .map_err(std::io::Error::other)
    })
    .with_context(|| format!("locking loop run lock `{}`", path.display()))?
    {
        LeaseAttempt::Acquired(lease) => Ok(RunLockAttempt::Acquired(RunLockGuard {
            _lease: lease,
            // `claim` runs the body writer before it reports an acquired lease.
            info: holder.expect("acquired lease wrote its holder"),
        })),
        LeaseAttempt::Held(body) => Ok(RunLockAttempt::Held(read_run_lock_info(&body))),
    }
}

fn read_run_lock_info(body: &[u8]) -> Option<RunLockInfo> {
    serde_json::from_slice(body).ok()
}

#[cfg(test)]
mod tests;
