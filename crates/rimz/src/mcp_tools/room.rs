//! The room a call acts on, opened as a participant for that call alone.

use std::path::Path;

use anyhow::Context;

use crate::mcp::Failure;
use crate::store::snapshot::SidebarSnapshot;
use crate::workspace::{WorkspaceErr, WorkspaceResolver};
use crate::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

/// The room `dir` takes part in, with its existing store.
pub(super) struct Room {
    pub(super) workspace: ResolvedWorkspace,
    pub(super) store: Store,
}

impl Room {
    /// Open the room `dir` takes part in; a room with no readable workspace
    /// record is no room, and nothing here creates one.
    pub(super) fn open(dir: &Path) -> Result<Self, Failure> {
        let workspace = WorkspaceResolver::resolve_participant(dir, None)
            .context("resolving current workspace")?;
        let paths = StatePaths::for_project_root(&workspace.project_root)
            .context("preparing store paths")?;
        let no_room = || {
            Failure::new(
                WorkspaceErr::NoRoom {
                    location: format!("at {}", workspace.project_root.display()),
                }
                .to_string(),
            )
        };
        if crate::workspace::record::read(&paths.workspace_record).is_err() {
            return Err(no_room());
        }
        let runtime = RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
        let store = Store::open_existing(paths, runtime).ok_or_else(no_room)?;
        Ok(Self { workspace, store })
    }

    /// One private pane read folded over the rollup, as a command resolves agents against.
    pub(super) fn resolution_snapshot(&self) -> anyhow::Result<SidebarSnapshot> {
        Ok(crate::sidebar::produce::resolution_snapshot(
            &self.workspace,
            &self.store,
            None,
        )?)
    }

    pub(super) fn resolution_snapshot_with_context(&self) -> anyhow::Result<SidebarSnapshot> {
        Ok(self
            .resolution_snapshot()?
            .with_agent_context(crate::store::agent_context::read_all(
                self.store.runtime_paths(),
            )))
    }

    /// The cached rollup with the daemon-mode reap applied, the roster a stop or join of the
    /// command of the same job reads.
    pub(super) fn alive_snapshot(&self) -> anyhow::Result<SidebarSnapshot> {
        let base = self
            .store
            .snapshot_cached()
            .context("reading agent snapshot")?;
        Ok(crate::sidebar::consumer::cached_alive_snapshot(
            base,
            self.store.runtime_paths(),
            &self.workspace.session_name,
        ))
    }

    /// The room a stop acts in, on the backend the room's own pin names.
    pub(super) fn stop_room(&self) -> crate::launcher::stop::StopRoom<'_> {
        crate::launcher::stop::StopRoom {
            workspace: &self.workspace,
            store: &self.store,
            mux: None,
        }
    }

    /// The room snapshot the sidebar host published.
    pub(super) fn published_snapshot(&self) -> anyhow::Result<SidebarSnapshot> {
        crate::sidebar::consumer::PublishedSnapshotReader::new(
            self.store.runtime_paths().clone(),
            self.workspace.session_name.clone(),
            None,
        )
        .read(self.store.paths())
        .context("reading the room snapshot")
    }
}
