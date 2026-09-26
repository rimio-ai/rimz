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
    /// Orphaned server / leaked daemon pids signalled.
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
    teardown_runtime(
        backend,
        &workspace.workspace_id,
        &workspace.session_name,
        &runtime,
    );
    if let Some(recorded) = recorded
        && recorded.session_name != workspace.session_name
    {
        teardown_runtime(
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
/// Beside the runtime report comes the outcome of removing room tmp and
/// rewritten skill copies (already gone is `Ok`), which only this full
/// teardown attempts.
pub fn teardown_room(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    runtime: &RuntimePaths,
    state: &StatePaths,
) -> (TeardownReport, Result<(), PathErr>) {
    let report = teardown_runtime(backend, workspace_id, session_name, runtime);
    (report, state.remove_tmp_dir())
}

pub(super) fn teardown_runtime(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    runtime: &RuntimePaths,
) -> TeardownReport {
    // Delete the session first, so the only server matching this exact name in
    // the sweep below is the corpse — never a freshly-born replacement.
    let session_killed = backend.kill_session(session_name).is_ok();
    let cache_removed = backend.purge_resurrection_cache(session_name);
    if let Err(err) = runtime.remove_disposable_dirs() {
        tracing::warn!(error = %err, "room runtime removal failed");
    }
    // The session is already a corpse (killed above), so sweeping its lingering
    // mux server is cleanup, not destruction.
    let processes_swept =
        crate::mux::recovery::sweep_orphan_processes(workspace_id.as_str(), session_name, true);
    TeardownReport {
        session_killed,
        cache_removed,
        processes_swept,
    }
}
