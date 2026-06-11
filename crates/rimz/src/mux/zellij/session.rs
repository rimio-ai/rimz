//! Zellij session discovery and pane-list reads.

use std::time::Duration;

use super::parse::{SessionState, is_transient_empty, session_state_from_line};
use super::raw_pane::{
    RawPane, RawPaneListing, SessionCleanliness, classify_session_panes, read_fresh_topology_cache,
};
use super::{HEALTH_PROBE_TIMEOUT, LIST_PANES_ATTEMPTS, LIST_PANES_RETRY_DELAY, ZellijBackend};
use crate::ids::WorkspaceId;
use crate::mux::{MuxErr, Result};

impl ZellijBackend {
    pub(super) fn list_panes_with_session(&self, session: Option<&str>) -> Result<Vec<RawPane>> {
        self.list_panes_bounded(session, super::super::COMMAND_TIMEOUT)
    }

    /// `list-panes` with a caller-chosen per-attempt bound. The pre-attach health
    /// probe passes [`HEALTH_PROBE_TIMEOUT`] so a hung server cannot stall the
    /// launch; everyone else inherits [`super::super::COMMAND_TIMEOUT`].
    pub(super) fn list_panes_bounded(
        &self,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Vec<RawPane>> {
        let mut spec = self.cmd();
        if let Some(name) = session {
            spec = spec.args(["--session".to_owned(), name.to_owned()]);
        }
        spec = spec.args(["action", "list-panes", "-j", "-a"]);
        for attempt in 0..LIST_PANES_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(LIST_PANES_RETRY_DELAY);
            }
            let output = spec.run_with_timeout(timeout)?;
            if is_transient_empty(&output.stdout) {
                continue;
            }
            return serde_json::from_slice::<Vec<RawPane>>(&output.stdout).map_err(|e| {
                MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!("parsing list-panes JSON: {e}"),
                }
            });
        }
        Err(MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("list-panes returned no output after {LIST_PANES_ATTEMPTS} attempts"),
        })
    }

    pub(super) fn list_panes_cached_or_cli(
        &self,
        session: Option<&str>,
        workspace_id: Option<&WorkspaceId>,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<RawPaneListing> {
        if let Some(panes) = session
            .zip(workspace_id)
            .and_then(|(session, workspace_id)| {
                read_fresh_topology_cache(session, workspace_id, min_topology_produced_at_ms)
            })
        {
            return Ok(panes);
        }
        let observed_at_ms = crate::sidebar::cache::unix_now_ms();
        self.list_panes_bounded(session, timeout)
            .map(|panes| RawPaneListing::from_cli(panes, observed_at_ms))
    }

    /// Classify `name`'s live room from a bounded pane listing. A running
    /// (non-held) `rimz-sidebar` pane plus no held command pane is clean. A held
    /// sidebar means Zellij is waiting on the user (no heartbeats); a held command
    /// pane is the resurrection fingerprint — Zellij brought a serialized room
    /// back with `start_suspended` panes. Either inspected condition makes the
    /// room non-functional and safe to rebirth.
    ///
    /// A failed or timed-out listing is different: the room is uninspectable, not
    /// proven stale. Preserve it and let the caller surface the stuck-room path
    /// rather than force-deleting panes it could not see.
    pub(super) fn session_cleanliness(&self, name: &str) -> Result<SessionCleanliness> {
        self.list_panes_bounded(Some(name), HEALTH_PROBE_TIMEOUT)
            .map(|panes| classify_session_panes(&panes))
    }

    /// Classify `name`'s liveness from `zellij list-sessions`. A present session
    /// always lists with exit code 0; the command only fails ("No active zellij
    /// sessions found.", exit 1) when there are none, so any failure here means
    /// the session is absent and a fresh birth should proceed.
    pub(super) fn session_state(&self, name: &str) -> SessionState {
        let Ok(output) = self.cmd().args(["list-sessions", "--no-formatting"]).run() else {
            return SessionState::Absent;
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| session_state_from_line(line, name))
            .unwrap_or(SessionState::Absent)
    }

    /// Force-delete a session (exited or live) so the next create births a clean
    /// one from the layout rather than resurrecting a stale serialized layout or
    /// attaching to a sidebar-less leftover. `--force` also kills a live session.
    /// A session that vanished between the liveness check and here is already in
    /// the state we want, so "not found" is success.
    pub(super) fn delete_session(&self, name: &str) -> Result<()> {
        match self.cmd().args(["delete-session", name, "--force"]).run() {
            Ok(_) => Ok(()),
            Err(MuxErr::Command { stderr, .. })
                if stderr.to_ascii_lowercase().contains("not found") =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}
