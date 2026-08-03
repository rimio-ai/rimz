//! The shared entry for participant-facing commands: resolve the current
//! workspace, open its store, and derive the current channel in one step.
//!
//! Commands that address a running room open a `Ctx` and read what they need
//! from it. Commands that name or create a room by path — `start`, `attach`,
//! `gc`, `setup` — resolve directly through `WorkspaceResolver::resolve`
//! instead; they take a varying path and have no store to open.

use anyhow::{Context, Result};

use rimz::ids::{MuxName, WorkspaceId};
use rimz::store::snapshot::SidebarSnapshot;
use rimz::store::workspace_record;
use rimz::workspace::WorkspaceResolver;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

use super::GlobalFlags;

/// The current room, opened as a participant.
pub(crate) struct Ctx {
    pub(crate) workspace: ResolvedWorkspace,
    pub(crate) store: Store,
    channel: Option<String>,
    mux: Option<MuxName>,
}

impl Ctx {
    /// Open the room the current directory participates in.
    pub(crate) fn open(globals: &GlobalFlags) -> Result<Self> {
        let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
            .context("resolving current workspace")?;
        let store = super::open_store(&workspace)?;
        let channel = super::current_channel(&workspace);
        let mux = globals.mux;
        Ok(Self {
            workspace,
            store,
            channel,
            mux,
        })
    }

    /// Open a workspace named by a detached helper without recording it again.
    pub(crate) fn for_workspace(
        workspace_id: WorkspaceId,
        mux_hint: Option<MuxName>,
    ) -> Result<Self> {
        let paths =
            StatePaths::for_workspace(workspace_id.clone()).context("preparing store paths")?;
        let runtime =
            RuntimePaths::for_workspace(workspace_id.clone()).context("preparing runtime paths")?;
        let record = workspace_record::read(&paths.workspace_record).with_context(|| {
            format!(
                "reading workspace record `{}`",
                paths.workspace_record.display()
            )
        })?;
        let store = Store::open(paths, runtime).context("opening store")?;
        let workspace = ResolvedWorkspace {
            workspace_id,
            project_root: record.project_root.clone(),
            cwd_project_root: None,
            root_class: record.root_class,
            worktree_root: record.project_root,
            worktree_branch: None,
            session_name: record.session_name,
            mux_hint,
        };
        let channel = super::current_channel(&workspace);
        Ok(Self {
            workspace,
            store,
            channel,
            mux: mux_hint,
        })
    }

    /// The channel this command runs in, when it is scoped to a named lane.
    pub(crate) fn channel(&self) -> Option<&str> {
        self.channel.as_deref()
    }

    /// The runtime paths for this workspace, as the open store already resolved them.
    pub(crate) fn runtime(&self) -> &RuntimePaths {
        self.store.runtime_paths()
    }

    /// The cached agent rollup.
    pub(crate) fn cached_snapshot(&self) -> Result<SidebarSnapshot> {
        self.store
            .snapshot_cached()
            .context("reading agent snapshot")
    }

    /// The agent roster the sidebar shows: the cached rollup with the daemon-mode
    /// reap applied, so paneless Codex ghosts the app-server no longer holds drop
    /// exactly as `rimz agents list` and the sidebar drop them. Best-effort and
    /// fail-safe — an absent daemon-reap cache keeps every session
    /// (see `SidebarSnapshot::reap_runtime`).
    pub(crate) fn alive_snapshot(&self) -> Result<SidebarSnapshot> {
        super::alive_snapshot(&self.store, &self.workspace.session_name)
    }

    /// The snapshot a command resolves an address against. Unlike the
    /// rollup-only `cached_snapshot`, this folds a *fresh* live pane frame onto the
    /// rollup without the render spine, so a just-started sessionless pane is
    /// addressable without paying group-root, spending, account, dashboard, or git
    /// enrichment. `min_pane_cache_ms` floors the pane pull at now, bypassing the
    /// producer's pane cache (up to 10s old in event mode). One mux roster read;
    /// falls back to the rollup when there is no mux to enumerate.
    pub(crate) fn resolution_snapshot(&self) -> Result<SidebarSnapshot> {
        Ok(rimz::sidebar::produce::resolution_snapshot(
            &self.workspace,
            &self.store,
            self.mux,
        )?)
    }

    pub(crate) fn fold_agent_context(&self, snapshot: SidebarSnapshot) -> SidebarSnapshot {
        snapshot.with_agent_context(rimz::store::agent_context::read_all(self.runtime()))
    }

    pub(crate) fn resolution_snapshot_with_context(&self) -> Result<SidebarSnapshot> {
        self.resolution_snapshot()
            .map(|snapshot| self.fold_agent_context(snapshot))
    }
}
