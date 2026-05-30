//! Sidebar snapshot caches and the in-process consumer read.
//!
//! The producer (the elected eldest renderer, via `rimz sidebar snapshot`)
//! publishes two runtime caches: the snapshot base (`snapshot.json`: the ledger
//! rollup plus the live pane list) and the per-worktree git facts
//! (`diff-stats.json`). Every other per-tab renderer is a *consumer* — it reads
//! those caches and folds its own pane exclusion in process, never forking a
//! `list-panes`/git of its own.
//!
//! [`read_published_snapshot`] is that consumer read: it lives in the library so
//! the native renderer calls it directly (no subprocess per tick) and the
//! `rimz sidebar snapshot --no-produce` CLI path shares one implementation. The
//! producer's write side (single-flight election, the git forks) stays in
//! `cli::sidebar`, which constructs these same cache types.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::feed::PaneRef;
use crate::ids::PaneId;
use crate::{
    RuntimePaths, SidebarOwnView, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind,
};

/// Coalescing window for the shared snapshot cache. Well under the default 2s
/// data tick: when one ledger-delta wakeup wakes every sidebar at once, the
/// first produces the heavy snapshot and the rest read it back within this
/// window instead of each spawning their own `list-panes`. Short enough that
/// live pane/git drift (which fires no ledger delta) still surfaces inside one
/// tick — the same staleness budget the diff-stats cache already accepts.
pub const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// How long a worktree's git diff-stats stay cached before the per-worktree
/// `git` forks behind them are re-run. A working-tree edit fires no ledger
/// delta, so this column is never push-refreshed — it rides this TTL plus the
/// sidebar's backstop poll.
pub const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// Shared, single-flight snapshot cache. Holds the ledger rollup plus the live
/// pane list keyed to one `(workspace, session)` — the per-workspace runtime
/// root scopes the workspace; `session_name` guards against serving one
/// session's panes (which the Zellij backend stamps from the requested session,
/// not the true owner) to a sidebar pinned to another during a detach or
/// session-rotation handoff. Per-sidebar exclusion and own-view are applied by
/// the reader, so the cached snapshot is pre-pane-fold.
#[derive(Serialize, Deserialize)]
pub struct SnapshotCache {
    pub produced_at_ms: u64,
    pub session_name: String,
    pub panes: Vec<PaneRef>,
    pub snapshot: SidebarSnapshot,
}

/// Read a same-session cache entry regardless of coalescing freshness. `None`
/// when it is absent, for another session, or unreadable. Used as the
/// hold-last-good base for a consumer read and the degraded-read fallback.
pub fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<SnapshotCache> {
    let bytes = std::fs::read(cache_path).ok()?;
    let cache: SnapshotCache = serde_json::from_slice(&bytes).ok()?;
    (cache.session_name == session).then_some(cache)
}

/// The producer's last published base (ledger rollup + live pane list) for a
/// consumer renderer. Returns the same-session cache regardless of freshness —
/// a non-producer holds the last good frame rather than forking its own
/// `list-panes`; the elder's next publish refreshes it. `None` when no
/// same-session cache exists yet, so the caller falls back to the bare rollup
/// until the producer's first publish.
fn read_published_base(
    runtime: &RuntimePaths,
    session: &str,
) -> Option<(SidebarSnapshot, Vec<PaneRef>)> {
    let cache_path = runtime.root.join("snapshot.json");
    read_snapshot_cache(&cache_path, session).map(|cache| (cache.snapshot, cache.panes))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCache {
    pub entries: BTreeMap<String, DiffStatsCacheEntry>,
    /// The repo's worktree checkout roots, cached under the same TTL as the
    /// per-worktree diff stats. The set changes only on `git worktree
    /// add/remove`, so grouping reuses it across ticks instead of forking
    /// `git worktree list` every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<WorktreeRootsCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRootsCache {
    pub refreshed_at_ms: u64,
    pub roots: Vec<PathBuf>,
}

impl WorktreeRootsCache {
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffStatsCacheEntry {
    pub refreshed_at_ms: u64,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Live branch resolved from the worktree path, cached under the same TTL
    /// as the diff stats so the group header tracks `git checkout` without a
    /// git call every tick.
    #[serde(default)]
    pub branch: Option<String>,
}

impl DiffStatsCacheEntry {
    pub fn new(refreshed_at_ms: u64, stats: Option<DiffStats>, branch: Option<String>) -> Self {
        Self {
            refreshed_at_ms,
            added: stats.map(|stats| stats.added),
            removed: stats.map(|stats| stats.removed),
            branch,
        }
    }

    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
    }

    pub fn stats(&self) -> Option<DiffStats> {
        self.added
            .zip(self.removed)
            .map(|(added, removed)| DiffStats { added, removed })
    }
}

pub fn read_diff_stats_cache(path: &Path) -> DiffStatsCache {
    let Ok(bytes) = std::fs::read(path) else {
        return DiffStatsCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// The repo's worktree checkout roots the producer last published, read-only
/// (no `git worktree list` fork). A consumer reuses whatever the elder cached,
/// even stale; an empty set leaves the reducer's project-root prefix test to
/// stand alone.
pub fn cached_worktree_roots(runtime: &RuntimePaths) -> Vec<PathBuf> {
    read_diff_stats_cache(&runtime.root.join("diff-stats.json"))
        .worktrees
        .map(|cache| cache.roots)
        .unwrap_or_default()
}

/// The worktree path a group's rows share, if any. The group key may carry a
/// branch suffix (a path that holds more than one branch), so the bare path is
/// recovered from the rows — every row in a group shares it.
fn worktree_group_path(group: &SidebarWorktreeGroup) -> Option<&str> {
    group
        .rows
        .iter()
        .find_map(|row| row.worktree_path.as_deref())
        .filter(|path| !path.is_empty())
}

/// The live worktree paths this snapshot needs git facts for: a `Worktree`-kind
/// group whose recovered path is a live directory, de-duplicated so two
/// branch-split groups for one dir share a single git read. The producer feeds
/// this to its git refresh; projection ([`project_diff_stats`]) re-derives the
/// same live-dir set so a stale entry for a now-missing worktree never resurfaces.
pub fn needed_worktree_paths(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut needed: Vec<String> = Vec::new();
    for group in &snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group) else {
            continue;
        };
        if Path::new(path).is_dir() && !needed.iter().any(|known| known == path) {
            needed.push(path.to_owned());
        }
    }
    needed
}

/// Project the cached git facts onto each worktree group: the diff stats shown
/// on the header and the live branch label. Both are properties of the worktree
/// *path*, not of any one agent, so they belong to the group — which also
/// settles the shared-worktree "whose branch?" ambiguity. Only live-dir paths
/// carry stats, so a stale entry for a now-missing worktree never resurfaces.
/// Pure projection (no git): the producer refreshes the cache first, a consumer
/// projects whatever the elder last published.
pub fn project_diff_stats(snapshot: &mut SidebarSnapshot, cache: &DiffStatsCache) {
    for group in &mut snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group).map(ToOwned::to_owned) else {
            continue;
        };
        if !Path::new(&path).is_dir() {
            continue;
        }
        let Some(entry) = cache.entries.get(&path).cloned() else {
            continue;
        };
        if let Some(stats) = entry.stats() {
            group.diff_added = Some(stats.added);
            group.diff_removed = Some(stats.removed);
        }
        if let Some(branch) = entry.branch.filter(|branch| !branch.is_empty()) {
            group.label = branch;
        }
    }
}

/// Whether any supported agent has its hooks installed. Environment, not ledger,
/// so the reducer can't know it — the renderer's first-run hint points at
/// `rimz hooks install` until a supported agent is wired.
pub fn agent_hooks_ready() -> bool {
    crate::agents::KNOWN_AGENTS.iter().any(|name| {
        crate::agents::integration_by_name(name)
            .map(|agent| agent.supports_hook_install() && agent.hooks_installed())
            .unwrap_or(false)
    })
}

/// Render the published snapshot for a consumer renderer, entirely from runtime
/// caches and sidecars — no `list-panes`, no git, no ledger projection. Reads
/// the producer's `snapshot.json` base, folds the session's statusline context
/// and per-tool activity, overlays the live panes with this renderer's own-pane
/// exclusion, and projects the cached diff stats. `None` until the producer has
/// published a same-session base, so the caller holds its last good frame.
///
/// This is the in-process twin of the producer's `rimz sidebar snapshot`: the
/// native renderer calls it directly each tick instead of forking, and the
/// `--no-produce` CLI path (the plugin rail's read) shares it.
pub fn read_published_snapshot(
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> Option<SidebarSnapshot> {
    let (base, panes) = read_published_base(runtime, session)?;
    Some(enrich_consumer(base, Some(panes), runtime, exclude))
}

/// Fold the read-only enrichments onto a consumer's base snapshot: the cached
/// worktree roots, each session's statusline context and per-tool activity, the
/// live-pane overlay with this renderer's own-pane exclusion, and the cached
/// diff-stats projection. Every input is a runtime cache or sidecar read — no
/// `list-panes`, no git, no ledger lock. `panes` is `None` only on a cold start
/// (no base published yet), where the bare rollup's groups stand until the
/// producer's first publish, mirroring the producer's own pane-fold guard.
pub fn enrich_consumer(
    mut snapshot: SidebarSnapshot,
    panes: Option<Vec<PaneRef>>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
) -> SidebarSnapshot {
    if snapshot.project_root.is_some() {
        snapshot = snapshot.with_worktree_roots(cached_worktree_roots(runtime));
    }
    if !snapshot.agents.is_empty() {
        snapshot = snapshot.with_agent_context(crate::ledger::agent_context::read_all(runtime));
        let activity = crate::agent_activity::read_all(runtime);
        snapshot = snapshot.with_agent_activity(&activity);
    }
    if let Some(panes) = panes {
        if let Some(own) = exclude {
            snapshot.own_view = SidebarOwnView::from_panes(own, &panes);
        }
        snapshot = snapshot.with_live_panes(panes, exclude);
    }
    snapshot.agent_hooks_ready = agent_hooks_ready();
    // Per-machine display preferences (row density) are environment, not ledger,
    // so the producer's published rollup carries the default. Fold them here so a
    // consumer tab honours the user's preference too — a cheap config read, never
    // a fork or a ledger lock. Best-effort: a read failure falls back to the
    // compact default rather than failing the frame.
    snapshot.sidebar = crate::config::MachineConfig::load()
        .map(|config| config.sidebar)
        .unwrap_or_default();

    let cache = read_diff_stats_cache(&runtime.root.join("diff-stats.json"));
    project_diff_stats(&mut snapshot, &cache);
    snapshot
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, WorkspaceId};
    use crate::ledger::atomic;

    fn pane(id: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, id),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    fn pane_in_tab(id: &str, view_id: &str) -> PaneRef {
        PaneRef {
            view_id: Some(view_id.to_owned()),
            ..pane(id, "zsh", "/tmp")
        }
    }

    #[test]
    fn read_published_snapshot_folds_caches_without_forking() {
        // A real on-disk worktree so the live-dir projection fires.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let wt = worktree.to_string_lossy().into_owned();

        // Publish a base: a rollup whose project root is the worktree, plus one
        // pane in it. `own` is excluded; a sibling pane becomes a row.
        let mut rollup = SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new());
        rollup = rollup.with_project_root(Some(worktree.clone()));
        let panes = vec![
            pane("terminal_0", "zsh", &wt),
            pane("terminal_own", "rimz-sidebar", &wt),
        ];
        let base = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-test".to_owned(),
            panes,
            snapshot: rollup,
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();

        // Publish diff stats for the worktree path: +7 / -2 on branch `feat`.
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            wt.clone(),
            DiffStatsCacheEntry::new(
                unix_now_ms(),
                Some(DiffStats {
                    added: 7,
                    removed: 2,
                }),
                Some("feat".to_owned()),
            ),
        );
        atomic::write_temp_then_rename_cache(&runtime.root.join("diff-stats.json"), &diff).unwrap();

        let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
        let snapshot =
            read_published_snapshot(&runtime, "rimz-test", Some(&own)).expect("published base");

        // The worktree group carries the cached +7/-2 and the live branch label,
        // projected from the cache with no git fork.
        let group = snapshot
            .worktree_groups
            .iter()
            .find(|group| group.kind == SidebarWorktreeKind::Worktree)
            .expect("a worktree group");
        assert_eq!(group.diff_added, Some(7));
        assert_eq!(group.diff_removed, Some(2));
        assert_eq!(group.label, "feat");
        // The own (sidebar) pane is excluded; the sibling renders as a row.
        assert!(
            snapshot
                .worktree_groups
                .iter()
                .flat_map(|group| &group.rows)
                .all(|row| {
                    row.pane
                        .as_ref()
                        .is_none_or(|pane| pane.pane_id.as_str() != own.as_str())
                }),
            "the renderer's own pane is never a row"
        );
    }

    #[test]
    fn consumer_own_view_counts_siblings_in_its_own_tab() {
        // A consumer reads the producer's session-wide pane list (`list-panes
        // -a`) and folds its own-view from it. An orphan sidebar — alone in its
        // tab — must see `Some(0)` siblings so self-close can fire, even though
        // the producer lives in another tab with its own siblings.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        let main_sb = pane_in_tab("main_sb", "@0");
        let main_term = pane_in_tab("main_term", "@0");
        let orphan_sb = pane_in_tab("orphan_sb", "@1");
        let base = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-test".to_owned(),
            panes: vec![main_sb, main_term, orphan_sb],
            snapshot: SidebarSnapshot::build(workspace, Vec::new(), Vec::new()),
        };
        atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();

        let orphan_own = PaneId::from_parts(MuxName::Zellij, "orphan_sb");
        let snapshot =
            read_published_snapshot(&runtime, "rimz-test", Some(&orphan_own)).expect("base");
        assert_eq!(
            snapshot.own_view.map(|view| view.sibling_count),
            Some(0),
            "an orphan sidebar sees zero siblings in its own tab so self-close can fire"
        );
    }

    #[test]
    fn read_published_snapshot_is_none_until_the_producer_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        assert!(read_published_snapshot(&runtime, "rimz-test", None).is_none());
    }

    #[test]
    fn read_snapshot_cache_misses_a_different_session() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let path = dir.path().join("snapshot.json");
        let cache = SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: "rimz-one".to_owned(),
            panes: Vec::new(),
            snapshot: SidebarSnapshot::build(workspace, Vec::new(), Vec::new()),
        };
        atomic::write_temp_then_rename(&path, &cache).unwrap();
        assert!(read_snapshot_cache(&path, "rimz-one").is_some());
        assert!(read_snapshot_cache(&path, "rimz-other").is_none());
    }

    #[test]
    fn diff_stats_cache_entry_expires_after_ttl() {
        let entry = DiffStatsCacheEntry::new(
            1_000,
            Some(DiffStats {
                added: 2,
                removed: 1,
            }),
            Some("feature-migration".to_owned()),
        );

        assert!(entry.is_fresh(1_000 + DIFF_STATS_TTL.as_millis() as u64));
        assert!(!entry.is_fresh(1_001 + DIFF_STATS_TTL.as_millis() as u64));
        assert_eq!(
            entry.stats(),
            Some(DiffStats {
                added: 2,
                removed: 1,
            })
        );
        assert_eq!(entry.branch.as_deref(), Some("feature-migration"));
    }
}
