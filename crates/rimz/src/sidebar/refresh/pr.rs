//! Producer-only pull-request link enrichment.
//!
//! The probe shells out to the repo's forge CLI on a long TTL, publishes
//! `pr-state.json`, and lets consumers project the cached map without forking.
//! `gh` reports `MERGED` as a first-class PR state, so one `gh pr list`
//! resolves open/closed/merged. `tea pr list` reports only open/closed, so
//! `probe_tea` follows a closed candidate with a `tea pr <n>` detail read to
//! tell merged from closed. Both tea list calls page through
//! [`crate::forge::tea_pr_list_args`] with the same `--limit`. GitHub includes
//! CI in its open-PR list. Tea reads Gitea's combined commit status with one
//! `tea api` call per open branch. Those calls are best-effort enrichment and
//! do not invalidate a successful PR-state probe.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::forge::{self, ForgeCli};
use crate::sidebar::refresh::git_stats::{
    DiffStatsCache, focused_worktree_paths, hot_worktree_paths, needed_worktree_paths,
    read_diff_stats_cache,
};
use crate::sidebar::timing::{PR_STATE_HOT_TTL, PR_STATE_RETRY_TTL, PR_STATE_TTL, unix_now_ms};
use crate::{SidebarSnapshot, WorktreePrCi, WorktreePrState};

const PR_STATE_WAIT_STEP: Duration = Duration::from_millis(20);
const PR_STATE_WAIT_STEPS: u32 = 15;
const PR_STATE_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PARALLEL_PR_PROBES: usize = 8;
const UNSUPPORTED_REPO_KEY: &str = "<unsupported>";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PrStateCache {
    /// PR link by absolute worktree path.
    #[serde(default)]
    pub states: BTreeMap<String, PrLink>,
    /// Probe freshness by origin repo key.
    #[serde(default)]
    pub repos: BTreeMap<String, RepoProbe>,
    /// Last HEAD SHA observed when a worktree's repo was probed.
    #[serde(default)]
    pub head_seen: BTreeMap<String, String>,
    /// Last repo classification seen for a worktree. Supported repos store
    /// their repo key; unsupported/undiscoverable paths store an empty marker.
    /// This preserves the no-fork fresh path: per-repo TTL can be evaluated
    /// before shelling out for branch/remote metadata.
    #[serde(default)]
    pub path_repos: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLink {
    pub state: WorktreePrState,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub ci: Option<WorktreePrCi>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoProbe {
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
) -> BTreeMap<String, PrLink> {
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
        return cache.states;
    }

    let targets = build_targets(&needed, &diff_cache);
    let groups = group_targets(targets);
    let target_paths = current_target_paths(&groups);
    let due = due_repo_keys(&groups, &cache, &hot, &focused, now_ms);
    let needs_reconcile = needs_target_reconcile(&cache, &needed, &diff_cache, &target_paths)
        || unsupported_probe_due(&cache, &needed, &hot, &focused, now_ms);
    if due.is_empty() && !needs_reconcile {
        return cache.states;
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
        crate::store::single_flight::Coalesced::Shared(cache) => cache.states,
        crate::store::single_flight::Coalesced::Produce(_guard) => {
            let prior = read_pr_state_cache(&path);
            let now_ms = unix_now_ms();
            let due = due_repo_keys(&groups, &prior, &hot, &focused, now_ms);
            let needs_reconcile =
                needs_target_reconcile(&prior, &needed, &diff_cache, &target_paths)
                    || unsupported_probe_due(&prior, &needed, &hot, &focused, now_ms);
            if due.is_empty() && !needs_reconcile {
                return prior.states;
            }
            let cache = probe_due_repos(&groups, &due, &prior, &needed, &diff_cache, now_ms);
            write_pr_state_cache(&path, &cache);
            cache.states
        }
        crate::store::single_flight::Coalesced::ProduceLocal => {
            let prior = read_pr_state_cache(&path);
            let now_ms = unix_now_ms();
            let due = due_repo_keys(&groups, &prior, &hot, &focused, now_ms);
            let needs_reconcile =
                needs_target_reconcile(&prior, &needed, &diff_cache, &target_paths)
                    || unsupported_probe_due(&prior, &needed, &hot, &focused, now_ms);
            if due.is_empty() && !needs_reconcile {
                prior.states
            } else {
                probe_due_repos(&groups, &due, &prior, &needed, &diff_cache, now_ms).states
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
        let pending_ci = cache.states.get(path).is_some_and(|link| {
            link.state == WorktreePrState::Open && link.ci == Some(WorktreePrCi::Pending)
        });
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
                    || cache.states.get(&target.path).is_some_and(|link| {
                        link.state == WorktreePrState::Open
                            && link.ci == Some(WorktreePrCi::Pending)
                    })
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
    remote: String,
    forge_cli: ForgeCli,
    repo_key: String,
    repo_slug: Option<String>,
    worktree: PathBuf,
    head_sha: Option<String>,
}

#[derive(Clone, Debug)]
struct RepoGroup {
    forge_cli: ForgeCli,
    repo_slug: Option<String>,
    worktree: PathBuf,
    targets: Vec<Target>,
}

fn build_targets(needed: &[String], diff_cache: &DiffStatsCache) -> Vec<Target> {
    let mut targets = Vec::new();
    for path in needed {
        let worktree = Path::new(path);
        let Some(branch) = git_branch(worktree) else {
            continue;
        };
        let Some(remote) = git_line(worktree, &["remote", "get-url", "origin"]) else {
            continue;
        };
        let Some(forge_cli) = forge::forge_cli_for_remote(&remote) else {
            continue;
        };
        let repo_slug = forge::remote_repo_slug(&remote);
        targets.push(Target {
            path: path.clone(),
            branch,
            remote: remote.clone(),
            forge_cli,
            repo_key: repo_key(forge_cli, &remote, repo_slug.as_deref()),
            repo_slug,
            worktree: worktree.to_path_buf(),
            head_sha: target_head_sha(diff_cache, path).map(str::to_owned),
        });
    }
    targets
}

fn repo_key(forge_cli: ForgeCli, remote: &str, repo_slug: Option<&str>) -> String {
    let host = forge::remote_host(remote).to_ascii_lowercase();
    let repo = repo_slug.unwrap_or_else(|| remote.trim());
    format!("{}:{host}:{repo}", forge_cli_key(forge_cli))
}

fn forge_cli_key(forge_cli: ForgeCli) -> &'static str {
    match forge_cli {
        ForgeCli::Gh => "gh",
        ForgeCli::Tea => "tea",
    }
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
        .head_seen
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    cache
        .path_repos
        .retain(|path, _| current_needed_paths.contains(path) && target_paths.contains(path));
    let mut saw_unsupported = false;
    for path in needed.iter().filter(|path| !target_paths.contains(*path)) {
        saw_unsupported = true;
        cache.states.remove(path);
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
    transitions: Vec<Target>,
}

fn assign_states(
    targets: &[Target],
    open_map: &BTreeMap<String, forge::PrCandidate>,
    prior: &BTreeMap<String, PrLink>,
) -> AssignedStates {
    let mut states = BTreeMap::new();
    let mut transitions = Vec::new();
    for target in targets {
        if let Some(candidate) = open_map.get(&target.branch) {
            states.insert(
                target.path.clone(),
                PrLink {
                    state: WorktreePrState::Open,
                    number: Some(candidate.number),
                    ci: candidate.ci,
                },
            );
            continue;
        }
        if let Some(link) = prior.get(&target.path).copied() {
            match link.state {
                WorktreePrState::Merged | WorktreePrState::Closed => {
                    states.insert(target.path.clone(), link);
                }
                WorktreePrState::Open => transitions.push(target.clone()),
            }
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
    for chunk in due_groups.chunks(MAX_PARALLEL_PR_PROBES) {
        std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|(repo_key, group)| {
                    scope.spawn(move || probe_repo_group(repo_key, group, &prior.states))
                })
                .collect::<Vec<_>>();
            for handle in handles {
                if let Ok(result) = handle.join() {
                    for target in &result.targets {
                        cache.states.remove(&target.path);
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
            }
        });
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
    ok: bool,
}

fn probe_repo_group(
    repo_key: &str,
    group: &RepoGroup,
    prior: &BTreeMap<String, PrLink>,
) -> RepoGroupProbe {
    let open_map = match query_open_prs(group) {
        Some(open_map) => open_map,
        None => {
            return RepoGroupProbe {
                repo_key: repo_key.to_owned(),
                targets: group.targets.clone(),
                states: carry_prior_states(&group.targets, prior),
                ok: false,
            };
        }
    };
    let assigned = assign_states(&group.targets, &open_map, prior);
    let mut states = assigned.states;
    if group.forge_cli == ForgeCli::Tea
        && let Some(repo_slug) = group.repo_slug.as_deref()
    {
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
    for target in assigned.transitions {
        let result = probe_transition(&target);
        if !result.ok {
            ok = false;
            if let Some(link) = prior.get(&target.path).copied() {
                states.insert(target.path.clone(), link);
            }
            continue;
        }
        if let Some(state) = result.state {
            states.insert(target.path.clone(), state);
        }
    }
    RepoGroupProbe {
        repo_key: repo_key.to_owned(),
        targets: group.targets.clone(),
        states,
        ok,
    }
}

fn carry_prior_states(
    targets: &[Target],
    prior: &BTreeMap<String, PrLink>,
) -> BTreeMap<String, PrLink> {
    targets
        .iter()
        .filter_map(|target| {
            prior
                .get(&target.path)
                .copied()
                .map(|link| (target.path.clone(), link))
        })
        .collect()
}

fn query_open_prs(group: &RepoGroup) -> Option<BTreeMap<String, forge::PrCandidate>> {
    let output = match group.forge_cli {
        ForgeCli::Gh => command_stdout(
            &group.worktree,
            "gh",
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--json",
                "number,state,headRefName,statusCheckRollup",
                "--limit",
                "500",
            ],
        )?,
        ForgeCli::Tea => command_stdout(
            &group.worktree,
            "tea",
            &forge::tea_pr_list_args("open", group.repo_slug.as_deref()),
        )?,
    };
    match group.forge_cli {
        ForgeCli::Gh => forge::parse_gh_pr_list_links(&output),
        ForgeCli::Tea => forge::parse_tea_pr_list_links(&output),
    }
    .map_err(|err| {
        tracing::debug!(error = %err, "forge PR open-set parse failed");
        err
    })
    .ok()
}

struct ProbeState {
    state: Option<PrLink>,
    ok: bool,
}

fn probe_transition(target: &Target) -> ProbeState {
    match target.forge_cli {
        ForgeCli::Gh => probe_github(&target.worktree, &target.branch),
        ForgeCli::Tea => probe_tea(&target.worktree, &target.branch, &target.remote),
    }
}

fn probe_github(worktree: &Path, branch: &str) -> ProbeState {
    let Some(output) = command_stdout(
        worktree,
        "gh",
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state",
        ],
    ) else {
        return ProbeState {
            state: None,
            ok: false,
        };
    };
    match forge::parse_gh_pr_state_json(&output) {
        Ok(candidate) => ProbeState {
            state: candidate.map(|candidate| PrLink {
                state: candidate.state,
                number: Some(candidate.number),
                ci: None,
            }),
            ok: true,
        },
        Err(err) => {
            tracing::debug!(error = %err, "github PR state parse failed");
            ProbeState {
                state: None,
                ok: false,
            }
        }
    }
}

fn probe_tea(worktree: &Path, branch: &str, remote: &str) -> ProbeState {
    let repo = forge::remote_repo_slug(remote);
    let list_args = forge::tea_pr_list_args("all", repo.as_deref());
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
    if candidate.state == WorktreePrState::Closed {
        let number = candidate.number.to_string();
        let mut detail_args = vec!["pr", number.as_str(), "--output", "json"];
        if let Some(repo) = repo.as_deref() {
            detail_args.extend_from_slice(&["--repo", repo]);
        }
        // `tea pr list` omits merged metadata; the detail object carries
        // `merged`/`merged_at` so closed candidates can become merged.
        if let Some(output) = command_stdout(worktree, "tea", &detail_args)
            && let Ok(Some(WorktreePrState::Merged)) = forge::parse_tea_pr_detail_json(&output)
        {
            return ProbeState {
                state: Some(PrLink {
                    state: WorktreePrState::Merged,
                    number: Some(candidate.number),
                    ci: None,
                }),
                ok: true,
            };
        }
    }
    ProbeState {
        state: Some(PrLink {
            state: candidate.state,
            number: Some(candidate.number),
            ci: None,
        }),
        ok: true,
    }
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
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
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
mod tests {
    use super::*;
    use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};

    #[test]
    fn failure_ttl_escalates_to_success_ttl_cap() {
        assert_eq!(pr_state_failure_ttl(0, PR_STATE_TTL), PR_STATE_RETRY_TTL);
        assert_eq!(pr_state_failure_ttl(1, PR_STATE_TTL), PR_STATE_RETRY_TTL);
        assert_eq!(
            pr_state_failure_ttl(2, PR_STATE_TTL),
            Duration::from_secs(60)
        );
        assert_eq!(
            pr_state_failure_ttl(3, PR_STATE_TTL),
            Duration::from_secs(120)
        );
        assert_eq!(
            pr_state_failure_ttl(4, PR_STATE_TTL),
            Duration::from_secs(240)
        );
        assert_eq!(pr_state_failure_ttl(5, PR_STATE_TTL), PR_STATE_TTL);
        assert_eq!(pr_state_failure_ttl(u32::MAX, PR_STATE_TTL), PR_STATE_TTL);
        assert_eq!(pr_state_failure_ttl(1, PR_STATE_HOT_TTL), PR_STATE_HOT_TTL);
    }

    #[test]
    fn failure_counter_resets_on_success_and_saturates_on_failure() {
        let mut prior = RepoProbe {
            ok: false,
            consecutive_failures: 7,
            ..RepoProbe::default()
        };

        assert_eq!(next_consecutive_failures(Some(&prior), true), 0);
        assert_eq!(next_consecutive_failures(Some(&prior), false), 8);
        assert_eq!(next_consecutive_failures(None, false), 1);

        prior.consecutive_failures = u32::MAX;
        assert_eq!(next_consecutive_failures(Some(&prior), false), u32::MAX);
    }

    #[test]
    fn repo_due_tracks_fresh_stale_nudge_uncached_and_failure_backoff() {
        let ttl = Duration::from_secs(20);
        let ttl_ms = ttl.as_millis() as u64;
        let probe = RepoProbe {
            refreshed_at_ms: 1_000,
            ok: true,
            consecutive_failures: 0,
        };

        assert!(!repo_due(Some(&probe), ttl, 1_000 + ttl_ms, false, false));
        assert!(repo_due(Some(&probe), ttl, 1_001 + ttl_ms, false, false));
        assert!(repo_due(None, ttl, 1_000, false, false));
        assert!(repo_due(Some(&probe), ttl, 1_000, true, false));
        assert!(repo_due(Some(&probe), ttl, 1_000, false, true));

        let failed = RepoProbe {
            refreshed_at_ms: 1_000,
            ok: false,
            consecutive_failures: 1,
        };
        assert!(!repo_due(Some(&failed), ttl, 1_000 + ttl_ms, false, false));
        assert!(repo_due(Some(&failed), ttl, 1_001 + ttl_ms, false, false));
    }

    #[test]
    fn assign_states_handles_open_terminal_transition_closed_and_absent() {
        let targets = vec![
            target("/repo/open", "open"),
            target("/repo/merged", "merged"),
            target("/repo/transition", "transition"),
            target("/repo/closed", "closed"),
            target("/repo/none", "none"),
        ];
        let mut open_map = BTreeMap::new();
        open_map.insert(
            "open".to_owned(),
            forge::PrCandidate {
                number: 91,
                state: WorktreePrState::Open,
                ci: Some(WorktreePrCi::Passing),
            },
        );
        let mut prior = BTreeMap::new();
        prior.insert(
            "/repo/merged".to_owned(),
            PrLink {
                state: WorktreePrState::Merged,
                number: Some(80),
                ci: Some(WorktreePrCi::Failing),
            },
        );
        prior.insert(
            "/repo/transition".to_owned(),
            PrLink {
                state: WorktreePrState::Open,
                number: Some(81),
                ci: Some(WorktreePrCi::Pending),
            },
        );
        prior.insert(
            "/repo/closed".to_owned(),
            PrLink {
                state: WorktreePrState::Closed,
                number: Some(82),
                ci: None,
            },
        );

        let assigned = assign_states(&targets, &open_map, &prior);

        assert_eq!(
            assigned.states.get("/repo/open"),
            Some(&PrLink {
                state: WorktreePrState::Open,
                number: Some(91),
                ci: Some(WorktreePrCi::Passing),
            })
        );
        assert_eq!(
            assigned.states.get("/repo/merged"),
            Some(&PrLink {
                state: WorktreePrState::Merged,
                number: Some(80),
                ci: Some(WorktreePrCi::Failing),
            })
        );
        assert_eq!(
            assigned.states.get("/repo/closed"),
            Some(&PrLink {
                state: WorktreePrState::Closed,
                number: Some(82),
                ci: None,
            })
        );
        assert!(!assigned.states.contains_key("/repo/none"));
        assert_eq!(
            assigned
                .transitions
                .iter()
                .map(|target| target.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/repo/transition"]
        );
    }

    #[test]
    fn legacy_cache_defaults_and_leaves_repos_due() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pr-state.json");
        std::fs::write(
            &path,
            r#"{
                "refreshed_at_ms": 1000,
                "ok": true,
                "consecutive_failures": 0,
                "states": {"/repo/a": "open"}
            }"#,
        )
        .unwrap();
        let cache = read_pr_state_cache(&path);
        let groups = group_targets(vec![target("/repo/a", "a")]);

        assert!(cache.states.is_empty());
        assert!(cache.repos.is_empty());
        assert!(cache.head_seen.is_empty());
        assert!(
            due_repo_keys(&groups, &cache, &BTreeSet::new(), &BTreeSet::new(), 1_000)
                .contains("gh:github.com:org/repo")
        );
    }

    #[test]
    fn legacy_pr_link_without_ci_defaults_to_unknown() {
        let link: PrLink = serde_json::from_str(r#"{"state":"open","number":91}"#).unwrap();

        assert_eq!(link.ci, None);
    }

    #[test]
    fn pending_ci_keeps_repo_on_hot_ttl() {
        let repo_key = "gh:github.com:org/repo".to_owned();
        let path = "/repo/a".to_owned();
        let mut cache = PrStateCache::default();
        cache.repos.insert(
            repo_key.clone(),
            RepoProbe {
                refreshed_at_ms: 1_000,
                ok: true,
                consecutive_failures: 0,
            },
        );
        cache.path_repos.insert(path.clone(), repo_key.clone());
        cache.head_seen.insert(path.clone(), String::new());
        cache.states.insert(
            path.clone(),
            PrLink {
                state: WorktreePrState::Open,
                number: Some(91),
                ci: Some(WorktreePrCi::Pending),
            },
        );
        let needed = vec![path];
        let groups = group_targets(vec![target("/repo/a", "a")]);
        let now_ms = 1_001 + PR_STATE_HOT_TTL.as_millis() as u64;

        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &DiffStatsCache::default(),
                &BTreeSet::new(),
                &BTreeSet::new(),
                now_ms,
            )
            .unwrap()
            .contains(&repo_key)
        );
        assert!(
            due_repo_keys(&groups, &cache, &BTreeSet::new(), &BTreeSet::new(), now_ms,)
                .contains(&repo_key)
        );
    }

    #[test]
    fn cached_due_uses_head_nudge_without_git_metadata() {
        let mut cache = PrStateCache::default();
        cache.repos.insert(
            "gh:github.com:org/repo".to_owned(),
            RepoProbe {
                refreshed_at_ms: 1_000,
                ok: true,
                consecutive_failures: 0,
            },
        );
        cache
            .head_seen
            .insert("/repo/a".to_owned(), "old".to_owned());
        cache
            .path_repos
            .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            "/repo/a".to_owned(),
            DiffStatsCacheEntry {
                head_sha: Some("old".to_owned()),
                ..DiffStatsCacheEntry::default()
            },
        );
        let needed = vec!["/repo/a".to_owned()];

        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &diff,
                &BTreeSet::new(),
                &BTreeSet::new(),
                1_000 + PR_STATE_TTL.as_millis() as u64
            )
            .unwrap()
            .is_empty()
        );

        diff.entries.get_mut("/repo/a").unwrap().head_sha = Some("new".to_owned());

        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &diff,
                &BTreeSet::new(),
                &BTreeSet::new(),
                1_000
            )
            .unwrap()
            .contains("gh:github.com:org/repo")
        );
    }

    #[test]
    fn uncached_path_requires_target_assembly() {
        let cache = PrStateCache::default();
        let needed = vec!["/repo/a".to_owned()];
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            "/repo/a".to_owned(),
            DiffStatsCacheEntry {
                head_sha: Some("new".to_owned()),
                ..DiffStatsCacheEntry::default()
            },
        );

        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &diff,
                &BTreeSet::new(),
                &BTreeSet::new(),
                1_000
            )
            .is_none()
        );
    }

    #[test]
    fn unsupported_reconcile_drops_state_and_marks_head_seen() {
        let mut cache = PrStateCache::default();
        cache.states.insert(
            "/repo/a".to_owned(),
            PrLink {
                state: WorktreePrState::Open,
                number: Some(91),
                ci: None,
            },
        );
        cache
            .path_repos
            .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
        cache
            .head_seen
            .insert("/repo/a".to_owned(), "old".to_owned());
        let needed = vec!["/repo/a".to_owned()];
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            "/repo/a".to_owned(),
            DiffStatsCacheEntry {
                head_sha: Some("new".to_owned()),
                ..DiffStatsCacheEntry::default()
            },
        );
        let target_paths = BTreeSet::new();

        assert!(needs_target_reconcile(
            &cache,
            &needed,
            &diff,
            &target_paths
        ));

        let cache = reconcile_target_bookkeeping(cache, &needed, &diff, &target_paths, 1_000);

        assert!(!cache.states.contains_key("/repo/a"));
        assert_eq!(
            cache.path_repos.get("/repo/a").map(String::as_str),
            Some(UNSUPPORTED_REPO_KEY)
        );
        assert_eq!(
            cache.head_seen.get("/repo/a").map(String::as_str),
            Some("new")
        );
        assert_eq!(
            cache
                .repos
                .get(UNSUPPORTED_REPO_KEY)
                .map(|probe| probe.refreshed_at_ms),
            Some(1_000)
        );
        assert!(!needs_target_reconcile(
            &cache,
            &needed,
            &diff,
            &target_paths
        ));
    }

    #[test]
    fn unsupported_cached_path_does_not_reassemble_on_head_nudge() {
        let mut cache = PrStateCache::default();
        cache
            .path_repos
            .insert("/repo/a".to_owned(), UNSUPPORTED_REPO_KEY.to_owned());
        cache.head_seen.insert("/repo/a".to_owned(), String::new());
        cache.repos.insert(
            UNSUPPORTED_REPO_KEY.to_owned(),
            RepoProbe {
                refreshed_at_ms: 1_000,
                ok: true,
                consecutive_failures: 0,
            },
        );
        let needed = vec!["/repo/a".to_owned()];
        let mut diff = DiffStatsCache::default();
        diff.entries.insert(
            "/repo/a".to_owned(),
            DiffStatsCacheEntry {
                head_sha: Some("new".to_owned()),
                ..DiffStatsCacheEntry::default()
            },
        );

        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &diff,
                &BTreeSet::new(),
                &BTreeSet::new(),
                1_000
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            cached_due_repo_keys(
                &cache,
                &needed,
                &diff,
                &BTreeSet::new(),
                &BTreeSet::new(),
                1_001 + PR_STATE_TTL.as_millis() as u64
            )
            .unwrap()
            .contains(UNSUPPORTED_REPO_KEY)
        );
    }

    #[test]
    fn repo_bookkeeping_prunes_stale_repo_stamps() {
        let mut prior = PrStateCache::default();
        prior
            .path_repos
            .insert("/repo/a".to_owned(), "gh:github.com:org/repo".to_owned());
        prior.repos.insert(
            "gh:github.com:org/repo".to_owned(),
            RepoProbe {
                refreshed_at_ms: 1_000,
                ok: true,
                consecutive_failures: 0,
            },
        );
        prior.repos.insert(
            "gh:github.com:old/repo".to_owned(),
            RepoProbe {
                refreshed_at_ms: 1_000,
                ok: true,
                consecutive_failures: 0,
            },
        );
        let groups = group_targets(vec![target("/repo/a", "a")]);
        let needed = vec!["/repo/a".to_owned()];

        let cache = probe_due_repos(
            &groups,
            &BTreeSet::new(),
            &prior,
            &needed,
            &DiffStatsCache::default(),
            2_000,
        );

        assert!(cache.repos.contains_key("gh:github.com:org/repo"));
        assert!(!cache.repos.contains_key("gh:github.com:old/repo"));
    }

    fn target(path: &str, branch: &str) -> Target {
        Target {
            path: path.to_owned(),
            branch: branch.to_owned(),
            remote: "https://github.com/org/repo.git".to_owned(),
            forge_cli: ForgeCli::Gh,
            repo_key: "gh:github.com:org/repo".to_owned(),
            repo_slug: Some("org/repo".to_owned()),
            worktree: PathBuf::from("/repo"),
            head_sha: Some("sha".to_owned()),
        }
    }
}
