//! Producer-only pull-request status enrichment.
//!
//! The probe shells out to the repo's forge CLI on a long TTL, publishes
//! `pr-state.json`, and lets consumers project the cached map without forking.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::forge::{self, ForgeCli};
use crate::sidebar::refresh::git_stats::{needed_worktree_paths, worktree_group_path};
use crate::sidebar::timing::{PR_STATE_RETRY_TTL, PR_STATE_TTL, unix_now_ms};
use crate::{SidebarSnapshot, SidebarWorktreeKind, WorktreePrState};

const PR_STATE_WAIT_STEP: Duration = Duration::from_millis(20);
const PR_STATE_WAIT_STEPS: u32 = 15;
const MAX_PARALLEL_PR_PROBES: usize = 8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PrStateCache {
    pub refreshed_at_ms: u64,
    /// Whether the last PR-state probe completed without an infrastructure
    /// failure. A logged-in repo with no PR is a success and keeps an empty map.
    #[serde(default = "pr_state_probe_ok_default")]
    pub ok: bool,
    /// PR state by absolute worktree path.
    #[serde(default)]
    pub states: BTreeMap<String, crate::WorktreePrState>,
}

fn pr_state_probe_ok_default() -> bool {
    true
}

impl PrStateCache {
    pub(crate) fn is_fresh(&self, now_ms: u64) -> bool {
        let ttl = if self.ok {
            PR_STATE_TTL
        } else {
            PR_STATE_RETRY_TTL
        };
        now_ms.saturating_sub(self.refreshed_at_ms) <= ttl.as_millis() as u64
    }
}

pub(crate) fn produce_pr_states(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, WorktreePrState> {
    let path = runtime.pr_state_path();
    let cache = read_pr_state_cache(&path);
    let now_ms = unix_now_ms();
    if cache.is_fresh(now_ms) {
        return cache.states;
    }

    let lock_path = runtime.root.join("pr-state.lock");
    let fresh = || {
        let cache = read_pr_state_cache(&path);
        cache.is_fresh(unix_now_ms()).then_some(cache)
    };
    match crate::ledger::single_flight::coalesce(
        &lock_path,
        PR_STATE_WAIT_STEP,
        PR_STATE_WAIT_STEPS,
        fresh,
    ) {
        crate::ledger::single_flight::Coalesced::Shared(cache) => cache.states,
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            let (states, ok) = probe_pr_states(snapshot);
            let cache = PrStateCache {
                refreshed_at_ms: unix_now_ms(),
                ok,
                states,
            };
            write_pr_state_cache(&path, &cache);
            cache.states
        }
        crate::ledger::single_flight::Coalesced::ProduceLocal => probe_pr_states(snapshot).0,
    }
}

fn probe_pr_states(snapshot: &SidebarSnapshot) -> (BTreeMap<String, WorktreePrState>, bool) {
    let paths = needed_pr_worktree_paths(snapshot);
    let mut states = BTreeMap::new();
    let mut ok = true;
    for chunk in paths.chunks(MAX_PARALLEL_PR_PROBES) {
        std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|path| scope.spawn(move || probe_worktree(path)))
                .collect::<Vec<_>>();
            for handle in handles {
                if let Ok(result) = handle.join() {
                    ok &= result.ok;
                    if let Some(state) = result.state {
                        states.insert(result.path, state);
                    }
                }
            }
        });
    }
    (states, ok)
}

fn needed_pr_worktree_paths(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut paths = needed_worktree_paths(snapshot);
    for group in &snapshot.worktree_groups {
        if group.kind != SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group) else {
            continue;
        };
        if Path::new(path).is_dir() && !paths.iter().any(|known| known == path) {
            paths.push(path.to_owned());
        }
    }
    paths
}

struct ProbeResult {
    path: String,
    state: Option<WorktreePrState>,
    ok: bool,
}

fn probe_worktree(path: &str) -> ProbeResult {
    let worktree = Path::new(path);
    let result = probe_worktree_state(worktree);
    ProbeResult {
        path: path.to_owned(),
        state: result.state,
        ok: result.ok,
    }
}

struct ProbeState {
    state: Option<WorktreePrState>,
    ok: bool,
}

fn probe_worktree_state(worktree: &Path) -> ProbeState {
    let Some(branch) = git_branch(worktree) else {
        return ProbeState {
            state: None,
            ok: true,
        };
    };
    let Some(remote) = git_line(worktree, &["remote", "get-url", "origin"]) else {
        return ProbeState {
            state: None,
            ok: true,
        };
    };
    let Some(cli) = forge::forge_cli_for_remote(&remote) else {
        return ProbeState {
            state: None,
            ok: true,
        };
    };
    match cli {
        ForgeCli::Gh => probe_github(worktree, &branch),
        ForgeCli::Tea => probe_tea(worktree, &branch),
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
        Ok(state) => ProbeState { state, ok: true },
        Err(err) => {
            tracing::debug!(error = %err, "github PR state parse failed");
            ProbeState {
                state: None,
                ok: false,
            }
        }
    }
}

fn probe_tea(worktree: &Path, branch: &str) -> ProbeState {
    let Some(output) = command_stdout(
        worktree,
        "tea",
        &["pr", "list", "--state", "all", "--output", "json"],
    ) else {
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
        if let Some(output) = command_stdout(
            worktree,
            "tea",
            &["pr", number.as_str(), "--output", "json"],
        ) && let Ok(Some(WorktreePrState::Merged)) = forge::parse_tea_pr_detail_json(&output)
        {
            return ProbeState {
                state: Some(WorktreePrState::Merged),
                ok: true,
            };
        }
    }
    ProbeState {
        state: Some(candidate.state),
        ok: true,
    }
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
    crate::proc::testkit::count_spawn();
    let output = Command::new(program)
        .current_dir(worktree)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
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
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.pr_state_write",
            error = &err as &dyn std::error::Error,
            "sidebar PR-state cache write failed",
        );
    }
}
