//! Shared helpers for agent launch commands that open panes in the current room.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};

use rimz::ResolvedWorkspace;
use rimz::workspace::RootClass;

/// The room-identity environment a pane opened into the current session
/// carries: the `RIMZ` marker plus the workspace pin (id, project root, and the
/// room's worktree root). Splits inherit the session env already; setting these
/// explicitly keeps a freshly split pane pinned to the same room as the
/// new-tab launch path. The pane's own working directory is set separately.
pub(crate) fn launch_identity_env(
    workspace: &ResolvedWorkspace,
    channel: Option<&str>,
    inherit_channel: bool,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("RIMZ".to_owned(), "1".to_owned()),
        (
            "RIMZ_WORKSPACE_ID".to_owned(),
            workspace.workspace_id.to_string(),
        ),
        (
            "RIMZ_PROJECT_ROOT".to_owned(),
            workspace.project_root.display().to_string(),
        ),
        (
            rimz::harness::run::ENV_WORKTREE_PATH.to_owned(),
            workspace.worktree_root.display().to_string(),
        ),
    ]);
    let channel = channel
        .map(ToOwned::to_owned)
        .or_else(|| {
            inherit_channel
                .then(|| std::env::var(rimz::harness::run::ENV_CHANNEL).ok())
                .flatten()
        })
        .filter(|value| !value.is_empty());
    if let Some(channel) = channel {
        env.insert(rimz::harness::run::ENV_CHANNEL.to_owned(), channel);
    }
    env
}

pub(crate) fn resolve_cwd(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    worktree: Option<&str>,
    from_pr: Option<&rimz::forge::PrTarget>,
) -> Result<ResolvedCwd> {
    if let Some(pr) = from_pr {
        if workspace.root_class != RootClass::Repo {
            bail!("--from-pr requires a git repository-backed room");
        }
        let name = worktree.map(str::trim).filter(|name| !name.is_empty());
        let created = rimz::worktree::create_from_pr(
            &workspace.project_root,
            config,
            pr,
            name,
            None,
            name.is_some(),
        )?;
        return Ok(ResolvedCwd {
            cwd: created.path,
            worktree_name: Some(created.name),
            generated_worktree: false,
        });
    }

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
        bail!("{}", live_session_guidance(session_name))
    }
}

pub(crate) fn live_session_guidance(session_name: &str) -> String {
    format!(
        "no live Rimz room `{session_name}`; run `rimz start` first or enter one with `rimz attach`"
    )
}
