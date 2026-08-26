//! Durable sidebar upgrade and structural repair orchestration.
//!
//! `rimz reload` stages the installed binary, writes it as each live room's
//! durable target, nudges supervisors, hash-gates Zellij plugin convergence,
//! and reports the bounded convergence window without changing terminal panes.
//! `rimz sidebar repair` is the independent structural close/add pass. Held
//! `rimz stats --refresh` dashboards are signalled to re-exec in place before
//! workspace enumeration, so standalone dashboards reload even when no rooms
//! exist. A workspace whose session is gone has its stale runtime files and
//! leftover daemons swept. Every step is best-effort: a hiccup on one workspace
//! is logged and never blocks the rest.

use std::collections::HashSet;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::diag::record::DiagEvent;
use crate::ids::{MuxName, PaneId};
use crate::mux::recovery;
use crate::mux::{
    MuxBackend, PaneListOptions, PaneListing, PaneReadConsistency, SidebarLiveness,
    SidebarPaneOptions, SidebarWidth, backend_for,
};
use crate::proc::ProcInfo;
use crate::room::session::LiveSessions;
use crate::sidebar::heartbeat::SidebarHeartbeat;
use crate::sidebar::timing::{
    RECONCILE_LIST_TIMEOUT, RELOAD_CONVERGE_POLL, RELOAD_CONVERGE_TIMEOUT, unix_now_ms,
};
use crate::store::{RuntimePaths, StatePaths, workspace_record};
use crate::workspace::{self, KnownWorkspace};

/// Immutable executable generation shared by every long-lived process in a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedBuild {
    pub path: PathBuf,
    pub build: String,
}

const STAGED_BUILD_GC_GRACE: Duration = Duration::from_secs(60);
const REAP_CONFIRM_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum StageBuildErr {
    #[error("the running RimZ executable has no readable on-disk source to stage")]
    MissingSource,
    #[error("cannot stage RimZ build at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Atomic(#[from] crate::store::atomic::AtomicErr),
}

/// Copy the invoking build into the durable user-scoped build store.
pub fn stage_current_build() -> Result<StagedBuild, StageBuildErr> {
    let source = current_reexec_target().ok_or(StageBuildErr::MissingSource)?;
    stage_build_under(&source, &crate::store::paths::state_home())
}

fn stage_build_under(source: &Path, state_root: &Path) -> Result<StagedBuild, StageBuildErr> {
    let bytes = fs::read(source).map_err(|source_err| StageBuildErr::Io {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    let build = crate::build_id::of_bytes(&bytes);
    let builds_dir = crate::store::paths::builds_dir_under(state_root);
    let path = builds_dir.join(&build).join("rimz");
    let reusable = path.is_file()
        && crate::build_id::of_file(&path).is_ok_and(|staged_build| staged_build == build);
    if !reusable {
        crate::store::atomic::write_executable_bytes_atomically(&path, &bytes)?;
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
    let workspaces = crate::store::paths::workspaces_dir_under(state_root);
    if let Ok(entries) = fs::read_dir(workspaces) {
        for entry in entries.flatten() {
            let record_path = entry.path().join("workspace.json");
            if let Ok(record) = workspace_record::read(&record_path)
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
    let record = match workspace_record::read(&paths.workspace_record) {
        Ok(record) => record,
        Err(workspace_record::WorkspaceRecordErr::Io { source, .. })
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
pub fn resolve_reexec_target(exe: PathBuf) -> Option<PathBuf> {
    crate::proc::resolve_existing_or_replacement(&exe)
}

/// What a user-wide reload did, aggregated across workspaces, for the CLI report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Live sessions reconciled.
    pub sessions: usize,
    /// Zellij sessions whose presence channel did not publish after converge.
    pub presence_dead: usize,
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
    /// Leftover processes swept from workspaces whose session is gone.
    pub dead_swept: usize,
    /// `rimz stats --refresh` dashboards signalled to re-exec onto the new build.
    pub stats_reloaded: usize,
    /// Views whose sidebar add or repair could not complete this pass.
    pub failed: usize,
    /// Views whose in-place add or geometry repair was deferred (no attached
    /// client).
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
    /// Working sidebar panes that remain outside the verified full-height left
    /// dock after the bounded repair path.
    pub misdocked: usize,
}

impl ReloadOutcome {
    fn merge(&mut self, delta: Self) {
        let Self {
            sessions,
            presence_dead,
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
            dead_swept,
            stats_reloaded,
            failed,
            deferred,
            redocked,
            misdocked,
        } = delta;
        self.sessions += sessions;
        self.presence_dead += presence_dead;
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
        self.dead_swept += dead_swept;
        self.stats_reloaded += stats_reloaded;
        self.failed += failed;
        self.deferred += deferred;
        self.redocked += redocked;
        self.misdocked += misdocked;
    }
}

/// Publish the installed build as durable intent and nudge every live sidebar
/// toward it without changing pane structure.
pub fn reload_user_sidebars() -> Result<ReloadOutcome, StageBuildErr> {
    let staged = stage_current_build()?;
    let mut outcome = ReloadOutcome {
        stats_reloaded: recovery::reload_stats_dashboards().len(),
        ..ReloadOutcome::default()
    };
    let (live_targets, dead_swept) = live_targets(true);
    outcome.dead_swept = dead_swept;
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
                scope.spawn(move || upgrade_live(target, staged, machine_config))
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(delta) => outcome.merge(delta),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });

    Ok(outcome)
}

/// Repair missing, duplicate, wedged, or mis-docked sidebars without
/// publishing a new build target.
pub fn repair_user_sidebars() -> ReloadOutcome {
    let (live_targets, _) = live_targets(false);
    let machine_config = MachineConfig::load_lenient();
    let mut outcome = ReloadOutcome::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = live_targets
            .iter()
            .map(|target| {
                let machine_config = &machine_config;
                scope.spawn(move || repair_live(target, machine_config))
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(delta) => outcome.merge(delta),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    outcome
}

struct LiveTarget {
    mux: MuxName,
    workspace: KnownWorkspace,
    runtime: RuntimePaths,
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
        let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
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
                crate::sidebar::sweep_orphan_runtime(&runtime);
                dead_swept += recovery::sweep_orphan_processes(
                    ws.workspace_id.as_str(),
                    &ws.session_name,
                    false,
                )
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

/// Publish and nudge one live session, then reap owners whose panes are gone.
fn upgrade_live(
    target: &LiveTarget,
    staged: &StagedBuild,
    machine_config: &MachineConfig,
) -> ReloadOutcome {
    let LiveTarget {
        mux,
        workspace: ws,
        runtime,
    } = target;
    let mut outcome = ReloadOutcome {
        sessions: 1,
        ..ReloadOutcome::default()
    };
    let backend = backend_for(*mux);
    let room_bin = record_live_room_bin(ws, runtime, staged).unwrap_or_else(|| staged.path.clone());
    let before_signal = session_heartbeats(runtime, *mux, &ws.session_name);

    // 1. Signal live sidebars to re-exec onto the freshly-installed binary.
    match crate::sidebar::wakeup::reload_all(runtime) {
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
    outcome.unverified += unlocated_unverified_count(&before_signal, build);
    let post_wait = if awaiting.is_empty() {
        before_signal
    } else {
        wait_for_convergence(runtime, *mux, &ws.session_name, &awaiting, build)
    };
    let current = current_build_claims(&post_wait, build);
    outcome.reexeced += awaiting.intersection(&current).count();
    outcome.unconverged += awaiting.difference(&current).count();
    // 2. Converge the session's presence plugin onto the current wasm — reload
    //    is the explicit upgrade verb. Stale instances retire only after the
    //    replacement publishes topology from its new writer generation; a
    //    detached or degraded session keeps its prior plugin and retries on a
    //    later reload. Best-effort like every step here.
    let mux_config = MultiplexerConfig::from(machine_config);
    if *mux == MuxName::Zellij
        && let Some(wasm) = crate::mux::zellij::ensure_presence_plugin_artifact()
    {
        let presence = crate::mux::PresencePluginOptions {
            session_name: ws.session_name.clone(),
            workspace_id: ws.workspace_id.clone(),
            wasm,
            rimz_bin: room_bin.clone(),
            converge: true,
            focus_key: crate::config::SidebarConfig::key_label(&machine_config.sidebar.focus_key)
                .map(str::to_owned),
            zoom_key: crate::config::SidebarConfig::key_label(&machine_config.sidebar.zoom_key)
                .map(str::to_owned),
            focus_follows_mouse: mux_config.zellij.focus_follows_mouse,
            mouse_click_through: mux_config.zellij.mouse_click_through,
        };
        let desired_config = crate::mux::zellij::presence_plugin_config_hash_for(&presence);
        let cache = crate::sidebar::cache::read_pane_topology_cache(runtime, &ws.session_name);
        let current_writer = current_presence_plugin_writer(
            cache.as_ref(),
            unix_now_ms(),
            crate::mux::zellij::presence_plugin_build(),
            &desired_config,
        );
        let needs_convergence = match current_writer {
            Some(writer) => match crate::mux::ZellijBackend::new()
                .cleanup_current_presence_plugin_for(&presence, writer)
            {
                Ok(crate::mux::zellij::PresencePluginCleanup::Current) => {
                    outcome.plugin_current += 1;
                    false
                }
                Ok(crate::mux::zellij::PresencePluginCleanup::Reconciled) => {
                    outcome.plugin_reconciled += 1;
                    false
                }
                Err(err) => {
                    tracing::debug!(
                        session = %ws.session_name,
                        error = &err as &dyn std::error::Error,
                        "presence live-id inspection failed; falling back to full convergence",
                    );
                    true
                }
            },
            None => true,
        };
        if needs_convergence {
            match backend.ensure_presence_plugin(&presence) {
                Ok(()) => outcome.plugin_upgraded += 1,
                Err(err) => {
                    tracing::warn!(
                        session = %ws.session_name,
                        tags.operation = "reload.presence_converge",
                        error = &err as &dyn std::error::Error,
                        "reload: presence plugin convergence failed",
                    );
                }
            }
        }
    }

    // Reap orphan sidebar processes whose pane is gone — the mux cannot close a
    // pane that no longer exists, so a wedged renderer would otherwise linger.
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), *mux, ws);

    // Sweep runtime files whose owner is gone — stale heartbeats and ownerless
    // sockets accumulate in a live session too (every SIGKILLed or reaped
    // renderer leaves a pair), and the sweep already spares anything fresh or
    // still starting.
    crate::sidebar::sweep_orphan_runtime(runtime);
    outcome
}

fn repair_live(target: &LiveTarget, machine_config: &MachineConfig) -> ReloadOutcome {
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
        return outcome;
    };
    let Ok(build) = crate::build_id::of_file(&rimz_bin) else {
        outcome.failed += 1;
        return outcome;
    };
    let backend = backend_for(*mux);
    let mut liveness =
        current_build_liveness(&session_heartbeats(runtime, *mux, &ws.session_name), &build);
    let mux_config = MultiplexerConfig::from(machine_config);

    let topology_floor_ms = if *mux == MuxName::Zellij {
        let presence_floor_ms = unix_now_ms();
        if let Some(wasm) = crate::mux::zellij::ensure_presence_plugin_artifact() {
            let presence = crate::mux::PresencePluginOptions {
                session_name: ws.session_name.clone(),
                workspace_id: ws.workspace_id.clone(),
                wasm,
                rimz_bin: StatePaths::for_workspace(ws.workspace_id.clone())
                    .map(|paths| paths.room_bin)
                    .unwrap_or_else(|_| rimz_bin.clone()),
                converge: false,
                focus_key: crate::config::SidebarConfig::key_label(
                    &machine_config.sidebar.focus_key,
                )
                .map(str::to_owned),
                zoom_key: crate::config::SidebarConfig::key_label(&machine_config.sidebar.zoom_key)
                    .map(str::to_owned),
                focus_follows_mouse: mux_config.zellij.focus_follows_mouse,
                mouse_click_through: mux_config.zellij.mouse_click_through,
            };
            if let Err(err) = backend.ensure_presence_plugin(&presence) {
                tracing::warn!(
                    session = %ws.session_name,
                    tags.operation = "sidebar.repair.presence_ensure",
                    error = &err as &dyn std::error::Error,
                    "sidebar repair: presence plugin ensure failed",
                );
            }
            if let Err(err) = crate::mux::ZellijBackend::new().dump_topology_for(&presence) {
                tracing::warn!(
                    session = %ws.session_name,
                    tags.operation = "sidebar.repair.presence_probe",
                    error = &err as &dyn std::error::Error,
                    "sidebar repair: presence topology request failed",
                );
            }
        }
        if !presence_channel_is_live(*mux, runtime, &ws.session_name, presence_floor_ms) {
            outcome.presence_dead += 1;
            crate::sidebar::sweep_orphan_runtime(runtime);
            return outcome;
        }
        Some(presence_floor_ms)
    } else {
        None
    };
    liveness.topology_floor_ms = topology_floor_ms;

    let width = SidebarWidth::from_config(&machine_config.theme);
    let target = crate::sidebar::width_target::resolve(runtime, width, None);
    let opts = SidebarPaneOptions {
        session_name: ws.session_name.clone(),
        workspace_id: ws.workspace_id.clone(),
        project_root: ws.project_root.clone(),
        extra_env: crate::agents::registry::room_env(runtime),
        cwd: ws.project_root.clone(),
        target,
        detected_view_size: None,
        rimz_bin,
        pristine_birth: false,
        config: mux_config,
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    liveness.young_panes = crate::sidebar::young_sidebar_panes(
        *mux,
        ws.workspace_id.as_str(),
        &ws.session_name,
        jiff::Timestamp::now(),
    );
    match backend.reconcile_sidebars(&opts, &liveness) {
        Ok(report) => {
            outcome.recovered += report.recovered;
            outcome.closed += report.closed;
            outcome.failed += report.failed;
            outcome.deferred += report.deferred;
            outcome.redocked += report.redocked;
            outcome.misdocked += report.misdocked;
        }
        Err(err) => {
            outcome.failed += 1;
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "sidebar.repair.reconcile",
                error = &err as &dyn std::error::Error,
                "sidebar repair: structural pass failed",
            );
        }
    }
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), *mux, ws);
    crate::sidebar::sweep_orphan_runtime(runtime);
    outcome
}

fn current_presence_plugin_writer<'a>(
    cache: Option<&'a crate::mux::zellij::pane_topology::PaneTopologyCache>,
    now_ms: u64,
    desired_build: &str,
    desired_config: &str,
) -> Option<&'a crate::mux::zellij::pane_topology::TopologyWriter> {
    let cache = cache
        .filter(|cache| crate::sidebar::cache::pane_topology_cache_is_fresh(cache, now_ms, None))?;
    cache.writer.as_ref().filter(|writer| {
        writer.build.as_deref() == Some(desired_build)
            && writer.config.as_deref() == Some(desired_config)
    })
}

const RELOAD_PRESENCE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn presence_channel_is_live(
    mux: MuxName,
    runtime: &RuntimePaths,
    session_name: &str,
    min_produced_at_ms: u64,
) -> bool {
    if mux != MuxName::Zellij {
        return true;
    }
    let deadline = Instant::now() + RELOAD_PRESENCE_PROBE_TIMEOUT;
    loop {
        let now_ms = unix_now_ms();
        if crate::sidebar::cache::read_pane_topology_cache(runtime, session_name).is_some_and(
            |cache| {
                crate::sidebar::cache::pane_topology_cache_is_fresh(
                    &cache,
                    now_ms,
                    Some(min_produced_at_ms),
                )
            },
        ) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(crate::mux::zellij::TOPOLOGY_CACHE_POLL_STEP);
    }
}

fn record_live_room_bin(
    ws: &KnownWorkspace,
    runtime: &RuntimePaths,
    staged: &StagedBuild,
) -> Option<PathBuf> {
    let Ok(paths) = StatePaths::for_workspace(ws.workspace_id.clone()) else {
        return None;
    };
    record_live_room_bin_at(ws, staged, &paths, runtime)
}

fn record_live_room_bin_at(
    ws: &KnownWorkspace,
    staged: &StagedBuild,
    paths: &StatePaths,
    runtime: &RuntimePaths,
) -> Option<PathBuf> {
    let Ok(record) = workspace_record::read(&paths.workspace_record) else {
        return None;
    };
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
    let result = crate::store::Store::open(paths.clone(), runtime.clone()).and_then(|store| {
        store.record_room_bin(&workspace, staged.path.clone(), staged.build.clone())
    });
    if let Err(err) = result {
        tracing::debug!(
            workspace = %ws.workspace_id,
            error = %err,
            "reload: recording room binary failed",
        );
        return None;
    }
    Some(paths.room_bin.clone())
}

fn session_heartbeats(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
) -> Vec<SidebarHeartbeat> {
    crate::sidebar::fresh_sidebar_heartbeats(runtime)
        .into_iter()
        .filter(|heartbeat| heartbeat.mux == mux && heartbeat.session_name == session_name)
        .collect()
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

fn current_build_liveness(heartbeats: &[SidebarHeartbeat], build: &str) -> SidebarLiveness {
    heartbeat_liveness(
        heartbeats
            .iter()
            .filter(|hb| hb.build.as_deref() == Some(build)),
    )
}

fn heartbeat_liveness<'a>(
    heartbeats: impl IntoIterator<Item = &'a SidebarHeartbeat>,
) -> SidebarLiveness {
    let mut live = SidebarLiveness::default();
    for heartbeat in heartbeats {
        match heartbeat.pane_id.as_ref() {
            Some(pane) => {
                live.claimed_panes.insert(pane.clone());
            }
            None => live.has_unlocated = true,
        }
    }
    live
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
mod tests {
    use super::*;

    #[test]
    fn reexec_target_resolves_the_replacement_after_an_install() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rimz");
        std::fs::write(&real, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        assert!(!deleted.is_file(), "the annotated path must not exist");
        assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
        assert_eq!(resolve_reexec_target(real.clone()), Some(real));
    }

    #[test]
    fn reexec_target_is_none_when_nothing_exists_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
        assert_eq!(resolve_reexec_target(deleted), None);
        assert_eq!(resolve_reexec_target(missing), None);
    }

    #[test]
    fn staging_is_idempotent_and_executable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source-rimz");
        std::fs::write(&source, b"one immutable build").unwrap();

        let first = stage_build_under(&source, dir.path()).unwrap();
        let second = stage_build_under(&source, dir.path()).unwrap();

        assert_eq!(first, second);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"one immutable build");
        assert_eq!(crate::build_id::of_file(&first.path).unwrap(), first.build);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn staging_sweeps_only_unreferenced_builds() {
        let dir = tempfile::tempdir().unwrap();
        let first_source = dir.path().join("first");
        let second_source = dir.path().join("second");
        std::fs::write(&first_source, b"first build").unwrap();
        std::fs::write(&second_source, b"second build").unwrap();
        let first = stage_build_under(&first_source, dir.path()).unwrap();

        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let workspace = crate::workspace::WorkspaceResolver::resolve(&project, None).unwrap();
        let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let mut record = crate::store::workspace_record::WorkspaceRecord::from_resolved(&workspace);
        record.rimz_bin = Some(first.path.clone());
        record.rimz_build = Some(first.build.clone());
        workspace_record::write(&paths, &record).unwrap();
        let known = KnownWorkspace {
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            session_name: workspace.session_name.clone(),
            root_class: workspace.root_class,
            rimz_bin: record.rimz_bin.clone(),
            updated_at: record.updated_at,
        };

        let unreferenced = crate::store::paths::builds_dir_under(dir.path()).join("unused");
        std::fs::create_dir_all(&unreferenced).unwrap();
        std::fs::write(unreferenced.join("rimz"), b"unused").unwrap();
        std::fs::File::open(&unreferenced)
            .unwrap()
            .set_modified(
                std::time::SystemTime::now() - STAGED_BUILD_GC_GRACE - Duration::from_secs(1),
            )
            .unwrap();
        let second = stage_build_under(&second_source, dir.path()).unwrap();
        let room_bin = record_live_room_bin_at(&known, &second, &paths, &runtime).unwrap();

        assert!(
            first.path.is_file(),
            "previously recorded build remains staged"
        );
        assert!(second.path.is_file(), "new build remains staged");
        assert!(!unreferenced.exists(), "unreferenced build is swept");
        assert_eq!(std::fs::read(room_bin).unwrap(), b"second build");
        let updated = workspace_record::read(&paths.workspace_record).unwrap();
        assert_eq!(updated.rimz_bin.as_deref(), Some(second.path.as_path()));
        assert_eq!(updated.rimz_build.as_deref(), Some(second.build.as_str()));
    }

    #[test]
    fn recorded_target_requires_the_recorded_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rimz");
        std::fs::write(&path, b"verified build").unwrap();
        let build = crate::build_id::of_file(&path).unwrap();

        assert_eq!(
            resolve_recorded_target(Some(path.clone()), Some(build.clone())),
            WorkspaceReexecTarget::Verified(StagedBuild {
                path: path.clone(),
                build,
            })
        );
        assert_eq!(
            resolve_recorded_target(Some(path.clone()), Some("wrong".to_owned())),
            WorkspaceReexecTarget::Invalid
        );
        assert_eq!(
            resolve_recorded_target(Some(path), None),
            WorkspaceReexecTarget::Absent
        );
        assert_eq!(
            resolve_recorded_target(None, Some("orphan".to_owned())),
            WorkspaceReexecTarget::Invalid
        );
    }

    #[test]
    fn born_recently_holds_inside_the_grace_and_for_clock_fuzz() {
        let now = jiff::Timestamp::from_second(1_000_000).unwrap();
        let grace = crate::sidebar::FRESH_PANE_GRACE;
        let at = |secs_ago: i64| jiff::Timestamp::from_second(1_000_000 - secs_ago).unwrap();
        assert!(crate::sidebar::born_recently(at(0), now, grace));
        assert!(crate::sidebar::born_recently(
            at(i64::try_from(grace.as_secs()).unwrap()),
            now,
            grace
        ));
        assert!(
            !crate::sidebar::born_recently(
                at(i64::try_from(grace.as_secs()).unwrap() + 1),
                now,
                grace,
            ),
            "one second past the grace is old",
        );
        assert!(
            crate::sidebar::born_recently(at(-3), now, grace),
            "a start after `now` is a just-spawned process under clock fuzz",
        );
    }

    #[test]
    fn live_session_claim_is_once_per_mux_session() {
        let mut seen = HashSet::new();
        assert!(claim_live_session(
            &mut seen,
            MuxName::Zellij,
            "rimz-query-engine"
        ));
        assert!(!claim_live_session(
            &mut seen,
            MuxName::Zellij,
            "rimz-query-engine"
        ));
        assert!(
            claim_live_session(&mut seen, MuxName::Tmux, "rimz-query-engine"),
            "the same name on a different mux is a different live session",
        );
    }

    #[test]
    fn reload_outcome_merge_sums_every_field() {
        let mut base = ReloadOutcome {
            sessions: 1,
            presence_dead: 2,
            plugin_current: 3,
            plugin_upgraded: 4,
            plugin_reconciled: 18,
            already_current: 5,
            reexeced: 6,
            unconverged: 7,
            unverified: 8,
            recovered: 9,
            closed: 10,
            reaped: 11,
            dead_swept: 12,
            stats_reloaded: 13,
            failed: 14,
            deferred: 15,
            redocked: 16,
            misdocked: 17,
        };
        base.merge(ReloadOutcome {
            sessions: 18,
            presence_dead: 19,
            plugin_current: 20,
            plugin_upgraded: 21,
            plugin_reconciled: 35,
            already_current: 22,
            reexeced: 23,
            unconverged: 24,
            unverified: 25,
            recovered: 26,
            closed: 27,
            reaped: 28,
            dead_swept: 29,
            stats_reloaded: 30,
            failed: 31,
            deferred: 32,
            redocked: 33,
            misdocked: 34,
        });

        assert_eq!(
            base,
            ReloadOutcome {
                sessions: 19,
                presence_dead: 21,
                plugin_current: 23,
                plugin_upgraded: 25,
                plugin_reconciled: 53,
                already_current: 27,
                reexeced: 29,
                unconverged: 31,
                unverified: 33,
                recovered: 35,
                closed: 37,
                reaped: 39,
                dead_swept: 41,
                stats_reloaded: 43,
                failed: 45,
                deferred: 47,
                redocked: 49,
                misdocked: 51,
            }
        );
    }

    #[test]
    fn presence_plugin_gate_requires_fresh_matching_build_and_config() {
        use crate::mux::zellij::pane_topology::{PaneTopologyCache, TopologyWriter};

        let cache = |produced_at_ms, build: Option<&str>, config: Option<&str>| PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms,
            writer: Some(TopologyWriter {
                plugin_id: 7,
                loaded_at_ms: 10,
                build: build.map(str::to_owned),
                config: config.map(str::to_owned),
            }),
            focused_pane: None,
            clients: None,
            panes: Vec::new(),
        };
        let current = cache(1_000, Some("wasm"), Some("config"));
        assert!(current_presence_plugin_writer(Some(&current), 1_000, "wasm", "config").is_some());
        assert!(
            current_presence_plugin_writer(
                Some(&cache(1_000, None, None)),
                1_000,
                "wasm",
                "config"
            )
            .is_none()
        );
        assert!(
            current_presence_plugin_writer(
                Some(&cache(1_000, Some("old"), Some("config"))),
                1_000,
                "wasm",
                "config"
            )
            .is_none()
        );
        assert!(
            current_presence_plugin_writer(
                Some(&cache(1_000, Some("wasm"), Some("old"))),
                1_000,
                "wasm",
                "config"
            )
            .is_none()
        );
        assert!(
            current_presence_plugin_writer(
                Some(&current),
                1_000 + crate::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 1,
                "wasm",
                "config"
            )
            .is_none()
        );
    }

    fn heartbeat(raw: &str, build: Option<&str>) -> SidebarHeartbeat {
        let pane = PaneId::from_parts(MuxName::Tmux, raw);
        let mut hb = SidebarHeartbeat::new(
            crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            crate::SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            std::path::PathBuf::from(format!("/tmp/{raw}.sock")),
            Some(pane),
        );
        hb.build = build.map(str::to_owned);
        hb
    }

    fn unlocated_heartbeat(build: Option<&str>) -> SidebarHeartbeat {
        let mut hb = SidebarHeartbeat::new(
            crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            crate::SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            std::path::PathBuf::from("/tmp/unlocated.sock"),
            None,
        );
        hb.build = build.map(str::to_owned);
        hb
    }

    #[test]
    fn build_partition_marks_missing_or_stale_builds_as_awaiting() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%2", Some("old")),
            heartbeat("%3", None),
        ];

        assert_eq!(current_located_count(&heartbeats, "current"), 1);
        let awaiting = awaiting_panes(&heartbeats, "current");
        assert!(!awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%1")));
        assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%2")));
        assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%3")));
    }

    #[test]
    fn current_located_count_deduplicates_pane_claims() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%1", Some("current")),
        ];

        assert_eq!(current_located_count(&heartbeats, "current"), 1);
    }

    #[test]
    fn current_build_liveness_excludes_stale_claims() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%2", Some("old")),
            heartbeat("%3", None),
        ];

        let live = current_build_liveness(&heartbeats, "current");
        assert!(
            live.claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%1"))
        );
        assert!(
            !live
                .claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%2"))
        );
        assert!(
            !live
                .claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%3"))
        );
    }

    #[test]
    fn current_build_liveness_keeps_unlocated_current() {
        let heartbeats = vec![
            unlocated_heartbeat(Some("current")),
            heartbeat("%2", Some("old")),
        ];
        let live = current_build_liveness(&heartbeats, "current");

        assert!(live.has_unlocated);
    }

    #[test]
    fn convergence_wait_settles_when_awaiting_pane_flips_current() {
        let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();
        let mut reads = 0;

        let heartbeats = wait_for_convergence_with(
            &awaiting,
            "current",
            Duration::from_secs(1),
            Duration::ZERO,
            || {
                reads += 1;
                if reads == 1 {
                    vec![heartbeat("%1", Some("old"))]
                } else {
                    vec![heartbeat("%1", Some("current"))]
                }
            },
        );

        assert_eq!(reads, 2);
        assert_eq!(
            current_build_claims(&heartbeats, "current"),
            [PaneId::from_parts(MuxName::Tmux, "%1")].into(),
        );
    }

    #[test]
    fn convergence_wait_returns_latest_heartbeat_on_timeout() {
        let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();

        let heartbeats =
            wait_for_convergence_with(&awaiting, "current", Duration::ZERO, Duration::ZERO, || {
                vec![heartbeat("%1", Some("old"))]
            });

        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].build.as_deref(), Some("old"));
    }
}
