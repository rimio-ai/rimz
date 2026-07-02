use std::path::{Path, PathBuf};

use crate::ledger::atomic;
use crate::sidebar::produce::git::{WorktreeRootsCache, read_diff_stats_cache};
use crate::sidebar::timing::unix_now_ms;
use crate::workspace::RootClass;

/// The room's enumerated group roots, so a repo checkout parked outside the
/// project root still groups as project-related instead of folding into
/// `external`. Cached under
/// `WORKTREE_ROOTS_TTL`: the set changes only on `git worktree add/remove` or
/// on room re-record, so re-probing every tick would be pure overhead. The
/// cache slot is shared across root classes, and that is sound:
/// the persisted class flips only with a workspace re-record — a session
/// boundary, whose freshness floor refuses the cached set — so a stale
/// other-class enumeration never outlives the produce that learns the new
/// class. Empty on a directory/marker-less scratch room or a probe failure,
/// which leaves the reducer's row-derived git roots and `project_root` prefix
/// test to stand alone.
pub(in crate::sidebar::produce) fn project_group_roots(
    project_root: &Path,
    root_class: RootClass,
    runtime: &crate::RuntimePaths,
    min_refreshed_at_ms: Option<u64>,
) -> Vec<PathBuf> {
    let cache_path = runtime.diff_stats_path();
    let mut cache = read_diff_stats_cache(&cache_path);
    let now_ms = unix_now_ms();
    // The freshness floor mirrors the pane cache's: a session boundary sends
    // its wakeup with `--min-pane-cache-ms`, and an enumeration older than
    // that instant is refused even inside the TTL — a brand-new checkout
    // groups correctly on its first agent's first snapshot.
    let floor_ok =
        |w: &&WorktreeRootsCache| min_refreshed_at_ms.is_none_or(|min| w.refreshed_at_ms >= min);
    if let Some(cached) = cache
        .worktrees
        .as_ref()
        .filter(|w| w.is_fresh(now_ms))
        .filter(floor_ok)
    {
        return cached.roots.clone();
    }
    let roots = list_group_roots(project_root, root_class);
    cache.worktrees = Some(WorktreeRootsCache {
        refreshed_at_ms: now_ms,
        roots: roots.clone(),
    });
    if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar worktree-roots cache write failed");
    }
    roots
}

/// Enumerate group roots by the room root's class: a repo room reports its
/// worktree checkouts. Directory and non-git marker rooms enumerate no children;
/// git-backed agents contribute their own resolved worktree roots during the
/// row fold.
pub(super) fn list_group_roots(project_root: &Path, root_class: RootClass) -> Vec<PathBuf> {
    match root_class {
        RootClass::Repo => list_worktree_roots(project_root),
        RootClass::Directory => Vec::new(),
        RootClass::Marker => {
            if project_root.join(".git").exists() {
                list_worktree_roots(project_root)
            } else {
                Vec::new()
            }
        }
    }
}

/// Parse `git -C <root> worktree list --porcelain` into the absolute checkout
/// root of every worktree — main and linked alike — from its `worktree <path>`
/// lines. Linked worktrees report absolute paths, so a checkout outside the
/// project root is captured here exactly as the reducer needs it.
pub(super) fn list_worktree_roots(project_root: &Path) -> Vec<PathBuf> {
    let output = super::git_output(project_root, &["worktree", "list", "--porcelain"])
        .filter(|output| output.status.success());
    let Some(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path.trim()))
        .collect()
}
