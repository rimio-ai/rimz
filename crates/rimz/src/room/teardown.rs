//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by reset, attended auto-reset, and incompatible-room replacement.
//! Multiplexer teardown is proven in the integration tier.

use std::path::PathBuf;

use crate::disk::paths::{PathErr, check_workspace_layout};
use crate::ids::{WorkspaceDirName, WorkspaceId};
use crate::mux::MuxBackend;
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimePaths, StatePaths};

/// What the runtime teardown removed, for the user-facing `rimz reset` report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeardownReport {
    /// The session was deleted (or was already gone).
    pub session_killed: bool,
    /// Resurrection-cache paths removed.
    pub cache_removed: Vec<PathBuf>,
    /// Orphaned servers, daemons, and hook writers signalled.
    pub processes_swept: Vec<u32>,
}

/// The room replaced because an older layout wrote it.
pub struct ReplacedRoom {
    pub dir_name: WorkspaceDirName,
    pub layout: u32,
}

/// Tear down an incompatible room before birth, removing both whole trees.
/// Current-layout rooms and rooms without a record are left untouched.
pub fn replace_incompatible_room(
    backend: &dyn MuxBackend,
    workspace: &ResolvedWorkspace,
) -> Result<Option<ReplacedRoom>, PathErr> {
    let state = StatePaths::for_project_root(&workspace.project_root)?;
    let layout = match check_workspace_layout(&state.root) {
        Err(PathErr::Layout { layout, .. }) => layout,
        Err(err) => return Err(err),
        Ok(_) => return Ok(None),
    };
    let runtime = RuntimePaths::for_state(&state)?;
    #[derive(serde::Deserialize)]
    struct RecordedSession {
        session_name: String,
    }
    let recorded = std::fs::read(&state.workspace_record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RecordedSession>(&bytes).ok());
    teardown_room(
        backend,
        &workspace.workspace_id,
        &workspace.session_name,
        &runtime,
    );
    if let Some(recorded) = recorded
        && recorded.session_name != workspace.session_name
    {
        teardown_room(
            backend,
            &workspace.workspace_id,
            &recorded.session_name,
            &runtime,
        );
    }
    runtime.remove_root()?;
    state.remove_root()?;
    Ok(Some(ReplacedRoom {
        dir_name: state.dir_name,
        layout,
    }))
}

/// Tear the room down to a clean slate: delete the session, purge the backend's
/// resurrection cache, reap stale sidebar runtime files, and sweep orphaned
/// servers / leaked daemons scoped to this workspace. Every step is best-effort
/// and independent — a failure in one never blocks the others — so a later
/// rebirth always starts from the cleanest state reachable.
///
/// Room state (temp units, `shared/`, `out/`) is left for GC.
pub fn teardown_room(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    runtime: &RuntimePaths,
) -> TeardownReport {
    let paths = StatePaths::under_named(
        workspace_id.clone(),
        runtime.dir_name.clone(),
        &crate::disk::paths::rimz_home(),
    );
    let roster = crate::store::live_roster::read(&paths.live_roster);
    // Delete the session first, so the only server matching this exact name in
    // the sweep below is the corpse — never a freshly-born replacement.
    let session_killed = backend.kill_session(session_name).is_ok();
    let cache_removed = backend.purge_resurrection_cache(session_name);
    // The session is already a corpse (killed above), so sweeping its lingering
    // mux server is cleanup, not destruction.
    let processes_swept = crate::mux::recovery::sweep_orphan_processes(
        workspace_id.as_str(),
        session_name,
        crate::mux::recovery::SweepScope::Teardown,
    )
    .signalled;
    // A dying producer can publish after kill_session returns. Restore only
    // after the sweep's exit barrier, before callers touch workspace records.
    if let Some(roster) = roster
        && let Err(err) = crate::store::live_roster::publish(&paths.live_roster, roster.agents)
    {
        tracing::warn!(error = %err, "could not restore the pre-teardown live roster");
    }
    match crate::store::ingress::stop_drainer(runtime) {
        Ok(_drainer) => {
            if let Err(err) = runtime.remove_disposable_dirs() {
                tracing::warn!(error = %err, "room runtime removal failed");
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "could not stop hook drainer; retaining room runtime")
        }
    }
    TeardownReport {
        session_killed,
        cache_removed,
        processes_swept,
    }
}
