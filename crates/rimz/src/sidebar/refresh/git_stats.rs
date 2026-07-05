//! The per-worktree git facts: the activity-tiered, single-flighted diff-stats
//! refresh (trunk ref → merge-base → numstat + status → one folded `rev-parse`
//! for head/branch/merge state → landed verdict + one `rev-list --left-right`),
//! and their parsers. Commit-tier facts (ahead/behind, landed, did_work) carry
//! forward while HEAD/trunk and the clean verdict stay unchanged. An unborn HEAD
//! makes the folded `rev-parse` fail, so head/branch/merge facts publish as
//! absent until the first commit.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

use crate::agents::AgentStatus;
use crate::ledger::atomic;
use crate::ledger::single_flight::{self, Coalesced};
use crate::sidebar::timing::{
    DIFF_STATS_FOCUSED_COMMIT_TTL, DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_IDLE_TTL,
    DIFF_STATS_TTL, WORKTREE_ROOTS_TTL, unix_now_ms,
};
use crate::worktree::{self, LandedVerdict};
use crate::{PaneId, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind};

/// How a non-producing sidebar waits for the elected producer's diff-stats
/// write before refreshing locally. ~1.5s total (75 × 20ms) — wide enough for
/// coverage-instrumented git chains, yet still under the ~2s backstop tick.
const DIFF_STATS_WAIT_STEP: Duration = Duration::from_millis(20);
const DIFF_STATS_WAIT_STEPS: u32 = 75;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCache {
    pub entries: BTreeMap<String, DiffStatsCacheEntry>,
    /// The repo's worktree checkout roots, cached under [`WORKTREE_ROOTS_TTL`]
    /// (with a session-boundary refresh floor). The set changes only on
    /// `git worktree add/remove`, so grouping reuses it across ticks instead
    /// of forking `git worktree list` every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<WorktreeRootsCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRootsCache {
    pub refreshed_at_ms: u64,
    pub roots: Vec<PathBuf>,
}

impl WorktreeRootsCache {
    /// Saturating, so a clock that ran backwards reads fresh rather than
    /// re-enumerating every tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= WORKTREE_ROOTS_TTL.as_millis() as u64
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiffStatsCacheEntry {
    /// Local/edit-sensitive facts stamp: churn, dirty/untracked state, and live
    /// branch label.
    pub refreshed_at_ms: u64,
    /// Commit/PR-shaped facts stamp: ahead/behind counts and landed markers.
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
    /// Whether the worktree carries work of its own: HEAD moved past the Rimz
    /// worktree marker's `base_ref` on a lineage outside the trunk's
    /// first-parent chain. `None` means the checkout is unmarked or unreadable.
    #[serde(default)]
    pub did_work: Option<bool>,
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
}

impl DiffStatsCacheEntry {
    /// Local-fact freshness under the caller's tier. Saturating, so a clock
    /// that ran backwards reads fresh rather than re-forking every tick.
    pub fn local_fresh_for(&self, now_ms: u64, ttl: Duration) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }

    /// Commit-fact freshness under the caller's tier. Old entries with no
    /// split stamp are commit-stale and get re-probed once.
    pub fn commit_fresh_for(&self, now_ms: u64, ttl: Duration) -> bool {
        self.commit_refreshed_at_ms
            .is_some_and(|stamp| now_ms.saturating_sub(stamp) <= ttl.as_millis() as u64)
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

/// Refresh the producer's per-worktree git facts. The git forks are the
/// producer's job — consumers and the fetch worker project the published cache
/// without reaching here. `configured_trunk` is the per-machine `[sidebar] trunk`
/// preference the trunk ladder tries first.
pub(crate) fn refresh_diff_stats_for(
    snapshot: &SidebarSnapshot,
    runtime: &crate::RuntimePaths,
    configured_trunk: Option<&str>,
) {
    let cache_path = runtime.diff_stats_path();
    let now_ms = unix_now_ms();
    // Focus tiers edit-sensitive facts first; activity still keeps recently
    // worked background worktrees on the hot TTL while the rest decay to idle.
    let needed = needed_worktree_paths(snapshot);
    let focused = focused_worktree_paths(snapshot);
    let hot = hot_worktree_paths(snapshot);
    let _ = refresh_diff_stats(
        &cache_path,
        runtime,
        &needed,
        &focused,
        &hot,
        now_ms,
        configured_trunk,
    );
}

/// The checkout path a group's git reads run against. A path-keyed group —
/// per-path or root-keyed — carries it as the key's first line (the key may
/// carry a `\n<branch>` suffix when one path holds two branches), which is
/// stabler than any one row's cwd: a root-keyed pod's rows can sit in
/// different subdirs of one checkout. A non-path key (`branch:…`, the
/// `external` catch-all) falls back to the rows' shared path.
pub(crate) fn worktree_group_path(group: &SidebarWorktreeGroup) -> Option<&str> {
    group
        .key
        .split('\n')
        .next()
        .filter(|key| Path::new(key).is_absolute())
        .or_else(|| {
            group
                .rows
                .iter()
                .find_map(|row| row.worktree_path.as_deref())
                .filter(|path| !path.is_empty())
        })
}

/// The live worktree paths this snapshot needs git facts for: a git-backed
/// group whose recovered path is a live directory, de-duplicated so two
/// branch-split groups for one dir share a single git read.
pub(crate) fn needed_worktree_paths(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut needed: Vec<String> = Vec::new();
    for group in &snapshot.worktree_groups {
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
/// worktree groups trust their path; channel groups must carry the Rimz
/// worktree marker whose name matches the lane label before they inherit that
/// checkout's git story.
pub(crate) fn git_backed_worktree_path(group: &SidebarWorktreeGroup) -> Option<String> {
    let path = worktree_group_path(group)?;
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

/// The worktree paths whose git facts refresh on the fast [`DIFF_STATS_TTL`].
pub(crate) fn hot_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let window = SignedDuration::try_from(crate::sidebar::timing::GIT_ACTIVITY_WINDOW)
        .unwrap_or(SignedDuration::MAX);
    let mut hot = BTreeSet::new();
    for group in &snapshot.worktree_groups {
        let Some(path) = git_backed_worktree_path(group) else {
            continue;
        };
        let any_hot = group.rows.iter().any(|row| {
            row.is_agent()
                && (row.status() == Some(AgentStatus::Running)
                    || snapshot.now.duration_since(row.last_activity) <= window)
        });
        if any_hot {
            hot.insert(path);
        }
    }
    hot
}

/// The worktree paths whose edit-sensitive git facts refresh on the focused
/// tier.
pub(crate) fn focused_worktree_paths(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let viewed: HashSet<&PaneId> = snapshot.viewed_panes.iter().collect();
    let mut focused = BTreeSet::new();
    for group in &snapshot.worktree_groups {
        let Some(path) = git_backed_worktree_path(group) else {
            continue;
        };
        if group.rows.iter().any(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| viewed.contains(&pane.pane_id))
        }) {
            focused.insert(path);
        }
    }
    focused
}

/// Refresh the diff stats for `needed` worktree paths and return the cache map
/// to project. Single-flighted across the fleet, mirroring the snapshot cache:
/// the common case — every needed entry already fresh — touches no lock and
/// forks no git. Otherwise one elected producer forks git for the stale entries
/// and writes the shared cache once; the rest read its write back, or (if it
/// wedges) refresh locally for their own frame without writing — never
/// clobbering the producer's fresher map.
fn refresh_diff_stats(
    cache_path: &Path,
    runtime: &crate::RuntimePaths,
    needed: &[String],
    focused: &BTreeSet<String>,
    hot: &BTreeSet<String>,
    now_ms: u64,
    configured_trunk: Option<&str>,
) -> DiffStatsCache {
    // One closure carries the focus/activity tiering, and every freshness verdict —
    // the no-lock fast path, the single-flight loser's probe, and both produce
    // arms — goes through it, so the tiers cannot disagree and a loser never
    // spin-produces what the winner correctly skipped.
    let stale = |cache: &DiffStatsCache| -> Vec<(String, DueFacts)> {
        needed
            .iter()
            .filter_map(|path| {
                let (local_ttl, commit_ttl) = diff_stats_tier(path, focused, hot);
                let Some(entry) = cache.entries.get(path.as_str()) else {
                    return Some((path.clone(), DueFacts::all()));
                };
                let mut due = DueFacts {
                    local: !entry.local_fresh_for(now_ms, local_ttl),
                    commit: !entry.commit_fresh_for(now_ms, commit_ttl),
                };
                if local_ttl == commit_ttl {
                    due = DueFacts::same(due.local || due.commit);
                }
                due.any().then(|| (path.clone(), due))
            })
            .collect()
    };

    let cache = read_diff_stats_cache(cache_path);
    // Fast path: nothing stale — no lock, no git, as the all-fresh tick already
    // behaved before the single-flight.
    if stale(&cache).is_empty() {
        return cache;
    }

    let lock_path = runtime.root.join("diff-stats.lock");
    let fresh = || {
        let cache = read_diff_stats_cache(cache_path);
        stale(&cache).is_empty().then_some(cache)
    };
    match single_flight::coalesce(
        &lock_path,
        DIFF_STATS_WAIT_STEP,
        DIFF_STATS_WAIT_STEPS,
        fresh,
    ) {
        // A peer already refreshed every entry we need.
        Coalesced::Shared(cache) => cache,
        // We won: re-read (a peer may have written between our miss and the
        // lock), refresh only what is still stale against that read — git forks
        // run in parallel across worktrees — and write once.
        Coalesced::Produce(_guard) => {
            let mut cache = read_diff_stats_cache(cache_path);
            let refreshed = refresh_entries(&stale(&cache), &cache, configured_trunk);
            let changed = !refreshed.is_empty();
            for (path, entry) in refreshed {
                cache.entries.insert(path, entry);
            }
            if changed && let Err(err) = atomic::write_temp_then_rename_cache(cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar diff-stats cache write failed");
            }
            cache
        }
        // The producer wedged: refresh locally for our own frame, but do not
        // write — the producer's map will be fresher.
        Coalesced::ProduceLocal => {
            let mut cache = cache;
            for (path, entry) in refresh_entries(&stale(&cache), &cache, configured_trunk) {
                cache.entries.insert(path, entry);
            }
            cache
        }
    }
}

fn diff_stats_tier(
    path: &str,
    focused: &BTreeSet<String>,
    hot: &BTreeSet<String>,
) -> (Duration, Duration) {
    if focused.contains(path) {
        (DIFF_STATS_FOCUSED_LOCAL_TTL, DIFF_STATS_FOCUSED_COMMIT_TTL)
    } else if hot.contains(path) {
        (DIFF_STATS_TTL, DIFF_STATS_TTL)
    } else {
        (DIFF_STATS_IDLE_TTL, DIFF_STATS_IDLE_TTL)
    }
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
    branch: Option<String>,
    clean: Option<bool>,
    merge_in_progress: Option<bool>,
}

struct CommitFacts {
    commits: Option<u32>,
    behind: Option<u32>,
    trunk: Option<String>,
    landed: Option<bool>,
    did_work: Option<bool>,
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

/// Refresh several worktrees' due diff-stats facts concurrently, returning each
/// path's merged entry. Independent worktrees run in parallel — bounded to
/// [`MAX_PARALLEL_GIT`] live `git` chains at a time — while each path's own
/// `trunk ref → merge-base → selected facts` chain stays sequential. Runs on
/// the diff-stats producer (the fetch worker), never the render thread.
fn refresh_entries(
    paths: &[(String, DueFacts)],
    cache: &DiffStatsCache,
    configured_trunk: Option<&str>,
) -> Vec<(String, DiffStatsCacheEntry)> {
    if paths.is_empty() {
        return Vec::new();
    }
    let lane = crate::lane::current();
    let next = AtomicUsize::new(0);
    let workers = MAX_PARALLEL_GIT.min(paths.len());
    let mut out = Vec::with_capacity(paths.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    crate::lane::set(lane);
                    let mut local = Vec::new();
                    loop {
                        let idx = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, due)) = paths.get(idx) else {
                            break;
                        };
                        let prior = cache.entries.get(path.as_str()).cloned();
                        local.push((
                            path.clone(),
                            refresh_entry(path, prior.as_ref(), *due, configured_trunk),
                        ));
                    }
                    local
                })
            })
            .collect();
        for handle in handles {
            if let Ok(local) = handle.join() {
                out.extend(local);
            }
        }
    });
    out
}

/// Produce a merged diff-stats entry for one worktree path. The trunk and
/// merge-base are resolved once when either half is due; local and commit facts
/// update only their own fields and completion stamps, so a focused worktree's
/// edit tick does not pay for commit/landed facts.
fn refresh_entry(
    path: &str,
    prior: Option<&DiffStatsCacheEntry>,
    due: DueFacts,
    configured_trunk: Option<&str>,
) -> DiffStatsCacheEntry {
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

    if due.local {
        let local = refresh_local_facts(
            worktree,
            base.as_deref(),
            head.branch.clone(),
            head.merge_in_progress,
        );
        entry.added = local.stats.map(|stats| stats.added);
        entry.removed = local.stats.map(|stats| stats.removed);
        entry.branch = local.branch;
        entry.clean = local.clean;
        entry.merge_in_progress = local.merge_in_progress;
    }
    if due.commit {
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
            );
            entry.commits = commit.commits;
            entry.behind = commit.behind;
            entry.trunk = commit.trunk;
            entry.landed = commit.landed;
            entry.did_work = commit.did_work;
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

    let completed_at_ms = unix_now_ms();
    if due.local {
        entry.refreshed_at_ms = completed_at_ms;
    }
    if due.commit {
        entry.commit_refreshed_at_ms = Some(completed_at_ms);
    }
    entry
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

fn landed_fact_reusable(prior: &DiffStatsCacheEntry) -> bool {
    matches!(
        (prior.commits, prior.clean, prior.landed),
        (Some(0), _, Some(true))
            | (Some(_), Some(true), Some(_))
            | (Some(_), Some(false) | None, None)
    )
}

fn refresh_local_facts(
    worktree: &Path,
    base: Option<&str>,
    branch: Option<String>,
    merge_in_progress: Option<bool>,
) -> LocalFacts {
    let stats = base.and_then(|base| worktree_diff_stats(worktree, base));
    let status = worktree_status(worktree);
    let clean = status.as_ref().map(|status| status.clean);
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
        branch,
        clean,
        merge_in_progress,
    }
}

fn refresh_commit_facts(
    worktree: &Path,
    base: Option<&str>,
    trunk: Option<&str>,
    clean: Option<bool>,
    head_sha: Option<&str>,
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
                LandedVerdict::Pending => Some(false),
                LandedVerdict::Unknown => None,
            }
        }
        _ => None,
    };
    let did_work = worktree::read_marker_for_worktree(worktree)
        .ok()
        .flatten()
        .and_then(|marker| {
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
        &["diff", "--no-ext-diff", "--numstat", base, "--"],
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
}

/// `git status --porcelain=v1 -z --untracked-files=all`: emptiness → clean, and
/// each `??` entry's file line-counts into the untracked churn.
/// `--untracked-files=all` lists files inside untracked directories
/// individually so the count sees them; `-z` keeps newline-bearing filenames
/// parseable (paths decode lossily, so a non-UTF-8 path still dirties the tree
/// but counts no lines); and `--no-optional-locks` keeps this background probe
/// from taking `index.lock`, so it never races the user's own git commands in
/// the worktree.
fn worktree_status(worktree: &Path) -> Option<WorktreeStatus> {
    let output = git_output(
        worktree,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    )?;
    if !output.status.success() {
        return None;
    }
    let mut budget = UNTRACKED_READ_BUDGET;
    Some(parse_status_entries(
        &String::from_utf8_lossy(&output.stdout),
        |path| untracked_added_lines(&worktree.join(path), &mut budget),
    ))
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
    let mut tokens = output.split('\0').filter(|token| !token.is_empty());
    while let Some(entry) = tokens.next() {
        clean = false;
        let Some((code, path)) = entry.split_at_checked(3) else {
            continue;
        };
        if code.get(..2).is_some_and(|xy| xy.contains(['R', 'C'])) {
            tokens.next();
        }
        if code.starts_with("??") {
            untracked_added = untracked_added.saturating_add(untracked_lines(path));
        }
    }
    WorktreeStatus {
        clean,
        untracked_added,
    }
}

/// Shared byte budget for one status probe's untracked reads. A file past the
/// remaining budget still marks the tree dirty through its status entry; the
/// budget only bounds the churn line count, so one refresh never reads more
/// than this however many untracked files the tree holds.
const UNTRACKED_READ_BUDGET: u64 = 8 * 1024 * 1024;

/// The added lines one untracked file contributes to the `+` churn — what
/// numstat would report if the file were tracked — spending the probe's shared
/// read `budget`. Unreadable, over-budget, and non-file paths contribute
/// nothing; the status entry already marks the tree dirty, so an uncounted
/// file costs accuracy, never the markers.
fn untracked_added_lines(path: &Path, budget: &mut u64) -> u32 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > *budget {
        return 0;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    *budget = budget.saturating_sub(bytes.len() as u64);
    count_added_lines(&bytes)
}

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

/// Run `git -C <worktree> <args>` and return its stdout's first non-empty line,
/// or `None` on a missing git binary, a non-zero exit, or empty output.
fn git_line(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(worktree, args)?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if line.is_empty() { None } else { Some(line) }
}

fn git_output(worktree: &Path, args: &[&str]) -> Option<std::process::Output> {
    crate::proc::git_command(worktree).args(args).output().ok()
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
