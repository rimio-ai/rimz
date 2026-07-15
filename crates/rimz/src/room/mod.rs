//! Managed room context and lifecycle seam.

mod birth;
pub mod session;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::harness::rebirth::RebirthPlan;
use crate::ids::{MuxName, WorkspaceId};
use crate::mux::{
    BackgroundViewOptions, CommandSpec, MuxBackend, MuxErr, PresencePluginOptions, SessionHealth,
    SessionOptions, SidebarPaneOptions, SidebarWidth,
};
use crate::store::workspace_record;
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimePaths, StatePaths, Store, WorkspaceRecord};

pub use birth::{
    AttendedRecovery, BackgroundViewBirth, BirthOutcome, NormalBirth, NormalRebirth,
    ResetRecoveryError, RoomBirth, RoomResetReport, SupervisedBirth,
};

#[derive(Debug, thiserror::Error)]
pub enum LiveRoomErr {
    #[error(
        "no live Rimz room `{session_name}`; run `rimz start` first or enter one with `rimz attach`"
    )]
    Unavailable { session_name: String },
    #[error(transparent)]
    Mux(#[from] MuxErr),
}

pub type LiveRoomResult<T> = std::result::Result<T, LiveRoomErr>;

/// Select the configured multiplexer and require this workspace's room to be live.
pub fn require_live_mux(
    explicit: Option<MuxName>,
    workspace: &ResolvedWorkspace,
) -> LiveRoomResult<MuxName> {
    let mux = crate::mux::auto_detect_backend(explicit).map_err(|_| LiveRoomErr::Unavailable {
        session_name: workspace.session_name.clone(),
    })?;
    let backend = crate::mux::backend_for(mux);
    require_live_session(backend.as_ref(), &workspace.session_name)?;
    Ok(mux)
}

/// Require one managed room session on an already-selected backend.
pub fn require_live_session(backend: &dyn MuxBackend, session_name: &str) -> LiveRoomResult<()> {
    let sessions = backend.list_sessions()?;
    if sessions.iter().any(|session| session == session_name) {
        Ok(())
    } else {
        Err(LiveRoomErr::Unavailable {
            session_name: session_name.to_owned(),
        })
    }
}

/// Build the room identity pin carried by a pane opened in a managed session.
pub fn pane_identity_env(
    workspace: &ResolvedWorkspace,
    channel: Option<&str>,
    inherit_channel: bool,
) -> BTreeMap<String, String> {
    let ambient_channel = inherit_channel
        .then(|| std::env::var(crate::harness::run::ENV_CHANNEL).ok())
        .flatten();
    pane_identity_env_with_ambient(workspace, channel, ambient_channel.as_deref())
}

fn pane_identity_env_with_ambient(
    workspace: &ResolvedWorkspace,
    channel: Option<&str>,
    ambient_channel: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env = crate::workspace::pin_env(&workspace.workspace_id, &workspace.project_root);
    env.insert("RIMZ".to_owned(), "1".to_owned());
    env.insert(
        crate::harness::run::ENV_WORKTREE_PATH.to_owned(),
        workspace.worktree_root.display().to_string(),
    );
    if let Some(channel) = channel
        .or(ambient_channel)
        .filter(|value| !value.is_empty())
    {
        env.insert(
            crate::harness::run::ENV_CHANNEL.to_owned(),
            channel.to_owned(),
        );
    }
    env
}

/// Terminal sizing policy for room operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomSizing {
    /// Probe once for a room/session or gallery birth.
    Birth,
    /// Leave view sizing unknown for a tab opened inside a live room.
    OrdinaryTab,
}

/// Owned managed-room identity and runtime configuration.
pub struct RoomContext {
    workspace: ResolvedWorkspace,
    backend: Box<dyn MuxBackend>,
    machine_config: Arc<MachineConfig>,
    mux_config: MultiplexerConfig,
    extra_env: std::collections::BTreeMap<String, String>,
    width: SidebarWidth,
    detected_size: Option<(u16, u16)>,
    rimz_bin: PathBuf,
    runtime: RuntimePaths,
    remote_control_readiness: Option<crate::remote_control::ReadinessSnapshot>,
}

impl RoomContext {
    /// Build context from freshly resolved workspace identity.
    pub fn from_resolved(
        workspace: &ResolvedWorkspace,
        machine_config: Arc<MachineConfig>,
        mux: MuxName,
        sizing: RoomSizing,
    ) -> Result<Self> {
        let mut workspace = workspace.clone();
        workspace.mux_hint = Some(mux);
        let rimz_bin = recorded_room_bin(&workspace.workspace_id);
        Self::new(workspace, machine_config, mux, sizing, rimz_bin)
    }

    /// Build context from durable workspace identity.
    pub fn from_record(
        record: &WorkspaceRecord,
        machine_config: Arc<MachineConfig>,
        mux: MuxName,
        sizing: RoomSizing,
    ) -> Result<Self> {
        let workspace = Self::workspace_from_record(record, mux);
        Self::new(
            workspace,
            machine_config,
            mux,
            sizing,
            crate::workspace::resolve_recorded_rimz_bin(record.rimz_bin.as_deref()),
        )
    }

    fn workspace_from_record(record: &WorkspaceRecord, mux: MuxName) -> ResolvedWorkspace {
        ResolvedWorkspace {
            workspace_id: record.workspace_id.clone(),
            project_root: record.project_root.clone(),
            root_class: record.root_class,
            worktree_root: record
                .worktree_root
                .clone()
                .unwrap_or_else(|| record.project_root.clone()),
            worktree_branch: None,
            session_name: record.session_name.clone(),
            mux_hint: Some(mux),
        }
    }

    fn new(
        workspace: ResolvedWorkspace,
        machine_config: Arc<MachineConfig>,
        mux: MuxName,
        sizing: RoomSizing,
        rimz_bin: PathBuf,
    ) -> Result<Self> {
        let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())
            .context("preparing adapter runtime paths")?;
        runtime
            .ensure_dirs()
            .context("preparing adapter runtime directories")?;
        let mux_config = MultiplexerConfig::from(machine_config.as_ref());
        let width = SidebarWidth::from_config(&machine_config.theme.display);
        let detected_size = match sizing {
            RoomSizing::Birth => crate::mux::detect_terminal_size(),
            RoomSizing::OrdinaryTab => None,
        };
        let extra_env = crate::agents::registry::room_env(&runtime);
        Ok(Self {
            workspace,
            backend: crate::mux::backend_for(mux),
            machine_config,
            mux_config,
            extra_env,
            width,
            detected_size,
            rimz_bin,
            runtime,
            remote_control_readiness: None,
        })
    }

    /// Claim this room for the running RimZ binary and durably record it.
    pub fn claim_owner(&mut self) -> Result<()> {
        let rimz_bin = crate::reload::current_reexec_target().unwrap_or_else(crate::proc::rimz_exe);
        let paths = StatePaths::for_workspace(self.workspace.workspace_id.clone())
            .context("preparing store paths")?;
        let store = Store::open(paths, self.runtime.clone()).context("opening store")?;
        store
            .record_room_bin(&self.workspace, rimz_bin.clone())
            .context("recording room binary")?;
        self.rimz_bin = rimz_bin;
        Ok(())
    }

    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace.workspace_id
    }

    pub fn session_name(&self) -> &str {
        &self.workspace.session_name
    }

    pub fn mux_name(&self) -> MuxName {
        self.backend.name()
    }

    pub fn backend(&self) -> &dyn MuxBackend {
        self.backend.as_ref()
    }

    pub fn set_remote_control_readiness(
        &mut self,
        readiness: crate::remote_control::ReadinessSnapshot,
    ) {
        self.remote_control_readiness = Some(readiness);
    }

    /// Probe a selected backend before first-run config can construct final context.
    pub fn session_is_healthy_live(mux: MuxName, session_name: &str) -> bool {
        let backend = crate::mux::backend_for(mux);
        Self::probe_healthy_live(backend.as_ref(), session_name)
    }

    fn probe_healthy_live(backend: &dyn MuxBackend, session_name: &str) -> bool {
        let exists = backend
            .list_sessions()
            .map(|sessions| sessions.iter().any(|name| name == session_name))
            .unwrap_or(false);
        exists
            && matches!(
                backend.probe_session_health(session_name),
                Ok(SessionHealth::Healthy)
            )
    }

    /// Inspect previous incarnation state without mutating it.
    pub fn inspect_rebirth(
        &self,
        disabled: bool,
    ) -> std::result::Result<RebirthPlan, crate::harness::rebirth::RebirthErr> {
        RebirthPlan::inspect(
            self.backend.as_ref(),
            &self.workspace.workspace_id,
            &self.workspace.session_name,
            &self.workspace.project_root,
            &self.machine_config,
            disabled,
        )
    }

    /// Build options for an ordinary tab inside this room.
    pub fn sidebar_options(
        &self,
        cwd: &Path,
        resume_tabs: Vec<crate::mux::ResumeTab>,
        refresh_ms: Option<u16>,
    ) -> SidebarPaneOptions {
        self.sidebar_options_with_size(cwd, resume_tabs, refresh_ms, self.detected_size)
    }

    fn sidebar_options_with_size(
        &self,
        cwd: &Path,
        resume_tabs: Vec<crate::mux::ResumeTab>,
        refresh_ms: Option<u16>,
        detected_size: Option<(u16, u16)>,
    ) -> SidebarPaneOptions {
        let width_override = crate::sidebar::width_override::load(&self.runtime);
        SidebarPaneOptions {
            session_name: self.workspace.session_name.clone(),
            workspace_id: self.workspace.workspace_id.clone(),
            project_root: self.workspace.project_root.clone(),
            extra_env: self.extra_env.clone(),
            cwd: cwd.to_path_buf(),
            width: self.width,
            birth_size: self
                .width
                .birth_size_with_override(detected_size.map(|(cols, _)| cols), width_override),
            width_override,
            rimz_bin: self.rimz_bin.clone(),
            replace_existing: false,
            pristine_birth: false,
            config: self.mux_config.clone(),
            resume_tabs,
            refresh_ms,
        }
    }

    fn session_options(&self, cwd: &Path) -> SessionOptions {
        SessionOptions {
            session_name: self.workspace.session_name.clone(),
            workspace_id: self.workspace.workspace_id.clone(),
            project_root: self.workspace.project_root.clone(),
            extra_env: self.extra_env.clone(),
            cwd: cwd.to_path_buf(),
            config: self.mux_config.clone(),
            detected_size: self.detected_size,
            truecolor: crate::tui::truecolor(),
        }
    }

    /// Build and return attach command after clearing stale resurrection state.
    pub fn prepare_attach(&self) -> CommandSpec {
        let cache_removed = self
            .backend
            .purge_resurrection_cache(&self.workspace.session_name);
        if !cache_removed.is_empty() {
            tracing::debug!(
                session = %self.workspace.session_name,
                paths = ?cache_removed,
                "purged stale resurrection cache before attach",
            );
        }
        self.backend
            .attach_command(&self.workspace.session_name, &self.mux_config)
    }

    /// Ask the backend to enable browser sharing for this room.
    pub fn share_web(&self) -> bool {
        let Some(opts) = self.presence_options(true) else {
            tracing::debug!(
                session = %self.workspace.session_name,
                "presence plugin unavailable; Zellij web sharing was not requested",
            );
            return false;
        };
        if let Err(err) = self.backend.share_web_session(&opts) {
            tracing::debug!(session = %self.workspace.session_name, error = %err, "Zellij web-sharing pipe failed");
            return false;
        }
        true
    }

    fn presence_options(&self, materialize_artifact: bool) -> Option<PresencePluginOptions> {
        let wasm = if materialize_artifact {
            crate::mux::zellij::ensure_presence_plugin_artifact()?
        } else {
            crate::mux::zellij::presence_plugin_path()?
        };
        Some(PresencePluginOptions {
            session_name: self.workspace.session_name.clone(),
            workspace_id: self.workspace.workspace_id.clone(),
            wasm,
            rimz_bin: self.rimz_bin.clone(),
            converge: false,
            seed_permissions: self.machine_config.web.enabled,
            focus_key: self
                .machine_config
                .sidebar
                .focus_key_label()
                .map(str::to_owned),
            focus_follows_mouse: self.machine_config.zellij.focus_follows_mouse,
            mouse_click_through: self.machine_config.zellij.mouse_click_through,
        })
    }

    fn load_presence(&self) {
        let Some(opts) = self.presence_options(false) else {
            tracing::debug!(
                session = %self.workspace.session_name,
                "presence plugin unavailable; the producer keeps its pane poll",
            );
            return;
        };
        if let Err(err) = self.backend.ensure_presence_plugin(&opts) {
            tracing::debug!(
                session = %self.workspace.session_name,
                error = %err,
                "presence plugin load failed; the producer keeps its pane poll",
            );
        }
    }

    fn register_focus_key(&self) {
        let Some(label) = self.machine_config.sidebar.focus_key_label() else {
            return;
        };
        let rimz_bin = crate::proc::rimz_exe();
        let Some(binding) = crate::mux::FocusKeyBinding::resolve(label, &rimz_bin) else {
            tracing::warn!(
                focus_key = label,
                "ignoring invalid [sidebar] focus_key; expected e.g. Alt+p"
            );
            return;
        };
        if let Err(err) = self.backend.register_focus_key(&binding) {
            tracing::debug!(error = %err, "registering the focus-sidebar keybind failed");
        }
    }

    /// Assemble configured daemon view for a normal start flow.
    fn background_view(&self, refresh_ms: Option<u16>) -> BackgroundViewOptions {
        let rimz_bin = self.rimz_bin.clone();
        let remote_control = self.remote_control_readiness.clone().unwrap_or_else(|| {
            crate::remote_control::ReadinessSnapshot::probe(&self.machine_config.remote_control)
        });
        BackgroundViewOptions {
            view: crate::daemon_view::daemon_view_spec(crate::daemon_view::DaemonViewSpecParams {
                remote_control: &remote_control,
                daemon: &self.machine_config.daemon,
                rimz_bin: &rimz_bin,
                workspace_id: &self.workspace.workspace_id,
                session_name: &self.workspace.session_name,
                project_root: &self.workspace.project_root,
                worktree_root: &self.workspace.worktree_root,
                codex_present: which::which("codex").is_ok(),
            }),
            sidebar: self.sidebar_options(&self.workspace.worktree_root, Vec::new(), refresh_ms),
        }
    }
}

fn recorded_room_bin(workspace_id: &WorkspaceId) -> PathBuf {
    let recorded = StatePaths::for_workspace(workspace_id.clone())
        .ok()
        .and_then(|paths| workspace_record::read(&paths.workspace_record).ok())
        .and_then(|record| record.rimz_bin);
    crate::workspace::resolve_recorded_rimz_bin(recorded.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::RootClass;

    fn workspace() -> ResolvedWorkspace {
        let project_root = PathBuf::from("/code/rimz");
        ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            root_class: RootClass::Repo,
            worktree_root: project_root.join("../rimz-worktrees/demo"),
            worktree_branch: Some("demo".to_owned()),
            session_name: "rimz-rimz".to_owned(),
            mux_hint: None,
        }
    }

    #[test]
    fn pane_identity_pins_workspace_and_prefers_explicit_channel() {
        let workspace = workspace();

        let env = pane_identity_env_with_ambient(&workspace, Some("explicit"), Some("ambient"));

        assert_eq!(env.get("RIMZ").map(String::as_str), Some("1"));
        assert_eq!(
            env.get(crate::workspace::ENV_WORKSPACE_ID)
                .map(String::as_str),
            Some(workspace.workspace_id.as_str())
        );
        assert_eq!(
            env.get(crate::workspace::ENV_PROJECT_ROOT)
                .map(String::as_str),
            Some("/code/rimz")
        );
        assert_eq!(
            env.get(crate::harness::run::ENV_WORKTREE_PATH)
                .map(String::as_str),
            Some("/code/rimz/../rimz-worktrees/demo")
        );
        assert_eq!(
            env.get(crate::harness::run::ENV_CHANNEL)
                .map(String::as_str),
            Some("explicit")
        );
    }

    #[test]
    fn pane_identity_inherits_nonempty_ambient_channel_only_when_supplied() {
        let workspace = workspace();

        let inherited = pane_identity_env_with_ambient(&workspace, None, Some("ambient"));
        let empty = pane_identity_env_with_ambient(&workspace, None, Some(""));
        let scoped = pane_identity_env_with_ambient(&workspace, None, None);

        assert_eq!(
            inherited
                .get(crate::harness::run::ENV_CHANNEL)
                .map(String::as_str),
            Some("ambient")
        );
        assert!(!empty.contains_key(crate::harness::run::ENV_CHANNEL));
        assert!(!scoped.contains_key(crate::harness::run::ENV_CHANNEL));
    }

    #[test]
    fn record_to_workspace_preserves_identity_and_normalizes_missing_fields() {
        let project_root = PathBuf::from("/code/rimz");
        let record = WorkspaceRecord {
            workspace_id: WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            worktree_root: None,
            session_name: "rimz-rimz".to_owned(),
            root_class: RootClass::Marker,
            rimz_bin: Some(PathBuf::from("/opt/rimz/bin/rimz")),
            updated_at: jiff::Timestamp::now(),
        };

        let workspace = RoomContext::workspace_from_record(&record, MuxName::Tmux);

        assert_eq!(workspace.workspace_id, record.workspace_id);
        assert_eq!(workspace.project_root, project_root);
        assert_eq!(workspace.worktree_root, project_root);
        assert_eq!(workspace.root_class, RootClass::Marker);
        assert_eq!(workspace.session_name, record.session_name);
        assert_eq!(workspace.worktree_branch, None);
        assert_eq!(workspace.mux_hint, Some(MuxName::Tmux));
    }

    #[test]
    fn live_room_errors_keep_command_guidance() {
        let unavailable = LiveRoomErr::Unavailable {
            session_name: "rimz-demo".to_owned(),
        };
        let mux = LiveRoomErr::Mux(MuxErr::NoMuxFound);
        let expected =
            "no live Rimz room `rimz-demo`; run `rimz start` first or enter one with `rimz attach`";

        assert_eq!(unavailable.to_string(), expected);
        assert!(std::error::Error::source(&unavailable).is_none());
        assert_eq!(mux.to_string(), MuxErr::NoMuxFound.to_string());
        assert!(matches!(mux, LiveRoomErr::Mux(MuxErr::NoMuxFound)));
    }
}
