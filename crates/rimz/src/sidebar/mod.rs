//! Sidebar process liveness and presence-ingestion helpers.
//!
//! The sidebar heartbeat remains a latency hint. A stale, unreadable, or
//! protocol-mismatched heartbeat never blocks a fresh launch.
//! One room host owns the data plane and paints every subscribed pane.
//!
pub mod agent_projection;
pub(crate) mod body_filter;
pub mod cache;
pub mod consumer;
pub mod enrich;
pub mod event_store;
pub mod frame;
pub mod fuse;
pub mod meter;
pub mod notify;
pub mod observe;
pub mod presence;
pub mod produce;
pub mod read_marks;
pub mod refresh;
#[cfg(test)]
pub(crate) mod test_support;
pub mod timing;
pub mod unread;
pub mod workspace_projection;

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use tracing::debug;

use crate::disk::paths::RuntimePaths;
use crate::disk::single_flight::{self, Coalesced};
use crate::ids::{MuxName, PaneId, SidebarInstanceId};
use crate::mux::{DaemonView, MuxBackend, SidebarLiveness, SidebarPaneOptions};
use crate::wakeup::heartbeat::{
    SIDEBAR_HEARTBEAT_TTL, SidebarHeartbeat, SidebarSize, fresh_sidebar_heartbeats,
};

/// Launch-lock poll cadence: the producer holds the election lock while the
/// daemon it spawned starts and publishes its first heartbeat, and a peer queued
/// behind it polls this long before giving up to the runtime reconciliation. Longer
/// than the diff-stats window because production here is an async process spawn,
/// not a synchronous git fork. `25ms × 60 ≈ 1.5s`.
const LAUNCH_WAIT_STEP: Duration = Duration::from_millis(25);
const LAUNCH_WAIT_STEPS: u32 = 60;

/// One live renderer of this workspace.
#[cfg(feature = "testkit")]
pub struct LiveSidebar {
    pub heartbeat: SidebarHeartbeat,
}

/// Discover this workspace's live renderers in instance-id order.
#[cfg(feature = "testkit")]
pub fn live_sidebars(runtime: &RuntimePaths) -> Vec<LiveSidebar> {
    let mut heartbeats: Vec<_> = fresh_sidebar_heartbeats(runtime)
        .into_iter()
        .filter(|heartbeat| heartbeat.workspace_id == runtime.workspace_id)
        .collect();
    heartbeats.sort_by(|left, right| left.instance_id.as_str().cmp(right.instance_id.as_str()));
    heartbeats
        .into_iter()
        .map(|heartbeat| LiveSidebar { heartbeat })
        .collect()
}

/// The pane size a live renderer in `session` reports, so a frame drawn outside
/// the room matches the sidebar on screen. Every renderer converges on one
/// room-wide width target, so the most recent beat speaks for the session.
pub fn live_sidebar_size(runtime: &RuntimePaths, session: &str) -> Option<SidebarSize> {
    fresh_sidebar_heartbeats(runtime)
        .into_iter()
        .filter(|heartbeat| {
            heartbeat.workspace_id == runtime.workspace_id && heartbeat.session_name == session
        })
        .filter_map(|heartbeat| Some((heartbeat.last_seen, heartbeat.size?)))
        .max_by_key(|(last_seen, _)| *last_seen)
        .map(|(_, size)| size)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarLaunchOutcome {
    SkippedFresh,
    Opened,
    Failed,
}

trait SidebarMux {
    fn name(&self) -> MuxName;
    fn open_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> crate::mux::Result<()>;
    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery>;
}

impl<T: MuxBackend + ?Sized> SidebarMux for T {
    fn name(&self) -> MuxName {
        MuxBackend::name(self)
    }

    fn open_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> crate::mux::Result<()> {
        MuxBackend::open_sidebar(self, opts, daemon)
    }

    fn reconcile_sidebars(
        &self,
        opts: &SidebarPaneOptions,
        live: &SidebarLiveness,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery> {
        MuxBackend::reconcile_sidebars(self, opts, live)
    }
}

/// A live sidebar serving one session from a different RimZ build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBuildDrift {
    /// Distinct semantic versions reported by foreign-build writers, sorted.
    /// Empty means those renderers predate the heartbeat version field.
    pub versions: Vec<String>,
}

/// Return build drift for the live sidebars serving `(mux, session_name)`.
///
/// A missing build id on either this process or a heartbeat is inconclusive
/// and does not create drift.
pub fn session_build_drift(
    rt: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
) -> Option<SessionBuildDrift> {
    let own_build = crate::build_id::current()?;
    let heartbeats = fresh_sidebar_heartbeats(rt);
    session_build_drift_from(
        heartbeats
            .iter()
            .filter(|heartbeat| heartbeat.mux == mux && heartbeat.session_name == session_name)
            .map(|heartbeat| (heartbeat.build.as_deref(), heartbeat.version.as_deref())),
        own_build,
    )
}

fn session_build_drift_from<'a>(
    writers: impl IntoIterator<Item = (Option<&'a str>, Option<&'a str>)>,
    own_build: &str,
) -> Option<SessionBuildDrift> {
    let mut has_foreign = false;
    let mut versions = BTreeSet::new();
    for (build, version) in writers {
        if build.is_some_and(|build| build != own_build) {
            has_foreign = true;
            versions.extend(version.map(str::to_owned));
        }
    }
    has_foreign.then(|| SessionBuildDrift {
        versions: versions.into_iter().collect(),
    })
}

fn fresh_sidebar_instances(rt: &RuntimePaths) -> Vec<SidebarInstanceId> {
    fresh_sidebar_heartbeats(rt)
        .into_iter()
        .map(|heartbeat| heartbeat.instance_id)
        .collect()
}

/// Grace for a just-spawned sidebar pane before reconcile may read "no
/// heartbeat yet" as "wedged": two heartbeat windows, so a just-added sidebar
/// is never closed before its first heartbeat lands — even by a reload run
/// seconds after the one that added it.
pub(crate) const FRESH_PANE_GRACE: Duration = SIDEBAR_HEARTBEAT_TTL.saturating_mul(2);

/// The live sidebars for one mux session and executable generation: every pane
/// a fresh, current-protocol heartbeat for `build` claims, plus whether any
/// matching heartbeat is unlocated (no pane id). Attach folds this into the
/// reconcile planner so an older generation is replaced
/// add-before-close instead of protected as healthy.
fn sidebar_liveness(
    rt: &RuntimePaths,
    build: &str,
    mux: MuxName,
    session_name: &str,
) -> SidebarLiveness {
    let mut live = SidebarLiveness::default();
    for heartbeat in fresh_sidebar_heartbeats(rt)
        .into_iter()
        .filter(|heartbeat| {
            heartbeat.build.as_deref() == Some(build)
                && heartbeat.mux == mux
                && heartbeat.session_name == session_name
        })
    {
        match heartbeat.pane_id {
            Some(pane) => {
                live.claimed_panes.insert(pane);
            }
            None => live.has_unlocated = true,
        }
    }
    live
}

/// Pane-attributed sidebar processes still inside the first-heartbeat grace.
/// Repair keeps these panes tentatively so an attach cannot replace a worker
/// that has mounted but not published yet.
pub(crate) fn young_sidebar_panes(
    mux: MuxName,
    workspace_id: &str,
    session_name: &str,
    now: jiff::Timestamp,
) -> HashSet<PaneId> {
    crate::proc::list_processes()
        .iter()
        .filter(|proc| {
            crate::mux::recovery::is_sidebar_serve(&proc.cmdline, workspace_id, session_name)
        })
        .filter(|proc| {
            crate::proc::process_start(proc.pid)
                .is_some_and(|start| born_recently(start, now, FRESH_PANE_GRACE))
        })
        .filter_map(|proc| crate::mux::recovery::attributed_pane(proc.pid, mux))
        .collect()
}

pub(crate) fn born_recently(start: jiff::Timestamp, now: jiff::Timestamp, grace: Duration) -> bool {
    let grace = i64::try_from(grace.as_secs()).unwrap_or(0);
    now.as_second().saturating_sub(start.as_second()) <= grace
}

pub fn fresh_sidebar_present(rt: &RuntimePaths) -> bool {
    !fresh_sidebar_instances(rt).is_empty()
}

/// Purge sidebar heartbeats at a session rebirth boundary. Call only while the
/// workspace's mux session is provably absent: heartbeats are incarnation-scoped
/// liveness claims and must not outlive their session into a rebirth.
pub(crate) fn purge_rebirth_heartbeats(rt: &RuntimePaths) {
    let entries = match fs::read_dir(&rt.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            debug!(
                path = %rt.heartbeat_dir.display(),
                error = %err,
                "sidebar rebirth heartbeat purge skipped; heartbeat dir unreadable"
            );
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!(
                    path = %rt.heartbeat_dir.display(),
                    error = %err,
                    "sidebar rebirth heartbeat purge skipped unreadable entry"
                );
                continue;
            }
        };
        let path = entry.path();
        if SidebarHeartbeat::is_heartbeat_file(&path) {
            remove_rebirth_heartbeat(&path);
        }
    }
}

/// Launch the workspace sidebar daemon if no fresh one is present, coalescing
/// concurrent attaches through the single-flight election. `daemon` is forwarded
/// to a session (re)birth so `rimz start` can lead the session with the daemon
/// view (Zellij's only way to order it first); every other caller passes `None`.
pub fn launch_sidebar_if_needed(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
) -> SidebarLaunchOutcome {
    launch_sidebar(backend, runtime, opts, daemon)
}

fn launch_sidebar<B: SidebarMux + ?Sized>(
    backend: &B,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
) -> SidebarLaunchOutcome {
    crate::harness::auto_gc::sweep_runtime_claims(runtime);
    // Fast path before contending — `single_flight`'s contract is that the
    // caller has already missed a fresh read by the time it elects.
    if fresh_sidebar_present(runtime) {
        ensure_session_view(backend, runtime, opts);
        return SidebarLaunchOutcome::SkippedFresh;
    }
    // Serialize check-then-launch through the shared single-flight election so
    // two concurrent attaches to one shared session can't both spawn a daemon.
    // A peer that finds a fresh heartbeat while polling skips; the winner holds
    // the lock until its daemon publishes one. No lock dir or a wedged producer
    // falls to a local launch, and the reconcile pass reaps the loser.
    let lock_path = runtime.lock_path("sidebar-launch.lock");
    let _guard =
        match single_flight::coalesce(&lock_path, LAUNCH_WAIT_STEP, LAUNCH_WAIT_STEPS, || {
            fresh_sidebar_present(runtime).then_some(())
        }) {
            Coalesced::Shared(()) => {
                ensure_session_view(backend, runtime, opts);
                return SidebarLaunchOutcome::SkippedFresh;
            }
            Coalesced::Produce(guard) => Some(guard),
            Coalesced::ProduceLocal => None,
        };
    match backend.open_sidebar(opts, daemon) {
        Ok(()) => {
            // Hold the election lock (`_guard`) until the new daemon publishes
            // its heartbeat, so an attach polling behind us reads it and skips.
            // The daemon writes the heartbeat just after start, well inside the
            // budget; a slow one falls to reconciliation rather than stalling the
            // attach further.
            wait_for_fresh_sidebar(runtime);
            SidebarLaunchOutcome::Opened
        }
        Err(
            err @ (crate::mux::MuxErr::SocketPathTooLong { .. }
            | crate::mux::MuxErr::SocketPathReportedTooLong { .. }),
        ) => {
            tracing::debug!(
                session = %opts.session_name,
                mux = %backend.name(),
                error = %err,
                "sidebar pane launch hit zellij socket path limit; attach gate reports the fix",
            );
            SidebarLaunchOutcome::Failed
        }
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                mux = %backend.name(),
                error = %err,
                "sidebar pane launch failed; continuing without sidebar",
            );
            SidebarLaunchOutcome::Failed
        }
    }
}

fn ensure_session_view<B: SidebarMux + ?Sized>(
    backend: &B,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
) {
    let build = match crate::build_id::of_file(&opts.rimz_bin) {
        Ok(build) => build,
        Err(err) => {
            tracing::warn!(
                path = %opts.rimz_bin.display(),
                error = %err,
                "ensuring the session sidebar view skipped; room build is unreadable",
            );
            return;
        }
    };
    let mut live = sidebar_liveness(runtime, &build, backend.name(), &opts.session_name);
    live.young_panes = young_sidebar_panes(
        backend.name(),
        opts.workspace_id.as_str(),
        &opts.session_name,
        jiff::Timestamp::now(),
    );
    match backend.reconcile_sidebars(opts, &live) {
        Ok(_) => {}
        Err(crate::mux::MuxErr::SessionNotFound { session }) => tracing::debug!(
            session = %session,
            mux = %backend.name(),
            "sidebar reconcile skipped; session not addressable yet (pre-attach gate will rebirth it)",
        ),
        Err(err) => tracing::warn!(
            session = %opts.session_name,
            mux = %backend.name(),
            error = %err,
            "ensuring the session sidebar view failed; continuing",
        ),
    }
}

/// Poll for a fresh sidebar heartbeat, returning as soon as one appears, on the
/// same cadence the launch election polls. Held under the election lock so the
/// next launcher observes the daemon we just spawned instead of racing it.
fn wait_for_fresh_sidebar(rt: &RuntimePaths) {
    for _ in 0..LAUNCH_WAIT_STEPS {
        if fresh_sidebar_present(rt) {
            return;
        }
        std::thread::sleep(LAUNCH_WAIT_STEP);
    }
}

fn remove_rebirth_heartbeat(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => debug!(path = %path.display(), "purged sidebar heartbeat at session rebirth"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            debug!(path = %path.display(), error = %err, "purging rebirth sidebar heartbeat failed")
        }
    }
}

#[cfg(test)]
mod tests;
