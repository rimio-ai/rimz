//! Checkout admission, settlement, and caller-path translation.

use super::{Interaction, Notice};
use crate::agents::AgentState;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn admit_launch_worktree_name(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::WorktreeConfig,
    agents: &[crate::agents::AgentState],
    worktree: Option<&str>,
) -> Result<()> {
    let Some(name) = worktree.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let requested = crate::worktree::parse_requested_name(name)?;
    if crate::worktree::worktree_path(workspace.launch_repo_root(), config, &requested.name)?
        .exists()
    {
        return Ok(());
    }
    crate::channel::admit_worktree_name(agents, &requested.name)?;
    Ok(())
}

pub fn resolve_launch_checkout(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::MachineConfig,
    worktree: Option<&str>,
    from_pr: Option<&crate::forge::PrTarget>,
    cwd: Option<&std::path::Path>,
    base: crate::worktree::LaunchBase,
    interaction: &mut dyn Interaction,
) -> Result<Option<crate::worktree::LaunchCheckout>> {
    if worktree.is_some() || from_pr.is_some() {
        config.require_readable_core("create a worktree")?;
    }
    let config = &config.agents.worktree;
    let checkout = crate::worktree::resolve_launch_checkout(
        workspace, config, worktree, from_pr, None, cwd, base,
    );
    settle_launch_checkout(
        workspace,
        config,
        worktree,
        from_pr,
        cwd,
        base,
        checkout,
        interaction,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn settle_launch_checkout(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::WorktreeConfig,
    worktree: Option<&str>,
    from_pr: Option<&crate::forge::PrTarget>,
    cwd: Option<&std::path::Path>,
    base: crate::worktree::LaunchBase,
    checkout: crate::worktree::Result<crate::worktree::LaunchCheckout>,
    interaction: &mut dyn Interaction,
) -> Result<Option<crate::worktree::LaunchCheckout>> {
    let launch = match checkout {
        Ok(launch) => launch,
        Err(err @ crate::worktree::WorktreeErr::PrBranchDiverged { .. }) => {
            let Some(choice) = interaction.pr_branch(&err)? else {
                interaction.notice(Notice::Warning("Launch aborted; nothing changed."))?;
                return Ok(None);
            };
            crate::worktree::resolve_launch_checkout(
                workspace,
                config,
                worktree,
                from_pr,
                Some(choice),
                cwd,
                base,
            )?
        }
        Err(crate::worktree::WorktreeErr::Unmarked { name, .. }) if from_pr.is_none() => {
            let launch =
                crate::worktree::resolve_unmanaged_launch_checkout(workspace, config, &name)?;
            if !interaction.unmanaged_checkout(&name, &launch.cwd)? {
                interaction.notice(Notice::Warning("Launch aborted; nothing changed."))?;
                return Ok(None);
            }
            launch
        }
        Err(err) => return Err(err.into()),
    };
    if let Some(pr) = from_pr.filter(|_| launch.reused) {
        interaction.notice(Notice::Warning(&format!(
            "reusing worktree `{}` at {} (PR #{} head branch `{}`)",
            launch.worktree_name.as_deref().unwrap_or_default(),
            launch.cwd.display(),
            pr.number,
            launch.branch.as_deref().unwrap_or_default(),
        )))?;
    }
    if let Some(marker) = &launch.created {
        record_creation(workspace, marker);
    }
    if let Some(reason) = launch.review_only_reason.as_deref() {
        interaction.notice(Notice::Warning(&format!(
            "review-only checkout ({reason}); pushes are not configured — install gh/tea for a pushable checkout",
        )))?;
    }
    if let Some(stale) = &launch.stale_base {
        interaction.notice(Notice::Warning(&format!(
            "warning: {stale}; fast-forward {} first, or set agents.worktree.base to fresh",
            stale.branch,
        )))?;
    }
    if let Some(failure) = &launch.trunk_fetch {
        interaction.notice(Notice::Warning(&format!("warning: {failure}")))?;
    }
    Ok(Some(launch))
}

fn record_creation(workspace: &crate::ResolvedWorkspace, marker: &crate::worktree::WorktreeMarker) {
    match open_launch_store(workspace) {
        Ok(store) => emit_worktree_created(&store, workspace, marker),
        Err(error) => tracing::debug!(%error, "skipping worktree signal: store unavailable"),
    }
}

pub(super) fn open_launch_store(workspace: &crate::ResolvedWorkspace) -> Result<crate::Store> {
    let paths = crate::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing store paths")?;
    let runtime = crate::RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
    let store = crate::Store::open(paths, runtime).context("opening store")?;
    store
        .record_workspace(workspace)
        .context("recording workspace metadata")?;
    Ok(store)
}

pub fn emit_worktree_created(
    store: &crate::Store,
    workspace: &crate::ResolvedWorkspace,
    marker: &crate::worktree::WorktreeMarker,
) {
    let mut payload = serde_json::Map::from_iter([
        ("name".into(), serde_json::json!(marker.name)),
        ("branch".into(), serde_json::json!(marker.branch)),
        (
            "path".into(),
            serde_json::json!(marker.worktree_path.to_string_lossy()),
        ),
        (
            "repo".into(),
            serde_json::json!(marker.repo_root.to_string_lossy()),
        ),
        (
            "base".into(),
            serde_json::json!(marker.base_branch.as_deref().unwrap_or(&marker.base_ref)),
        ),
    ]);
    if let Some(number) = marker.from_pr {
        payload.insert("from_pr".into(), serde_json::json!(number));
    }
    let signal = crate::harness::schedule::signal::Signal {
        name: "worktree.created"
            .parse()
            .expect("built-in signal name is valid"),
        payload,
        source: crate::store::event::SignalSource::Worktree,
        watch: None,
    };
    crate::harness::schedule::signal::emit_in_process(
        store,
        &workspace.session_name,
        &workspace.project_root,
        &signal,
    );
}

/// The host path behind an absolute path a calling agent names. A scoped
/// caller names its temp unit as `/tmp`, while the launch reads the path
/// outside that view; `paths` defaults to the room the caller is pinned to.
pub fn caller_host_path(
    absolute: &std::path::Path,
    paths: Option<&crate::StatePaths>,
) -> Result<std::path::PathBuf> {
    use crate::config::Isolation;
    if !Isolation::ambient(&crate::agents::ambient_env()).is_some_and(Isolation::is_scoped) {
        return Ok(absolute.to_path_buf());
    }
    let pinned;
    let paths = match paths {
        Some(paths) => paths,
        None => {
            let Some(root) = std::env::var(crate::workspace::ENV_WORKSPACE_ID)
                .ok()
                .zip(std::env::var_os(crate::workspace::ENV_PROJECT_ROOT))
                .and_then(|(id, root)| {
                    crate::workspace::verify_pin(&id, std::path::Path::new(&root))
                })
            else {
                return Ok(absolute.to_path_buf());
            };
            pinned = crate::StatePaths::for_workspace(crate::ids::WorkspaceId::from_project_root(
                &root,
            ))?;
            &pinned
        }
    };
    let caller = crate::harness::ancestry::CallerIdentity::from_env();
    let view = crate::sandbox::TmpView::current(
        Isolation::Scoped,
        caller.as_ref().and_then(|caller| caller.name.as_deref()),
        paths,
    );
    Ok(view.host_path(absolute))
}

pub fn confirm_cross_repo_worktree(
    workspace: &crate::ResolvedWorkspace,
    interaction: &mut dyn Interaction,
) -> Result<bool> {
    if workspace
        .cwd_project_root
        .as_deref()
        .is_none_or(|root| root == workspace.project_root)
    {
        return Ok(true);
    }
    interaction.cross_repo(workspace)
}

pub fn pinned_room_root() -> Option<PathBuf> {
    std::env::var(crate::workspace::ENV_WORKSPACE_ID)
        .ok()
        .zip(std::env::var_os(crate::workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| crate::workspace::verify_pin(&id, &PathBuf::from(root)))
}

pub fn check_launch_room(root: Option<&Path>) -> Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    let Some(caller) = crate::harness::ancestry::CallerIdentity::from_env() else {
        return Ok(());
    };
    let pinned = std::env::var(crate::workspace::ENV_WORKSPACE_ID)
        .ok()
        .zip(std::env::var_os(crate::workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| crate::workspace::verify_pin(&id, std::path::Path::new(&root)));
    let Some(pinned) = pinned else {
        return Ok(());
    };
    let workspace = crate::WorkspaceResolver::resolve_participant(".", Some(root.to_path_buf()))?;
    let pinned =
        crate::StatePaths::for_workspace(crate::ids::WorkspaceId::from_project_root(&pinned))?;
    let target = crate::StatePaths::for_project_root(&workspace.project_root)?;
    crate::harness::ancestry::check_launch_room(
        Some(&caller),
        Some((&pinned.workspace_id, &pinned.dir_name)),
        (&target.workspace_id, &target.dir_name),
        root,
    )?;
    Ok(())
}

pub fn anchor_caller_workspace(
    workspace: crate::ResolvedWorkspace,
    caller: Option<&crate::agents::AgentState>,
    root: Option<&Path>,
) -> Result<crate::ResolvedWorkspace> {
    let Some(path) = caller.and_then(AgentState::lane_path) else {
        tracing::debug!("caller has no recorded checkout; keeping invoking cwd");
        return Ok(workspace);
    };
    if !Path::new(path).is_dir() {
        anyhow::bail!(
            "the calling agent's checkout `{path}` no longer exists; restart that agent from an existing checkout before it launches another"
        );
    }
    let mut anchored =
        crate::workspace::WorkspaceResolver::resolve_participant(path, root.map(Path::to_path_buf))
            .with_context(|| format!("resolving the calling agent's checkout `{path}`"))?;
    anchored.worktree_root = Path::new(path)
        .canonicalize()
        .with_context(|| format!("resolving the calling agent's launch directory `{path}`"))?;
    Ok(anchored)
}

pub fn resolve_launch_cwd(
    path: Option<&std::path::Path>,
    paths: &crate::StatePaths,
) -> Result<Option<std::path::PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let absolute = std::env::current_dir()
        .context("reading the caller cwd")?
        .join(path);
    let host = caller_host_path(&absolute, Some(paths))?;
    if !host.is_dir() {
        anyhow::bail!(
            "--cwd `{}` is not a directory; create it first or pass an existing one",
            path.display()
        );
    }
    host.canonicalize()
        .map(Some)
        .with_context(|| format!("resolving --cwd `{}`", path.display()))
}
