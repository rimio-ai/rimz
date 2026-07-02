//! Producer-owned heavy lane refresh for the sidebar data plane.
//!
//! The elected producer folds pane/sidecar truth first, then this module runs
//! the TTL-gated probes and cache publishes that are too expensive for every
//! renderer. The returned lane values feed a final fold when a caller needs the
//! freshest view in the same process.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::AgentAccount;
use crate::agents::spending::{SpendingCaches, SpendingWalker};
use crate::config::MachineConfig;
use crate::{RuntimePaths, SidebarSnapshot, WorktreePrState};
use serde::{Deserialize, Serialize};

use super::enrich::{
    apply_rate_limit_cache, fold_machine_config_with, produce_accounts, produce_pr_states,
    read_auto_continue_resume_messages, refresh_account_usage, refresh_codex_sessions,
};
use super::produce::{git, spending};
use super::timing::CODEX_DAEMON_REAP_TTL;
use super::timing::unix_now_ms;

#[derive(Clone, Debug)]
pub struct RefreshedLanes {
    pub spending: SpendingCaches,
    pub accounts: BTreeMap<String, AgentAccount>,
    pub pr_states: BTreeMap<String, WorktreePrState>,
}

/// Producer-published inputs for the Codex daemon ghost reaper. Consumers read
/// this cache so the fast lane can apply the same reap without proc scans or
/// app-server probes.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CodexDaemonReap {
    pub produced_at_ms: u64,
    pub daemon_pids: BTreeSet<u32>,
    pub loaded: Option<BTreeSet<String>>,
}

fn codex_daemon_reap_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("codex-daemon-reap.json")
}

pub fn write_codex_daemon_reap(
    runtime: &RuntimePaths,
    cache: &CodexDaemonReap,
) -> crate::ledger::atomic::Result<()> {
    crate::ledger::atomic::write_temp_then_rename_cache(&codex_daemon_reap_path(runtime), cache)
}

pub fn read_codex_daemon_reap(runtime: &RuntimePaths) -> Option<CodexDaemonReap> {
    let bytes = std::fs::read(codex_daemon_reap_path(runtime)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn daemon_reap_due(cache: &Option<CodexDaemonReap>, now_ms: u64) -> bool {
    cache.as_ref().is_none_or(|cache| {
        now_ms.saturating_sub(cache.produced_at_ms) > CODEX_DAEMON_REAP_TTL.as_millis() as u64
    })
}

fn should_probe_codex_daemon_reap(snapshot: &SidebarSnapshot) -> bool {
    snapshot.agents.iter().any(|agent| {
        agent.kind == "codex" && agent.pane.is_none() && agent.parent_agent_id.is_none()
    })
}

pub(crate) fn refresh_codex_daemon_reap_cache(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    now_ms: u64,
) -> CodexDaemonReap {
    let current = read_codex_daemon_reap(runtime);
    if !should_probe_codex_daemon_reap(snapshot) || !daemon_reap_due(&current, now_ms) {
        return current.unwrap_or_default();
    }
    let daemon_pids = crate::remote_control::codex_daemon_pids();
    let loaded = if daemon_pids.is_empty() {
        None
    } else {
        crate::agents::codex::loaded_daemon_threads()
    };
    let inputs = CodexDaemonReap {
        produced_at_ms: now_ms,
        daemon_pids,
        loaded,
    };
    if let Err(err) = write_codex_daemon_reap(runtime, &inputs) {
        tracing::debug!(
            error = %err,
            "codex daemon reap cache write failed"
        );
    }
    inputs
}

pub fn refresh_heavy_lanes(
    base: &SidebarSnapshot,
    state_messages_dir: &Path,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    walker: &mut SpendingWalker,
) -> RefreshedLanes {
    refresh_codex_daemon_reap_cache(base, runtime, unix_now_ms());

    let spending = spending::compute_fleet_spending_with_walker(
        walker,
        runtime,
        base,
        &config.headline_spec(),
    );
    let accounts = produce_accounts(base, runtime);
    let pr_states = produce_pr_states(base, runtime);
    let lanes = RefreshedLanes {
        spending,
        accounts,
        pr_states,
    };

    // Rate-limit persistence is rebuilt from resolved provider panels, not the
    // bare rollup. Build that scoped panel view once, write the cache, and drop
    // it; final folds merge the just-written cache read-only.
    let mut panels = fold_machine_config_with(
        base.clone(),
        config.clone(),
        lanes.accounts.clone(),
        &lanes.spending.provider.spending.by_provider,
    );
    apply_rate_limit_cache(&mut panels, runtime, true);

    refresh_codex_sessions(base, runtime);
    refresh_account_usage(base, runtime);
    let resume_messages = read_auto_continue_resume_messages(
        Some(state_messages_dir),
        &config.resume,
        base.resume_outcomes.as_deref().unwrap_or_default(),
    );
    crate::harness::auto_continue::resume_parked(base, runtime, &config.resume, &resume_messages);
    git::refresh_diff_stats_for(base, runtime, config.sidebar.trunk.as_deref());

    lanes
}
