//! Snapshot reads: fresh store rollup over the published pane cache.
//!
//! The host's fast lane and CLI readers share this in-process fold: no mux,
//! git, or provider probe, and no durable store writes.

use crate::ids::PaneId;
use crate::store::snapshot::SidebarSnapshot;
use crate::{RuntimePaths, StatePaths, Store};

use super::cache::read_snapshot_cache;
use super::enrich::{FoldOpts, WorkspaceSnapshot, enrich_workspace, project_local};

#[cfg(test)]
mod tests;

/// Re-exported for long-lived consumers (the sidebar fetch worker), which sit
/// behind this module's read-only boundary and never import `crate::store`.
pub use crate::store::snapshot::RollupCursor;

/// Published-cache reader and incremental store-rollup state.
pub struct PublishedSnapshotReader {
    runtime: RuntimePaths,
    session: String,
    exclude: Option<PaneId>,
    cursor: RollupCursor,
}

impl PublishedSnapshotReader {
    pub fn new(runtime: RuntimePaths, session: impl Into<String>, exclude: Option<PaneId>) -> Self {
        Self {
            runtime,
            session: session.into(),
            exclude,
            cursor: RollupCursor::new(),
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
}

/// Read the event-fresh store rollup through a warm incremental cursor.
/// A one-shot caller passes a fresh cursor; failures preserve the unreadable cause.
pub fn rollup_snapshot(
    state: &StatePaths,
    cursor: &mut RollupCursor,
) -> crate::store::snapshot::Result<SidebarSnapshot> {
    match crate::store::snapshot::read_fresh_latest(state) {
        Some(snapshot) => Ok(snapshot),
        None => crate::store::snapshot::build_with_cursor(state, cursor),
    }
}

/// Fold the event-fresh rollup over published panes and sidecars without external probes.
pub(super) fn read_published_snapshot(
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
        .map(|frame| base.card_admitted_live_panes(frame.to_pane_refs(), None))
        .unwrap_or_default();
    let agent_projection = super::agent_projection::read_published(runtime, session, &panes);
    let workspace = fold_workspace(base, cache.as_deref(), agent_projection, state, runtime);
    Ok((workspace, cache))
}

pub(crate) fn fold_workspace(
    base: SidebarSnapshot,
    frame: Option<&super::frame::PaneFrame>,
    agent_projection: super::agent_projection::AgentProjection,
    state: &StatePaths,
    runtime: &RuntimePaths,
) -> WorkspaceSnapshot {
    let store = Store::open_existing(state.clone(), runtime.clone());
    enrich_workspace(
        base,
        frame,
        state,
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
    )
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
            read_published_agent_projection(&base, frame_panes, runtime, session, None);
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
    let cache = super::refresh::read_codex_daemon_reap(runtime, crate::utils::time::unix_now_ms())
        .unwrap_or_default();
    snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
        daemon_pids: &cache.daemon_pids,
        loaded: cache.loaded.as_ref(),
        frame_panes,
        exclude_pane: None,
    });
}

fn read_published_agent_projection(
    snapshot: &SidebarSnapshot,
    frame_panes: Vec<crate::pane::PaneRef>,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> (
    Vec<crate::pane::PaneRef>,
    super::agent_projection::AgentProjection,
) {
    let panes = snapshot.card_admitted_live_panes(frame_panes, exclude);
    let projection = super::agent_projection::read_published(runtime, session, &panes);
    (panes, projection)
}
