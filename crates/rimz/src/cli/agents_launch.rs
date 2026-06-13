//! Shared helpers for agent launch commands that open panes in the current room.

use std::path::PathBuf;

use anyhow::{Result, bail};

use rimz::workspace::RootClass;

pub(crate) fn resolve_cwd(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    worktree: Option<&str>,
) -> Result<ResolvedCwd> {
    let Some(raw_name) = worktree else {
        return Ok(ResolvedCwd {
            cwd: workspace.worktree_root.clone(),
            worktree_name: None,
            generated_worktree: false,
        });
    };
    if workspace.root_class != RootClass::Repo {
        bail!("--worktree requires a git repository-backed room");
    }
    let name = raw_name.trim();
    let created = rimz::worktree::create(
        &workspace.project_root,
        config,
        (!name.is_empty()).then_some(name),
        None,
        None,
        !name.is_empty(),
    )?;
    Ok(ResolvedCwd {
        cwd: created.path,
        worktree_name: Some(created.name),
        generated_worktree: name.is_empty(),
    })
}

pub(crate) struct ResolvedCwd {
    pub(crate) cwd: PathBuf,
    pub(crate) worktree_name: Option<String>,
    pub(crate) generated_worktree: bool,
}

pub(crate) fn ensure_live_session(
    backend: &dyn rimz::mux::MuxBackend,
    session_name: &str,
) -> Result<()> {
    let sessions = backend.list_sessions()?;
    if sessions.iter().any(|session| session == session_name) {
        Ok(())
    } else {
        bail!("no live Rimz room `{session_name}`; run `rimz start` first")
    }
}
