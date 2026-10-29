//! The per-worktree git facts: the activity-tiered, single-flighted diff-stats
//! refresh (trunk ref → merge-base → status + numstat, where a clean tree at the
//! HEAD and merge-base the stored `+/-` was measured at skips the numstat →
//! one folded `rev-parse` for head/branch/merge state → landed verdict + one
//! `rev-list --left-right`), and their parsers. Commit-tier facts (ahead/behind,
//! landed, did_work) carry forward while HEAD/trunk and the clean verdict stay
//! unchanged. An unborn HEAD makes the folded `rev-parse` fail, so
//! head/branch/merge facts publish as absent until the first commit.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

use super::{git_line, git_output};
use crate::PaneId;
use crate::agents::AgentStatus;
use crate::disk::atomic;
use crate::disk::single_flight::{self, Coalesced};
use crate::sidebar::timing::{
    DIFF_STATS_FOCUSED_COMMIT_TTL, DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_HIBERNATED_TTL,
    DIFF_STATS_IDLE_TTL, DIFF_STATS_QUIET_TTL, DIFF_STATS_TTL, WORKTREE_ROOTS_TTL,
};
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind};
use crate::utils::time::unix_now_ms;
use crate::worktree::{self, LandedVerdict};

/// How a non-producing sidebar waits for the elected producer's diff-stats
/// write before refreshing locally. ~1.5s total (75 × 20ms) — wide enough for
/// coverage-instrumented git chains, yet still under the ~2s backstop tick.
const DIFF_STATS_WAIT_STEP: Duration = Duration::from_millis(20);
const DIFF_STATS_WAIT_STEPS: u32 = 75;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCache {
    pub entries: BTreeMap<String, DiffStatsCacheEntry>,
}

/// Long-lived producer state for content-derived Git facts that do not belong
/// in the renderer-consumed cache. Unneeded roots retain only unexpired lock windows.
#[derive(Debug, Default)]
pub(super) struct GitRefreshState {
    untracked: HashMap<String, UntrackedLineMemo>,
    hibernated: BTreeSet<String>,
    index_refresh: HashMap<String, IndexRefreshState>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IndexRefreshState {
    stale_entries: u64,
    last_locked_at: Option<Instant>,
}

const INDEX_REFRESH_STALE_THRESHOLD: u64 = 128;
const INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

impl IndexRefreshState {
    fn may_lock(&self, now: Instant) -> bool {
        self.stale_entries >= INDEX_REFRESH_STALE_THRESHOLD
            && self.last_locked_at.is_none_or(|started| {
                now.saturating_duration_since(started) >= INDEX_REFRESH_INTERVAL
            })
    }
}

impl GitRefreshState {
    fn retain_needed(&mut self, needed: &[String]) {
        self.untracked
            .retain(|path, _| needed.iter().any(|needed| needed == path));
        let now = Instant::now();
        self.index_refresh.retain(|path, state| {
            needed.iter().any(|needed| needed == path)
                || state.last_locked_at.is_some_and(|started| {
                    now.saturating_duration_since(started) < INDEX_REFRESH_INTERVAL
                })
        });
    }

    fn replace_if_successful(&mut self, path: String, memo: Option<UntrackedLineMemo>) {
        if let Some(memo) = memo {
            self.untracked.insert(path, memo);
        }
    }
}

type UntrackedLineMemo = BTreeMap<String, UntrackedLineMemoEntry>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UntrackedLineMemoEntry {
    modified: SystemTime,
    len: u64,
    added: u32,
}

/// The `lanes/group-roots.json` record: the repo's worktree checkout roots,
/// published whole by the producer's root enumeration, its one writer, and
/// cached under `WORKTREE_ROOTS_TTL` with a session-boundary refresh floor. The
/// set changes only on `git worktree add/remove`, so grouping reuses it across
/// ticks instead of forking `git worktree list` every snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::sidebar) struct WorktreeRootsCache {
    pub refreshed_at_ms: u64,
    pub roots: Vec<PathBuf>,
    /// Exact checkout-root to RimZ marker-name classifications.
    pub marker_names: BTreeMap<PathBuf, String>,
}

impl WorktreeRootsCache {
    /// Saturating, so a clock that ran backwards reads fresh rather than
    /// re-enumerating every tick.
    pub(in crate::sidebar) fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= WORKTREE_ROOTS_TTL.as_millis() as u64
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCacheEntry {
    /// Pass-start stamp for local/edit-sensitive facts: churn, dirty/untracked
    /// state, and live branch label.
    pub refreshed_at_ms: u64,
    /// Pass-start stamp for commit/PR facts: ahead/behind and landed markers.
    /// `None` means stale for entries written before the split.
    #[serde(default)]
    pub commit_refreshed_at_ms: Option<u64>,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Commits the worktree carries ahead of the trunk (`rev-list --count
    /// <merge-base>..HEAD`), refreshed on the same git tick as the diff.
    #[serde(default)]
    pub commits: Option<u32>,
    /// Commits the trunk has advanced past the fork point (`rev-list --count
    /// <merge-base>..<trunk>`), refreshed on the same git tick.
    #[serde(default)]
    pub behind: Option<u32>,
    /// The trunk ref the stats compared against, as the ladder resolved it
    /// (configured `[sidebar] trunk`, else `main`/`master`/remote default).
    /// Names the header's `≡` equal and `✓` clear markers.
    #[serde(default)]
    pub trunk: Option<String>,
    /// Live branch resolved from the worktree path, cached under the same TTL
    /// as the diff stats so the group header tracks `git checkout` without a
    /// git call every tick.
    #[serde(default)]
    pub branch: Option<String>,
    /// Whether the working tree is clean — `git status --porcelain` emptiness,
    /// untracked files included — the safe-to-remove verdict both content-landed
    /// markers (`≡` at the trunk tip, `✓` behind it) require. `None` on an old
    /// cache entry or a failed status read, which the renderer treats as not
    /// proven clean.
    #[serde(default)]
    pub clean: Option<bool>,
    /// Whether committed content is proven landed on the resolved trunk.
    /// `None` means unknown or an old cache entry.
    #[serde(default)]
    pub landed: Option<bool>,
    /// Whether the worktree carries work of its own: HEAD moved past the RimZ
    /// worktree marker's `base_ref` on a lineage outside the trunk's
    /// first-parent chain. `None` means the checkout is unmarked or unreadable.
    #[serde(default)]
    pub did_work: Option<bool>,
    /// Pull-request provenance stamped by `worktree create --from-pr`.
    #[serde(default)]
    pub from_pr: Option<u64>,
    /// Whether git reports an in-progress rebase, merge, or cherry-pick in the
    /// worktree. `None` means the probe could not inspect git paths.
    #[serde(default)]
    pub merge_in_progress: Option<bool>,
    /// HEAD sha observed while resolving ancestry facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// Trunk sha observed while resolving ancestry facts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk_sha: Option<String>,
    /// Merge-base between HEAD and the resolved trunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_base: Option<String>,
    /// HEAD sha the stored `added`/`removed` were measured at, or reused for.
    /// Unlike `head_sha`, a commit-only refresh leaves it alone, so it names
    /// the commits behind the numbers. `None` when no diff ran or was reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats_head_sha: Option<String>,
    /// Merge-base the stored `added`/`removed` were measured against; pairs
    /// with `stats_head_sha`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats_merge_base: Option<String>,
}

impl DiffStatsCacheEntry {
    /// Local-fact freshness under the caller's tier. Saturating, so a clock
    /// that ran backwards reads fresh rather than re-forking every tick.
    fn local_fresh_for(&self, now_ms: u64, ttl: Duration) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }

    /// Commit-fact freshness under the caller's tier. Old entries with no
    /// split stamp are commit-stale and get re-probed once.
    fn commit_fresh_for(&self, now_ms: u64, ttl: Duration) -> bool {
        self.commit_refreshed_at_ms
            .is_some_and(|stamp| now_ms.saturating_sub(stamp) <= ttl.as_millis() as u64)
    }

    pub(in crate::sidebar) fn stats(&self) -> Option<DiffStats> {
        self.added
            .zip(self.removed)
            .map(|(added, removed)| DiffStats { added, removed })
    }
}

/// Whether `branch` names this repository's trunk checkout. `main` remains a
/// trunk name even when the repository resolves a different configured or
/// remote-default trunk.
pub(in crate::sidebar) fn is_trunk_branch(branch: &str, trunk: Option<&str>) -> bool {
    branch == "main"
        || trunk.map(|trunk| trunk.strip_prefix("origin/").unwrap_or(trunk)) == Some(branch)
}

/// Which pass is refreshing: the long-lived host takes a bounded share of its
/// due worktrees each pass and works off a backlog over the next ones, while a
/// one-shot CLI has no next pass and refreshes everything due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar) enum GitPass {
    Host,
    OneShot,
}

impl GitPass {
    fn entry_limit(self) -> Option<usize> {
        match self {
            Self::Host => Some(MAX_HOST_PASS_ENTRIES),
            Self::OneShot => None,
        }
    }
}

/// Refresh the producer's per-worktree git facts. The git forks are the
/// producer's job — consumers and the fetch worker project the published cache
/// without reaching here. `configured_trunk` is the per-machine `[sidebar] trunk`
/// preference the trunk ladder tries first.
pub(super) fn refresh_diff_stats_for(
    snapshot: &SidebarSnapshot,
    runtime: &crate::RuntimePaths,
    configured_trunk: Option<&str>,
    pass: GitPass,
    state: &mut GitRefreshState,
) {
    let cache_path = runtime.diff_stats_path();
    let now_ms = unix_now_ms();
    // Focus tiers edit-sensitive facts first; activity gates recently worked
    // background worktrees. Hibernated roots take the slowest tier unless a
    // live condition demands their facts.
    let condition_scopes =
        crate::harness::schedule::when::probe_demand(runtime, snapshot.project_root.as_deref());
    let condition_scopes = condition_scopes.into_keys().collect();
    let hibernated = hibernated_worktree_paths(snapshot, &condition_scopes);
    let needed = needed_worktree_paths(snapshot, condition_scopes);
    let focused = focused_worktree_paths(snapshot);
    let hot = hot_worktree_paths(snapshot);
    let _ = refresh_diff_stats(
        &cache_path,
        runtime,
        &needed,
        &focused,
        &hot,
        &hibernated,
        now_ms,
        configured_trunk,
        pass,
        state,
    );
}

/// The checkout path a group's git reads run against. A path-keyed group —
/// per-path or root-keyed — carries it as the key's first line (the key may
/// carry a `\n<branch>` suffix when one path holds two branches), which is
/// stabler than any one row's cwd: a root-keyed pod's rows can sit in
/// different subdirs of one checkout. A non-path key (`branch:…`, the
/// `external` catch-all) falls back to the rows' shared path.
pub(in crate::sidebar) fn worktree_group_path_fields<'a>(
    key: &'a str,
    rows: &'a [crate::store::snapshot::SidebarRow],
) -> Option<&'a str> {
    key.split('\n')
        .next()
        .filter(|key| Path::new(key).is_absolute())
        .or_else(|| {
            rows.iter()
                .find_map(|row| row.lane_path())
                .filter(|path| !path.is_empty())
        })
}

/// The live worktree paths this snapshot needs git facts for: a git-backed
/// group whose recovered path is a live directory, de-duplicated so two
/// branch-split groups and live CI conditions for one dir share a single git read.
pub(super) fn needed_worktree_paths(
    snapshot: &SidebarSnapshot,
    condition_scopes: BTreeSet<String>,
) -> Vec<String> {
    let mut needed: Vec<String> = condition_scopes.into_iter().collect();
    for group in snapshot.worktree_groups.iter() {
        let Some(path) = git_backed_worktree_path(group) else {
            continue;
        };
        if !needed.iter().any(|known| known == &path) {
            needed.push(path);
        }
    }
    needed
}

/// The checkout path a worktree-like group reads git facts from. Ordinary
/// worktree groups trust their path; channel groups must carry the RimZ
/// worktree marker whose name matches the lane label before they inherit that
/// checkout's git story.
fn git_backed_worktree_path(group: &SidebarWorktreeGroup) -> Option<String> {
    let path = worktree_group_path_fields(&group.key, &group.rows)?;
    if !Path::new(path).is_dir() {
        return None;
    }
    match group.kind {
        SidebarWorktreeKind::Worktree => Some(path.to_owned()),
        SidebarWorktreeKind::Channel => {
            is_worktree_channel(path, &group.label).then(|| path.to_owned())
        }
        SidebarWorktreeKind::Root | SidebarWorktreeKind::External => None,
    }
}

fn is_worktree_channel(path: &str, label: &str) -> bool {
    worktree::read_marker_from_checkout_metadata(Path::new(path))
        .ok()
        .flatten()
        .is_some_and(|marker| marker.name == label)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HotWorktree {
    latest_activity_ms: u64,
    tool_hooked: bool,
}

/// Hot paths, each with the latest agent activity of its hot groups and
/// whether every agent kind in those groups supplies native tool hooks.
pub(super) fn hot_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeMap<String, HotWorktree> {
    let window = SignedDuration::try_from(crate::sidebar::timing::GIT_ACTIVITY_WINDOW)
        .unwrap_or(SignedDuration::MAX);
    let mut paths: BTreeMap<String, HotWorktree> = BTreeMap::new();
    for group in snapshot.worktree_groups.iter() {
        let mut agents = group.rows.iter().filter(|row| row.is_agent());
        let Some(latest) = agents.clone().map(|row| row.last_activity).max() else {
            continue;
        };
        let any_hot = agents.clone().any(|row| {
            row.status() == Some(AgentStatus::Running)
                || snapshot.now.duration_since(row.last_activity) <= window
        });
        if !any_hot {
            continue;
        }
        let Some(path) = git_backed_worktree_path(group) else {
            continue;
        };
        let tool_hooked = agents.all(|row| {
            crate::agents::spec_by_kind(&row.name)
                .is_some_and(|spec| spec.lifecycle_hooks.tool_used.is_native())
        });
        let activity = paths.entry(path).or_insert(HotWorktree {
            latest_activity_ms: 0,
            tool_hooked: true,
        });
        activity.latest_activity_ms = activity
            .latest_activity_ms
            .max(u64::try_from(latest.as_millisecond()).unwrap_or(0));
        activity.tool_hooked &= tool_hooked;
    }
    paths
}

/// The worktree paths whose edit-sensitive git facts refresh on the focused
/// tier.
pub(super) fn focused_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let viewed: HashSet<&PaneId> = snapshot.viewed_panes.iter().collect();
    let mut focused = BTreeSet::new();
    for group in snapshot.worktree_groups.iter() {
        if group.rows.iter().any(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| viewed.contains(&pane.pane_id))
        }) && let Some(path) = git_backed_worktree_path(group)
        {
            focused.insert(path);
        }
    }
    focused
}

/// Refresh the diff stats for `needed` worktree paths and return the cache map
/// to project. Single-flighted across the fleet, mirroring the snapshot cache:
/// the common case — every needed entry already fresh — touches no lock and
/// forks no git. Otherwise one elected producer forks git for the stale entries
/// ([`select_for_pass`]: on a [`GitPass::Host`] pass only a capped share) and
/// writes the shared cache once; the rest read its write back once their own
/// selection is fresh, or refresh that selection locally after a bounded wait
/// without writing — never overwriting the elected producer's map.
#[allow(
    clippy::too_many_arguments,
    reason = "the refresh boundary carries its selected roots, cadence tiers, and process-local memo"
)]
fn refresh_diff_stats(
    cache_path: &Path,
    runtime: &crate::RuntimePaths,
    needed: &[String],
    focused: &BTreeSet<String>,
    hot: &BTreeMap<String, HotWorktree>,
    hibernated: &BTreeSet<String>,
    now_ms: u64,
    configured_trunk: Option<&str>,
    pass: GitPass,
    state: &mut GitRefreshState,
) -> DiffStatsCache {
    state.retain_needed(needed);
    let cache: DiffStatsCache = atomic::read_json_cache(cache_path);
    let thawed: BTreeMap<_, _> = state
        .hibernated
        .difference(hibernated)
        .filter(|path| needed.contains(path))
        .map(|path| {
            let baseline = cache.entries.get(path).map_or((0, 0), |entry| {
                (
                    entry.refreshed_at_ms,
                    entry.commit_refreshed_at_ms.unwrap_or(0),
                )
            });
            (path.clone(), baseline)
        })
        .collect();
    state.hibernated = hibernated.clone();
    // One function carries the focus/activity/hold tiering, and every freshness
    // verdict — the no-lock fast path, the single-flight loser's probe, and both
    // produce arms — goes through it, so the tiers cannot disagree and a loser
    // never spin-produces what the winner correctly skipped.
    let stale = |cache: &DiffStatsCache| {
        stale_paths(cache, needed, focused, hot, hibernated, &thawed, now_ms)
    };

    // Fast path: nothing stale — no lock, no git, as the all-fresh tick already
    // behaved before the single-flight.
    if stale(&cache).is_empty() {
        return cache;
    }

    // The capped winner leaves some entries due on purpose, so the loser waits
    // for, and falls back to, only the selection it fixed from its first read.
    let select = |cache: &DiffStatsCache| {
        select_for_pass(stale(cache), cache, focused, &thawed, pass.entry_limit())
    };
    let selection = select(&cache);
    let selected: HashSet<&str> = selection.iter().map(|(path, _)| path.as_str()).collect();
    let lock_path = runtime.lock_path("diff-stats.lock");
    let fresh = || {
        let cache: DiffStatsCache = atomic::read_json_cache(cache_path);
        let selection_stale = stale(&cache)
            .iter()
            .any(|(path, _)| selected.contains(path.as_str()));
        (!selection_stale).then_some(cache)
    };
    match single_flight::coalesce(
        &lock_path,
        DIFF_STATS_WAIT_STEP,
        DIFF_STATS_WAIT_STEPS,
        fresh,
    ) {
        // A peer already refreshed every entry we selected.
        Coalesced::Shared(cache) => cache,
        // We won: re-read (a peer may have written between our miss and the
        // lock), refresh this pass's selection of what is still stale — git forks
        // run in parallel across worktrees — and write once.
        Coalesced::Produce(_guard) => {
            let mut cache: DiffStatsCache = atomic::read_json_cache(cache_path);
            let refreshed = refresh_entries(
                &select(&cache),
                &cache,
                now_ms,
                configured_trunk,
                pass,
                state,
            );
            let changed = !refreshed.is_empty();
            for (path, entry, memo, index_refresh) in refreshed {
                cache.entries.insert(path.clone(), entry);
                if pass == GitPass::Host {
                    state.index_refresh.insert(path.clone(), index_refresh);
                }
                state.replace_if_successful(path, memo);
            }
            if changed && let Err(err) = atomic::write_temp_then_rename_cache(cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar diff-stats cache write failed");
            }
            cache
        }
        // No peer publication read fresh in time: refresh locally for our own
        // frame, without overwriting the elected producer's map.
        Coalesced::ProduceLocal => {
            let mut cache = cache;
            for (path, entry, memo, index_refresh) in
                refresh_entries(&selection, &cache, now_ms, configured_trunk, pass, state)
            {
                cache.entries.insert(path.clone(), entry);
                if pass == GitPass::Host {
                    state.index_refresh.insert(path.clone(), index_refresh);
                }
                state.replace_if_successful(path, memo);
            }
            cache
        }
    }
}

/// The `needed` paths whose cached entry is due under their tier, with which
/// fact groups are due, in `needed` order.
fn stale_paths(
    cache: &DiffStatsCache,
    needed: &[String],
    focused: &BTreeSet<String>,
    hot: &BTreeMap<String, HotWorktree>,
    hibernated: &BTreeSet<String>,
    thawed: &BTreeMap<String, (u64, u64)>,
    now_ms: u64,
) -> Vec<(String, DueFacts)> {
    needed
        .iter()
        .filter_map(|path| {
            let entry = cache.entries.get(path.as_str());
            let Some(entry) = entry else {
                return Some((path.clone(), DueFacts::all()));
            };
            // A peer must advance both stamps beyond our first read. Equal
            // millisecond stamps do not prove that it observed the thaw.
            if thawed.get(path).is_some_and(|(local, commit)| {
                entry.refreshed_at_ms <= *local
                    || entry.commit_refreshed_at_ms.unwrap_or(0) <= *commit
            }) {
                return Some((path.clone(), DueFacts::all()));
            }
            let (local_ttl, commit_ttl) = diff_stats_tier(path, focused, hot, hibernated);
            let mut due = DueFacts {
                local: !entry.local_fresh_for(now_ms, local_ttl),
                commit: !entry.commit_fresh_for(now_ms, commit_ttl),
            };
            if !focused.contains(path)
                && let Some(activity) = hot.get(path).filter(|activity| activity.tool_hooked)
            {
                due.local &= activity.latest_activity_ms >= entry.refreshed_at_ms
                    || !entry.local_fresh_for(now_ms, DIFF_STATS_QUIET_TTL);
                due.commit &= entry
                    .commit_refreshed_at_ms
                    .is_none_or(|stamp| activity.latest_activity_ms >= stamp)
                    || !entry.commit_fresh_for(now_ms, DIFF_STATS_QUIET_TTL);
            }
            if local_ttl == commit_ttl {
                due = DueFacts::same(due.local || due.commit);
            }
            due.any().then(|| (path.clone(), due))
        })
        .collect()
}

/// The due entries this pass refreshes, in refresh order: focused paths, then
/// thawed ones (each in `needed` order), then oldest due first (a missing entry
/// or stamp reads 0), ties in `needed` order. A limit keeps every priority
/// entry and fills up to it from the rest.
fn select_for_pass(
    due: Vec<(String, DueFacts)>,
    cache: &DiffStatsCache,
    focused: &BTreeSet<String>,
    thawed: &BTreeMap<String, (u64, u64)>,
    limit: Option<usize>,
) -> Vec<(String, DueFacts)> {
    let (mut priority, mut rest): (Vec<_>, Vec<_>) = due
        .into_iter()
        .partition(|(path, _)| focused.contains(path) || thawed.contains_key(path));
    // Stable sorting keeps `needed` order within focused and thawed tiers.
    priority.sort_by_key(|(path, _)| !focused.contains(path));
    rest.sort_by_key(|(path, _)| oldest_due_stamp(cache.entries.get(path.as_str())));
    let take = limit.map_or(rest.len(), |limit| limit.saturating_sub(priority.len()));
    rest.truncate(take);
    priority.extend(rest);
    priority
}

fn oldest_due_stamp(entry: Option<&DiffStatsCacheEntry>) -> u64 {
    entry.map_or(0, |entry| {
        entry
            .refreshed_at_ms
            .min(entry.commit_refreshed_at_ms.unwrap_or(0))
    })
}

/// `(local, commit)` TTLs: focused, hot, hibernated, else idle.
fn diff_stats_tier(
    path: &str,
    focused: &BTreeSet<String>,
    hot: &BTreeMap<String, HotWorktree>,
    hibernated: &BTreeSet<String>,
) -> (Duration, Duration) {
    if focused.contains(path) {
        (DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_FOCUSED_COMMIT_TTL)
    } else if hot.contains_key(path) {
        (DIFF_STATS_TTL, DIFF_STATS_TTL)
    } else if hibernated.contains(path) {
        (DIFF_STATS_HIBERNATED_TTL, DIFF_STATS_HIBERNATED_TTL)
    } else {
        (DIFF_STATS_IDLE_TTL, DIFF_STATS_IDLE_TTL)
    }
}

fn hibernated_worktree_paths(
    snapshot: &SidebarSnapshot,
    condition_scopes: &BTreeSet<String>,
) -> BTreeSet<String> {
    let agents: HashMap<_, _> = snapshot
        .agents
        .iter()
        .map(|agent| ((agent.kind.as_str(), agent.agent_id.as_str()), agent))
        .collect();
    let mut hibernated = BTreeSet::new();
    let mut disqualified = BTreeSet::new();
    for group in snapshot.worktree_groups.iter() {
        let Some(path) = git_backed_worktree_path(group) else {
            continue;
        };
        for row in group.rows.iter() {
            if !row.is_agent() {
                disqualified.insert(path.clone());
                continue;
            }
            match agents.get(&(row.name.as_str(), row.id.as_str())) {
                Some(agent) if agent.ended_at.is_some() => {}
                Some(agent) if agent.hibernation.is_some() => {
                    hibernated.insert(path.clone());
                }
                _ => {
                    disqualified.insert(path.clone());
                }
            }
        }
    }
    hibernated.retain(|path| !disqualified.contains(path) && !condition_scopes.contains(path));
    hibernated
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DueFacts {
    local: bool,
    commit: bool,
}

impl DueFacts {
    fn all() -> Self {
        Self::same(true)
    }

    fn same(due: bool) -> Self {
        Self {
            local: due,
            commit: due,
        }
    }

    fn any(self) -> bool {
        self.local || self.commit
    }
}

struct LocalFacts {
    stats: Option<DiffStats>,
    /// The `(head, merge-base)` pair `stats` was measured or reused at.
    stats_at: Option<(String, String)>,
    branch: Option<String>,
    clean: Option<bool>,
    merge_in_progress: Option<bool>,
    untracked_memo: Option<UntrackedLineMemo>,
}

struct CommitFacts {
    commits: Option<u32>,
    behind: Option<u32>,
    trunk: Option<String>,
    landed: Option<bool>,
    did_work: Option<bool>,
    from_pr: Option<u64>,
}

#[derive(Default)]
struct HeadFacts {
    head_sha: Option<String>,
    branch: Option<String>,
    merge_in_progress: Option<bool>,
}

/// Most worktrees probed concurrently. A bounded worker pool keeps independent
/// worktrees saturated while each worktree's own chain stays sequential; the cap
/// keeps a many-worktree fleet from bursting a fork storm.
const MAX_PARALLEL_GIT: usize = 8;

/// Most due worktrees one host pass refreshes, one wave of the worker pool. A
/// cold start or a clump of expiring stamps then spreads over several passes
/// instead of bursting in one. Viewed and just-thawed worktrees count against
/// it but are always selected, first.
const MAX_HOST_PASS_ENTRIES: usize = 8;

type RefreshedEntry = (
    String,
    DiffStatsCacheEntry,
    Option<UntrackedLineMemo>,
    IndexRefreshState,
);

/// Refresh several worktrees' due diff-stats facts concurrently, returning each
/// path's merged entry. Independent worktrees run in parallel — bounded to
/// [`MAX_PARALLEL_GIT`] live `git` chains at a time — while each path's own
/// `trunk ref → merge-base → selected facts` chain stays sequential. Runs on
/// the diff-stats producer (the fetch worker), never the render thread.
fn refresh_entries(
    paths: &[(String, DueFacts)],
    cache: &DiffStatsCache,
    now_ms: u64,
    configured_trunk: Option<&str>,
    pass: GitPass,
    state: &GitRefreshState,
) -> Vec<RefreshedEntry> {
    crate::utils::parallel::bounded_map(
        crate::lane::current(),
        MAX_PARALLEL_GIT,
        paths,
        |(path, due)| {
            let prior = cache.entries.get(path.as_str()).cloned();
            let prior_memo = state
                .untracked
                .get(path.as_str())
                .cloned()
                .unwrap_or_default();
            let mut index_refresh = state
                .index_refresh
                .get(path.as_str())
                .copied()
                .unwrap_or_default();
            let (entry, memo) = refresh_entry_with_memo(
                path,
                prior.as_ref(),
                *due,
                now_ms,
                configured_trunk,
                &prior_memo,
                (pass == GitPass::Host).then_some(&mut index_refresh),
            );
            (path.clone(), entry, memo, index_refresh)
        },
    )
}

/// Produce a merged diff-stats entry for one worktree path. The trunk and
/// merge-base are resolved once when either half is due; local and commit facts
/// update only their own fields and pass-start stamps, so a focused worktree's
/// edit tick does not pay for commit/landed facts.
#[cfg(test)]
fn refresh_entry(
    path: &str,
    prior: Option<&DiffStatsCacheEntry>,
    due: DueFacts,
    configured_trunk: Option<&str>,
) -> DiffStatsCacheEntry {
    refresh_entry_with_memo(
        path,
        prior,
        due,
        unix_now_ms(),
        configured_trunk,
        &Default::default(),
        None,
    )
    .0
}

fn refresh_entry_with_memo(
    path: &str,
    prior: Option<&DiffStatsCacheEntry>,
    due: DueFacts,
    now_ms: u64,
    configured_trunk: Option<&str>,
    prior_memo: &UntrackedLineMemo,
    index_refresh: Option<&mut IndexRefreshState>,
) -> (DiffStatsCacheEntry, Option<UntrackedLineMemo>) {
    let mut entry = prior.cloned().unwrap_or_default();
    let worktree = Path::new(path);
    let refs = super::git_refs::resolve(worktree, configured_trunk);
    let trunk = refs
        .as_ref()
        .map(|refs| refs.trunk_name.clone())
        .or_else(|| trunk_ref(worktree, configured_trunk));
    let base = match (refs.as_ref(), prior) {
        (Some(refs), Some(prior)) if cached_refs_match(prior, refs) => prior.merge_base.clone(),
        _ => trunk
            .as_deref()
            .and_then(|trunk| diff_base(worktree, trunk)),
    };
    let head = refs
        .as_ref()
        .map(|refs| HeadFacts {
            head_sha: Some(refs.head_sha.clone()),
            branch: refs.head_branch.clone(),
            merge_in_progress: Some(refs.merge_in_progress),
        })
        .unwrap_or_else(|| head_facts(worktree));

    let mut replacement_memo = None;
    if due.local {
        let measured_at = refs
            .as_ref()
            .zip(base.as_deref())
            .map(|(refs, base)| (refs.head_sha.as_str(), base));
        let local = refresh_local_facts(
            worktree,
            base.as_deref(),
            measured_at,
            prior,
            &head,
            prior_memo,
            index_refresh,
        );
        entry.added = local.stats.map(|stats| stats.added);
        entry.removed = local.stats.map(|stats| stats.removed);
        let (stats_head_sha, stats_merge_base) = local.stats_at.unzip();
        entry.stats_head_sha = stats_head_sha;
        entry.stats_merge_base = stats_merge_base;
        entry.branch = local.branch;
        entry.clean = local.clean;
        entry.merge_in_progress = local.merge_in_progress;
        replacement_memo = local.untracked_memo;
    }
    if due.commit {
        // Read marker provenance even when git ancestry facts are reusable so
        // entries written before `from_pr` existed upgrade on their next tick.
        let marker = worktree::read_marker_from_checkout_metadata(worktree)
            .ok()
            .flatten();
        entry.from_pr = marker.as_ref().and_then(|marker| marker.from_pr);
        let reuse = refs
            .as_ref()
            .zip(prior)
            .is_some_and(|(refs, prior)| cached_commit_facts_match(prior, refs, entry.clean));
        if !reuse {
            let commit = refresh_commit_facts(
                worktree,
                base.as_deref(),
                trunk.as_deref(),
                entry.clean,
                head.head_sha.as_deref(),
                marker.as_ref(),
            );
            entry.commits = commit.commits;
            entry.behind = commit.behind;
            entry.trunk = commit.trunk;
            entry.landed = commit.landed;
            entry.did_work = commit.did_work;
            entry.from_pr = commit.from_pr;
        }
    }
    if let Some(refs) = refs {
        entry.head_sha = Some(refs.head_sha);
        entry.trunk_sha = Some(refs.trunk_sha);
        entry.merge_base = base;
    } else {
        entry.head_sha = head.head_sha.clone();
        entry.trunk_sha = None;
        entry.merge_base = base;
    }

    if due.local {
        entry.refreshed_at_ms = now_ms;
    }
    if due.commit {
        entry.commit_refreshed_at_ms = Some(now_ms);
    }
    (entry, replacement_memo)
}

fn cached_refs_match(prior: &DiffStatsCacheEntry, refs: &super::git_refs::GitRefs) -> bool {
    prior.head_sha.as_deref() == Some(refs.head_sha.as_str())
        && prior.trunk_sha.as_deref() == Some(refs.trunk_sha.as_str())
        && prior.trunk.as_deref() == Some(refs.trunk_name.as_str())
        && prior.merge_base.is_some()
}

fn cached_commit_facts_match(
    prior: &DiffStatsCacheEntry,
    refs: &super::git_refs::GitRefs,
    clean: Option<bool>,
) -> bool {
    cached_refs_match(prior, refs)
        && prior.commit_refreshed_at_ms.is_some()
        && prior.clean == clean
        && prior.commits.is_some()
        && prior.behind.is_some()
        && landed_fact_reusable(prior)
}

/// The prior `added`/`removed`, when a clean tree at the same `(head, base)`
/// they were measured at makes a fresh `git diff` redundant. `measured_at` is
/// `None` without resolved refs or a base, which always diffs.
fn reusable_diff_stats(
    prior: Option<&DiffStatsCacheEntry>,
    measured_at: Option<(&str, &str)>,
    clean: Option<bool>,
) -> Option<DiffStats> {
    let (head_sha, merge_base) = measured_at?;
    let prior = prior?;
    (clean == Some(true)
        && prior.clean == Some(true)
        && prior.stats_head_sha.as_deref() == Some(head_sha)
        && prior.stats_merge_base.as_deref() == Some(merge_base))
    .then(|| prior.stats())?
}

fn landed_fact_reusable(prior: &DiffStatsCacheEntry) -> bool {
    matches!(
        (prior.commits, prior.clean, prior.landed),
        (Some(0), _, Some(true))
            | (Some(_), Some(true), Some(_))
            | (Some(_), Some(false) | None, None)
    )
}

/// Status runs first: a clean tree whose diff inputs match the prior entry's
/// measurement reuses its numbers and forks no `git diff`.
fn refresh_local_facts(
    worktree: &Path,
    base: Option<&str>,
    measured_at: Option<(&str, &str)>,
    prior: Option<&DiffStatsCacheEntry>,
    head: &HeadFacts,
    prior_memo: &UntrackedLineMemo,
    index_refresh: Option<&mut IndexRefreshState>,
) -> LocalFacts {
    let (status, untracked_memo) = worktree_status(worktree, prior_memo, index_refresh);
    let clean = status.as_ref().map(|status| status.clean);
    let stats = reusable_diff_stats(prior, measured_at, clean)
        .or_else(|| base.and_then(|base| worktree_diff_stats(worktree, base)));
    let stats_at = stats
        .and(measured_at)
        .map(|(head_sha, merge_base)| (head_sha.to_owned(), merge_base.to_owned()));
    // Untracked content is change the diff is blind to: fold its line count
    // into the `+` churn so an untracked-only worktree reads as carrying work,
    // never as landed.
    let stats = match (stats, &status) {
        (Some(stats), Some(status)) => Some(DiffStats {
            added: stats.added.saturating_add(status.untracked_added),
            removed: stats.removed,
        }),
        (stats, _) => stats,
    };
    LocalFacts {
        stats,
        stats_at,
        branch: head.branch.clone(),
        clean,
        merge_in_progress: head.merge_in_progress,
        untracked_memo,
    }
}

fn refresh_commit_facts(
    worktree: &Path,
    base: Option<&str>,
    trunk: Option<&str>,
    clean: Option<bool>,
    head_sha: Option<&str>,
    marker: Option<&worktree::WorktreeMarker>,
) -> CommitFacts {
    let (commits, behind) = base
        .zip(trunk)
        .and_then(|(_, trunk)| commits_ahead_behind(worktree, trunk))
        .map(|(commits, behind)| (Some(commits), Some(behind)))
        .unwrap_or((None, None));
    let landed = match (commits, clean, trunk) {
        (Some(0), _, _) => Some(true),
        (Some(_), Some(true), Some(trunk)) => {
            match worktree::content_landed(worktree, trunk, "HEAD") {
                LandedVerdict::Landed => Some(true),
                LandedVerdict::Unlanded => Some(false),
                LandedVerdict::Unknown => None,
            }
        }
        _ => None,
    };
    let did_work = marker.and_then(|marker| {
        let head = head_sha?;
        Some(if head == marker.base_ref.as_str() {
            false
        } else if let Some(trunk) = trunk {
            !worktree::on_trunk_first_parent(worktree, trunk, head)
        } else {
            true
        })
    });
    CommitFacts {
        commits,
        behind,
        trunk: trunk.map(ToOwned::to_owned),
        landed,
        did_work,
        from_pr: marker.and_then(|marker| marker.from_pr),
    }
}

fn head_facts(worktree: &Path) -> HeadFacts {
    let output = git_output(
        worktree,
        &[
            "rev-parse",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
            "--git-path",
            "MERGE_HEAD",
            "--git-path",
            "CHERRY_PICK_HEAD",
            "--git-path",
            "rebase-merge",
            "--git-path",
            "rebase-apply",
        ],
    );
    let Some(output) = output else {
        return HeadFacts::default();
    };
    if !output.status.success() {
        return HeadFacts::default();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines().map(str::trim);
    let head_sha = lines
        .next()
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned);
    let branch = lines
        .next()
        .filter(|line| !line.is_empty() && *line != "HEAD")
        .map(ToOwned::to_owned);
    let merge_in_progress = lines
        .take(4)
        .filter(|line| !line.is_empty())
        .any(|raw| git_path_from_rev_parse(worktree, raw).exists());

    HeadFacts {
        head_sha,
        branch,
        merge_in_progress: Some(merge_in_progress),
    }
}

fn git_path_from_rev_parse(worktree: &Path, raw: &str) -> std::path::PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    }
}

/// The total diff the worktree carries relative to `main`: committed, staged,
/// and unstaged changes folded into one `+/-`. We diff the *working tree*
/// against the `base` merge-base with the trunk, so it counts what this branch
/// added on top of where it forked — never the trunk's own progress since the
/// fork — and `git diff <commit>` reads the tree on disk, so staged and unstaged
/// work land in the same number as committed work. Untracked files are
/// invisible to `git diff`; [`refresh_local_facts`] folds their line count in
/// from the status probe.
fn worktree_diff_stats(worktree: &Path, base: &str) -> Option<DiffStats> {
    let output = git_output(
        worktree,
        &[
            "--no-optional-locks",
            "-c",
            "diff.autoRefreshIndex=false",
            "diff",
            "--no-ext-diff",
            "--numstat",
            base,
            "--",
        ],
    )?;
    if !output.status.success() {
        return None;
    }
    Some(parse_numstat(&String::from_utf8_lossy(&output.stdout)))
}

/// The commits the worktree carries ahead of trunk, and the commits trunk has
/// advanced past the worktree. The symmetric-difference form gives left
/// (HEAD-only) and right (trunk-only) counts in one fork; callers gate it on a
/// resolved merge-base so unrelated histories keep publishing no counts.
fn commits_ahead_behind(worktree: &Path, trunk: &str) -> Option<(u32, u32)> {
    let range = format!("HEAD...{trunk}");
    let line = git_line(
        worktree,
        &["rev-list", "--count", "--left-right", range.as_str()],
    )?;
    let mut counts = line.split_whitespace();
    let ahead = parse_commit_count(counts.next()?)?;
    let behind = parse_commit_count(counts.next()?)?;
    if counts.next().is_some() {
        return None;
    }
    Some((ahead, behind))
}

/// One worktree's `git status` verdict: whether the working tree is clean — no
/// staged, unstaged, or untracked change, which the header's content-landed
/// `≡`/`✓` markers require — plus the added lines its untracked files carry,
/// folded into the header's `+` churn.
struct WorktreeStatus {
    clean: bool,
    untracked_added: u32,
    /// Entries a refresh cannot bring up to date: worktree
    /// modifications, type changes, intent-to-add paths, and worktree renames
    /// or copies. Deletions fail `lstat` first, and refresh skips unmerged entries
    /// (an unmerged `AA` or `UA` still counts, erring toward no lock).
    scanned_changes: u64,
    /// Deletions, including worktree rename sources, reach `sum_lstat` but not `sum_scan`.
    deleted_paths: u64,
}

/// Preload leaves only racy or stale entries for refresh to stat. Without it,
/// lstat counts the whole index, so only rehashed entries prove staleness.
fn index_stale_count(stderr: &[u8], status: &WorktreeStatus) -> u64 {
    #[derive(Deserialize)]
    struct TraceData<'a> {
        event: &'a str,
        #[serde(default)]
        sid: &'a str,
        #[serde(default)]
        category: &'a str,
        #[serde(default)]
        key: &'a str,
        #[serde(default)]
        value: &'a str,
    }

    let mut preloaded = false;
    let mut lstatted: u64 = 0;
    let mut scanned: u64 = 0;
    let mut session = None;
    for data in stderr
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<TraceData<'_>>(line).ok())
    {
        if data.sid != *session.get_or_insert(data.sid)
            || data.event != "data"
            || data.category != "index"
        {
            continue;
        }
        match data.key {
            "preload/sum_lstat" => preloaded = true,
            "refresh/sum_lstat" => lstatted = data.value.parse().unwrap_or(0),
            "refresh/sum_scan" => scanned = data.value.parse().unwrap_or(0),
            _ => {}
        }
    }
    if preloaded {
        lstatted
            .saturating_sub(status.scanned_changes)
            .saturating_sub(status.deleted_paths)
    } else {
        scanned.saturating_sub(status.scanned_changes)
    }
}

/// `git status --porcelain=v1 -z --untracked-files=all`: emptiness → clean, and
/// each `??` entry's file line-counts into the untracked churn.
/// `--untracked-files=all` lists files inside untracked directories
/// individually so the count sees them; `-z` keeps newline-bearing filenames
/// parseable (paths decode lossily, so a non-UTF-8 path still dirties the tree
/// but counts no lines). Only host status enables trace2. Optional index locks
/// stay disabled unless the prior status found at least 128 stale entries
/// through [`index_stale_count`], and no locked status started within 60 seconds.
/// A contended attempt consumes that window too; one-shot callers never lock.
fn worktree_status(
    worktree: &Path,
    prior: &UntrackedLineMemo,
    mut index_refresh: Option<&mut IndexRefreshState>,
) -> (Option<WorktreeStatus>, Option<UntrackedLineMemo>) {
    let mut command = crate::proc::git_command_uncounted(worktree);
    let locked = if let Some(state) = index_refresh.as_deref_mut() {
        command.env("GIT_TRACE2_EVENT", "1");
        let started = Instant::now();
        let locked = state.may_lock(started);
        state.stale_entries = 0;
        if locked {
            state.last_locked_at = Some(started);
        }
        locked
    } else {
        false
    };
    if !locked {
        command.arg("--no-optional-locks");
    }
    command.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
    let Ok(output) = crate::proc::run_bounded_output(&mut command, super::GIT_COMMAND_TIMEOUT)
    else {
        return (None, None);
    };
    if output.timed_out || !output.status.success() {
        return (None, None);
    }
    let mut budget = UNTRACKED_READ_BUDGET;
    let mut next = UntrackedLineMemo::new();
    let status = parse_status_entries(&String::from_utf8_lossy(&output.stdout), |path| {
        memoized_untracked_added_lines(worktree, path, prior, &mut next, &mut budget)
    });
    if let Some(state) = index_refresh {
        state.stale_entries = index_stale_count(&output.stderr, &status);
    }
    (Some(status), Some(next))
}

fn memoized_untracked_added_lines(
    worktree: &Path,
    relative: &str,
    prior: &UntrackedLineMemo,
    next: &mut UntrackedLineMemo,
    budget: &mut u64,
) -> u32 {
    memoized_untracked_added_lines_with(worktree, relative, prior, next, budget, &mut |path| {
        std::fs::read(path)
    })
}

fn memoized_untracked_added_lines_with(
    worktree: &Path,
    relative: &str,
    prior: &UntrackedLineMemo,
    next: &mut UntrackedLineMemo,
    budget: &mut u64,
    read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
) -> u32 {
    let path = worktree.join(relative);
    let Ok(meta) = std::fs::metadata(&path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > *budget {
        return 0;
    }
    let Ok(modified) = meta.modified() else {
        return 0;
    };
    if let Some(hit) = prior
        .get(relative)
        .filter(|hit| hit.modified == modified && hit.len == meta.len())
        .copied()
    {
        *budget = budget.saturating_sub(hit.len);
        next.insert(relative.to_owned(), hit);
        return hit.added;
    }
    let Ok(bytes) = read(&path) else {
        return 0;
    };
    *budget = budget.saturating_sub(meta.len());
    let added = count_added_lines(&bytes);
    next.insert(
        relative.to_owned(),
        UntrackedLineMemoEntry {
            modified,
            len: meta.len(),
            added,
        },
    );
    added
}

/// Fold a porcelain v1 `-z` status stream: any entry means a dirty tree, and
/// each untracked (`??`) path feeds the line counter. A rename/copy entry —
/// detected in either status column — carries its source path as a second
/// NUL-separated token, consumed and skipped so it never reads as an entry of
/// its own.
fn parse_status_entries(
    output: &str,
    mut untracked_lines: impl FnMut(&str) -> u32,
) -> WorktreeStatus {
    let mut clean = true;
    let mut untracked_added: u32 = 0;
    let mut scanned_changes = 0;
    let mut deleted_paths = 0;
    let mut tokens = output.split('\0').filter(|token| !token.is_empty());
    while let Some(entry) = tokens.next() {
        clean = false;
        let Some((code, path)) = entry.split_at_checked(3) else {
            continue;
        };
        if code.get(..2).is_some_and(|xy| xy.contains(['R', 'C'])) {
            tokens.next();
        }
        if matches!(code.as_bytes()[1], b'M' | b'T' | b'A' | b'R' | b'C') {
            scanned_changes += 1;
        }
        if matches!(code.as_bytes()[1], b'D' | b'R') {
            deleted_paths += 1;
        }
        if code.starts_with("??") {
            untracked_added = untracked_added.saturating_add(untracked_lines(path));
        }
    }
    WorktreeStatus {
        clean,
        untracked_added,
        scanned_changes,
        deleted_paths,
    }
}

/// Shared byte budget for one status probe's untracked reads. A file past the
/// remaining budget still marks the tree dirty through its status entry; the
/// budget only bounds the churn line count, so one refresh never reads more
/// than this however many untracked files the tree holds.
const UNTRACKED_READ_BUDGET: u64 = 8 * 1024 * 1024;

/// Line count of a blob the way numstat counts an added file: newlines plus a
/// trailing partial line. A NUL in the first 8000 bytes reads as binary (git's
/// own heuristic) and counts nothing, mirroring numstat's `-` cells.
fn count_added_lines(bytes: &[u8]) -> u32 {
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let tail = usize::from(bytes.last().is_some_and(|byte| *byte != b'\n'));
    (newlines + tail).min(u32::MAX as usize) as u32
}

fn parse_commit_count(count: &str) -> Option<u32> {
    count
        .parse::<u64>()
        .ok()
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
}

/// The commit a worktree's diff is measured against: the merge-base between its
/// HEAD and the repo's trunk — the fork point a PR diffs against. Returns
/// `None` (so the header simply omits stats) when there is no shared ancestor,
/// e.g. an orphan branch.
fn diff_base(worktree: &Path, trunk: &str) -> Option<String> {
    git_line(worktree, &["merge-base", "HEAD", trunk])
}

/// The repo's trunk branch: the configured `[sidebar] trunk` when it resolves
/// in this repo, else the local `main`/`master` a worktree forks from and
/// merges back into, falling back to the remote's advertised default for a
/// non-standard name. The configured name is a machine-wide *preference* — a
/// repo without that branch falls through to detection rather than losing its
/// stats — and an option-shaped name (leading `-`) is never handed to git.
/// Branch refs are shared across a repo's worktrees, so this resolves from
/// inside any of them.
fn trunk_ref(worktree: &Path, configured: Option<&str>) -> Option<String> {
    let configured = configured.filter(|name| !name.is_empty() && !name.starts_with('-'));
    for name in configured.into_iter().chain(["main", "master"]) {
        if git_line(worktree, &["rev-parse", "--verify", "--quiet", name]).is_some() {
            return Some(name.to_owned());
        }
    }
    git_line(
        worktree,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
}

fn parse_numstat(output: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in output.lines() {
        let mut columns = line.split('\t');
        stats.added = stats
            .added
            .saturating_add(parse_numstat_cell(columns.next()));
        stats.removed = stats
            .removed
            .saturating_add(parse_numstat_cell(columns.next()));
    }
    stats
}

fn parse_numstat_cell(cell: Option<&str>) -> u32 {
    cell.and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
