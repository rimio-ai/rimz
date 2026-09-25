//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by `rimz reset` and attached `rimz start` auto-reset, so teardown
//! lives in exactly one place and is testable without a real multiplexer.

use std::path::PathBuf;

use crate::disk::paths::PathErr;
use crate::ids::WorkspaceId;
use crate::mux::MuxBackend;
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
