//! Zellij session discovery and topology-cache reads.

use std::time::{Duration, Instant};

use super::pane_topology::PaneTopologyCache;
use super::pane_topology::PaneTopologyPane;
use super::parse::{is_no_active_sessions, session_state_from_line};
use super::{TOPOLOGY_CACHE_POLL_STEP, ZellijBackend, health_probe_timeout};
use crate::config::{MachineConfig, MultiplexerConfig};
use crate::ids::WorkspaceId;
use crate::mux::{MuxErr, Result, SessionLiveness};
use crate::mux::{PaneReadConsistency, PresencePluginOptions};
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
    ) -> Result<Vec<PaneTopologyPane>> {
        self.topology_listing(
            Some(session),
            None,
            None,
            min_topology_produced_at_ms,
            timeout,
        )
        .map(|listing| listing.panes)
    }

    pub(super) fn topology_panes_for_workspace(
        &self,
        session: &str,
        workspace_id: &WorkspaceId,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<Vec<PaneTopologyPane>> {
        self.topology_listing(
            Some(session),
            None,
            Some(workspace_id),
            min_topology_produced_at_ms,
            timeout,
        )
        .map(|listing| listing.panes)
    }

    pub(super) fn topology_listing(
        &self,
        session: Option<&str>,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<PaneTopologyCache> {
        self.read_topology(
            session,
            runtime_paths,
            workspace_id,
            min_topology_produced_at_ms,
            PaneReadConsistency::Cached,
            timeout,
        )
    }

    pub(super) fn read_topology(
        &self,
        session: Option<&str>,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        min_topology_produced_at_ms: Option<u64>,
        consistency: PaneReadConsistency,
        timeout: Duration,
    ) -> Result<PaneTopologyCache> {
        let session = self.resolve_topology_session(session)?;
        match consistency {
            PaneReadConsistency::Cached => self.cached_topology(
                session,
                runtime_paths,
                workspace_id,
                min_topology_produced_at_ms,
                timeout,
            ),
            PaneReadConsistency::PreferAuthoritative => self
                .authoritative_pane_listing(
                    &session,
                    runtime_paths,
                    workspace_id,
                    timeout,
                )
                .or_else(|err| {
                    tracing::debug!(session = %session, error = %err, "authoritative Zellij pane listing failed; falling back to topology cache");
                    self.cached_topology(
                        session,
                        runtime_paths,
                        workspace_id,
                        min_topology_produced_at_ms,
                        timeout,
                    )
                }),
            PaneReadConsistency::RequireAuthoritative => self.authoritative_pane_listing(
                &session,
                runtime_paths,
                workspace_id,
                timeout,
            ),
        }
    }

    fn cached_topology(
        &self,
        session: String,
        runtime_paths: Option<&RuntimePaths>,
        workspace_id: Option<&WorkspaceId>,
        min_topology_produced_at_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<PaneTopologyCache> {
        let known = self.resolve_topology_workspace(&session, workspace_id)?;
        let runtime_storage;
        let runtime = match runtime_paths {
            Some(runtime) => runtime,
            None => {
                runtime_storage = self.runtime_paths_for_workspace(known.workspace_id.clone())?;
                &runtime_storage
            }
        };
        let now_ms = unix_now_ms();
        if let Some(cache) =
            Self::fresh_cached_topology(runtime, &session, now_ms, min_topology_produced_at_ms)
        {
            return Ok(cache);
        }
        if self.session_state(&session) != SessionLiveness::Live {
            return Err(MuxErr::SessionNotFound { session });
        }
        let floor_ms = min_topology_produced_at_ms.unwrap_or(now_ms);
        self.request_topology_dump(&known);
        let deadline = Instant::now() + timeout;
        loop {
            let now_ms = unix_now_ms();
            if let Some(cache) =
                Self::fresh_cached_topology(runtime, &session, now_ms, Some(floor_ms))
            {
                return Ok(cache);
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

    pub(super) fn fresh_cached_topology(
        runtime: &RuntimePaths,
        session: &str,
        now_ms: u64,
        min_topology_produced_at_ms: Option<u64>,
    ) -> Option<PaneTopologyCache> {
        read_pane_topology_cache(runtime, session).filter(|cache| {
            pane_topology_cache_is_fresh(cache, now_ms, min_topology_produced_at_ms)
        })
    }

    pub(super) fn resolve_topology_session(&self, session: Option<&str>) -> Result<String> {
        if let Some(session) = session.filter(|session| !session.is_empty()) {
            return Ok(session.to_owned());
        }
        std::env::var("ZELLIJ_SESSION_NAME")
            .ok()
            .filter(|session| !session.is_empty())
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: "pane listing on Zellij needs a RimZ room session".to_owned(),
            })
    }

    pub(super) fn resolve_topology_workspace(
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
                updated_at: jiff::Timestamp::now(),
            });
        }
        self.known_workspaces()
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!("reading RimZ workspace registry: {err}"),
            })?
            .into_iter()
            .find(|known| known.session_name == session)
            .ok_or_else(|| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: format!(
                    "pane listing on Zellij needs a RimZ room; found no workspace for session `{session}`"
                ),
            })
    }

    fn known_workspaces(&self) -> std::io::Result<Vec<KnownWorkspace>> {
        match &self.runtime_dir {
            Some(root) => workspace::known_workspaces_under(&paths::workspaces_dir_under(root)),
            None => workspace::known_workspaces(),
        }
    }

    pub(super) fn runtime_paths_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<RuntimePaths> {
        match &self.runtime_dir {
            Some(dir) => RuntimePaths::under(workspace_id, dir),
            None => RuntimePaths::for_workspace(workspace_id),
        }
        .map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("resolving RimZ runtime paths: {err}"),
        })
    }

    pub(super) fn state_paths_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<StatePaths> {
        match &self.runtime_dir {
            Some(dir) => StatePaths::under(workspace_id, dir),
            None => StatePaths::for_workspace(workspace_id),
        }
        .map_err(|err| MuxErr::Output {
            program: "zellij".to_owned(),
            reason: format!("resolving RimZ state paths: {err}"),
        })
    }

    fn recorded_rimz_bin(&self, workspace_id: &WorkspaceId) -> Option<std::path::PathBuf> {
        let paths = self.state_paths_for_workspace(workspace_id.clone()).ok()?;
        crate::store::workspace_record::read(&paths.workspace_record)
            .ok()
            .and_then(|record| record.rimz_bin)
    }

    fn request_topology_dump(&self, known: &KnownWorkspace) {
        let Some(wasm) = self.presence_plugin_path() else {
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
            rimz_bin: workspace::resolve_recorded_rimz_bin(
                &known.workspace_id,
                known.rimz_bin.as_deref(),
            ),
            converge: false,
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

    /// Prove `name`'s live room can be inspected through a bounded pane listing.
    /// A failed or timed-out listing preserves the uninspectable room; any
    /// successful inspection lets the slow-path caller rebirth it.
    pub(super) fn inspect_session_panes(
        &self,
        name: &str,
        workspace_id: &WorkspaceId,
    ) -> Result<()> {
        self.topology_panes_for_workspace(name, workspace_id, None, health_probe_timeout())
            .map(drop)
    }

    /// Lossy birth-path classification. A probe failure leaves the existing
    /// fail-open birth behaviour intact by reading as absence.
    pub(super) fn session_state(&self, name: &str) -> SessionLiveness {
        self.session_state_checked(name)
            .unwrap_or(SessionLiveness::Absent)
    }

    /// Classify `name` from `zellij list-sessions` while preserving command
    /// failures. Zellij's exit-1 no-sessions response is definitive absence;
    /// timeouts and every other failure remain unavailable to callers.
    pub(super) fn session_state_checked(&self, name: &str) -> Result<SessionLiveness> {
        let output = match self.cmd().args(["list-sessions", "--no-formatting"]).run() {
            Ok(output) => output,
            Err(MuxErr::Command { ref stderr, .. }) if is_no_active_sessions(stderr.as_bytes()) => {
                return Ok(SessionLiveness::Absent);
            }
            Err(err) => return Err(err),
        };
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| session_state_from_line(line, name))
            .unwrap_or(SessionLiveness::Absent))
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
