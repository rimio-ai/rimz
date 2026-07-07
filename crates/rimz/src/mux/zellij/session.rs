//! Zellij session discovery and topology-cache reads.

use std::time::{Duration, Instant};

use super::parse::{SessionState, session_state_from_line};
use super::raw_pane::{RawPaneListing, SessionCleanliness, classify_session_panes};
use super::{TOPOLOGY_CACHE_POLL_STEP, ZellijBackend, health_probe_timeout};
use crate::config::{MachineConfig, MultiplexerConfig};
use crate::ids::WorkspaceId;
use crate::mux::PresencePluginOptions;
use crate::mux::{MuxErr, Result};
use crate::sidebar::cache::{pane_topology_cache_is_fresh, read_pane_topology_cache};
use crate::sidebar::timing::unix_now_ms;
use crate::store::paths::{self, RuntimePaths, StatePaths};
use crate::workspace::{self, KnownWorkspace};

impl ZellijBackend {
    pub(super) fn topology_panes(
        &self,
        session: &str,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<Vec<super::raw_pane::RawPane>> {
        self.topology_listing(Some(session), None, min_topology_produced_at_ms, timeout)
            .map(|listing| listing.panes)
    }

    pub(super) fn topology_panes_for_workspace(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<Vec<super::raw_pane::RawPane>> {
        self.topology_listing(
            Some(session),
            Some(workspace_id),
            min_topology_produced_at_ms,
            timeout,
        )
        .map(|listing| listing.panes)
    }

    pub(super) fn topology_listing(
        &self,
        session: Option<&str>,
        workspace_id: Option<&WorkspaceId>,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<RawPaneListing> {
        let session = self.resolve_topology_session(session)?;
        let known = self.resolve_topology_workspace(&session, workspace_id)?;
        let runtime = self.runtime_paths_for_workspace(known.workspace_id.clone())?;
        let now_ms = unix_now_ms();
        if let Some(cache) = read_pane_topology_cache(&runtime, &session)
            && pane_topology_cache_is_fresh(&cache, now_ms, min_topology_produced_at_ms)
        {
            return Ok(RawPaneListing::from_topology(cache));
        }
        if self.session_state(&session) != SessionState::Live {
            return Err(MuxErr::SessionNotFound { session });
        }
        let floor_ms = min_topology_produced_at_ms.unwrap_or(now_ms);
        self.request_topology_dump(&known);
        let deadline = Instant::now() + timeout;
        loop {
            let now_ms = unix_now_ms();
            if let Some(cache) = read_pane_topology_cache(&runtime, &session)
                && pane_topology_cache_is_fresh(&cache, now_ms, Some(floor_ms))
            {
                return Ok(RawPaneListing::from_topology(cache));
            }
            if Instant::now() >= deadline {
                return Err(MuxErr::Output {
                    program: "zellij".to_owned(),
                    reason: format!(
                        "Zellij topology unavailable for session `{session}`; run `rimz doctor`"
                    ),
                });
            }
            std::thread::sleep(TOPOLOGY_CACHE_POLL_STEP);
        }
    }

    fn resolve_topology_session(&self, session: Option<&str>) -> Result<String> {
        if let Some(session) = session.filter(|session| !session.is_empty()) {
            return Ok(session.to_owned());
        }
        std::env::var("ZELLIJ_SESSION_NAME")
            .ok()
            .filter(|session| !session.is_empty())
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: "pane listing on Zellij needs a Rimz room session".to_owned(),
            })
    }

    fn resolve_topology_workspace(
        &self,
        session: &str,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<KnownWorkspace> {
        if let Some(workspace_id) = workspace_id {
            return Ok(KnownWorkspace {
                workspace_id: workspace_id.clone(),
                project_root: std::path::PathBuf::new(),
                session_name: session.to_owned(),
                root_class: workspace::RootClass::Directory,
                rimz_bin: self.recorded_rimz_bin(workspace_id),
            });
        }
        self.known_workspaces()
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("reading Rimz workspace registry: {err}"),
            })?
            .into_iter()
            .find(|known| known.session_name == session)
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!(
                    "pane listing on Zellij needs a Rimz room; found no workspace for session `{session}`"
                ),
            })
    }

    fn known_workspaces(&self) -> std::io::Result<Vec<KnownWorkspace>> {
        match &self.runtime_dir {
            Some(root) => workspace::known_workspaces_under(&paths::workspaces_dir_under(root)),
            None => workspace::known_workspaces(),
        }
    }

    fn runtime_paths_for_workspace(&self, workspace_id: WorkspaceId) -> Result<RuntimePaths> {
        match &self.runtime_dir {
            Some(dir) => RuntimePaths::under(workspace_id, dir),
            None => RuntimePaths::for_workspace(workspace_id),
        }
        .map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("resolving Rimz runtime paths: {err}"),
        })
    }

    fn recorded_rimz_bin(&self, workspace_id: &WorkspaceId) -> Option<std::path::PathBuf> {
        let paths = match &self.runtime_dir {
            Some(dir) => StatePaths::under(workspace_id.clone(), dir),
            None => StatePaths::for_workspace(workspace_id.clone()),
        }
        .ok()?;
        crate::store::workspace_record::read(&paths.workspace_record)
            .ok()
            .and_then(|record| record.rimz_bin)
    }

    fn request_topology_dump(&self, known: &KnownWorkspace) {
        let Some(wasm) = super::presence_plugin_path() else {
            tracing::debug!(
                session = %known.session_name,
                "Zellij topology refresh skipped because the presence plugin artifact is unavailable",
            );
            return;
        };
        let machine_config = MachineConfig::load_lenient();
        let mux_config = MultiplexerConfig::from(machine_config.as_ref());
        let opts = PresencePluginOptions {
            session_name: known.session_name.clone(),
            workspace_id: known.workspace_id.clone(),
            wasm,
            rimz_bin: workspace::resolve_recorded_rimz_bin(known.rimz_bin.as_deref()),
            converge: false,
            seed_permissions: machine_config.web.enabled,
            focus_key: machine_config.sidebar.focus_key_label().map(str::to_owned),
            focus_follows_mouse: mux_config.zellij.focus_follows_mouse,
            mouse_click_through: mux_config.zellij.mouse_click_through,
        };
        if let Err(err) = self.dump_topology_for(&opts) {
            tracing::debug!(
                session = %known.session_name,
                error = %err,
                "Zellij topology refresh pipe failed",
            );
        }
    }

    /// Classify `name`'s live room from a bounded pane listing. A running
    /// live sidebar chrome pane plus no held command pane is clean. A held
    /// sidebar means Zellij is waiting on the user (no heartbeats); a held command
    /// pane is the resurrection fingerprint — Zellij brought a serialized room
    /// back with `start_suspended` panes. Either inspected condition makes the
    /// room non-functional and safe to rebirth.
    ///
    /// A failed or timed-out listing is different: the room is uninspectable, not
    /// proven stale. Preserve it and let the caller surface the stuck-room path
    /// rather than force-deleting panes it could not see.
    pub(super) fn session_cleanliness(
        &self,
        name: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<SessionCleanliness> {
        self.topology_panes_for_workspace(name, workspace_id, None, health_probe_timeout())
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
