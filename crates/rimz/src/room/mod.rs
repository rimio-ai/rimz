//! Managed room context and lifecycle seam.

mod birth;
mod recovery;
pub mod session;
mod standing;
pub mod teardown;

use std::collections::BTreeMap;
use std::num::NonZeroU16;
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
use crate::remote_control::ReadinessSnapshot;
use crate::workspace::{ResolvedWorkspace, record};
use crate::{RuntimePaths, StatePaths, Store, workspace::record::WorkspaceRecord};

pub use birth::{
    AttendedRecovery, BirthOutcome, NormalRebirth, ResetRecoveryError, RoomBirth, RoomResetReport,
};
pub use recovery::ParkedRecoveryOutcome;
pub use standing::{
    AccountStanding, Deciding, Scope, Scopes, live_agents_by_login, other_live_agents_on,
};

#[derive(Debug, thiserror::Error)]
pub enum LiveRoomErr {
    #[error(
        "no live RimZ room `{session_name}`; run `rimz start` first or enter one with `rimz attach`"
    )]
    Unavailable { session_name: String },
    #[error("reading RimZ workspace records: {source}")]
    WorkspaceRecords {
        #[source]
        source: std::io::Error,
    },
    #[error("reading the agents of live room `{session_name}` at {}", project_root.display())]
    RoomAgents {
        session_name: String,
        project_root: PathBuf,
    },
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
fn require_live_session(backend: &dyn MuxBackend, session_name: &str) -> LiveRoomResult<()> {
    let sessions = backend.list_sessions()?;
    if sessions.iter().any(|session| session == session_name) {
        Ok(())
    } else {
        Err(LiveRoomErr::Unavailable {
            session_name: session_name.to_owned(),
        })
    }
}

/// Decide the provider accounts a room is born under: its recorded defaults win,
/// then the explicit request, then the trusted project's `[accounts]`, then
/// the machine's `[accounts.use]`. Their health is the caller's to judge: room
/// entry checks every selected account, a supervised launch only its own.
pub fn select_birth_logins(
    project_root: &Path,
    machine_config: &MachineConfig,
    requested: &crate::ids::RoomLogins,
    was_live: bool,
) -> Result<crate::ids::RoomLogins> {
    let state = StatePaths::for_project_root(project_root).context("preparing store paths")?;
    let frozen = record::read_optional(&state.workspace_record)
        .context("reading the room's accounts")?
        .and_then(|record| record.logins);
    select_birth_logins_with_frozen(project_root, machine_config, requested, was_live, frozen)
}

/// Select the accounts a room is entered under, then require every one of them
/// usable before anything launches into the room.
pub fn resolve_birth_logins(
    project_root: &Path,
    machine_config: &MachineConfig,
    requested: &crate::ids::RoomLogins,
    was_live: bool,
) -> Result<crate::ids::RoomLogins> {
    let logins = select_birth_logins(project_root, machine_config, requested, was_live)?;
    let ambient = crate::agents::ambient_env();
    for login in
        crate::agents::LoginCatalog::from_config(&machine_config.accounts)?.room(&logins)?
    {
        login.preflight(&ambient)?;
    }
    Ok(logins)
}

/// The layer choosing a fresh room's account, without an explicit request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BirthLoginLayer {
    Project,
    Machine,
    Provider,
}

impl std::fmt::Display for BirthLoginLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Project => "project default",
            Self::Machine => "machine default",
            Self::Provider => "provider default",
        })
    }
}

/// Select one kind's fresh-room account, ignoring any existing room record.
/// The caller checks the selected home's preconditions before switching.
pub fn reset_login_selection(
    project_root: &Path,
    machine_config: &MachineConfig,
    kind: &crate::ids::AgentKind,
) -> Result<(crate::ids::LoginName, BirthLoginLayer)> {
    machine_config.require_readable_core("reset this room's account")?;
    let mut project = project_birth_logins(project_root)?;
    project.retain(|project_kind, _| project_kind == kind);
    let machine = machine_config
        .accounts
        .use_accounts
        .get(kind)
        .map(|name| (kind.clone(), name.clone()))
        .into_iter()
        .collect();
    let catalog = crate::agents::LoginCatalog::from_config(&machine_config.accounts)?;
    let selected =
        catalog.birth_selection(None, &crate::ids::RoomLogins::new(), &project, &machine)?;
    let layer = if project.contains_key(kind) {
        BirthLoginLayer::Project
    } else if machine.contains_key(kind) {
        BirthLoginLayer::Machine
    } else {
        BirthLoginLayer::Provider
    };
    Ok((selected.get(kind).cloned().unwrap_or_default(), layer))
}

fn project_birth_logins(project_root: &Path) -> Result<crate::ids::RoomLogins> {
    match crate::trust::project_logins(project_root)? {
        crate::trust::ProjectLogins::Unconfigured => Ok(crate::ids::RoomLogins::new()),
        crate::trust::ProjectLogins::Apply(logins) => Ok(logins),
        crate::trust::ProjectLogins::Blocked(state) => {
            anyhow::bail!(standing::blocked_project_logins(state))
        }
    }
}

fn select_birth_logins_with_frozen(
    project_root: &Path,
    machine_config: &MachineConfig,
    requested: &crate::ids::RoomLogins,
    was_live: bool,
    mut frozen: Option<crate::ids::RoomLogins>,
) -> Result<crate::ids::RoomLogins> {
    let empty = crate::ids::RoomLogins::new();
    match &frozen {
        None if !was_live => machine_config.require_readable_core("start a new room")?,
        // Named accounts resolve to their homes through the machine config's
        // declarations, which an unreadable file has lost.
        Some(logins) if logins.values().any(|name| !name.is_default()) => {
            machine_config.require_readable_core("start a room on its named accounts")?
        }
        _ => {}
    }
    let catalog = crate::agents::LoginCatalog::from_config(&machine_config.accounts)?;
    if frozen.is_none() && was_live {
        // A room already running before accounts were recorded runs under the
        // provider's own homes; the project cannot re-point it mid-life.
        frozen = Some(catalog.birth_selection(None, &empty, &empty, &empty)?);
    }
    let machine = if frozen.is_some() {
        &empty
    } else {
        &machine_config.accounts.use_accounts
    };
    let project = match frozen {
        Some(_) => empty.clone(),
        None => project_birth_logins(project_root)?,
    };
    Ok(catalog.birth_selection(frozen.as_ref(), requested, &project, machine)?)
}

/// Build the room identity pin carried by a pane opened in a managed session.
pub fn pane_identity_env(
    workspace: &ResolvedWorkspace,
    cwd: &Path,
    channel: Option<&str>,
    inherit_channel: bool,
) -> BTreeMap<String, String> {
    let ambient_channel = inherit_channel
        .then(|| std::env::var(crate::workspace::ENV_CHANNEL).ok())
        .flatten();
    pane_identity_env_with_ambient(workspace, cwd, channel, ambient_channel.as_deref())
}

fn pane_identity_env_with_ambient(
    workspace: &ResolvedWorkspace,
    cwd: &Path,
    channel: Option<&str>,
    ambient_channel: Option<&str>,
) -> BTreeMap<String, String> {
    crate::workspace::pane_pin_env(
        &workspace.workspace_id,
        &workspace.project_root,
        cwd,
        channel.or(ambient_channel),
    )
}

/// Terminal sizing policy for room operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomSizing {
    /// Resolve one terminal-size seed for a room/session or gallery birth.
    Birth,
    /// Leave view sizing unknown for a tab opened inside a live room.
    OrdinaryTab,
}

/// Identity source for a managed room birth.
#[derive(Clone, Copy)]
pub enum RoomBirthSource<'a> {
    Resolved(&'a ResolvedWorkspace),
    Recorded(&'a WorkspaceRecord),
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
}

impl RoomContext {
    /// Prepare birth identity, claiming only freshly resolved workspaces.
    pub fn prepare_birth(
        source: RoomBirthSource<'_>,
        machine_config: Arc<MachineConfig>,
        mux: MuxName,
        logins: Option<&crate::ids::RoomLogins>,
    ) -> Result<Self> {
        let context = match source {
            RoomBirthSource::Resolved(workspace) => {
                let mut context =
                    Self::from_resolved(workspace, machine_config, mux, RoomSizing::Birth)?;
                context.claim_owner()?;
                context
            }
            RoomBirthSource::Recorded(record) => {
                Self::from_record(record, machine_config, mux, RoomSizing::Birth)?
            }
        };
        if let Some(logins) = logins {
            context.freeze_logins(logins)?;
        }
        Ok(context)
    }

    /// Open an ordinary tab context, requiring its room to be live.
    pub fn live_tab(
        workspace: &ResolvedWorkspace,
        machine_config: Arc<MachineConfig>,
        explicit: Option<MuxName>,
    ) -> Result<Self> {
        let mux = crate::mux::auto_detect_backend(explicit)?;
        let context = Self::from_resolved(workspace, machine_config, mux, RoomSizing::OrdinaryTab)?;
        require_live_session(context.backend(), &workspace.session_name)?;
        Ok(context)
    }

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
            crate::workspace::resolve_recorded_rimz_bin(
                &record.workspace_id,
                record.rimz_bin.as_deref(),
            ),
        )
    }

    fn workspace_from_record(record: &WorkspaceRecord, mux: MuxName) -> ResolvedWorkspace {
        ResolvedWorkspace {
            workspace_id: record.workspace_id.clone(),
            project_root: record.project_root.clone(),
            cwd_project_root: None,
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
        let state = StatePaths::for_project_root(&workspace.project_root)
            .context("preparing adapter store paths")?;
        let runtime = RuntimePaths::for_state(&state).context("preparing adapter runtime paths")?;
        runtime
            .ensure_dirs()
            .context("preparing adapter runtime directories")?;
        let mux_config = MultiplexerConfig::from(machine_config.as_ref());
        let width = SidebarWidth::from_config(&machine_config.theme);
        let detected_size = match sizing {
            RoomSizing::Birth => {
                crate::mux::detect_terminal_size().or_else(crate::mux::client_size_from_env)
            }
            RoomSizing::OrdinaryTab => None,
        };
        let mut extra_env = crate::agents::registry::room_env(&runtime);
        extra_env.insert(
            crate::harness::schedule::LOOP_TASK_ENV.to_owned(),
            String::new(),
        );
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
        })
    }

    /// Claim this room for the running RimZ binary and durably record it.
    fn claim_owner(&mut self) -> Result<()> {
        let staged = crate::reload::stage_current_build().context("staging room binary")?;
        let paths = StatePaths::for_project_root(&self.workspace.project_root)
            .context("preparing store paths")?;
        let room_bin = paths.room_bin.clone();
        let store = Store::open(paths, self.runtime.clone()).context("opening store")?;
        store
            .record_room_bin(&self.workspace, staged.path.clone(), staged.build.clone())
            .context("recording room binary")?;
        self.rimz_bin = room_bin;
        Ok(())
    }

    /// Freeze the provider accounts this room launches under, before any
    /// agent is seeded, so every session it stamps reads the same selection.
    pub fn freeze_logins(&self, logins: &crate::ids::RoomLogins) -> Result<()> {
        let paths = StatePaths::for_project_root(&self.workspace.project_root)
            .context("preparing store paths")?;
        let store = Store::open(paths, self.runtime.clone()).context("opening store")?;
        store
            .record_room_logins(&self.workspace, logins)
            .context("recording room accounts")?;
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

    /// Probe a selected backend before first-run config can construct final context.
    /// Refuses an unresponsive live session immediately; other live verdicts
    /// are returned for reuse by the birth gate, while absent and exited
    /// sessions return `None`. The nested workspace-record result preserves
    /// managed, foreign, and unknown ownership for safe recovery guidance.
    pub fn preflight_live_session(
        mux: MuxName,
        session_name: &str,
        workspace_record_id: std::result::Result<Option<&WorkspaceId>, ()>,
    ) -> Result<Option<SessionHealth>> {
        let backend = crate::mux::backend_for(mux);
        if !backend
            .list_sessions()?
            .iter()
            .any(|candidate| candidate == session_name)
        {
            return Ok(None);
        }
        let health = backend.probe_session_health(session_name)?;
        let ownership = match workspace_record_id {
            Ok(Some(_)) => SessionOwnership::Managed,
            Ok(None) => SessionOwnership::External,
            Err(()) => SessionOwnership::Unknown,
        };
        classify_preflight_health(session_name, ownership, health)
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

    /// Open one resume tab in this room's live session.
    pub fn open_resume_tab(
        &self,
        tab: crate::mux::ResumeTab,
        focus: bool,
    ) -> crate::mux::Result<()> {
        let sidebar = self.sidebar_options(&tab.cwd);
        self.backend.open_tab(&crate::mux::TabOptions {
            env: tab.env,
            title: tab.label,
            panes: tab.layout,
            focus,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
    }

    /// Build options for an ordinary tab inside this room.
    pub fn sidebar_options(&self, cwd: &Path) -> SidebarPaneOptions {
        self.sidebar_options_with_resume(cwd, Vec::new(), None)
    }

    fn sidebar_options_with_resume(
        &self,
        cwd: &Path,
        resume_tabs: Vec<crate::mux::ResumeTab>,
        refresh_ms: Option<u16>,
    ) -> SidebarPaneOptions {
        let target = match self
            .detected_size
            .and_then(|(cols, _)| NonZeroU16::new(cols))
        {
            Some(view_cols) => {
                crate::mux::width_target::adopt(&self.runtime, self.width, view_cols)
            }
            None => crate::mux::width_target::resolve(&self.runtime, self.width, None),
        };
        SidebarPaneOptions {
            runtime: self.runtime.clone(),
            session_name: self.workspace.session_name.clone(),
            workspace_id: self.workspace.workspace_id.clone(),
            project_root: self.workspace.project_root.clone(),
            extra_env: self.extra_env.clone(),
            cwd: cwd.to_path_buf(),
            target,
            detected_view_size: self.detected_size,
            rimz_bin: self.rimz_bin.clone(),
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

    fn presence_options(&self) -> Option<PresencePluginOptions> {
        let wasm = crate::mux::zellij::presence_plugin_path()?;
        Some(PresencePluginOptions::from_config(
            &self.workspace.session_name,
            &self.workspace.workspace_id,
            wasm,
            StatePaths::for_project_root(&self.workspace.project_root)
                .ok()?
                .room_bin,
            &self.machine_config.sidebar,
            &self.machine_config.zellij,
        ))
    }

    fn load_presence(&self) {
        let Some(opts) = self.presence_options() else {
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

    fn register_room_keys(&self) {
        let rimz_bin = crate::proc::rimz_exe();
        for (name, label, args) in [
            (
                "focus_key",
                crate::config::SidebarConfig::key_label(&self.machine_config.sidebar.focus_key),
                &["sidebar", "focus", "--toggle"][..],
            ),
            (
                "zoom_key",
                crate::config::SidebarConfig::key_label(&self.machine_config.sidebar.zoom_key),
                &["pane", "zoom"][..],
            ),
        ] {
            let Some(label) = label else { continue };
            let Some(binding) = crate::mux::RoomKeyBinding::resolve(label, &rimz_bin, args) else {
                tracing::warn!(
                    key = name,
                    value = label,
                    "ignoring invalid [sidebar] key; expected e.g. Alt+p"
                );
                continue;
            };
            if let Err(err) = self.backend.register_room_key(&binding) {
                tracing::debug!(key = name, error = %err, "registering room keybind failed");
            }
        }
    }

    /// Assemble configured daemon view for a normal start flow.
    fn background_view(
        &self,
        readiness: &ReadinessSnapshot,
        refresh_ms: Option<u16>,
    ) -> BackgroundViewOptions {
        let rimz_bin = self.rimz_bin.clone();
        BackgroundViewOptions {
            view: crate::daemon_view::daemon_view_spec(crate::daemon_view::DaemonViewSpecParams {
                claude_host_argv: readiness.claude_host_argv(),
                daemon: &self.machine_config.daemon,
                rimz_bin: &rimz_bin,
                workspace_id: &self.workspace.workspace_id,
                session_name: &self.workspace.session_name,
                project_root: &self.workspace.project_root,
                worktree_root: &self.workspace.worktree_root,
                codex_present: crate::agents::runtime_control::installed_broker_bin().is_some(),
            }),
            sidebar: self.sidebar_options_with_resume(
                &self.workspace.worktree_root,
                Vec::new(),
                refresh_ms,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionOwnership {
    Managed,
    External,
    Unknown,
}

fn classify_preflight_health(
    session_name: &str,
    ownership: SessionOwnership,
    health: SessionHealth,
) -> Result<Option<SessionHealth>> {
    match health {
        SessionHealth::Healthy => Ok(Some(health)),
        SessionHealth::Unresponsive => Err(unresponsive_error(session_name, ownership)),
        SessionHealth::Reborn | SessionHealth::Stuck => Ok(None),
    }
}

fn unresponsive_error(session_name: &str, ownership: SessionOwnership) -> anyhow::Error {
    let recovery = match ownership {
        SessionOwnership::Managed => "Run `rimz doctor` to inspect it or `rimz reset` to rebuild it (destructive; prompts first).".to_owned(),
        SessionOwnership::External => {
            let session_name = shell_quote(session_name);
            format!(
                "This session is not RimZ-managed. Run `zellij delete-session --force {session_name}` to destroy it without a confirmation prompt, or `zellij attach {session_name}` to bypass RimZ if you insist."
            )
        }
        SessionOwnership::Unknown => {
            let session_name = shell_quote(session_name);
            format!(
                "RimZ could not determine whether this session is managed. Run `rimz doctor` to inspect it, or `zellij attach {session_name}` to bypass RimZ if you choose to attach directly."
            )
        }
    };
    anyhow::anyhow!(
        "The '{session_name}' Zellij room is live but not responding to Zellij control commands.\n\
         Attaching could open a black screen, so RimZ left the room untouched.\n\
         {recovery}",
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn recorded_room_bin(workspace_id: &WorkspaceId) -> PathBuf {
    let recorded = StatePaths::for_workspace(workspace_id.clone())
        .ok()
        .and_then(|paths| record::read(&paths.workspace_record).ok())
        .and_then(|record| record.rimz_bin);
    crate::workspace::resolve_recorded_rimz_bin(workspace_id, recorded.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::RootClass;

    fn reset_config() -> MachineConfig {
        MachineConfig {
            accounts: toml::from_str("[codex.work]\nhome = \"/srv/work\"\n[codex.team]\nhome = \"/srv/team\"\n[use]\ncodex = \"work\"\n").unwrap(),
            ..Default::default()
        }
    }

    fn project_accounts(root: &Path, value: &str) {
        std::fs::create_dir_all(root.join(".rimz")).unwrap();
        std::fs::write(root.join(".rimz/config.toml"), value).unwrap();
    }

    #[test]
    fn reset_selection_reports_each_layer_and_ignores_the_room_record() {
        let root = tempfile::tempdir().unwrap();
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let mut config = reset_config();
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        std::fs::create_dir_all(paths.workspace_record.parent().unwrap()).unwrap();
        let mut workspace = workspace();
        workspace.project_root = root.path().to_owned();
        workspace.workspace_id = WorkspaceId::from_project_root(root.path());
        let mut recorded = WorkspaceRecord::from_resolved(&workspace);
        recorded.logins = Some(crate::ids::RoomLogins::from([(
            kind.clone(),
            "team".parse().unwrap(),
        )]));
        std::fs::write(
            &paths.workspace_record,
            serde_json::to_vec(&recorded).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reset_login_selection(root.path(), &config, &kind).unwrap(),
            ("work".parse().unwrap(), BirthLoginLayer::Machine)
        );
        config.accounts.use_accounts.clear();
        assert_eq!(
            reset_login_selection(root.path(), &config, &kind).unwrap(),
            (
                crate::ids::LoginName::default_login(),
                BirthLoginLayer::Provider
            )
        );
        config
            .accounts
            .use_accounts
            .insert(kind.clone(), "missing".parse().unwrap());
        project_accounts(root.path(), "[accounts]\ncodex = \"team\"\n");
        crate::trust::grant(root.path()).unwrap();
        assert_eq!(
            reset_login_selection(root.path(), &config, &kind).unwrap(),
            ("team".parse().unwrap(), BirthLoginLayer::Project)
        );
    }

    #[test]
    fn reset_selection_validates_only_the_requested_kind() {
        let root = tempfile::tempdir().unwrap();
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let mut config = reset_config();
        config
            .accounts
            .use_accounts
            .insert(kind.clone(), "missing".parse().unwrap());
        let error = reset_login_selection(root.path(), &config, &kind).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<crate::agents::BirthLoginErr>(),
                Some(crate::agents::BirthLoginErr::MachineUnknown { .. })
            ),
            "{error}"
        );
        config.accounts.use_accounts.remove(&kind);
        config.accounts.use_accounts.insert(
            crate::ids::AgentKind::new_unchecked("claude"),
            "missing".parse().unwrap(),
        );
        assert_eq!(
            reset_login_selection(root.path(), &config, &kind)
                .unwrap()
                .0,
            crate::ids::LoginName::default_login()
        );
        project_accounts(root.path(), "[accounts]\ncodex = \"missing\"\n");
        crate::trust::grant(root.path()).unwrap();
        let error = reset_login_selection(root.path(), &config, &kind)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown codex account `missing`"), "{error}");
    }

    #[test]
    fn reset_selection_refuses_untrusted_and_stale_projects_even_for_other_kinds() {
        let root = tempfile::tempdir().unwrap();
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        let config = reset_config();
        project_accounts(root.path(), "[accounts]\nclaude = \"default\"\n");
        let error = reset_login_selection(root.path(), &config, &kind)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("project account selections in .rimz/config.toml are untrusted")
                && error.contains("rimz trust grant"),
            "{error}"
        );
        crate::trust::grant(root.path()).unwrap();
        project_accounts(root.path(), "[accounts]\nclaude = \"work\"\n");
        let error = reset_login_selection(root.path(), &config, &kind)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("project account selections in .rimz/config.toml are stale")
                && error.contains("since your last grant")
                && error.contains("rimz trust grant"),
            "{error}"
        );
    }

    #[test]
    fn reset_selection_refuses_unreadable_core_before_project_trust() {
        let root = tempfile::tempdir().unwrap();
        project_accounts(root.path(), "[accounts]\nclaude = \"default\"\n");
        let mut config = reset_config();
        config
            .notices
            .unreadable_files
            .insert(MachineConfig::config_path(), "broken TOML".to_owned());
        let error = reset_login_selection(
            root.path(),
            &config,
            &crate::ids::AgentKind::new_unchecked("codex"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("cannot reset this room's account") && error.contains("broken TOML"),
            "{error}"
        );
    }

    #[test]
    fn fresh_birth_refuses_unreadable_core_but_existing_rooms_and_other_files_do_not() {
        let root = tempfile::tempdir().unwrap();
        let empty = crate::ids::RoomLogins::new();
        let mut config = MachineConfig::default();
        let path = MachineConfig::config_path();
        config
            .notices
            .unreadable_files
            .insert(path.clone(), "broken TOML".to_owned());
        let error = select_birth_logins_with_frozen(root.path(), &config, &empty, false, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("broken TOML")
                && error.contains("rimz config get")
                && error.contains(&path.display().to_string()),
            "{error}"
        );
        config.accounts.use_accounts.insert(
            crate::ids::AgentKind::new_unchecked("codex"),
            "missing".parse().unwrap(),
        );
        assert!(
            select_birth_logins_with_frozen(
                root.path(),
                &config,
                &empty,
                false,
                Some(empty.clone())
            )
            .is_ok()
        );
        assert!(select_birth_logins_with_frozen(root.path(), &config, &empty, true, None).is_ok());
        let frozen_named = crate::ids::RoomLogins::from([(
            crate::ids::AgentKind::new_unchecked("codex"),
            "rimio".parse().unwrap(),
        )]);
        let error = select_birth_logins_with_frozen(
            root.path(),
            &config,
            &empty,
            false,
            Some(frozen_named),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("broken TOML") && error.contains("named accounts"),
            "{error}"
        );
        config.notices.unreadable_files.clear();
        let error = select_birth_logins_with_frozen(root.path(), &config, &empty, false, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("[accounts.use]"), "{error}");
        config.accounts.use_accounts.clear();
        for file in ["theme.toml", "loop.toml"] {
            config
                .notices
                .unreadable_files
                .insert(path.with_file_name(file), "broken TOML".to_owned());
        }
        assert!(select_birth_logins_with_frozen(root.path(), &config, &empty, false, None).is_ok());
    }

    fn workspace() -> ResolvedWorkspace {
        let project_root = PathBuf::from("/code/rimz");
        ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            cwd_project_root: Some(project_root.clone()),
            root_class: RootClass::Repo,
            worktree_root: project_root.join("../rimz-worktrees/demo"),
            worktree_branch: Some("demo".to_owned()),
            session_name: "rimz-rimz".to_owned(),
            mux_hint: None,
        }
    }

    #[test]
    fn pane_identity_env_pins_workspace_and_selects_channel() {
        let workspace = workspace();
        let base = BTreeMap::from([
            ("RIMZ".to_owned(), "1".to_owned()),
            (
                "RIMZ_WORKSPACE_ID".into(),
                workspace.workspace_id.to_string(),
            ),
            ("RIMZ_PROJECT_ROOT".to_owned(), "/code/rimz".to_owned()),
            ("RIMZ_WORKTREE_PATH".into(), "/other/checkout".to_owned()),
        ]);
        for (explicit, ambient, channel) in [
            (Some("explicit"), Some("ambient"), Some("explicit")),
            (None, Some("ambient"), Some("ambient")),
            (None, Some(""), None),
            (None, None, None),
        ] {
            let mut expected = base.clone();
            if let Some(channel) = channel {
                expected.insert("RIMZ_CHANNEL".to_owned(), channel.to_owned());
            }
            let actual = pane_identity_env_with_ambient(
                &workspace,
                Path::new("/other/checkout"),
                explicit,
                ambient,
            );
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn record_to_workspace_preserves_identity_and_normalizes_missing_fields() {
        let project_root = PathBuf::from("/code/rimz");
        let record = WorkspaceRecord {
            layout: 2,
            workspace_id: WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            worktree_root: None,
            session_name: "rimz-rimz".to_owned(),
            root_class: RootClass::Marker,
            rimz_bin: None,
            rimz_build: None,
            logins: None,
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
        let mux = LiveRoomErr::from(MuxErr::NoMuxFound);
        let expected =
            "no live RimZ room `rimz-demo`; run `rimz start` first or enter one with `rimz attach`";

        assert_eq!(unavailable.to_string(), expected);
        assert!(std::error::Error::source(&unavailable).is_none());
        assert_eq!(mux.to_string(), MuxErr::NoMuxFound.to_string());
    }

    #[test]
    fn preflight_reuses_only_a_healthy_live_verdict() {
        assert_eq!(
            classify_preflight_health(
                "rimz-demo",
                SessionOwnership::Managed,
                SessionHealth::Healthy,
            )
            .expect("healthy verdict"),
            Some(SessionHealth::Healthy)
        );
        for health in [SessionHealth::Reborn, SessionHealth::Stuck] {
            assert_eq!(
                classify_preflight_health("rimz-demo", SessionOwnership::Managed, health)
                    .expect("non-live verdict"),
                None
            );
        }
        assert!(
            classify_preflight_health(
                "rimz-demo",
                SessionOwnership::Managed,
                SessionHealth::Unresponsive,
            )
            .is_err()
        );
    }

    #[test]
    fn unresponsive_recovery_matches_session_ownership() {
        let managed = unresponsive_error("rimz-demo", SessionOwnership::Managed).to_string();
        assert!(managed.contains("`rimz reset`"));
        assert!(managed.contains("prompts first"));

        let external =
            unresponsive_error("someone else's session", SessionOwnership::External).to_string();
        assert!(external.contains("not RimZ-managed"));
        assert!(external.contains("`zellij delete-session --force 'someone else'\\''s session'`"));
        assert!(external.contains("`zellij attach 'someone else'\\''s session'`"));
        assert!(external.contains("without a confirmation prompt"));
        assert!(!external.contains("rimz reset"));
        assert!(!external.contains("prompts first"));

        let unknown = unresponsive_error("rimz-demo", SessionOwnership::Unknown).to_string();
        assert!(unknown.contains("could not determine whether this session is managed"));
        assert!(unknown.contains("`rimz doctor`"));
        assert!(unknown.contains("`zellij attach 'rimz-demo'`"));
        assert!(!unknown.contains("delete-session"));
        assert!(!unknown.contains("rimz reset"));
    }
}
