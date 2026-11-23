//! The one layout launch the CLI and an MCP tool go through: enter the room, then open the layout.

use super::*;

/// What a layout launch enters its room with. `root` ranks above the process directory, so a
/// caller whose directory is outside the room passes the room's project root.
pub struct LayoutEntry<'a> {
    pub root: Option<PathBuf>,
    pub mux: Option<crate::ids::MuxName>,
    pub machine: Arc<crate::config::MachineConfig>,
    pub worktree: Option<&'a str>,
    pub from_pr: bool,
    pub fresh: bool,
    pub cwd: Option<&'a Path>,
    /// A launch under a loop fire resolves no caller.
    pub under_fire: bool,
}

/// The room a layout launch runs in, anchored at its caller's checkout.
pub struct LayoutRoom {
    pub workspace: crate::ResolvedWorkspace,
    pub store: crate::Store,
    root: Option<PathBuf>,
    mux: Option<crate::ids::MuxName>,
    machine: Arc<crate::config::MachineConfig>,
    cwd: Option<PathBuf>,
}

impl LayoutRoom {
    /// Refuse what cannot launch, before anything opens, and open the existing room.
    pub fn enter(entry: LayoutEntry<'_>) -> Result<Self> {
        require_named_worktree(entry.fresh, entry.worktree)?;
        if entry.worktree.is_some() || entry.from_pr {
            entry.machine.require_readable_core("create a worktree")?;
        }
        checkout::check_launch_room(entry.root.as_deref())?;
        let workspace = crate::WorkspaceResolver::resolve_participant(".", entry.root.clone())
            .context("resolving current workspace")?;
        let store = open_existing_store(&workspace)?;
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let caller = (!entry.under_fire)
            .then(|| crate::harness::ancestry::resolve_calling_agent(&snapshot.agents).ok())
            .flatten();
        let workspace =
            checkout::anchor_caller_workspace(workspace, caller, entry.root.as_deref())?;
        let cwd = checkout::resolve_launch_cwd(entry.cwd, store.paths())?;
        Ok(Self {
            workspace,
            store,
            root: entry.root,
            mux: entry.mux,
            machine: entry.machine,
            cwd,
        })
    }
}

fn open_existing_store(workspace: &crate::ResolvedWorkspace) -> Result<crate::Store> {
    let no_room = || {
        crate::workspace::WorkspaceErr::NoRoom {
            location: format!("at {}", workspace.project_root.display()),
        }
        .to_string()
    };
    let paths = crate::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing store paths")?;
    if crate::workspace::record::read(&paths.workspace_record).is_err() {
        bail!(no_room());
    }
    let runtime = crate::RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
    crate::Store::open_existing(paths, runtime).with_context(no_room)
}

/// `--fresh` replaces a named worktree's closed cohort, so it needs the name.
pub fn require_named_worktree(fresh: bool, worktree: Option<&str>) -> Result<()> {
    if fresh && worktree.is_none_or(|name| name.trim().is_empty()) {
        bail!("--fresh needs a named worktree (-w NAME)");
    }
    Ok(())
}

/// Prepare `launch` in `room` and open its panes.
pub fn launch_layout(
    room: &LayoutRoom,
    launch: LayoutLaunch,
    allow_in_place: bool,
    interaction: &mut dyn Interaction,
) -> Result<LaunchOutcome> {
    let request = LayoutRequest {
        context: LaunchContext {
            workspace: room.workspace.clone(),
            store: room.store.clone(),
            machine: room.machine.clone(),
            mux: room.mux,
            root: room.root.clone(),
            channel: context::current_channel(&room.workspace, Some(&room.store)).into_name(),
        },
        launch,
        cwd: room.cwd.clone(),
        allow_in_place,
        entry: RoomEntry::Live,
        starter: Starter::Human,
        workflow: None,
        automation: None,
        leader_run: None,
    };
    match prepare_layout(request, interaction)? {
        Preparation::Prepared(prepared) => execute(*prepared, interaction),
        Preparation::Handled(outcome) => Ok(outcome),
    }
}
