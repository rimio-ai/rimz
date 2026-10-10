//! Destroy every trace of a possibly-corrupt room so the next birth is clean.
//! Shared by reset, attended auto-reset, and incompatible-room replacement.
//! Multiplexer teardown is proven in the integration tier.

use std::path::PathBuf;
use std::time::Duration;

use crate::disk::lock::RoomLock;
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

/// An incompatible room exclusively claimed for replacement.
pub struct IncompatibleRoom {
    state: StatePaths,
    runtime: RuntimePaths,
    session_name: String,
    layout: u32,
    _claim: RoomLock,
}

impl IncompatibleRoom {
    /// Claim without waiting, then re-check the layout before admitting replacement.
    /// Current-layout rooms and rooms without a record are left untouched.
    pub fn claim(workspace: &ResolvedWorkspace) -> anyhow::Result<Option<Self>> {
        let state = StatePaths::for_project_root(&workspace.project_root)?;
        match check_workspace_layout(&state.root) {
            Err(PathErr::Layout { .. }) => {}
            Err(err) => return Err(err.into()),
            Ok(_) => return Ok(None),
        }
        let runtime = RuntimePaths::for_state(&state)?;
        let claim = super::session::claim_room(&runtime, &workspace.session_name, Duration::ZERO)?;
        let layout = match check_workspace_layout(&state.root) {
            Err(PathErr::Layout { layout, .. }) => layout,
            Err(err) => return Err(err.into()),
            Ok(_) => return Ok(None),
        };
        Ok(Some(Self {
            state,
            runtime,
            session_name: workspace.session_name.clone(),
            layout,
            _claim: claim,
        }))
    }

    /// Tear down the claimed room and remove both trees before releasing the claim.
    pub fn replace(self, backend: &dyn MuxBackend) -> Result<ReplacedRoom, PathErr> {
        #[derive(serde::Deserialize)]
        struct RecordedSession {
            session_name: String,
        }
        let recorded = std::fs::read(&self.state.workspace_record)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RecordedSession>(&bytes).ok());
        teardown_room(
            backend,
            &self.state.workspace_id,
            &self.session_name,
            &self.runtime,
        );
        if let Some(recorded) = recorded
            && recorded.session_name != self.session_name
        {
            teardown_room(
                backend,
                &self.state.workspace_id,
                &recorded.session_name,
                &self.runtime,
            );
        }
        self.runtime.remove_root()?;
        self.state.remove_root()?;
        Ok(ReplacedRoom {
            dir_name: self.state.dir_name,
            layout: self.layout,
        })
    }
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
