//! Durable room-build upgrade, broker retirement, and structural repair orchestration.
//!
//! `rimz reload` stages the invoking binary, writes it as each live room's
//! durable target, restarts running hook drainers, nudges supervisors,
//! hash-gates Zellij plugin convergence, re-applies tmux room options, mounts a
//! sidebar in each occupied view that has none (docking it beside that view's
//! work panes, closing or replacing no pane, and touching no existing sidebar
//! or other view), and reports the bounded convergence window.
//! `rimz sidebar repair` is the independent structural close/add and daemon-view
//! reopen pass. Standalone stats dashboards and children of non-following
//! supervisors are signalled before workspace enumeration. The rimzd processes
//! follow the committed room
//! target through the shared record watcher. Outdated language-server brokers
//! retire against the PATH build,
//! without interrupting servers or dropping leases, before room enumeration.
//! A workspace whose session is gone has its stale runtime files and
//! leftover daemons swept. Every step is best-effort: a hiccup on one workspace
//! is logged and never blocks the rest.

use std::collections::HashSet;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::diag::record::DiagEvent;
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::harness::hook_drain::{self, RestartOutcome};
use crate::ids::{MuxName, PaneId};
use crate::mux::LiveSessions;
use crate::mux::recovery;
use crate::mux::zellij::RECONCILE_LIST_TIMEOUT;
use crate::mux::{
    MuxBackend, PaneListOptions, PaneListing, PaneReadConsistency, SidebarPaneOptions,
    SidebarWidth, backend_for,
};
use crate::proc::ProcInfo;
use crate::sidebar::timing::{RELOAD_CONVERGE_POLL, RELOAD_CONVERGE_TIMEOUT};
use crate::utils::time::unix_now_ms;
use crate::wakeup::heartbeat::{SidebarHeartbeat, fresh_foreign_protocol_claims};
use crate::workspace::{self, KnownWorkspace, record};

mod report;
pub use report::{MachineProblem, MachineReload, ReloadReport, RoomProblem, RoomReload, RoomState};

/// Immutable executable generation shared by every long-lived process in a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StagedBuild {
    pub path: PathBuf,
    pub build: String,
}

const RECORD_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecordChange {
    Verified(StagedBuild),
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct RecordWatch {
    workspace_id: crate::ids::WorkspaceId,
    pub(crate) record_path: PathBuf,
    last_seen_mtime: Option<SystemTime>,
    next_poll: Instant,
}

impl RecordWatch {
    pub(crate) fn new(workspace_id: &crate::ids::WorkspaceId) -> Self {
        let record_path = StatePaths::for_workspace(workspace_id.clone())
            .map(|paths| paths.workspace_record)
            .unwrap_or_default();
        let last_seen_mtime = record_mtime(&record_path);
        Self {
            workspace_id: workspace_id.clone(),
            record_path,
            last_seen_mtime,
            next_poll: Instant::now() + record_poll_interval(),
        }
    }

    pub(crate) fn poll_if_due(&mut self, now: Instant) -> Option<RecordChange> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + record_poll_interval();
        self.poll_now()
    }

    pub(crate) fn poll_now(&mut self) -> Option<RecordChange> {
        let mtime = record_mtime(&self.record_path);
        if mtime == self.last_seen_mtime {
            return None;
        }
        let target = recorded_reexec_target(&self.workspace_id);
        let change = record_change(self.last_seen_mtime, mtime, target);
        self.last_seen_mtime = mtime;
        change
    }
}

fn record_change(
    prior_mtime: Option<SystemTime>,
    mtime: Option<SystemTime>,
    target: WorkspaceReexecTarget,
) -> Option<RecordChange> {
    if mtime == prior_mtime {
        return None;
    }
    Some(match target {
        WorkspaceReexecTarget::Verified(target) => RecordChange::Verified(target),
        WorkspaceReexecTarget::Absent | WorkspaceReexecTarget::Invalid => RecordChange::Unavailable,
    })
}

fn record_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn record_generation<T>(
    mut mtime: impl FnMut() -> Option<SystemTime>,
    mut read: impl FnMut() -> T,
) -> (Option<SystemTime>, T) {
    loop {
        let before = mtime();
        let value = read();
        if mtime() == before {
            return (before, value);
        }
    }
}

pub(crate) fn record_poll_interval() -> Duration {
    #[cfg(feature = "testkit")]
    if let Some(interval) = std::env::var("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(interval);
    }
    RECORD_POLL_INTERVAL
}

/// Follow verified room-build changes after startup, never an initial mismatch.
pub struct RoomBuildFollower {
    watch: RecordWatch,
    running_build: Option<&'static str>,
    last_verified_build: Option<String>,
}

impl RoomBuildFollower {
    pub fn new(workspace_id: &crate::ids::WorkspaceId) -> Self {
        let mut watch = RecordWatch::new(workspace_id);
        let (mtime, target) = record_generation(
            || record_mtime(&watch.record_path),
            || recorded_reexec_target(workspace_id),
        );
        watch.last_seen_mtime = mtime;
        let last_verified_build = match target {
            WorkspaceReexecTarget::Verified(target) => Some(target.build),
            _ => None,
        };
        Self {
            watch,
            running_build: crate::build_id::current(),
            last_verified_build,
        }
    }

    pub fn poll_if_due(&mut self, now: Instant) -> Option<PathBuf> {
        let RecordChange::Verified(target) = self.watch.poll_if_due(now)? else {
            return None;
        };
        let previous = self.last_verified_build.replace(target.build.clone());
        if previous.as_deref() == Some(target.build.as_str()) {
            return None;
        }
        let running = self.running_build?;
        (target.build != running).then_some(target.path)
    }

    pub fn next_poll(&self) -> Instant {
        self.watch.next_poll
    }

    /// Re-read intent after stopping a child or restoring the terminal. Call only
    /// after a poll yielded a target: shutdown work may outlast another build move.
    pub fn recheck(&mut self) -> Option<PathBuf> {
        self.watch.last_seen_mtime = record_mtime(&self.watch.record_path);
        let running = self.running_build?;
        match recorded_reexec_target(&self.watch.workspace_id) {
            WorkspaceReexecTarget::Verified(target) => {
                self.last_verified_build = Some(target.build.clone());
                (target.build != running).then_some(target.path)
            }
            _ => None,
        }
    }
}

/// Replace this image while preserving its original OS-string arguments.
pub fn exec_current_args(target: &Path) -> io::Error {
    use std::os::unix::process::CommandExt;

    std::process::Command::new(target)
        .args(std::env::args_os().skip(1))
        .exec()
}

const STAGED_BUILD_GC_GRACE: Duration = Duration::from_secs(60);
const REAP_CONFIRM_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum StageBuildErr {
    #[error("the running RimZ executable has no readable on-disk source to stage")]
    MissingSource,
    #[error(
        "the RimZ executable at {path} changed while it was being staged; rerun once the build or install finishes"
    )]
    SourceChanged { path: PathBuf },
    #[error("cannot stage RimZ build at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
}

/// Copy the invoking build into the durable user-scoped build store.
pub(crate) fn stage_current_build() -> Result<StagedBuild, StageBuildErr> {
    let source = current_reexec_target().ok_or(StageBuildErr::MissingSource)?;
    stage_build_under(&source, &crate::disk::paths::rimz_home())
}

fn stage_build_under(source: &Path, state_root: &Path) -> Result<StagedBuild, StageBuildErr> {
    let build = crate::build_id::of_file(source).map_err(|source_err| StageBuildErr::Io {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    let builds_dir = crate::disk::paths::builds_dir_under(state_root);
    let path = builds_dir.join(&build).join("rimz");
    let reusable = path.is_file()
        && crate::build_id::of_file(&path).is_ok_and(|staged_build| staged_build == build);
    if !reusable {
        crate::disk::atomic::copy_executable_atomically(source, &path, |tmp| {
            let copied_build =
                crate::build_id::of_file(tmp).map_err(|source_err| StageBuildErr::Io {
                    path: tmp.to_path_buf(),
                    source: source_err,
                })?;
            if copied_build != build {
                return Err(StageBuildErr::SourceChanged {
                    path: source.to_path_buf(),
                });
            }
            Ok(())
        })?;
    }
    #[cfg(unix)]
    if reusable {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|source_err| {
            StageBuildErr::Io {
                path: path.clone(),
                source: source_err,
            }
        })?;
    }
    sweep_unreferenced_builds(state_root, &builds_dir, &build);
    Ok(StagedBuild { path, build })
}

fn sweep_unreferenced_builds(state_root: &Path, builds_dir: &Path, keep: &str) {
    let mut referenced = HashSet::from([keep.to_owned()]);
    let workspaces = crate::disk::paths::workspaces_dir_under(state_root);
    if let Ok(entries) = fs::read_dir(workspaces) {
        for entry in entries.flatten() {
            let record_path = entry.path().join("workspace.json");
            if let Ok(record) = record::read(&record_path)
                && let Some(build) = record.rimz_build
            {
                referenced.insert(build);
            }
        }
    }
    let Ok(entries) = fs::read_dir(builds_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if referenced.contains(name) {
            continue;
        }
        // ponytail: a short age lease bridges staging and the workspace-record
        // commit; replace it with explicit build leases if registration can
        // ever spend a minute between those two operations.
        if fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(io::Error::other))
            .is_ok_and(|age| age < STAGED_BUILD_GC_GRACE)
        {
            continue;
        }
        if let Err(err) = fs::remove_dir_all(&path) {
            tracing::debug!(path = %path.display(), error = %err, "staged build sweep failed");
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceReexecTarget {
    Absent,
    Verified(StagedBuild),
    Invalid,
}

pub(crate) fn recorded_reexec_target(
    workspace_id: &crate::ids::WorkspaceId,
) -> WorkspaceReexecTarget {
    let Ok(paths) = StatePaths::for_workspace(workspace_id.clone()) else {
        return WorkspaceReexecTarget::Invalid;
    };
    let record = match record::read(&paths.workspace_record) {
        Ok(record) => record,
        Err(record::WorkspaceRecordErr::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            return WorkspaceReexecTarget::Absent;
        }
        Err(_) => return WorkspaceReexecTarget::Invalid,
    };
    resolve_recorded_target(record.rimz_bin, record.rimz_build)
}

fn resolve_recorded_target(path: Option<PathBuf>, build: Option<String>) -> WorkspaceReexecTarget {
    let (path, build) = match (path, build) {
        (Some(path), Some(build)) => (path, build),
        (_, None) => return WorkspaceReexecTarget::Absent,
        (None, Some(_)) => return WorkspaceReexecTarget::Invalid,
    };
    match crate::build_id::of_file(&path) {
        Ok(actual) if actual == build => {
            WorkspaceReexecTarget::Verified(StagedBuild { path, build })
        }
        Ok(_) | Err(_) => WorkspaceReexecTarget::Invalid,
    }
}

pub(crate) fn reexec_target_for_workspace(
    workspace_id: &crate::ids::WorkspaceId,
) -> Option<PathBuf> {
    match recorded_reexec_target(workspace_id) {
        WorkspaceReexecTarget::Verified(target) => Some(target.path),
        WorkspaceReexecTarget::Absent => current_reexec_target(),
        WorkspaceReexecTarget::Invalid => None,
    }
}

/// Resolve the on-disk binary that should be executed after the current image
/// may have been replaced by an atomic install.
pub fn current_reexec_target() -> Option<PathBuf> {
    resolve_reexec_target(std::env::current_exe().ok()?)
}

/// Pick the live binary behind a `current_exe()` reading.
///
/// A fresh `cargo install` replaces our binary via atomic rename, which unlinks
/// the inode the running process still holds. The kernel then annotates
/// `/proc/self/exe` (what `current_exe()` reads) with a trailing " (deleted)",
/// so the raw path no longer resolves on disk. The replacement now lives at the
/// un-annotated path, so strip that marker and prefer whichever path is a real
/// file. `None` means neither path exists, such as during a partial install.
fn resolve_reexec_target(exe: PathBuf) -> Option<PathBuf> {
    crate::proc::resolve_existing_or_replacement(&exe)
}

/// Maintenance counters for one room, also summed by standalone sidebar repair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Live sessions reconciled.
    pub sessions: usize,
    /// Zellij sessions whose presence probe was silent or provably stale.
    pub presence_dead: usize,
    /// Sessions whose structural pass could not acquire the reconcile lock.
    pub busy: usize,
    /// Zellij sessions whose fresh plugin writer already matches the desired
    /// wasm and load configuration.
    pub plugin_current: usize,
    /// Zellij sessions whose plugin convergence completed this pass.
    pub plugin_upgraded: usize,
    /// Zellij sessions whose matching writer was retained while stale loaded
    /// plugin ids were retired.
    pub plugin_reconciled: usize,
    /// Located sidebars already publishing the on-disk build before reload.
    pub already_current: usize,
    /// Located sidebars that published the on-disk build after the reload signal.
    pub reexeced: usize,
    /// Located sidebars that did not publish the staged build before timeout.
    pub unconverged: usize,
    /// Live sidebars whose build could not be verified.
    pub unverified: usize,
    /// Sidebars added (a view had none, or its only one was unresponsive).
    pub recovered: usize,
    /// Duplicate or unresponsive sidebar panes closed.
    pub closed: usize,
    /// Orphaned sidebar processes (pane gone) reaped.
    pub reaped: usize,
    /// Hook drainers stopped for replacement on the room's current build.
    pub drainers_restarted: usize,
    /// Hook drainers whose frame-boundary stop failed.
    pub drainers_stop_failed: usize,
    /// Views whose sidebar add or repair could not complete this pass.
    pub failed: usize,
    /// Views whose in-place add or geometry repair was deferred (no attached
    /// client).
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
    /// Working sidebar panes, added by bare reload's mount or kept or added by
    /// repair, that remain outside the verified full-height left dock after the
    /// bounded docking path.
    pub misdocked: usize,
    /// Live sessions with no fresh sidebar heartbeat of any protocol after bare
    /// reload's mount pass: panes between beats, which settle on their own.
    pub sidebar_silent: usize,
    /// Live sessions whose only fresh sidebar heartbeats carry a protocol this
    /// build cannot signal.
    pub sidebar_foreign_protocol: usize,
    /// Sessions whose wholly closed daemon view was reopened.
    pub daemon_reopened: usize,
    /// Sessions whose daemon view reopen awaits an attached client.
    pub daemon_reopen_deferred: usize,
    /// Sessions whose daemon view could not be reopened.
    pub daemon_reopen_failed: usize,
}

impl ReloadOutcome {
    fn merge(&mut self, delta: Self) {
        let Self {
            sessions,
            presence_dead,
            busy,
            plugin_current,
            plugin_upgraded,
            plugin_reconciled,
            already_current,
            reexeced,
            unconverged,
            unverified,
            recovered,
            closed,
            reaped,
            drainers_restarted,
            drainers_stop_failed,
            failed,
            deferred,
            redocked,
            misdocked,
            sidebar_silent,
            sidebar_foreign_protocol,
            daemon_reopened,
            daemon_reopen_deferred,
            daemon_reopen_failed,
        } = delta;
        self.sessions += sessions;
        self.presence_dead += presence_dead;
        self.busy += busy;
        self.plugin_current += plugin_current;
        self.plugin_upgraded += plugin_upgraded;
        self.plugin_reconciled += plugin_reconciled;
        self.already_current += already_current;
        self.reexeced += reexeced;
        self.unconverged += unconverged;
        self.unverified += unverified;
        self.recovered += recovered;
        self.closed += closed;
        self.reaped += reaped;
        self.drainers_restarted += drainers_restarted;
        self.drainers_stop_failed += drainers_stop_failed;
        self.failed += failed;
        self.deferred += deferred;
        self.redocked += redocked;
        self.misdocked += misdocked;
        self.sidebar_silent += sidebar_silent;
        self.sidebar_foreign_protocol += sidebar_foreign_protocol;
        self.daemon_reopened += daemon_reopened;
        self.daemon_reopen_deferred += daemon_reopen_deferred;
        self.daemon_reopen_failed += daemon_reopen_failed;
    }
}

/// Publish the invoking build as durable intent, nudge every live sidebar
/// toward it, and, without `repair`, mount a sidebar in each occupied view that
/// has none, docked beside that view's work panes, closing or replacing no pane
/// and touching no existing sidebar or other view. Retire outdated
/// language-server brokers without interrupting their servers or dropping
/// leases. Optional structural repair runs before returning each room's report.
pub fn reload_user_sidebars(repair: bool) -> Result<ReloadReport, StageBuildErr> {
    let staged = stage_current_build()?;
    let brokers = crate::lsp::retire::run();
    let mut result = ReloadReport {
        machine: MachineReload {
            stats_reloaded: recovery::reload_stats_dashboards().len(),
            brokers_retired: brokers.retired,
            brokers_marked: brokers.marked,
            brokers_kept: brokers.kept,
            brokers_unchecked: brokers.unchecked,
            ..MachineReload::default()
        },
        ..ReloadReport::default()
    };
    let (live_targets, dead_swept) = live_targets(true);
    result.machine.dead_swept = dead_swept;
    let machine_config = MachineConfig::load_lenient();
    // Claimed sessions are independent: each target owns its mux server
    // round-trips and filters heartbeats by `(mux, session_name)`. Shared
    // workspace wakeup fanout and orphan-runtime sweeps are idempotent,
    // best-effort, and tolerate races.
    std::thread::scope(|scope| {
        let handles: Vec<_> = live_targets
            .iter()
            .map(|target| {
                let staged = &staged;
                let machine_config = &machine_config;
                scope.spawn(move || {
                    let mut room = upgrade_live(target, staged, machine_config, !repair);
                    if repair {
                        let repair = repair_live(target, machine_config);
                        let heartbeats = session_heartbeats(
                            &target.runtime,
                            target.mux,
                            &target.workspace.session_name,
                        );
                        room.record_repair(
                            repair.outcome,
                            repair.presence,
                            &heartbeats,
                            &staged.build,
                        );
                    }
                    room
                })
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(room) => result.rooms.push(room),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });

    Ok(result)
}

/// Repair sidebar structure and reopen closed daemon views without publishing
/// a new build target.
pub fn repair_user_sidebars() -> ReloadReport {
    let (live_targets, _) = live_targets(false);
    let machine_config = MachineConfig::load_lenient();
    let mut result = ReloadReport::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = live_targets
            .iter()
            .map(|target| {
                let machine_config = &machine_config;
                scope.spawn(move || {
                    let mut room = room_result(target);
                    room.repair = Some(repair_live(target, machine_config).outcome);
                    room
                })
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(room) => result.rooms.push(room),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    result
}

struct LiveTarget {
    mux: MuxName,
    workspace: KnownWorkspace,
    runtime: RuntimePaths,
}

fn room_result(target: &LiveTarget) -> RoomReload {
    RoomReload {
        project_root: target.workspace.project_root.clone(),
        mux: target.mux,
        session_name: target.workspace.session_name.clone(),
        outcome: ReloadOutcome::default(),
        sidebars: 0,
        current_sidebars: 0,
        record_error: None,
        options_error: None,
        plugin_error: None,
        claude_restarts: false,
        pending: crate::mux::room_settings::PendingSettings::Tracked(Vec::new()),
        repair: None,
    }
}

fn live_targets(sweep_dead: bool) -> (Vec<LiveTarget>, usize) {
    let workspaces = match workspace::known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(
                tags.operation = "reload.enumerate_workspaces",
                error = &err as &dyn std::error::Error,
                "sidebar maintenance: cannot enumerate workspaces",
            );
            return (Vec::new(), 0);
        }
    };
    let live = LiveSessions::probe();
    let mut claimed = HashSet::new();
    let mut targets = Vec::new();
    let mut dead_swept = 0;
    for ws in workspaces {
        let runtime = match RuntimePaths::for_project_root(&ws.project_root) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(
                    workspace = %ws.workspace_id,
                    tags.operation = "reload.runtime_paths",
                    error = &err as &dyn std::error::Error,
                    "sidebar maintenance: runtime paths",
                );
                continue;
            }
        };
        let Some(mux) = live.mux_of(&ws.session_name) else {
            if sweep_dead {
                // A best-effort session probe can misread a live room, so sweep
                // only respawnable daemons and runtime hints, never mux servers.
                crate::harness::auto_gc::sweep_runtime_claims(&runtime);
                dead_swept += recovery::sweep_orphan_processes(
                    ws.workspace_id.as_str(),
                    &ws.session_name,
                    recovery::SweepScope::LivenessProbe,
                )
                .signalled
                .len();
            }
            continue;
        };
        if !claim_live_session(&mut claimed, mux, &ws.session_name) {
            tracing::debug!(
                session = %ws.session_name,
                workspace = %ws.workspace_id,
                "sidebar maintenance: skipping duplicate workspace record for a claimed session",
            );
            continue;
        }
        targets.push(LiveTarget {
            mux,
            workspace: ws,
            runtime,
        });
    }
    (targets, dead_swept)
}

fn claim_live_session(
    seen: &mut HashSet<(MuxName, String)>,
    mux: MuxName,
    session_name: &str,
) -> bool {
    seen.insert((mux, session_name.to_owned()))
}

/// Publish and nudge one live session, mount its missing sidebars when `mount`,
/// then reap owners whose panes are gone.
fn upgrade_live(
    target: &LiveTarget,
    staged: &StagedBuild,
    machine_config: &MachineConfig,
    mount: bool,
) -> RoomReload {
    let LiveTarget {
        mux,
        workspace: ws,
        runtime,
    } = target;
    let mut outcome = ReloadOutcome {
        sessions: 1,
        ..ReloadOutcome::default()
    };
    let mut room = room_result(target);
    let backend = backend_for(*mux);
    let recorded = record_live_room_bin(ws, runtime, staged, machine_config);
    if recorded.is_ok()
        && let Ok(paths) = StatePaths::for_project_root(&ws.project_root)
        && let Some(store) = crate::store::Store::open_existing(paths, runtime.clone())
    {
        match hook_drain::restart(&store) {
            Ok(RestartOutcome::NotRunning) => {}
            Ok(RestartOutcome::Restarted) => outcome.drainers_restarted += 1,
            Err(err) => {
                outcome.drainers_stop_failed += 1;
                tracing::warn!(
                    session = %ws.session_name,
                    error = %err,
                    "reload: stopping hook drainer failed",
                );
            }
        }
    }
    let room_bin = match recorded {
        Ok(recorded) => {
            room.claude_restarts = recorded.claude_restarts;
            recorded.bin
        }
        Err(error) => {
            room.record_error = Some(error.to_string());
            staged.path.clone()
        }
    };
    if *mux == MuxName::Tmux {
        crate::mux::register_room_keys(backend.as_ref(), machine_config, &room_bin);
    }
    let before_signal = session_heartbeats(runtime, *mux, &ws.session_name);

    // 1. Signal live sidebars to re-exec onto the freshly-installed binary.
    match crate::wakeup::reload_all(runtime) {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.signal_sidebars",
                error = &err as &dyn std::error::Error,
                "reload: signaling sidebars failed",
            );
        }
    }

    let build = staged.build.as_str();
    outcome.already_current += current_located_count(&before_signal, build);
    let awaiting = awaiting_panes(&before_signal, build);
    if !awaiting.is_empty() {
        wait_for_convergence(runtime, *mux, &ws.session_name, &awaiting, build);
    }
    // 2. Converge the session's presence plugin onto the current wasm — reload
    //    is the explicit upgrade verb. Stale instances retire only after the
    //    replacement publishes topology from its new writer generation; a
    //    detached or degraded session keeps its prior plugin and retries on a
    //    later reload. Best-effort like every step here.
    if *mux == MuxName::Zellij {
        let upgrade = presence_options(ws, room_bin, machine_config).and_then(|presence| {
            crate::mux::ZellijBackend::default()
                .upgrade_presence_plugin(&presence)
                .map_err(|err| err.to_string())
        });
        match upgrade {
            Ok(crate::mux::zellij::PresenceUpgrade::Current) => outcome.plugin_current += 1,
            Ok(crate::mux::zellij::PresenceUpgrade::Reconciled) => outcome.plugin_reconciled += 1,
            Ok(crate::mux::zellij::PresenceUpgrade::Upgraded) => outcome.plugin_upgraded += 1,
            Err(err) => {
                room.plugin_error = Some(err.clone());
                tracing::warn!(
                    session = %ws.session_name,
                    tags.operation = "reload.presence_converge",
                    error = %err,
                    "reload: presence plugin convergence failed",
                );
            }
        }
    }

    if *mux == MuxName::Tmux
        && let Err(err) = crate::mux::TmuxBackend::default()
            .apply_room_options(&ws.session_name, &MultiplexerConfig::from(machine_config))
    {
        room.options_error = Some(err.to_string());
        tracing::warn!(
            session = %ws.session_name,
            tags.operation = "reload.tmux_options",
            error = &err as &dyn std::error::Error,
            "reload: applying tmux room options failed",
        );
    }

    if mount {
        mount_missing_sidebars(target, backend.as_ref(), machine_config, &mut outcome);
    }

    // Reap orphan sidebar processes whose pane is gone — the mux cannot close a
    // pane that no longer exists, so a wedged renderer would otherwise linger.
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), *mux, ws);

    // Sweep runtime files whose owner is gone — stale heartbeats and ownerless
    // sockets accumulate in a live session too (every SIGKILLed or reaped
    // renderer leaves a pair), and the sweep already spares anything fresh or
    // still starting.
    crate::harness::auto_gc::sweep_runtime_claims(runtime);
    room.pending = crate::mux::room_settings::pending(runtime, machine_config, *mux);
    room.outcome = outcome;
    // Read after the mount pass and the reap, so a mounted sidebar is counted
    // and neither one's duration ages the read.
    let heartbeats = session_heartbeats(runtime, *mux, &ws.session_name);
    let foreign_protocol =
        heartbeats.is_empty() && foreign_protocol_heartbeat(runtime, *mux, &ws.session_name);
    room.record_upgrade(&awaiting, &heartbeats, build, foreign_protocol);
    room
}

/// Bare reload's mount pass: a sidebar for each occupied view that has none,
/// docked beside that view's work panes, with no pane closed or replaced, no
/// existing sidebar or other view touched, and no session reborn.
fn mount_missing_sidebars(
    target: &LiveTarget,
    backend: &dyn MuxBackend,
    machine_config: &MachineConfig,
    outcome: &mut ReloadOutcome,
) {
    let Some(rimz_bin) = reexec_target_for_workspace(&target.workspace.workspace_id) else {
        // No view is known to lack a sidebar, and `--repair` fails the same
        // way; the room's record problem carries the fix.
        tracing::warn!(
            session = target.workspace.session_name.as_str(),
            tags.operation = "reload.mount_sidebars",
            "reload: no verified room build to mount sidebars with",
        );
        return;
    };
    let opts = sidebar_pane_options(target, machine_config, rimz_bin);
    record_mount(
        outcome,
        &target.workspace.session_name,
        crate::sidebar::mount_missing_sidebars(backend, &opts, RELOAD_MOUNT_LOCK_WAIT),
    );
}

/// Another pass holding the room's reconcile lock this long is reported busy.
const RELOAD_MOUNT_LOCK_WAIT: Duration = Duration::from_secs(2);

fn record_mount(
    outcome: &mut ReloadOutcome,
    session: &str,
    result: Result<Option<crate::mux::SidebarRecovery>, crate::sidebar::SidebarReconcileErr>,
) {
    match result {
        Ok(Some(report)) => {
            outcome.recovered += report.recovered;
            outcome.failed += report.failed;
            outcome.deferred += report.deferred_adds;
            outcome.misdocked += report.misdocked;
        }
        Ok(None) => outcome.busy += 1,
        Err(err) => {
            outcome.failed += 1;
            tracing::warn!(
                session,
                tags.operation = "reload.mount_sidebars",
                error = &err as &dyn std::error::Error,
                "reload: mounting missing sidebars failed",
            );
        }
    }
}

fn sidebar_pane_options(
    target: &LiveTarget,
    machine_config: &MachineConfig,
    rimz_bin: PathBuf,
) -> SidebarPaneOptions {
    let LiveTarget {
        workspace: ws,
        runtime,
        ..
    } = target;
    let width = SidebarWidth::from_config(&machine_config.theme);
    SidebarPaneOptions {
        runtime: runtime.clone(),
        session_name: ws.session_name.clone(),
        workspace_id: ws.workspace_id.clone(),
        project_root: ws.project_root.clone(),
        extra_env: crate::agents::registry::room_env(runtime),
        cwd: ws.project_root.clone(),
        target: crate::mux::width_target::resolve(runtime, width, None),
        max_percent: width.max_percent,
        detected_view_size: None,
        rimz_bin,
        pristine_birth: false,
        config: MultiplexerConfig::from(machine_config),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

fn presence_options(
    ws: &KnownWorkspace,
    rimz_bin: PathBuf,
    config: &MachineConfig,
) -> Result<crate::mux::PresencePluginOptions, String> {
    let wasm = crate::mux::zellij::ensure_presence_plugin_artifact()
        .ok_or_else(|| "presence plugin artifact unavailable".to_owned())?;
    Ok(crate::mux::PresencePluginOptions::from_config(
        &ws.session_name,
        &ws.workspace_id,
        wasm,
        rimz_bin,
        config,
        &ws.project_root,
    ))
}

struct RepairResult {
    outcome: ReloadOutcome,
    presence: Option<Result<(), String>>,
}

fn repair_live(target: &LiveTarget, machine_config: &MachineConfig) -> RepairResult {
    let LiveTarget {
        mux,
        workspace: ws,
        runtime,
    } = target;
    let mut outcome = ReloadOutcome {
        sessions: 1,
        ..ReloadOutcome::default()
    };
    let Some(rimz_bin) = reexec_target_for_workspace(&ws.workspace_id) else {
        outcome.failed += 1;
        return RepairResult {
            outcome,
            presence: None,
        };
    };
    let backend = backend_for(*mux);

    let presence = if *mux == MuxName::Zellij {
        Some(
            presence_options(
                ws,
                StatePaths::for_project_root(&ws.project_root)
                    .map(|paths| paths.room_bin)
                    .unwrap_or_else(|_| rimz_bin.clone()),
                machine_config,
            )
            .and_then(|presence| {
                backend
                    .ensure_presence_plugin(&presence)
                    .map_err(|err| err.to_string())
            }),
        )
    } else {
        None
    };
    if let Some(Err(err)) = &presence {
        tracing::warn!(
            session = %ws.session_name,
            tags.operation = "sidebar.repair.presence_ensure",
            error = %err,
            "sidebar repair: presence plugin ensure failed",
        );
    }
    let presence_probe = if *mux == MuxName::Zellij {
        Some(crate::mux::ZellijBackend::default().probe_presence(
            runtime,
            &ws.session_name,
            RELOAD_PRESENCE_PROBE_TIMEOUT,
        ))
    } else {
        None
    };
    if let Some(crate::mux::zellij::PresenceProbe::Stale {
        ghost_panes,
        produced_at_ms,
        observed_at_ms,
        writer,
    }) = &presence_probe
    {
        crate::diag::DiagSink::for_workspace(ws.workspace_id.clone(), &ws.session_name, None).emit(
            DiagEvent::PresenceRosterStale {
                session_name: ws.session_name.clone(),
                ghost_panes: ghost_panes.clone(),
                produced_at_ms: *produced_at_ms,
                observed_at_ms: *observed_at_ms,
                plugin_id: writer.as_ref().map(|writer| writer.plugin_id),
                loaded_at_ms: writer.as_ref().map(|writer| writer.loaded_at_ms),
            },
        );
    }

    let opts = sidebar_pane_options(target, machine_config, rimz_bin);
    outcome = reconcile_repair(&ws.session_name, presence_probe.as_ref(), || {
        crate::sidebar::reconcile_session_sidebars(backend.as_ref(), &opts, Duration::from_secs(60))
    });
    // The reopen takes the same session lock, so a busy reconcile would only
    // spend a second full wait on it.
    if outcome.busy == 0 {
        reopen_closed_daemon_view(backend.as_ref(), &opts, machine_config, &mut outcome);
    }
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), *mux, ws);
    crate::harness::auto_gc::sweep_runtime_claims(runtime);
    RepairResult { outcome, presence }
}

fn reopen_closed_daemon_view(
    backend: &dyn MuxBackend,
    opts: &SidebarPaneOptions,
    machine_config: &MachineConfig,
    outcome: &mut ReloadOutcome,
) {
    let resolve = || {
        let record = StatePaths::for_workspace(opts.workspace_id.clone())
            .map_err(record::WorkspaceRecordErr::from)
            .and_then(|state| record::read(&state.workspace_record))
            .inspect_err(|error| {
                tracing::warn!(
                    session = %opts.session_name,
                    %error,
                    "daemon reopen record unavailable",
                );
            })
            .ok()?;
        let rimz_bin = reexec_target_for_workspace(&opts.workspace_id).or_else(|| {
            tracing::warn!(session = %opts.session_name, "daemon reopen build unavailable");
            None
        })?;
        Some((record, rimz_bin))
    };
    match crate::daemon_view::reopen_daemon_view(backend, opts, resolve, machine_config) {
        crate::daemon_view::ReopenOutcome::Reopened => outcome.daemon_reopened = 1,
        crate::daemon_view::ReopenOutcome::None => {}
        crate::daemon_view::ReopenOutcome::Deferred => outcome.daemon_reopen_deferred = 1,
        crate::daemon_view::ReopenOutcome::Busy => outcome.busy = 1,
        crate::daemon_view::ReopenOutcome::Failed => outcome.daemon_reopen_failed = 1,
    }
}

const RELOAD_PRESENCE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn reconcile_repair(
    session: &str,
    presence: Option<&crate::mux::zellij::PresenceProbe>,
    reconcile: impl FnOnce() -> Result<
        Option<crate::mux::SidebarRecovery>,
        crate::sidebar::SidebarReconcileErr,
    >,
) -> ReloadOutcome {
    let mut outcome = ReloadOutcome {
        sessions: 1,
        presence_dead: usize::from(matches!(
            presence,
            Some(
                crate::mux::zellij::PresenceProbe::Silent
                    | crate::mux::zellij::PresenceProbe::Stale { .. }
            )
        )),
        ..Default::default()
    };
    match reconcile() {
        Ok(Some(report)) => {
            outcome.recovered = report.recovered;
            outcome.closed = report.closed;
            outcome.failed = report.failed;
            outcome.deferred = report.deferred;
            outcome.redocked = report.redocked;
            outcome.misdocked = report.misdocked;
        }
        Ok(None) => outcome.busy = 1,
        Err(err) => {
            outcome.failed = 1;
            tracing::warn!(
                session,
                tags.operation = "sidebar.repair.reconcile",
                error = &err as &dyn std::error::Error,
                "sidebar repair: structural pass failed",
            );
        }
    }
    outcome
}

#[derive(Debug, thiserror::Error)]
enum RecordRoomErr {
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Record(#[from] record::WorkspaceRecordErr),
    #[error("room store unavailable for {0}")]
    Unavailable(crate::ids::WorkspaceId),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
}

struct RecordedRoom {
    bin: PathBuf,
    claude_restarts: bool,
}

fn record_live_room_bin(
    ws: &KnownWorkspace,
    runtime: &RuntimePaths,
    staged: &StagedBuild,
    config: &MachineConfig,
) -> Result<RecordedRoom, RecordRoomErr> {
    let paths = StatePaths::for_project_root(&ws.project_root)?;
    record_live_room_bin_at(ws, staged, &paths, runtime, config)
}

fn record_live_room_bin_at(
    ws: &KnownWorkspace,
    staged: &StagedBuild,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    config: &MachineConfig,
) -> Result<RecordedRoom, RecordRoomErr> {
    use crate::agents::runtime_control::{RuntimeControlLiveness, host_liveness};
    use crate::remote_control::{HostLoginEnvs, RemoteControlHost};

    let record = record::read(&paths.workspace_record)?;
    let enabled =
        crate::remote_control::room_config(&config.remote_control, &record).enabled_for("claude");
    let liveness = if enabled && record.rimz_build.as_deref() != Some(staged.build.as_str()) {
        let envs = HostLoginEnvs::for_room(&paths.workspace_record, config).ok();
        envs.as_ref()
            .and_then(|envs| envs.for_host(RemoteControlHost::Claude).ok())
            .map_or(RuntimeControlLiveness::Unknown, |env| {
                host_liveness("claude", &ws.project_root, env)
            })
    } else {
        RuntimeControlLiveness::Unknown
    };
    let claude_restarts = report::claude_restart(
        enabled,
        record.rimz_build.as_deref(),
        &staged.build,
        liveness,
    );
    let workspace = crate::workspace::ResolvedWorkspace {
        workspace_id: record.workspace_id,
        project_root: record.project_root.clone(),
        cwd_project_root: None,
        root_class: record.root_class,
        worktree_root: record.worktree_root.unwrap_or(record.project_root),
        worktree_branch: None,
        session_name: record.session_name,
        mux_hint: None,
    };
    let store = crate::store::Store::open_existing(paths.clone(), runtime.clone())
        .ok_or_else(|| RecordRoomErr::Unavailable(ws.workspace_id.clone()))?;
    store.record_room_bin(&workspace, staged.path.clone(), staged.build.clone())?;
    Ok(RecordedRoom {
        bin: paths.room_bin.clone(),
        claude_restarts,
    })
}

fn session_heartbeats(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
) -> Vec<SidebarHeartbeat> {
    crate::wakeup::heartbeat::fresh_sidebar_heartbeats(runtime)
        .into_iter()
        .filter(|heartbeat| heartbeat.mux == mux && heartbeat.session_name == session_name)
        .collect()
}

/// Whether a fresh heartbeat for this session carries a protocol the
/// current-protocol readers drop: a renderer this build cannot signal.
fn foreign_protocol_heartbeat(runtime: &RuntimePaths, mux: MuxName, session_name: &str) -> bool {
    fresh_foreign_protocol_claims(runtime)
        .into_iter()
        .any(|claim| {
            claim.mux.is_none_or(|claimed| claimed == mux.as_str())
                && claim
                    .session_name
                    .is_none_or(|claimed| claimed == session_name)
        })
}

fn current_located_count(heartbeats: &[SidebarHeartbeat], build: &str) -> usize {
    current_build_claims(heartbeats, build).len()
}

fn unlocated_unverified_count(heartbeats: &[SidebarHeartbeat], build: &str) -> usize {
    heartbeats
        .iter()
        .filter(|hb| hb.pane_id.is_none() && hb.build.as_deref() != Some(build))
        .count()
}

fn awaiting_panes(heartbeats: &[SidebarHeartbeat], build: &str) -> HashSet<PaneId> {
    heartbeats
        .iter()
        .filter(|hb| hb.build.as_deref() != Some(build))
        .filter_map(|hb| hb.pane_id.clone())
        .collect()
}

fn wait_for_convergence(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
    awaiting: &HashSet<PaneId>,
    build: &str,
) -> Vec<SidebarHeartbeat> {
    wait_for_convergence_with(
        awaiting,
        build,
        RELOAD_CONVERGE_TIMEOUT,
        RELOAD_CONVERGE_POLL,
        || session_heartbeats(runtime, mux, session_name),
    )
}

fn wait_for_convergence_with(
    awaiting: &HashSet<PaneId>,
    build: &str,
    timeout: Duration,
    poll: Duration,
    mut read_heartbeats: impl FnMut() -> Vec<SidebarHeartbeat>,
) -> Vec<SidebarHeartbeat> {
    let deadline = Instant::now() + timeout;
    loop {
        let heartbeats = read_heartbeats();
        let current = current_build_claims(&heartbeats, build);
        if awaiting.is_subset(&current) || Instant::now() >= deadline {
            return heartbeats;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(poll));
    }
}

fn current_build_claims(heartbeats: &[SidebarHeartbeat], build: &str) -> HashSet<PaneId> {
    heartbeats
        .iter()
        .filter(|hb| hb.build.as_deref() == Some(build))
        .filter_map(|hb| hb.pane_id.clone())
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReapCandidate {
    pid: u32,
    pane: PaneId,
}

#[derive(Debug)]
struct ReapConfirmation {
    confirmed: Vec<ReapCandidate>,
    spared: Vec<ReapCandidate>,
    first_panes: HashSet<PaneId>,
    first_observed_at_ms: u64,
    second_observed_at_ms: u64,
}

struct ReapCandidateInputs<'a> {
    procs: &'a [ProcInfo],
    my_uid: u32,
    protected: &'a HashSet<u32>,
    mux: MuxName,
    workspace: &'a KnownWorkspace,
    positive_panes: &'a HashSet<PaneId>,
    now: jiff::Timestamp,
}

fn assemble_reap_candidates(
    inputs: ReapCandidateInputs<'_>,
    mut process_start: impl FnMut(u32) -> Option<jiff::Timestamp>,
    mut attributed_pane: impl FnMut(u32, MuxName) -> Option<PaneId>,
    mut same_domain: impl FnMut(u32) -> bool,
) -> Vec<ReapCandidate> {
    let mut candidates = Vec::new();
    for proc in inputs.procs {
        if proc.real_uid != inputs.my_uid
            || inputs.protected.contains(&proc.pid)
            || !recovery::is_sidebar_serve(
                &proc.cmdline,
                inputs.workspace.workspace_id.as_str(),
                &inputs.workspace.session_name,
            )
            || !same_domain(proc.pid)
            || process_start(proc.pid).is_some_and(|start| {
                crate::sidebar::born_recently(start, inputs.now, crate::sidebar::FRESH_PANE_GRACE)
            })
        {
            continue;
        }
        let Some(pane) = attributed_pane(proc.pid, inputs.mux) else {
            continue;
        };
        if !inputs.positive_panes.contains(&pane) {
            candidates.push(ReapCandidate {
                pid: proc.pid,
                pane,
            });
        }
    }
    candidates
}

fn partition_confirmed(
    candidates: Vec<ReapCandidate>,
    first_panes: &HashSet<PaneId>,
    second_panes: &HashSet<PaneId>,
) -> (Vec<ReapCandidate>, Vec<ReapCandidate>) {
    candidates.into_iter().partition(|candidate| {
        !first_panes.contains(&candidate.pane) && !second_panes.contains(&candidate.pane)
    })
}

fn confirm_reap_candidates<E>(
    candidates: Vec<ReapCandidate>,
    mut list_panes: impl FnMut() -> std::result::Result<PaneListing, E>,
    pause: impl FnOnce(),
) -> std::result::Result<ReapConfirmation, E> {
    let first = list_panes()?;
    let first_observed_at_ms = first.observed_at_ms;
    let first_panes = first
        .panes
        .into_iter()
        .map(|pane| pane.pane_id)
        .collect::<HashSet<_>>();
    pause();
    let second = list_panes()?;
    let second_observed_at_ms = second.observed_at_ms;
    let second_panes = second
        .panes
        .into_iter()
        .map(|pane| pane.pane_id)
        .collect::<HashSet<_>>();
    let (confirmed, spared) = partition_confirmed(candidates, &first_panes, &second_panes);
    Ok(ReapConfirmation {
        confirmed,
        spared,
        first_panes,
        first_observed_at_ms,
        second_observed_at_ms,
    })
}

fn pane_cache_divergence_events(
    spared: &[ReapCandidate],
    cache_observed_at_ms: Option<u64>,
    first_panes: &HashSet<PaneId>,
    first_observed_at_ms: u64,
    second_observed_at_ms: u64,
) -> Vec<DiagEvent> {
    spared
        .iter()
        .map(|candidate| DiagEvent::PaneCacheDivergence {
            pane_id: candidate.pane.to_string(),
            pid: candidate.pid as i32,
            cache_observed_at_ms,
            authoritative_observed_at_ms: if first_panes.contains(&candidate.pane) {
                first_observed_at_ms
            } else {
                second_observed_at_ms
            },
        })
        .collect()
}

fn sidebar_orphan_reaped_events(
    confirmed: &[ReapCandidate],
    outcome: &recovery::KillOutcome,
    first_confirmed_at_ms: u64,
    second_confirmed_at_ms: u64,
) -> Vec<DiagEvent> {
    let sigkilled = outcome.sigkilled.iter().copied().collect::<HashSet<_>>();
    confirmed
        .iter()
        .map(|candidate| DiagEvent::SidebarOrphanReaped {
            pane_id: candidate.pane.to_string(),
            pid: candidate.pid as i32,
            first_confirmed_at_ms,
            second_confirmed_at_ms,
            sigkilled: sigkilled.contains(&candidate.pid),
        })
        .collect()
}

/// SIGTERM→SIGKILL this user's sidebar *processes* for `ws` only after proving
/// they share this room's mux endpoint namespace and two authoritative mux
/// rosters omit their panes. Foreign or unreadable environments and processes
/// we cannot attribute to a pane are left alone; any authoritative failure
/// aborts the reap.
fn reap_orphan_sidebars(backend: &dyn MuxBackend, mux: MuxName, ws: &KnownWorkspace) -> usize {
    let now = jiff::Timestamp::now();
    let floor_ms =
        unix_now_ms().saturating_sub(crate::sidebar::FRESH_PANE_GRACE.as_millis() as u64);
    // The cache is positive liveness evidence only. An omission nominates a
    // candidate for authoritative confirmation; it never licenses a kill.
    let (positive_panes, cache_observed_at_ms) = match backend.list_panes(PaneListOptions {
        session_name: Some(ws.session_name.clone()),
        runtime_paths: None,
        workspace_id: Some(ws.workspace_id.clone()),
        min_topology_produced_at_ms: Some(floor_ms),
        consistency: PaneReadConsistency::Cached,
        command_timeout: Some(RECONCILE_LIST_TIMEOUT),
    }) {
        Ok(listing) => (
            listing.panes.into_iter().map(|pane| pane.pane_id).collect(),
            Some(listing.observed_at_ms),
        ),
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.reap_list_panes",
                error = &err as &dyn std::error::Error,
                "reload: pane cache listing for orphan reap failed; escalating to mux truth",
            );
            (HashSet::new(), None)
        }
    };
    let procs = crate::proc::list_processes();
    let protected = recovery::protected_pids(&procs, std::process::id());
    let own_domain = crate::mux::domain::ProcessDomain::current();
    let candidates = assemble_reap_candidates(
        ReapCandidateInputs {
            procs: &procs,
            my_uid: recovery::current_uid(),
            protected: &protected,
            mux,
            workspace: ws,
            positive_panes: &positive_panes,
            now,
        },
        crate::proc::process_start,
        recovery::attributed_pane,
        |pid| own_domain.same_mux_endpoint_as_process(pid, mux),
    );
    if candidates.is_empty() {
        return 0;
    }

    let confirmation = match confirm_reap_candidates(
        candidates,
        || {
            backend.list_panes(PaneListOptions {
                session_name: Some(ws.session_name.clone()),
                workspace_id: Some(ws.workspace_id.clone()),
                consistency: PaneReadConsistency::RequireAuthoritative,
                command_timeout: Some(RECONCILE_LIST_TIMEOUT),
                ..Default::default()
            })
        },
        || std::thread::sleep(REAP_CONFIRM_DELAY),
    ) {
        Ok(confirmation) => confirmation,
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.reap_list_panes",
                error = &err as &dyn std::error::Error,
                "reload: authoritative pane confirmation failed; skipping orphan reap",
            );
            return 0;
        }
    };

    let diag = crate::diag::DiagSink::for_workspace(
        ws.workspace_id.clone(),
        ws.session_name.clone(),
        None,
    );
    for event in pane_cache_divergence_events(
        &confirmation.spared,
        cache_observed_at_ms,
        &confirmation.first_panes,
        confirmation.first_observed_at_ms,
        confirmation.second_observed_at_ms,
    ) {
        diag.emit(event);
    }

    let confirmed_pids = confirmation
        .confirmed
        .iter()
        .map(|candidate| candidate.pid)
        .collect::<Vec<_>>();
    let outcome = recovery::kill_pids(&confirmed_pids, recovery::SWEEP_GRACE);
    for event in sidebar_orphan_reaped_events(
        &confirmation.confirmed,
        &outcome,
        confirmation.first_observed_at_ms,
        confirmation.second_observed_at_ms,
    ) {
        diag.emit(event);
    }
    outcome.signalled.len()
}

#[cfg(test)]
#[path = "reload/reap_tests.rs"]
mod reap_tests;

#[cfg(test)]
mod tests;
