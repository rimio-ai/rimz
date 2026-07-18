//! Sidebar projection into fleet spending service requests and publications.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::agents::spending::{
    ProviderSpendingCache, SpendScope, SpendingCaches, WorkspaceSpendingCache,
    read_provider_spending_cache, read_workspace_spending_cache,
};
use crate::{RuntimePaths, SidebarSnapshot};

/// Refresh through elected account-global walker. Failure serves latest
/// compatible durable publications and retries on next cache tick.
pub(crate) fn compute_fleet_spending_via_service(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
    startup: crate::agents::spending::service::SpendingServiceStartup,
) -> SpendingCaches {
    let request = service_request(runtime, snapshot, spec);
    match crate::agents::spending::service::request(runtime, request, startup) {
        Ok(caches) => caches,
        Err(error) => {
            tracing::debug!(error = %error, "spending service unavailable; serving publication");
            consumer_spending_caches(runtime, snapshot)
        }
    }
}

/// Read producer-published caches without opening account-global cursor.
pub(crate) fn consumer_spending_caches(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
) -> SpendingCaches {
    let provider = current_provider_spending_cache(runtime);
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let workspace = if scope.is_empty() {
        WorkspaceSpendingCache::default()
    } else {
        matching_workspace_cache(runtime, &scope.hash())
    };
    SpendingCaches {
        provider,
        workspace,
    }
}

fn service_request(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
) -> crate::agents::spending::service::SpendingServiceRequest {
    crate::agents::spending::service::SpendingServiceRequest::workspace(
        runtime,
        runtime.workspace_id.clone(),
        snapshot.project_root.clone(),
        snapshot.worktree_roots.clone(),
        snapshot.worktree_home.clone(),
        codex_origin_overrides(snapshot),
        spec.clone(),
    )
}

fn current_provider_spending_cache(runtime: &RuntimePaths) -> ProviderSpendingCache {
    let cache = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    if cache.is_current_version() {
        cache
    } else {
        ProviderSpendingCache::default()
    }
}

fn matching_workspace_cache(runtime: &RuntimePaths, scope_hash: &str) -> WorkspaceSpendingCache {
    let cache = read_workspace_spending_cache(&runtime.workspace_spending_path(scope_hash));
    if cache.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
        && cache.scope_hash == scope_hash
    {
        cache
    } else {
        WorkspaceSpendingCache::default()
    }
}

fn codex_origin_overrides(snapshot: &SidebarSnapshot) -> HashMap<PathBuf, PathBuf> {
    let row_worktrees: HashMap<&str, &str> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.name == "codex")
        .filter_map(|row| Some((row.id.as_str(), row.worktree_path.as_deref()?)))
        .collect();
    let mut origins = HashMap::new();
    for agent in &snapshot.agents {
        if agent.kind.as_str() != "codex" {
            continue;
        }
        let Some(transcript_path) = agent.transcript_path.as_deref() else {
            continue;
        };
        let transcript_path = PathBuf::from(transcript_path);
        if !transcript_path.is_absolute() {
            continue;
        }
        let worktree = agent
            .worktree_path
            .as_deref()
            .or_else(|| row_worktrees.get(agent.agent_id.as_str()).copied());
        let Some(origin) =
            worktree.and_then(|worktree| crate::agents::spending::origin_path(Some(worktree)))
        else {
            continue;
        };
        origins.insert(transcript_path, origin);
    }
    origins
}

#[cfg(test)]
mod tests;
