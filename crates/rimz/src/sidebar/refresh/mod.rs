//! Producer-owned heavy lane refresh for the sidebar data plane.
//!
//! The elected producer folds pane/sidecar truth first, then this module runs
//! the TTL-gated probes and cache publishes that are too expensive for every
//! renderer. The returned lane values feed a final fold when a caller needs the
//! freshest view in the same process.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use crate::agents::spending::{
    ProviderSpendingCache, SpendScope, SpendingCaches, WorkspaceSpendingCache,
    read_provider_spending_cache, read_workspace_spending_cache,
};
use crate::agents::{AgentAccount, AgentState};
use crate::config::MachineConfig;
use crate::store::snapshot::SidebarSnapshot;
use crate::{RuntimePaths, Store};

pub mod accounts;
pub mod cohort_spend;
pub mod credits;
pub mod daemon_reap;
mod git_refs;
pub mod git_stats;
pub(crate) mod inputs;
pub mod live_spend;
pub mod pr;
pub mod rate_limits;
mod runner;
pub mod sessions;
mod trace;
pub mod usage;

pub use accounts::{AccountsCache, ProviderRecord, ProviderStatus, query_provider_accounts};
pub use credits::merge_provider_realtime_usage;
pub use daemon_reap::{CodexDaemonReap, read_codex_daemon_reap};
pub use live_spend::{apply_live_day_spend, apply_live_today_spend};
pub use pr::{PrLink, PrStateCache};
pub(crate) use rate_limits::merge_account_rate_limits;
pub use sessions::{
    ForcedSessionRefresh, force_refresh_session_context, refresh_session_transcript_context,
    refresh_session_transcript_context_from_watch,
};
pub use usage::{
    complete_realtime_account_usage, refresh_account_usage_if_due, refresh_account_usage_now,
    refresh_claimed_account_usage,
};

use self::accounts::produce_accounts;
use self::cohort_spend::refresh_cohort_spend_for;
use self::daemon_reap::refresh_codex_daemon_reap_cache;
use self::git_stats::refresh_diff_stats_for;
use self::pr::produce_pr_states;
use self::rate_limits::refresh_rate_limits;
use self::sessions::refresh_live_sessions;
use self::usage::refresh_account_usage;
use super::enrich::{
    RemoteControlServerHealth, fold_machine_config_with, read_auto_continue_resume_messages,
};
use super::timing::unix_now_ms;

const ORPHAN_SWEEP_SCAN_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct RefreshedLanes {
    pub spending: SpendingCaches,
    pub accounts: BTreeMap<String, AgentAccount>,
    pub pr_states: BTreeMap<String, PrLink>,
    pub branch_ci: BTreeMap<String, crate::store::snapshot::WorktreePrCi>,
}

/// Process-local memo state owned by one long-lived cache producer.
#[derive(Debug, Default)]
pub struct ProducerRefreshState {
    git: git_stats::GitRefreshState,
    cohort_rollup: crate::store::snapshot::RollupCursor,
    cohort_effort: crate::agents::spending::EffortParseMemo,
    orphan_sweep_checked_at_ms: Option<u64>,
}

/// Supply sidebar workspace scope to the account-global spending service. A
/// failure serves only compatible durable publications and retries next tick.
fn compute_fleet_spending_via_service(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
    startup: crate::agents::spending::service::SpendingServiceStartup,
) -> SpendingCaches {
    let request = crate::agents::spending::service::SpendingServiceRequest::workspace(
        runtime,
        runtime.workspace_id.clone(),
        snapshot.project_root.clone(),
        snapshot.worktree_roots.clone(),
        snapshot.worktree_home.clone(),
        codex_origin_overrides(snapshot),
        spec.clone(),
    );
    match crate::agents::spending::service::request(runtime, request, startup) {
        Ok(caches) => caches,
        Err(error) => {
            tracing::debug!(error = %error, "spending service unavailable; serving publication");
            consumer_spending_caches(runtime, snapshot)
        }
    }
}

/// Read producer-published spending without opening transcript or cursor data.
pub(super) fn consumer_spending_caches(
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
    if is_current_workspace_cache(&cache, Some(scope_hash)) {
        return cache;
    }
    sole_published_workspace_cache(runtime).unwrap_or_default()
}

fn is_current_workspace_cache(cache: &WorkspaceSpendingCache, scope_hash: Option<&str>) -> bool {
    cache.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
        && scope_hash.is_none_or(|scope_hash| cache.scope_hash == scope_hash)
}

/// The producer's published tally when this reader's scope hash misses.
///
/// A consumer derives its scope from cached worktree roots, so it reads a hash
/// the producer has already moved past whenever a checkout is added or removed.
/// The producer prunes every sidecar but the live one, so a lone surviving file
/// is that room's current publication: serving it keeps the cockpit on the
/// figure the producer actually published, where defaulting reports an empty
/// room. Anything other than exactly one candidate stays unknown.
fn sole_published_workspace_cache(runtime: &RuntimePaths) -> Option<WorkspaceSpendingCache> {
    let mut published = std::fs::read_dir(&runtime.root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(inputs::is_workspace_spending_file)
        });
    let candidate = published.next()?;
    if published.next().is_some() {
        return None;
    }
    let cache = read_workspace_spending_cache(&candidate);
    is_current_workspace_cache(&cache, None).then_some(cache)
}

fn codex_origin_overrides(snapshot: &SidebarSnapshot) -> HashMap<PathBuf, PathBuf> {
    let row_worktrees: HashMap<&str, &str> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.name == "codex")
        .filter_map(|row| Some((row.id.as_str(), row.worktree_path.as_deref()?)))
        .collect();
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind.as_str() == "codex")
        .filter_map(|agent| {
            let transcript = PathBuf::from(agent.transcript_path.as_deref()?);
            if !transcript.is_absolute() {
                return None;
            }
            let worktree = agent
                .worktree_path
                .as_deref()
                .or_else(|| row_worktrees.get(agent.agent_id.as_str()).copied());
            let origin = worktree
                .and_then(|worktree| crate::agents::spending::origin_path(Some(worktree)))?;
            Some((transcript, origin))
        })
        .collect()
}

pub fn refresh_heavy_lanes(
    base: &SidebarSnapshot,
    daemon_probe_agents: &[AgentState],
    state_paths: &crate::StatePaths,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    spending_startup: crate::agents::spending::service::SpendingServiceStartup,
    state: &mut ProducerRefreshState,
) -> RefreshedLanes {
    let store = Store::open_existing(state_paths.clone(), runtime.clone());
    refresh_codex_daemon_reap_cache(
        daemon_probe_agents,
        runtime,
        unix_now_ms(),
        config.remote_control.enabled_for("codex"),
    );

    let accounts = produce_accounts(base, runtime);
    let spending = compute_fleet_spending_via_service(
        runtime,
        base,
        &config.headline_spec(),
        spending_startup,
    );
    // Rate-limit persistence and same-pass usage scheduling use resolved
    // provider panels, not the bare rollup. Final folds merge the just-written
    // cache read-only.
    let mut panels = fold_machine_config_with(
        base.clone(),
        config,
        accounts.clone(),
        &spending.provider.spending.by_provider,
        // This scoped fold is not returned as the final snapshot.
        RemoteControlServerHealth::default(),
    );
    refresh_rate_limits(&mut panels, runtime);
    // `with_provider_aggregates` rebuilds panels with empty credit fields; the
    // scoped producer fold must reapply the shared cache before auto-redeem can
    // evaluate the already-known reset credits.
    credits::apply_credits_cache(&mut panels, runtime, &config.accounts);

    refresh_live_sessions(base, runtime);
    refresh_account_usage(&panels, runtime);
    let resume_messages = read_auto_continue_resume_messages(
        store.as_ref(),
        &config.resume,
        base.resume_outcomes.as_deref().unwrap_or_default(),
    );
    crate::harness::auto_continue::resume_parked(base, runtime, &config.resume, &resume_messages);
    crate::harness::idle_compact::compact_idle_agents(base, runtime, &config.harness);
    crate::harness::auto_redeem::redeem_credits(
        &panels.providers,
        runtime,
        &config.resume,
        base.now,
    );
    let mut budget_snapshot = base.clone();
    apply_live_day_spend(&mut budget_snapshot, &spending.workspace);
    crate::harness::budget::enforce(&budget_snapshot, runtime, store.as_ref(), config);
    let runs = crate::harness::run_timeout::enforce(state_paths, runtime, base.now);
    let now_ms = base.now.as_millisecond().max(0) as u64;
    if let Some(runs) = runs
        && orphan_sweep_due(state.orphan_sweep_checked_at_ms, now_ms)
    {
        state.orphan_sweep_checked_at_ms = Some(now_ms);
        crate::harness::orphan_sweep::enforce(state_paths, runtime, &runs, base.now);
    }
    refresh_diff_stats_for(
        base,
        runtime,
        config.sidebar.trunk.as_deref(),
        &mut state.git,
    );
    refresh_cohort_spend_for(
        base,
        state_paths,
        runtime,
        config.agents.attention.active_grace_secs.get(),
        unix_now_ms(),
        &mut state.cohort_rollup,
        &mut state.cohort_effort,
    );
    let pr_cache = produce_pr_states(base, runtime);

    RefreshedLanes {
        spending,
        accounts,
        pr_states: pr_cache.states,
        branch_ci: pr_cache.branch_ci,
    }
}

fn orphan_sweep_due(checked_at_ms: Option<u64>, now_ms: u64) -> bool {
    checked_at_ms.is_none_or(|checked_at| {
        now_ms.saturating_sub(checked_at) >= ORPHAN_SWEEP_SCAN_TTL.as_millis() as u64
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceId;
    use crate::agents::spending::WORKSPACE_SPENDING_VERSION;

    fn runtime_root() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn publish(runtime: &RuntimePaths, scope_hash: &str, usd: f64) {
        let mut cache = WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            scope_hash: scope_hash.to_owned(),
            ..WorkspaceSpendingCache::default()
        };
        cache.tally.year.usd = usd;
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.workspace_spending_path(scope_hash),
            &cache,
        )
        .unwrap();
    }

    #[test]
    fn scope_hash_miss_serves_the_sole_published_tally() {
        let (_dir, runtime) = runtime_root();
        publish(&runtime, &"a".repeat(64), 12.5);

        let served = matching_workspace_cache(&runtime, &"b".repeat(64));

        assert_eq!(
            served.tally.year.usd, 12.5,
            "a reader whose roots lag the producer reads the published tally, not an empty room"
        );
    }

    #[test]
    fn scope_hash_hit_prefers_its_own_sidecar() {
        let (_dir, runtime) = runtime_root();
        let mine = "c".repeat(64);
        publish(&runtime, &mine, 3.0);
        publish(&runtime, &"d".repeat(64), 99.0);

        let served = matching_workspace_cache(&runtime, &mine);

        assert_eq!(served.tally.year.usd, 3.0);
    }

    #[test]
    fn ambiguous_publications_stay_unknown() {
        let (_dir, runtime) = runtime_root();
        publish(&runtime, &"e".repeat(64), 7.0);
        publish(&runtime, &"f".repeat(64), 9.0);

        let served = matching_workspace_cache(&runtime, &"0".repeat(64));

        assert!(
            served.tally.is_zero(),
            "two candidates name no single producer publication to trust"
        );
    }

    #[test]
    fn no_publication_stays_unknown() {
        let (_dir, runtime) = runtime_root();

        let served = matching_workspace_cache(&runtime, &"a".repeat(64));

        assert!(served.tally.is_zero());
    }

    #[test]
    fn orphan_sweep_scan_is_ttl_gated() {
        assert!(orphan_sweep_due(None, 1_000));
        assert!(!orphan_sweep_due(Some(1_000), 1_001));
        assert!(orphan_sweep_due(
            Some(1_000),
            1_000 + ORPHAN_SWEEP_SCAN_TTL.as_millis() as u64
        ));
    }
}
