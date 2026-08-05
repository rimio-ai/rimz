//! Producer-only pull-request and branch-CI enrichment.
//!
//! The probe shells out to the repo's forge CLI on a long TTL, publishes `pr-state.json`, and lets consumers project the cached map without forking.
//! GitHub resolves each due repository in bounded GraphQL batches; Tea follows terminal candidates with a Gitea detail read for canonical merge state and commit metadata.
//! CI enrichment stays best-effort: GitHub projects aggregate check rollups, while Tea reads combined commit status for branches and commits.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::forge::{self, ForgeCli};
use crate::sidebar::refresh::git_stats::{
    DiffStatsCache, focused_worktree_paths, hot_worktree_paths, is_trunk_branch,
    needed_worktree_paths, read_diff_stats_cache,
};
use crate::sidebar::timing::{PR_STATE_HOT_TTL, PR_STATE_RETRY_TTL, PR_STATE_TTL, unix_now_ms};
use crate::store::snapshot::{SidebarSnapshot, WorktreePrCi, WorktreePrState};

const PR_STATE_WAIT_STEP: Duration = Duration::from_millis(20);
const PR_STATE_WAIT_STEPS: u32 = 15;
const PR_STATE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PARALLEL_PR_PROBES: usize = 8;
const GH_BULK_MAX_ALIASES: usize = 100;
const UNSUPPORTED_REPO_KEY: &str = "<unsupported>";
// Local worktree creation and forge PR creation use different clocks.
const TERMINAL_PR_CLOCK_SKEW: jiff::SignedDuration = jiff::SignedDuration::from_mins(5);

#[derive(Debug, Default, Clone, Serialize)]
pub struct PrStateCache {
    /// PR link by absolute worktree path.
    #[serde(default)]
    pub states: BTreeMap<String, PrLink>,
    /// Commit-level CI verdict for paths without a PR link.
    #[serde(default)]
    pub branch_ci: BTreeMap<String, WorktreePrCi>,
    /// Probe freshness by origin repo key.
    #[serde(default)]
    pub(crate) repos: BTreeMap<String, RepoProbe>,
    /// Last HEAD SHA observed when a worktree's repo was probed.
    #[serde(default)]
    pub(crate) head_seen: BTreeMap<String, String>,
    /// Last repo classification seen for a worktree. Supported repos store
    /// their repo key; unsupported/undiscoverable paths store an empty marker.
    /// This preserves the no-fork fresh path: per-repo TTL can be evaluated
    /// before shelling out for branch/remote metadata.
    #[serde(default)]
    pub(crate) path_repos: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLink {
    /// Head branch this link was resolved for. Legacy path-only links have no
    /// stamp and are re-resolved before reuse.
    #[serde(default)]
    pub branch: Option<String>,
    /// RimZ worktree creation time this link was resolved for. Legacy links
    /// have no stamp and are re-resolved before reuse by managed worktrees.
    #[serde(default)]
    pub incarnation: Option<jiff::Timestamp>,
    pub state: WorktreePrState,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub ci: Option<WorktreePrCi>,
    #[serde(default)]
    pub merge_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RepoProbe {
    pub refreshed_at_ms: u64,
    /// Whether the last repo probe completed without an infrastructure
    /// failure. A logged-in repo with no PR is a success and keeps an empty map.
    #[serde(default = "pr_state_probe_ok_default")]
    pub ok: bool,
    /// Consecutive failed probes, for escalating retry backoff on deterministic
    /// forge CLI failures.
    #[serde(default)]
    pub consecutive_failures: u32,
}

impl Default for RepoProbe {
    fn default() -> Self {
        Self {
            refreshed_at_ms: 0,
            ok: true,
            consecutive_failures: 0,
        }
    }
}

impl<'de> Deserialize<'de> for PrStateCache {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DiskCache {
            #[serde(default)]
            states: BTreeMap<String, PrLink>,
            #[serde(default)]
            branch_ci: Option<BTreeMap<String, WorktreePrCi>>,
            #[serde(default)]
            repos: BTreeMap<String, RepoProbe>,
            #[serde(default)]
            head_seen: BTreeMap<String, String>,
            #[serde(default)]
            path_repos: BTreeMap<String, String>,
        }

        let disk = DiskCache::deserialize(deserializer)?;
        let legacy = disk.branch_ci.is_none();
        let mut cache = Self {
            states: disk.states,
            branch_ci: disk.branch_ci.unwrap_or_default(),
            repos: disk.repos,
            head_seen: disk.head_seen,
            path_repos: disk.path_repos,
        };
        if legacy {
            // Old freshness stamps predate branch CI, and trunk paths were
            // classified as unsupported. Reclassify every path once so all
            // no-PR branches enter the commit probe without waiting for TTL.
            cache.repos.clear();
            cache.head_seen.clear();
            cache.path_repos.clear();
        }
        Ok(cache)
    }
}

fn pr_state_probe_ok_default() -> bool {
    true
}

fn pr_state_failure_ttl(consecutive_failures: u32, cap: Duration) -> Duration {
    let retry_ms = PR_STATE_RETRY_TTL.as_millis() as u64;
    let cap_ms = cap.as_millis() as u64;
    let shift = consecutive_failures.saturating_sub(1).min(63);
    let factor = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    Duration::from_millis(retry_ms.saturating_mul(factor).min(cap_ms))
}

pub(crate) fn produce_pr_states(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> PrStateCache {
    let path = runtime.pr_state_path();
    let cache = read_pr_state_cache(&path);
    let now_ms = unix_now_ms();
    let needed = needed_worktree_paths(snapshot);
    let diff_cache = read_diff_stats_cache(&runtime.diff_stats_path());
    let hot = hot_worktree_paths(snapshot);
    let focused = focused_worktree_paths(snapshot);
    if let Some(due) = cached_due_repo_keys(&cache, &needed, &diff_cache, &hot, &focused, now_ms)
        && due.is_empty()
    {
        return cache;
    }

    let targets = build_targets(&needed, &diff_cache);
    let groups = group_targets(targets);
    let target_paths = current_target_paths(&groups);
    let due = due_repo_keys(&groups, &cache, &hot, &focused, now_ms);
    let needs_reconcile = needs_target_reconcile(&cache, &needed, &diff_cache, &target_paths)
        || unsupported_probe_due(&cache, &needed, &hot, &focused, now_ms);
    if due.is_empty() && !needs_reconcile {
        return cache;
    }

    let lock_path = runtime.root.join("pr-state.lock");
    let fresh = || {
        let cache = read_pr_state_cache(&path);
        let now_ms = unix_now_ms();
        let due = due_repo_keys(&groups, &cache, &hot, &focused, now_ms);
        let needs_reconcile = needs_target_reconcile(&cache, &needed, &diff_cache, &target_paths)
            || unsupported_probe_due(&cache, &needed, &hot, &focused, now_ms);
        (due.is_empty() && !needs_reconcile).then_some(cache)
    };
    match crate::store::single_flight::coalesce(
        &lock_path,
        PR_STATE_WAIT_STEP,
        PR_STATE_WAIT_STEPS,
        fresh,
    ) {
        crate::store::single_flight::Coalesced::Shared(cache) => cache,
        crate::store::single_flight::Coalesced::Produce(_guard) => {
            let prior = read_pr_state_cache(&path);
            let now_ms = unix_now_ms();
            let due = due_repo_keys(&groups, &prior, &hot, &focused, now_ms);
            let needs_reconcile =
                needs_target_reconcile(&prior, &needed, &diff_cache, &target_paths)
                    || unsupported_probe_due(&prior, &needed, &hot, &focused, now_ms);
            if due.is_empty() && !needs_reconcile {
                return prior;
            }
            let cache = probe_due_repos(&groups, &due, &prior, &needed, &diff_cache, now_ms);
            write_pr_state_cache(&path, &cache);
            cache
        }
        crate::store::single_flight::Coalesced::ProduceLocal => {
            let prior = read_pr_state_cache(&path);
            let now_ms = unix_now_ms();
            let due = due_repo_keys(&groups, &prior, &hot, &focused, now_ms);
            let needs_reconcile =
                needs_target_reconcile(&prior, &needed, &diff_cache, &target_paths)
                    || unsupported_probe_due(&prior, &needed, &hot, &focused, now_ms);
            if due.is_empty() && !needs_reconcile {
                prior
            } else {
                probe_due_repos(&groups, &due, &prior, &needed, &diff_cache, now_ms)
            }
        }
    }
}

fn next_consecutive_failures(prior: Option<&RepoProbe>, ok: bool) -> u32 {
    if ok {
        0
    } else {
        prior
            .map(|probe| probe.consecutive_failures)
            .unwrap_or_default()
            .saturating_add(1)
    }
}

#[derive(Default)]
struct RepoDueInputs {
    hot: bool,
    nudged: bool,
    has_uncached: bool,
}

fn cached_due_repo_keys(
    cache: &PrStateCache,
    needed: &[String],
    diff_cache: &DiffStatsCache,
    hot: &BTreeSet<String>,
    focused: &BTreeSet<String>,
    now_ms: u64,
) -> Option<BTreeSet<String>> {
    let mut inputs = BTreeMap::<String, RepoDueInputs>::new();
    for path in needed {
        let head_sha = target_head_sha(diff_cache, path);
        let has_uncached = !cache.head_seen.contains_key(path);
        let pending_ci = path_has_pending_ci(cache, path);
        let Some(repo_key) = cache.path_repos.get(path) else {
            if has_uncached || head_nudged(&cache.head_seen, path, head_sha) {
                return None;
            }
            continue;
        };
        if repo_key == UNSUPPORTED_REPO_KEY {
            let input = inputs.entry(repo_key.clone()).or_default();
            input.hot |= hot.contains(path) || focused.contains(path) || pending_ci;
            input.has_uncached |= has_uncached;
            continue;
        }
        let input = inputs.entry(repo_key.clone()).or_default();
        input.hot |= hot.contains(path) || focused.contains(path) || pending_ci;
        input.nudged |= head_nudged(&cache.head_seen, path, head_sha);
        input.has_uncached |= has_uncached;
    }
    Some(
        inputs
            .into_iter()
            .filter_map(|(repo_key, input)| {
                let ttl = repo_tier_ttl(input.hot);
                repo_due(
                    cache.repos.get(&repo_key),
                    ttl,
                    now_ms,
                    input.nudged,
                    input.has_uncached,
                )
                .then_some(repo_key)
            })
            .collect(),
    )
}

fn target_head_sha<'a>(diff_cache: &'a DiffStatsCache, path: &str) -> Option<&'a str> {
    diff_cache
        .entries
        .get(path)
        .and_then(|entry| entry.head_sha.as_deref())
}

fn head_nudged(head_seen: &BTreeMap<String, String>, path: &str, head_sha: Option<&str>) -> bool {
    head_sha.is_some_and(|head_sha| head_seen.get(path).is_none_or(|seen| seen != head_sha))
}

fn due_repo_keys(
    groups: &BTreeMap<String, RepoGroup>,
    cache: &PrStateCache,
    hot: &BTreeSet<String>,
    focused: &BTreeSet<String>,
    now_ms: u64,
) -> BTreeSet<String> {
    groups
        .iter()
        .filter_map(|(repo_key, group)| {
            let repo_hot = group.targets.iter().any(|target| {
                hot.contains(&target.path)
                    || focused.contains(&target.path)
                    || path_has_pending_ci(cache, &target.path)
            });
            let nudged = group.targets.iter().any(|target| {
                head_nudged(&cache.head_seen, &target.path, target.head_sha.as_deref())
            });
            let has_uncached = group
                .targets
                .iter()
                .any(|target| !cache.head_seen.contains_key(&target.path));
            repo_due(
                cache.repos.get(repo_key),
                repo_tier_ttl(repo_hot),
                now_ms,
                nudged,
                has_uncached,
            )
            .then(|| repo_key.clone())
        })
        .collect()
}

fn repo_tier_ttl(repo_hot: bool) -> Duration {
    if repo_hot {
        PR_STATE_HOT_TTL
    } else {
        PR_STATE_TTL
    }
}

fn path_has_pending_ci(cache: &PrStateCache, path: &str) -> bool {
    cache.states.get(path).is_some_and(|link| {
        matches!(link.state, WorktreePrState::Open | WorktreePrState::Merged)
            && link.ci == Some(WorktreePrCi::Pending)
    }) || cache.branch_ci.get(path) == Some(&WorktreePrCi::Pending)
}

fn repo_due(
    probe: Option<&RepoProbe>,
    ttl: Duration,
    now_ms: u64,
    nudged: bool,
    has_uncached: bool,
) -> bool {
    if nudged || has_uncached {
        return true;
    }
    let Some(probe) = probe else {
        return true;
    };
    let ttl = if probe.ok {
        ttl
    } else {
        pr_state_failure_ttl(probe.consecutive_failures, ttl)
    };
    now_ms.saturating_sub(probe.refreshed_at_ms) > ttl.as_millis() as u64
}

#[derive(Clone, Debug)]
struct Target {
    path: String,
    branch: String,
    trunk: bool,
    forge_cli: ForgeCli,
    repo_key: String,
    repo_slug: Option<String>,
    remote: forge::RemoteRepo,
    worktree: PathBuf,
    head_sha: Option<String>,
    marker_created_at: Option<jiff::Timestamp>,
    from_pr: Option<u64>,
}

#[derive(Clone, Debug)]
struct RepoGroup {
    forge_cli: ForgeCli,
    repo_slug: Option<String>,
    worktree: PathBuf,
    targets: Vec<Target>,
}

impl Target {
    fn pr_link(
        &self,
        state: WorktreePrState,
        number: u64,
        ci: Option<WorktreePrCi>,
        merge_sha: Option<String>,
    ) -> PrLink {
        PrLink {
            branch: Some(self.branch.clone()),
            incarnation: self.marker_created_at,
            state,
            number: Some(number),
            url: self.remote.pr_web_url(number),
            ci,
            merge_sha,
        }
    }

    fn stamp_pr_url(&self, mut link: PrLink) -> PrLink {
        link.branch = Some(self.branch.clone());
        link.incarnation = self.marker_created_at;
        link.url = link
            .number
            .and_then(|number| self.remote.pr_web_url(number));
        link
    }

    fn owns_link(&self, link: &PrLink) -> bool {
        link.branch.as_deref() == Some(self.branch.as_str())
            && link.incarnation == self.marker_created_at
    }

    fn accepts_terminal_pr(&self, number: u64, created_at: Option<jiff::Timestamp>) -> bool {
        if self.from_pr == Some(number) {
            return true;
        }
        let (Some(marker_created_at), Some(created_at)) = (self.marker_created_at, created_at)
        else {
            return true;
        };
        created_at
            .checked_add(TERMINAL_PR_CLOCK_SKEW)
            .map_or(true, |created_at| created_at >= marker_created_at)
    }
}

fn build_targets(needed: &[String], diff_cache: &DiffStatsCache) -> Vec<Target> {
    let mut targets = Vec::new();
    for path in needed {
        let worktree = Path::new(path);
        let Some(branch) = git_branch(worktree) else {
            continue;
        };
        let trunk = is_trunk_branch(
            &branch,
            diff_cache
                .entries
                .get(path)
                .and_then(|entry| entry.trunk.as_deref()),
        );
        let Some(remote) = git_line(worktree, &["remote", "get-url", "origin"]) else {
            continue;
        };
        let Some(remote) = forge::RemoteRepo::parse(&remote) else {
            continue;
        };
        let Some(forge_cli) = remote.forge_cli() else {
            continue;
        };
        let marker = crate::worktree::read_marker_from_checkout_metadata(worktree)
            .ok()
            .flatten();
        let repo_slug = remote.repo_slug().map(str::to_owned);
        targets.push(Target {
            path: path.clone(),
            branch,
            trunk,
            forge_cli,
            repo_key: remote.repo_key(forge_cli),
            repo_slug,
            remote,
            worktree: worktree.to_path_buf(),
            head_sha: target_head_sha(diff_cache, path).map(str::to_owned),
            marker_created_at: marker.as_ref().map(|marker| marker.created_at),
            from_pr: marker.and_then(|marker| marker.from_pr),
        });
    }
    targets
}

fn group_targets(targets: Vec<Target>) -> BTreeMap<String, RepoGroup> {
    let mut groups = BTreeMap::<String, RepoGroup>::new();
    for target in targets {
        let entry = groups
            .entry(target.repo_key.clone())
            .or_insert_with(|| RepoGroup {
                forge_cli: target.forge_cli,
                repo_slug: target.repo_slug.clone(),
                worktree: target.worktree.clone(),
                targets: Vec::new(),
            });
        entry.targets.push(target);
    }
    groups
}

fn current_target_paths(groups: &BTreeMap<String, RepoGroup>) -> BTreeSet<String> {
    groups
        .values()
        .flat_map(|group| group.targets.iter().map(|target| target.path.clone()))
        .collect()
}

fn needs_target_reconcile(
    cache: &PrStateCache,
    needed: &[String],
    diff_cache: &DiffStatsCache,
    target_paths: &BTreeSet<String>,
) -> bool {
    needed
        .iter()
        .filter(|path| !target_paths.contains(*path))
        .any(|path| {
            cache.states.contains_key(path)
                || cache.branch_ci.contains_key(path)
                || cache.path_repos.get(path).map(String::as_str) != Some(UNSUPPORTED_REPO_KEY)
                || cache
                    .head_seen
                    .get(path)
                    .map(String::as_str)
                    .unwrap_or_default()
                    != target_head_sha(diff_cache, path).unwrap_or_default()
        })
}

fn unsupported_probe_due(
    cache: &PrStateCache,
    needed: &[String],
    hot: &BTreeSet<String>,
    focused: &BTreeSet<String>,
    now_ms: u64,
) -> bool {
    let mut has_unsupported = false;
    let mut unsupported_hot = false;
    for path in needed {
        if cache.path_repos.get(path).map(String::as_str) != Some(UNSUPPORTED_REPO_KEY) {
            continue;
        }
        has_unsupported = true;
        unsupported_hot |= hot.contains(path) || focused.contains(path);
    }
    has_unsupported
        && repo_due(
            cache.repos.get(UNSUPPORTED_REPO_KEY),
            repo_tier_ttl(unsupported_hot),
            now_ms,
            false,
            false,
        )
}

fn reconcile_target_bookkeeping(
    mut cache: PrStateCache,
    needed: &[String],
    diff_cache: &DiffStatsCache,
    target_paths: &BTreeSet<String>,
    now_ms: u64,
) -> PrStateCache {
    let current_needed_paths = needed.iter().cloned().collect::<BTreeSet<_>>();
    cache
        .states
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    cache
        .branch_ci
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    cache
        .head_seen
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    cache
        .path_repos
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    let mut saw_unsupported = false;
    for path in needed.iter().filter(|path| !target_paths.contains(*path)) {
        saw_unsupported = true;
        cache.states.remove(path);
        cache.branch_ci.remove(path);
        cache
            .path_repos
            .insert(path.clone(), UNSUPPORTED_REPO_KEY.to_owned());
        cache.head_seen.insert(
            path.clone(),
            target_head_sha(diff_cache, path)
                .unwrap_or_default()
                .to_owned(),
        );
    }
    if saw_unsupported {
        cache.repos.insert(
            UNSUPPORTED_REPO_KEY.to_owned(),
            RepoProbe {
                refreshed_at_ms: now_ms,
                ok: true,
                consecutive_failures: 0,
            },
        );
    }
    cache
}

struct AssignedStates {
    states: BTreeMap<String, PrLink>,
    transitions: Vec<(Target, Option<u64>)>,
}

fn assign_states(
    targets: &[Target],
    open_map: &BTreeMap<String, forge::PrCandidate>,
    prior: &BTreeMap<String, PrLink>,
) -> AssignedStates {
    let mut states = BTreeMap::new();
    let mut transitions = Vec::new();
    for target in targets {
        if target.trunk {
            continue;
        }
        if let Some(candidate) = open_map.get(&target.branch) {
            states.insert(
                target.path.clone(),
                target.pr_link(WorktreePrState::Open, candidate.number, None, None),
            );
            continue;
        }
        if let Some(link) = prior
            .get(&target.path)
            .filter(|link| target.owns_link(link))
            .cloned()
            .map(|link| target.stamp_pr_url(link))
        {
            match link.state {
                WorktreePrState::Merged
                    if matches!(link.ci, Some(WorktreePrCi::Passing | WorktreePrCi::Failing))
                        || (link.ci.is_none() && link.merge_sha.is_some()) =>
                {
                    states.insert(target.path.clone(), link);
                }
                WorktreePrState::Merged | WorktreePrState::Open => {
                    transitions.push((target.clone(), link.number));
                }
                WorktreePrState::Closed => {
                    states.insert(target.path.clone(), link);
                }
            }
        } else if prior.contains_key(&target.path) {
            transitions.push((target.clone(), None));
        }
    }
    AssignedStates {
        states,
        transitions,
    }
}

fn probe_due_repos(
    groups: &BTreeMap<String, RepoGroup>,
    due: &BTreeSet<String>,
    prior: &PrStateCache,
    needed: &[String],
    diff_cache: &DiffStatsCache,
    now_ms: u64,
) -> PrStateCache {
    let target_paths = current_target_paths(groups);
    let mut cache =
        reconcile_target_bookkeeping(prior.clone(), needed, diff_cache, &target_paths, now_ms);

    let due_groups = groups
        .iter()
        .filter(|(repo_key, _)| due.contains(*repo_key))
        .collect::<Vec<_>>();
    // PR probes stay on `Other`: their gh/tea/git forks have never counted against
    // the tick spawn budget in `meter.rs`, which is calibrated without them.
    let results = super::runner::bounded_map(
        crate::lane::WorkLane::Other,
        MAX_PARALLEL_PR_PROBES,
        &due_groups,
        |(repo_key, group)| probe_repo_group(repo_key, group, &prior.states, &prior.branch_ci),
    );
    for result in results {
        for target in &result.targets {
            cache.states.remove(&target.path);
            cache.branch_ci.remove(&target.path);
            cache.head_seen.insert(
                target.path.clone(),
                target.head_sha.clone().unwrap_or_default(),
            );
            cache
                .path_repos
                .insert(target.path.clone(), target.repo_key.clone());
        }
        for (path, state) in result.states {
            cache.states.insert(path, state);
        }
        for (path, ci) in result.branch_ci {
            cache.branch_ci.insert(path, ci);
        }
        let repo_key = result.repo_key.clone();
        cache.repos.insert(
            repo_key.clone(),
            RepoProbe {
                refreshed_at_ms: now_ms,
                ok: result.ok,
                consecutive_failures: next_consecutive_failures(
                    prior.repos.get(&repo_key),
                    result.ok,
                ),
            },
        );
    }
    let active_repo_keys = active_repo_keys(&cache);
    cache
        .repos
        .retain(|repo_key, _| active_repo_keys.contains(repo_key));
    cache
}

fn active_repo_keys(cache: &PrStateCache) -> BTreeSet<String> {
    cache.path_repos.values().cloned().collect()
}

struct RepoGroupProbe {
    repo_key: String,
    targets: Vec<Target>,
    states: BTreeMap<String, PrLink>,
    branch_ci: BTreeMap<String, WorktreePrCi>,
    ok: bool,
}

fn probe_repo_group(
    repo_key: &str,
    group: &RepoGroup,
    prior: &BTreeMap<String, PrLink>,
    prior_branch_ci: &BTreeMap<String, WorktreePrCi>,
) -> RepoGroupProbe {
    match group.forge_cli {
        ForgeCli::Gh => probe_github_repo_group(repo_key, group, prior, prior_branch_ci),
        ForgeCli::Tea => probe_tea_repo_group(repo_key, group, prior, prior_branch_ci),
    }
}

#[derive(Debug)]
struct GhQueryPlan {
    query: String,
    pr_targets: Vec<usize>,
    oids: Vec<String>,
}

fn plan_github_queries(group: &RepoGroup) -> Vec<GhQueryPlan> {
    let pr_targets = group
        .targets
        .iter()
        .enumerate()
        .filter_map(|(index, target)| (!target.trunk).then_some(index))
        .collect::<Vec<_>>();
    let mut seen_oids = BTreeSet::new();
    let oids = group
        .targets
        .iter()
        .filter_map(|target| target.head_sha.as_ref())
        .filter(|oid| seen_oids.insert((*oid).clone()))
        .cloned()
        .collect::<Vec<_>>();
    let repo_slug = group.repo_slug.as_deref().unwrap_or_default();
    let mut plans = Vec::new();
    let mut pr_offset = 0;
    let mut oid_offset = 0;
    while pr_offset < pr_targets.len() || oid_offset < oids.len() {
        let pr_end = (pr_offset + GH_BULK_MAX_ALIASES).min(pr_targets.len());
        let plan_pr_targets = pr_targets[pr_offset..pr_end].to_vec();
        let oid_capacity = GH_BULK_MAX_ALIASES - plan_pr_targets.len();
        let oid_end = (oid_offset + oid_capacity).min(oids.len());
        let plan_oids = oids[oid_offset..oid_end].to_vec();
        let branches = plan_pr_targets
            .iter()
            .map(|index| group.targets[*index].branch.as_str())
            .collect::<Vec<_>>();
        let oid_refs = plan_oids.iter().map(String::as_str).collect::<Vec<_>>();
        plans.push(GhQueryPlan {
            query: forge::github_bulk_query(repo_slug, &branches, &oid_refs),
            pr_targets: plan_pr_targets,
            oids: plan_oids,
        });
        pr_offset = pr_end;
        oid_offset = oid_end;
    }
    if plans.is_empty() {
        plans.push(GhQueryPlan {
            query: forge::github_bulk_query(repo_slug, &[], &[]),
            pr_targets: Vec::new(),
            oids: Vec::new(),
        });
    }
    plans
}

fn probe_github_repo_group(
    repo_key: &str,
    group: &RepoGroup,
    prior: &BTreeMap<String, PrLink>,
    prior_branch_ci: &BTreeMap<String, WorktreePrCi>,
) -> RepoGroupProbe {
    if group.repo_slug.is_none() {
        return failed_repo_group_probe(repo_key, group, prior, prior_branch_ci);
    }
    let plans = plan_github_queries(group);
    let mut batches = Vec::with_capacity(plans.len());
    for plan in plans {
        let query_arg = format!("query={}", plan.query);
        let Some(output) =
            command_stdout(&group.worktree, "gh", &["api", "graphql", "-f", &query_arg])
        else {
            return failed_repo_group_probe(repo_key, group, prior, prior_branch_ci);
        };
        let response = match forge::parse_github_bulk_response(
            &output,
            plan.pr_targets.len(),
            plan.oids.len(),
        ) {
            Ok(response) => response,
            Err(err) => {
                tracing::debug!(error = %err, "github bulk PR/CI parse failed");
                return failed_repo_group_probe(repo_key, group, prior, prior_branch_ci);
            }
        };
        batches.push((plan, response));
    }
    let (states, branch_ci) = project_github_group(group, &batches);
    RepoGroupProbe {
        repo_key: repo_key.to_owned(),
        targets: group.targets.clone(),
        states,
        branch_ci,
        ok: true,
    }
}

fn project_github_group(
    group: &RepoGroup,
    batches: &[(GhQueryPlan, forge::GhBulkResponse)],
) -> (BTreeMap<String, PrLink>, BTreeMap<String, WorktreePrCi>) {
    let mut prs = BTreeMap::new();
    let mut commits = BTreeMap::new();
    for (plan, response) in batches {
        for (alias_index, target_index) in plan.pr_targets.iter().enumerate() {
            if let Some(pr) = response.prs.get(alias_index).and_then(Option::as_ref) {
                prs.insert(*target_index, pr.clone());
            }
        }
        for (alias_index, oid) in plan.oids.iter().enumerate() {
            if let Some(ci) = response.commits.get(alias_index).copied().flatten() {
                commits.insert(oid.clone(), ci);
            }
        }
    }

    let mut states = BTreeMap::new();
    let mut branch_ci = BTreeMap::new();
    for (target_index, target) in group.targets.iter().enumerate() {
        if !target.trunk
            && let Some(pr) = prs.get(&target_index)
            && (pr.state == WorktreePrState::Open
                || target.accepts_terminal_pr(pr.number, pr.created_at))
        {
            let link = match pr.state {
                WorktreePrState::Open => target.pr_link(pr.state, pr.number, pr.head_ci, None),
                WorktreePrState::Merged => target.pr_link(
                    pr.state,
                    pr.number,
                    pr.merge_ci.or(pr.head_ci),
                    pr.merge_sha.clone(),
                ),
                WorktreePrState::Closed => target.pr_link(pr.state, pr.number, None, None),
            };
            states.insert(target.path.clone(), link);
            continue;
        }
        if let Some(ci) = target
            .head_sha
            .as_ref()
            .and_then(|oid| commits.get(oid))
            .copied()
        {
            branch_ci.insert(target.path.clone(), ci);
        }
    }
    (states, branch_ci)
}

fn failed_repo_group_probe(
    repo_key: &str,
    group: &RepoGroup,
    prior: &BTreeMap<String, PrLink>,
    prior_branch_ci: &BTreeMap<String, WorktreePrCi>,
) -> RepoGroupProbe {
    RepoGroupProbe {
        repo_key: repo_key.to_owned(),
        targets: group.targets.clone(),
        states: carry_prior_states(&group.targets, prior),
        branch_ci: carry_prior_branch_ci(&group.targets, prior_branch_ci),
        ok: false,
    }
}

fn probe_tea_repo_group(
    repo_key: &str,
    group: &RepoGroup,
    prior: &BTreeMap<String, PrLink>,
    prior_branch_ci: &BTreeMap<String, WorktreePrCi>,
) -> RepoGroupProbe {
    let open_map = match query_open_tea_prs(group) {
        Some(open_map) => open_map,
        None => {
            return failed_repo_group_probe(repo_key, group, prior, prior_branch_ci);
        }
    };
    let assigned = assign_states(&group.targets, &open_map, prior);
    let mut states = assigned.states;
    if let Some(repo_slug) = group.repo_slug.as_deref() {
        for target in &group.targets {
            let Some(link) = states.get_mut(&target.path) else {
                continue;
            };
            if link.state == WorktreePrState::Open {
                link.ci = probe_tea_ci(&group.worktree, repo_slug, &target.branch);
            }
        }
    }
    let mut ok = true;
    for (target, number) in assigned.transitions {
        let result = probe_tea(&target, number);
        if !result.ok {
            ok = false;
            if let Some(link) = prior
                .get(&target.path)
                .filter(|link| target.owns_link(link))
                .cloned()
            {
                states.insert(target.path.clone(), link);
            }
            continue;
        }
        if let Some(state) = result.state {
            states.insert(target.path.clone(), state);
        }
    }
    let mut branch_ci = BTreeMap::new();
    if let Some(repo_slug) = group.repo_slug.as_deref() {
        for target in &group.targets {
            if states.contains_key(&target.path) {
                continue;
            }
            let Some(sha) = target.head_sha.as_deref() else {
                continue;
            };
            if let Some(ci) = probe_tea_ci(&group.worktree, repo_slug, sha) {
                branch_ci.insert(target.path.clone(), ci);
            }
        }
    }
    RepoGroupProbe {
        repo_key: repo_key.to_owned(),
        targets: group.targets.clone(),
        states,
        branch_ci,
        ok,
    }
}

fn carry_prior_states(
    targets: &[Target],
    prior: &BTreeMap<String, PrLink>,
) -> BTreeMap<String, PrLink> {
    targets
        .iter()
        .filter(|target| !target.trunk)
        .filter_map(|target| {
            prior
                .get(&target.path)
                .filter(|link| target.owns_link(link))
                .cloned()
                .map(|link| (target.path.clone(), link))
        })
        .collect()
}

fn carry_prior_branch_ci(
    targets: &[Target],
    prior: &BTreeMap<String, WorktreePrCi>,
) -> BTreeMap<String, WorktreePrCi> {
    targets
        .iter()
        .filter_map(|target| {
            prior
                .get(&target.path)
                .copied()
                .map(|ci| (target.path.clone(), ci))
        })
        .collect()
}

fn query_open_tea_prs(group: &RepoGroup) -> Option<BTreeMap<String, forge::PrCandidate>> {
    let args = forge::tea_pr_list_args("open", group.repo_slug.as_deref());
    let output = command_stdout(&group.worktree, "tea", &args)?;
    forge::parse_tea_pr_list_links(&output)
        .inspect_err(|err| tracing::debug!(error = %err, "forge PR open-set parse failed"))
        .ok()
}

struct ProbeState {
    state: Option<PrLink>,
    ok: bool,
}

fn tea_pr_detail_args(number: u64, repo: &str) -> Vec<String> {
    vec!["api".to_owned(), format!("repos/{repo}/pulls/{number}")]
}

fn probe_tea(target: &Target, prior_number: Option<u64>) -> ProbeState {
    let worktree = &target.worktree;
    let branch = &target.branch;
    let repo = target.repo_slug.as_deref();
    if let Some(number) = prior_number
        && let Some(repo) = repo
        && let Some(state) = probe_tea_detail(target, repo, number)
    {
        return state;
    }
    let list_args = forge::tea_pr_list_args("all", repo);
    let Some(output) = command_stdout(worktree, "tea", &list_args) else {
        return ProbeState {
            state: None,
            ok: false,
        };
    };
    let candidate = match forge::parse_tea_pr_list_json(&output, branch) {
        Ok(candidate) => candidate,
        Err(err) => {
            tracing::debug!(error = %err, "gitea PR state parse failed");
            return ProbeState {
                state: None,
                ok: false,
            };
        }
    };
    let Some(candidate) = candidate else {
        return ProbeState {
            state: None,
            ok: true,
        };
    };
    // `tea pr list` can report `merged` directly but carries no SHAs or CI.
    // The API detail object is canonical for merge state, merge SHA, and CI.
    if matches!(
        candidate.state,
        WorktreePrState::Closed | WorktreePrState::Merged
    ) && let Some(repo) = repo
        && let Some(state) = probe_tea_detail(target, repo, candidate.number)
    {
        return state;
    }
    if candidate.state != WorktreePrState::Open
        && !target.accepts_terminal_pr(candidate.number, candidate.created_at)
    {
        return ProbeState {
            state: None,
            ok: true,
        };
    }
    ProbeState {
        state: Some(target.pr_link(candidate.state, candidate.number, None, None)),
        ok: true,
    }
}

fn probe_tea_detail(target: &Target, repo: &str, number: u64) -> Option<ProbeState> {
    let worktree = &target.worktree;
    let detail_args = tea_pr_detail_args(number, repo);
    let refs = detail_args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = command_stdout(worktree, "tea", &refs)?;
    let detail = forge::parse_tea_pr_detail_json(&output).ok()?;
    let state = detail.state?;
    if state != WorktreePrState::Open && !target.accepts_terminal_pr(number, detail.created_at) {
        return Some(ProbeState {
            state: None,
            ok: true,
        });
    }
    let merge_sha = (state == WorktreePrState::Merged)
        .then(|| {
            detail
                .merged_sha
                .clone()
                .or_else(|| detail.head_sha.clone())
        })
        .flatten();
    let ci = (state == WorktreePrState::Merged)
        .then(|| {
            detail
                .merged_sha
                .as_deref()
                .and_then(|sha| probe_tea_ci(worktree, repo, sha))
                .or_else(|| {
                    detail.head_sha.as_deref().and_then(|head_sha| {
                        if detail.merged_sha.as_deref() == Some(head_sha) {
                            None
                        } else {
                            probe_tea_ci(worktree, repo, head_sha)
                        }
                    })
                })
        })
        .flatten();
    Some(ProbeState {
        state: Some(target.pr_link(state, number, ci, merge_sha)),
        ok: true,
    })
}

fn probe_tea_ci(worktree: &Path, repo_slug: &str, branch: &str) -> Option<WorktreePrCi> {
    let endpoint = forge::tea_commit_status_endpoint(repo_slug, branch);
    let output = command_stdout(worktree, "tea", &["api", &endpoint])?;
    forge::parse_tea_combined_status(&output)
        .map_err(|err| {
            tracing::debug!(error = %err, "gitea combined commit-status parse failed");
            err
        })
        .ok()
        .flatten()
}

fn git_branch(worktree: &Path) -> Option<String> {
    let branch = git_line(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    (branch != "HEAD").then_some(branch)
}

fn git_line(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = crate::proc::git_command(worktree)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!line.is_empty()).then_some(line)
}

fn command_stdout(worktree: &Path, program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.current_dir(worktree).args(args);
    let output = crate::proc::run_bounded_output(&mut command, PR_STATE_COMMAND_TIMEOUT).ok()?;
    if output.timed_out || !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn read_pr_state_cache(path: &Path) -> PrStateCache {
    super::runner::read_json_cache(path)
}

fn write_pr_state_cache(path: &Path, cache: &PrStateCache) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.pr_state_write",
            error = &err as &dyn std::error::Error,
            "sidebar PR-state cache write failed",
        );
    }
}

#[cfg(test)]
mod tests;
