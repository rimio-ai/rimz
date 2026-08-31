//! Consumer-side snapshot read: fresh store rollup over the producer pane cache.
//!
//! Renderers that are not the elected producer stay in this lane: no mux call,
//! no git call, no provider probe, and no durable store writes.
//! Long-lived consumers use [`PublishedSnapshotReader`]; producer and test code
//! may use the low-level cursor-taking functions directly.

use crate::ids::PaneId;
use crate::store::parse_cache::StampedPath;
use crate::store::snapshot::SidebarSnapshot;
use crate::{RuntimePaths, StatePaths, Store};

use super::cache::read_snapshot_cache;
use super::enrich::{FoldOpts, WorkspaceSnapshot, enrich_workspace, project_local};
use super::workspace_projection::{
    WORKSPACE_PROJECTION_SCHEMA_VERSION, WorkspaceProjectionSource, read_workspace_projection,
    workspace_projection_path,
};

#[cfg(test)]
mod tests;

/// Re-exported for long-lived consumers (the sidebar fetch worker), which sit
/// behind this module's read-only boundary and never import `crate::store`.
pub use crate::store::snapshot::RollupCursor;

/// Long-lived consumer context and incremental store-rollup state.
pub struct PublishedSnapshotReader {
    runtime: RuntimePaths,
    session: String,
    exclude: Option<PaneId>,
    cursor: RollupCursor,
    source: ConsumerSnapshotSource,
    last_fold: Option<(ConsumerFoldInputsStamp, u64, ConsumerSnapshotSource)>,
    prepared_stamp: Option<(ConsumerSnapshotSource, ConsumerFoldInputsStamp)>,
}

/// Maximum time unchanged input metadata may suppress a real fold.
///
/// Stamps are a latency hint, not truth: bounded re-reads cover filesystem
/// metadata aliasing and future drift in the fold's input set. Skips never
/// advance this window; it measures from the last successful fold.
const CONSUMER_UNCHANGED_BACKSTOP_MS: u64 = 30_000;

impl PublishedSnapshotReader {
    pub fn new(runtime: RuntimePaths, session: impl Into<String>, exclude: Option<PaneId>) -> Self {
        Self {
            runtime,
            session: session.into(),
            exclude,
            cursor: RollupCursor::new(),
            source: ConsumerSnapshotSource::Fallback,
            last_fold: None,
            prepared_stamp: None,
        }
    }

    pub fn read(&mut self, state: &StatePaths) -> crate::store::snapshot::Result<SidebarSnapshot> {
        read_published_snapshot(
            &mut self.cursor,
            state,
            &self.runtime,
            &self.session,
            self.exclude.as_ref(),
        )
    }

    pub(crate) fn read_workspace(
        &mut self,
        state: &StatePaths,
    ) -> crate::store::snapshot::Result<(
        WorkspaceSnapshot,
        Option<std::sync::Arc<super::frame::PaneFrame>>,
    )> {
        read_published_workspace_snapshot(&mut self.cursor, state, &self.runtime, &self.session)
    }

    /// Adopt a matching producer projection, or run the full consumer fold.
    pub fn read_adopting(
        &mut self,
        state: &StatePaths,
    ) -> crate::store::snapshot::Result<SidebarSnapshot> {
        let frame = read_snapshot_cache(&self.runtime.pane_frame_path(), &self.session);
        let adopted = frame.as_deref().and_then(|frame| {
            let published = read_workspace_projection(&self.runtime)?;
            if published.schema_version != WORKSPACE_PROJECTION_SCHEMA_VERSION
                || published.session != self.session
            {
                return None;
            }
            let current = WorkspaceProjectionSource::current(state, frame)?;
            (current.is_matchable() && published.source == current).then(|| {
                project_local(
                    published.projection.clone(),
                    Some(frame),
                    self.exclude.as_ref(),
                )
            })
        });
        match adopted {
            Some(snapshot) => {
                self.source = ConsumerSnapshotSource::Adoption;
                Ok(snapshot)
            }
            None => {
                self.source = ConsumerSnapshotSource::Fallback;
                self.read(state)
            }
        }
    }

    /// Prepare the stamp for a possible fold and test it against the last success.
    ///
    /// This query does not advance the successful-fold timestamp. Its prepared
    /// stamp lets [`Self::record_fold`] avoid a second filesystem walk when the
    /// read keeps the same adoption/fallback source.
    pub(crate) fn fold_unchanged(&mut self, state: &StatePaths, now_ms: u64) -> bool {
        let source = self.source;
        let stamp = source.inputs_stamp(state, &self.runtime);
        let unchanged =
            self.last_fold
                .as_ref()
                .is_some_and(|(last_stamp, folded_at_ms, last_source)| {
                    *last_source == source
                        && last_stamp == &stamp
                        && now_ms.saturating_sub(*folded_at_ms) < CONSUMER_UNCHANGED_BACKSTOP_MS
                });
        self.prepared_stamp = Some((source, stamp));
        unchanged
    }

    /// Record a successful fold, reusing a prepared stamp when its source matches.
    ///
    /// Missing preparation or an adoption/fallback transition pays a fresh
    /// filesystem walk for the correct input set.
    pub(crate) fn record_fold(&mut self, state: &StatePaths, now_ms: u64) {
        let source = self.source;
        let stamp = match self.prepared_stamp.take() {
            Some((prepared_source, stamp)) if prepared_source == source => stamp,
            _ => source.inputs_stamp(state, &self.runtime),
        };
        self.last_fold = Some((stamp, now_ms, source));
    }

    /// Clear skip state and restore full inputs after a producer, mandatory, or failed fold.
    pub(crate) fn clear_fold(&mut self) {
        self.source = ConsumerSnapshotSource::Fallback;
        self.last_fold = None;
        self.prepared_stamp = None;
    }

    /// Producer lane escape hatch for sharing the warm rollup with pane production.
    pub(crate) fn cursor_mut(&mut self) -> &mut RollupCursor {
        &mut self.cursor
    }

    /// Discard possibly-partial incremental state after a caught producer unwind.
    pub(crate) fn reset_after_unwind(&mut self) {
        self.cursor = RollupCursor::new();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConsumerSnapshotSource {
    Adoption,
    Fallback,
}

impl ConsumerSnapshotSource {
    fn inputs_stamp(self, state: &StatePaths, runtime: &RuntimePaths) -> ConsumerFoldInputsStamp {
        match self {
            Self::Adoption => consumer_projection_inputs_stamp(state, runtime),
            Self::Fallback => consumer_fold_inputs_stamp(state, runtime),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConsumerFoldInputsStamp {
    state: Vec<StampedPath>,
    runtime: Vec<StampedPath>,
    dirs: Vec<StampedPath>,
    config_generation: u64,
}

/// The event-fresh store rollup, read in process: `latest.json` when it
/// reflects the log (lock-free, O(snapshot)), else a re-projection folded
/// through the caller's [`RollupCursor`] — O(new log bytes) per delta from
/// the in-memory base, and a fresh cursor folds cold, so a one-shot caller
/// just passes `&mut RollupCursor::new()`. The read-only twin of the
/// producer's `Store::snapshot_cached`, exposed so a consumer tab folds the
/// freshest rollup over the producer's coalesced panes without holding a
/// writer handle — the rollup is what makes a status change or a new agent in
/// an existing pane repaint within one wakeup, independent of the slower
/// pane-list cadence. `Err` preserves *why* the store was unreadable (a torn
/// frame, a permissions failure): the serve loop treats it as a soft miss —
/// hold the last good frame, name the cause on the health line — where a
/// produce or inspection call propagates it.
pub fn rollup_snapshot(
    state: &StatePaths,
    cursor: &mut RollupCursor,
) -> crate::store::snapshot::Result<SidebarSnapshot> {
    match crate::store::snapshot::read_fresh_latest(state) {
        Some(snapshot) => Ok(snapshot),
        None => crate::store::snapshot::build_with_cursor(state, cursor),
    }
}

/// Render the consumer snapshot entirely from runtime caches and sidecars — no
/// no mux roster read, no git. Reads the **event-fresh** rollup in process from
/// `latest.json` (`consumer_rollup`), folds the producer's coalesced pane list
/// from `snapshot.json` when one exists, folds the session and subagent
/// statusline context plus per-tool activity, overlays the panes with this
/// renderer's own-pane exclusion, and projects the cached diff stats. Before
/// the producer's first pane-frame publish, the fold is intentionally
/// frameless: `panes_produced_at_ms == None` and no pane-admitted cards render,
/// while store metadata can still paint. `Err` means the store rollup itself
/// was unreadable and carries why; the serve loop holds its last good frame
/// and surfaces the reason.
///
/// Pairing fresh rollup + coalesced panes is the lag fix: a `StoreDelta` folds
/// the new agent/status in this tab within one wakeup, while the slower pane
/// roster cadence only governs genuine pane open/close.
///
/// This is the producer's fast-lane twin: the native renderer calls it directly
/// each tick, and the `--no-produce` CLI path shares it.
///
/// The rollup folds through the caller's [`RollupCursor`], so a long-lived
/// reader (the sidebar fetch worker owns one across its loop) pays O(new log
/// bytes) per wakeup instead of a full `rollup.json` re-read; a fresh cursor
/// folds cold, so a one-shot caller passes `&mut RollupCursor::new()`.
pub fn read_published_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> crate::store::snapshot::Result<SidebarSnapshot> {
    let (workspace, frame) = read_published_workspace_snapshot(cursor, state, runtime, session)?;
    Ok(project_local(workspace, frame.as_deref(), exclude))
}

/// Whether the producer has published a pane frame for `session`.
///
/// The one-shot frame command uses this gate to prefer the passive consumer
/// path without mistaking a valid frameless rollup for a published frame.
pub fn published_frame_exists(runtime: &RuntimePaths, session: &str) -> bool {
    read_snapshot_cache(&runtime.pane_frame_path(), session).is_some()
}

fn read_published_workspace_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
) -> crate::store::snapshot::Result<(
    WorkspaceSnapshot,
    Option<std::sync::Arc<super::frame::PaneFrame>>,
)> {
    let base = rollup_snapshot(state, cursor)?;
    let cache = read_snapshot_cache(&runtime.pane_frame_path(), session);
    let panes = cache
        .as_deref()
        .map(|frame| SidebarSnapshot::card_admitted_live_panes(frame.to_pane_refs(), None))
        .unwrap_or_default();
    let agent_projection = super::agent_projection::read_published(runtime, session, &panes);
    let store = Store::open_existing(state.clone(), runtime.clone());
    let workspace = enrich_workspace(
        base,
        cache.as_deref(),
        runtime,
        store.as_ref(),
        FoldOpts {
            producing: false,
            fresh_roots: None,
            config: None,
            lanes: None,
            agent_projection,
        },
        &crate::diag::DiagSink::disabled(),
    );
    Ok((workspace, cache))
}

/// Apply cheap producer-published liveness to a cached store rollup.
pub fn cached_alive_snapshot(
    mut base: SidebarSnapshot,
    runtime: &RuntimePaths,
    session: &str,
) -> SidebarSnapshot {
    let frame_panes =
        read_snapshot_cache(&runtime.pane_frame_path(), session).map(|frame| frame.to_pane_refs());
    reap_cached_daemon_sessions_with(&mut base, runtime, frame_panes.as_deref());
    crate::store::agent_context::attach_rest_certificates(runtime, &mut base.agents);
    if let Some(frame_panes) = frame_panes {
        let (panes, projection) =
            read_published_agent_projection(frame_panes, runtime, session, None);
        base = base.with_local_sessions(&panes, projection.local_sessions);
    }
    base
}

/// Apply cached daemon-session reap without local-session enrichment.
pub fn reap_cached_daemon_sessions(
    mut snapshot: SidebarSnapshot,
    runtime: &RuntimePaths,
    session: &str,
) -> SidebarSnapshot {
    let frame_panes =
        read_snapshot_cache(&runtime.pane_frame_path(), session).map(|frame| frame.to_pane_refs());
    reap_cached_daemon_sessions_with(&mut snapshot, runtime, frame_panes.as_deref());
    snapshot
}

fn reap_cached_daemon_sessions_with(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    frame_panes: Option<&[crate::pane::PaneRef]>,
) {
    let cache = super::refresh::read_codex_daemon_reap(runtime).unwrap_or_default();
    snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
        daemon_pids: &cache.daemon_pids,
        loaded: cache.loaded.as_ref(),
        frame_panes,
        exclude_pane: None,
    });
}

fn read_published_agent_projection(
    frame_panes: Vec<crate::pane::PaneRef>,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> (
    Vec<crate::pane::PaneRef>,
    super::agent_projection::AgentProjection,
) {
    let panes = SidebarSnapshot::card_admitted_live_panes(frame_panes, exclude);
    let projection = super::agent_projection::read_published(runtime, session, &panes);
    (panes, projection)
}

/// Cheap identity of the files a consumer fold reads. A matching stamp lets a
/// long-lived renderer skip the fold and keep its last committed frame; the
/// poll backstop still forces a real fold periodically.
fn consumer_fold_inputs_stamp(
    state: &StatePaths,
    runtime: &RuntimePaths,
) -> ConsumerFoldInputsStamp {
    let state_files = [
        state.events_log.clone(),
        state.latest_snapshot.clone(),
        state.rollup_cache.clone(),
        state.agents_carryover.clone(),
        state.workspace_record.clone(),
        state.messages_dir.join("queue.json"),
    ];
    let runtime_files = [
        runtime.pane_frame_path(),
        runtime.unread_path(),
        crate::remote::link::stats_path(runtime),
        runtime.agent_projection_path(),
        runtime.root.join("metrics-sample.json"),
    ];
    let dirs = [
        state.messages_dir.as_path(),
        runtime.agent_context_dir.as_path(),
        runtime.subagent_context_dir.as_path(),
        runtime.agent_activity_dir.as_path(),
        runtime.active_time_dir.as_path(),
        runtime.read_marks_dir.as_path(),
    ];
    let mut runtime_stamps = runtime_files
        .iter()
        .map(|path| StampedPath::of(path.as_path()))
        .collect::<Vec<_>>();
    runtime_stamps.extend(
        super::refresh::inputs::published_lane_inputs(runtime)
            .iter()
            .map(|path| StampedPath::of(path)),
    );
    runtime_stamps.extend(filtered_runtime_inputs(runtime));

    ConsumerFoldInputsStamp {
        state: state_files
            .into_iter()
            .map(|path| StampedPath::of(&path))
            .collect::<Vec<_>>(),
        runtime: runtime_stamps,
        dirs: dirs.into_iter().map(StampedPath::of).collect::<Vec<_>>(),
        config_generation: crate::config::MachineConfig::load_stamp_generation(),
    }
}

/// Slim unchanged identity after a successful projection adoption. Every
/// source-identity input and the projection publication itself remains in the
/// set; broad enrichment sidecars return only after a fallback.
fn consumer_projection_inputs_stamp(
    state: &StatePaths,
    runtime: &RuntimePaths,
) -> ConsumerFoldInputsStamp {
    ConsumerFoldInputsStamp {
        state: [state.events_log.clone(), state.latest_snapshot.clone()]
            .into_iter()
            .map(|path| StampedPath::of(&path))
            .collect(),
        runtime: [
            runtime.pane_frame_path(),
            workspace_projection_path(runtime),
        ]
        .into_iter()
        .map(|path| StampedPath::of(&path))
        .collect(),
        dirs: Vec::new(),
        config_generation: crate::config::MachineConfig::load_stamp_generation(),
    }
}

fn filtered_runtime_inputs(runtime: &RuntimePaths) -> Vec<StampedPath> {
    let mut paths = filtered_paths(&runtime.root, |name| {
        (super::refresh::inputs::is_workspace_spending_file(name)
            || name.starts_with("budget.")
            || name.starts_with("auto-continue."))
            && name.ends_with(".json")
    });
    paths.extend(filtered_paths(&runtime.persistent_shared_root, |name| {
        name.starts_with("budget.account.") && name.ends_with(".json")
    }));
    paths.sort();
    paths
        .into_iter()
        .map(|path| StampedPath::of(&path))
        .collect()
}

fn filtered_paths(
    dir: &std::path::Path,
    include: impl Fn(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let mut paths = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(&include)
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
