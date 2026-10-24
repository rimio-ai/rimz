//! Launch-checkout creation records and signals.

use anyhow::{Context, Result};

pub fn record_creation(
    workspace: &crate::ResolvedWorkspace,
    marker: &crate::worktree::WorktreeMarker,
) {
    let open = || -> Result<crate::Store> {
        let paths = crate::StatePaths::for_project_root(&workspace.project_root)
            .context("preparing store paths")?;
        let runtime = crate::RuntimePaths::for_state(&paths).context("preparing runtime paths")?;
        let store = crate::Store::open(paths, runtime).context("opening store")?;
        store
            .record_workspace(workspace)
            .context("recording workspace metadata")?;
        Ok(store)
    };
    match open() {
        Ok(store) => emit_worktree_created(&store, workspace, marker),
        Err(error) => tracing::debug!(%error, "skipping worktree signal: store unavailable"),
    }
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
