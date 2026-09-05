//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by `rimz reset` and attached `rimz start` auto-reset, so teardown
//! lives in exactly one place and is testable without a real multiplexer.

use std::path::PathBuf;

use crate::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::mux::MuxBackend;

/// What [`teardown_room`] removed, for the user-facing `rimz reset` report.
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
pub fn teardown_room(
    backend: &dyn MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
    runtime: &RuntimePaths,
) -> TeardownReport {
    // Delete the session first, so the only server matching this exact name in
    // the sweep below is the corpse — never a freshly-born replacement.
    let session_killed = backend.kill_session(session_name).is_ok();
    let cache_removed = backend.purge_resurrection_cache(session_name);
    crate::sidebar::sweep_orphan_runtime(runtime);
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
