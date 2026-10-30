//! Producer-owned heavy lane refresh for the sidebar data plane.
//!
//! The room host folds pane/sidecar truth first, then this module runs
//! the TTL-gated probes and cache publishes that are too expensive for every
//! renderer. The returned lane values feed a final fold when a caller needs the
//! freshest view in the same process.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::agents::spending::{
    SpendScope, SpendingCaches, WorkspaceSpendingCache, read_workspace_spending_cache,
};
use crate::agents::{AgentAccount, AgentState};
use crate::config::MachineConfig;
use crate::forge::pr_state::PrLink;
use crate::harness::cache_keepalive::SeatPolicy;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::SidebarSnapshot;
use crate::{RuntimePaths, Store};

pub mod accounts;
pub(super) mod cohort_spend;
pub(super) mod credits;
pub(super) mod daemon_reap;
mod git_refs;
pub mod git_stats;
pub(super) mod keep_warm;
pub(super) mod live_spend;
pub(super) mod pipeline;
mod pr;
pub(super) mod rate_limits;
pub mod sessions;
mod trace;
mod trunk;
pub mod usage;

pub use accounts::query_provider_accounts;
pub(super) use daemon_reap::read_codex_daemon_reap;
pub use live_spend::apply_live_day_spend;
pub use sessions::{
    ForcedSessionRefresh, force_refresh_session_context,
    refresh_session_transcript_context_from_watch,
};
pub use usage::{
    complete_realtime_account_usage, publish_account_usage_snapshot, refresh_claimed_account_usage,
    refresh_provider_usage,
};

use self::accounts::produce_accounts;
use self::cohort_spend::refresh_cohort_spend_for;
use self::daemon_reap::refresh_codex_daemon_reap_cache;
use self::git_stats::refresh_diff_stats_for;
use self::pr::produce_pr_states;
use self::rate_limits::refresh_rate_limits;
use self::sessions::refresh_live_sessions;
use self::usage::refresh_account_usage;
use super::enrich::{PanelScope, RemoteControlServerHealth, fold_machine_config_with};
use super::meter::PassSpawns;
use crate::utils::time::unix_now_ms;

const ORPHAN_SWEEP_SCAN_TTL: Duration = Duration::from_secs(60);

// A hung git must not stall the refresh worker; this matches the forge command bound in pr.rs.
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Raw stdout for multi-line and NUL-delimited git commands; spawn failure or timeout returns `None`, while callers judge the exit status.
fn git_output(worktree: &Path, args: &[&str]) -> Option<crate::proc::BoundedOutput> {
    let output = crate::proc::run_bounded_git_output(worktree, args, GIT_COMMAND_TIMEOUT).ok()?;
    (!output.timed_out).then_some(output)
}

/// Run `git -C <worktree> <args>` and return its stdout's first non-empty line, or `None` on a missing git binary, a non-zero exit, timeout, or empty output.
fn git_line(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = git_output(worktree, args)?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

#[derive(Clone, Debug)]
pub struct RefreshedLanes {
    pub spending: SpendingCaches,
    pub accounts: BTreeMap<crate::ids::LoginKey, AgentAccount>,
    pub pr_states: BTreeMap<String, PrLink>,
    pub branch_ci: BTreeMap<String, crate::store::snapshot::WorktreeCi>,
}

/// Process-local memo state owned by one long-lived cache producer.
#[derive(Debug, Default)]
pub struct ProducerRefreshState {
    git: git_stats::GitRefreshState,
    pr_origins: BTreeMap<PathBuf, (Option<crate::disk::parse_cache::FileStamp>, Option<String>)>,
    /// This thread's audit cursor, shared by cohort spend and the orphan sweep.
    rollup: crate::store::snapshot::RollupCursor,
    cohort_effort: crate::agents::spending::EffortParseMemo,
    orphan_sweep_checked_at_ms: Option<u64>,
    auto_gc: crate::harness::auto_gc::AutoGcMemo,
    auto_continue_checks: HashMap<(AgentKind, AgentSessionId), Instant>,
    budget_checks: HashMap<(AgentKind, AgentSessionId), Instant>,
    session_checks: HashMap<(AgentKind, AgentSessionId), Instant>,
    /// Spawns per step of the last `refresh_heavy_lanes` pass, handed to the
    /// tick meter once.
    pass_spawns: BTreeMap<String, u64>,
}

impl ProducerRefreshState {
    /// The last pass's per-step spawn counts; empty when no pass ran since the
    /// previous call.
    pub(crate) fn take_pass_spawns(&mut self) -> BTreeMap<String, u64> {
        std::mem::take(&mut self.pass_spawns)
    }
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
    let provider =
        crate::agents::spending::cached_provider_spending(&runtime.shared_provider_spending_path());
    let provider = if provider.is_current_version() {
        provider
    } else {
        Default::default()
    };
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

fn matching_workspace_cache(runtime: &RuntimePaths, scope_hash: &str) -> WorkspaceSpendingCache {
    let cache = read_workspace_spending_cache(&runtime.workspace_spending_path(scope_hash));
    if is_current_workspace_cache(&cache, Some(scope_hash)) {
        return cache;
    }
    sole_published_workspace_cache(runtime).unwrap_or_default()
}

fn is_current_workspace_cache(cache: &WorkspaceSpendingCache, scope_hash: Option<&str>) -> bool {
    cache.is_current_version() && scope_hash.is_none_or(|scope_hash| cache.scope_hash == scope_hash)
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
    let mut published = runtime.workspace_spending_files().into_iter();
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
        .filter_map(|row| Some((row.id.as_str(), row.lane_path()?)))
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
                .lane_path()
                .or_else(|| row_worktrees.get(agent.agent_id.as_str()).copied());
            let origin = worktree
                .and_then(|worktree| crate::agents::spending::origin_path(Some(worktree)))?;
            Some((transcript, origin))
        })
        .collect()
}

#[allow(
    clippy::too_many_arguments,
    reason = "each caller states its spending startup and git pass explicitly; neither has a default"
)]
pub(super) fn refresh_heavy_lanes(
    base: &SidebarSnapshot,
    daemon_probe_agents: &[AgentState],
    state_paths: &crate::StatePaths,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    spending_startup: crate::agents::spending::service::SpendingServiceStartup,
    git_pass: git_stats::GitPass,
    state: &mut ProducerRefreshState,
) -> RefreshedLanes {
    let logins = crate::agents::RoomLoginSet::resolve(
        &state_paths.workspace_record,
        base.project_root.as_deref(),
        config,
    )
    .with_agents(&base.agents);
    let store = Store::open_existing(state_paths.clone(), runtime.clone());
    let room_remote_control = crate::remote_control::room_config_at(
        &config.remote_control,
        &state_paths.workspace_record,
    );
    let mut steps = PassSpawns::default();
    steps.step("codex_daemon_reap", || {
        refresh_codex_daemon_reap_cache(
            daemon_probe_agents,
            runtime,
            &logins,
            unix_now_ms(),
            room_remote_control.enabled_for("codex"),
        )
    });

    let accounts = steps.step("accounts", || produce_accounts(base, runtime, &logins));
    let spending = steps.step("spending", || {
        compute_fleet_spending_via_service(runtime, base, &config.headline_spec(), spending_startup)
    });
    let panels = steps.step("rate_limits", || {
        // Rate-limit persistence and same-pass usage scheduling use resolved
        // provider panels, not the bare rollup. Final folds merge the
        // just-written cache read-only.
        let mut panels = fold_machine_config_with(
            base.clone(),
            config,
            accounts.clone(),
            &spending.provider.spending.by_login,
            // This scoped fold is not returned as the final snapshot.
            RemoteControlServerHealth::default(),
            &room_remote_control,
            &logins,
            PanelScope::InUse,
        );
        refresh_rate_limits(&mut panels, runtime, &logins);
        // `with_provider_aggregates` rebuilds panels with empty credit fields;
        // the scoped producer fold must reapply the shared cache before
        // auto-redeem can evaluate the already-known reset credits.
        credits::apply_credits_cache(&mut panels, runtime, &config.accounts, &logins);
        panels
    });

    steps.step("sessions", || {
        refresh_live_sessions(base, runtime, &mut state.session_checks)
    });
    steps.step("account_usage", || {
        refresh_account_usage(&panels, runtime, &logins)
    });
    steps.step("auto_continue", || {
        let active: std::collections::HashSet<_> = base
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent() && !agent.agent_id.is_empty())
            .map(|agent| (&agent.kind, &agent.agent_id))
            .collect();
        state
            .auto_continue_checks
            .retain(|(kind, id), _| config.resume.auto_continue && active.contains(&(kind, id)));
        let resume_messages = crate::harness::auto_continue::read_resume_messages(
            store.as_ref(),
            &config.resume,
            base.resume_outcomes.as_deref().unwrap_or_default(),
        );
        crate::harness::auto_continue::resume_parked(
            |agent| {
                !super::cache::hibernated_check_due(
                    &mut state.auto_continue_checks,
                    (agent.kind.clone(), agent.agent_id.clone()),
                    agent.hibernation.is_some(),
                    Instant::now(),
                )
            },
            base,
            runtime,
            &logins,
            &config.resume,
            &resume_messages,
        )
    });
    let policy = if base
        .agents
        .iter()
        .any(|agent| agent.team.is_some() || agent.profile.is_some())
    {
        SeatPolicy::load(config, base.project_root.as_deref())
    } else {
        SeatPolicy::default()
    };
    let teams = &policy.teams;
    steps.step("idle_compact", || {
        crate::harness::idle_compact::compact_idle_agents(base, runtime, config, &policy)
    });
    steps.step("soft_compact", || {
        crate::harness::soft_compact::compact_agents(base, runtime, config, &policy)
    });
    steps.step("idle_stop", || {
        crate::harness::idle_stop::stop_idle_agents(base, state_paths, runtime)
    });
    steps.step("hibernate", || {
        crate::harness::hibernate::hibernate_idle_agents(
            base,
            state_paths,
            runtime,
            config,
            &policy,
        )
    });
    steps.step("cache_keepalive", || {
        crate::harness::cache_keepalive::keepalive_agents(base, runtime, &config.harness, &policy)
    });
    steps.step("keep_warm", || {
        keep_warm::publish(base, runtime, &config.harness, &policy)
    });
    steps.step("auto_redeem", || {
        crate::harness::auto_redeem::redeem_credits(
            &panels.providers,
            &credits::idle_reset_credits(runtime, &logins),
            runtime,
            &logins,
            &config.resume,
            base.now,
        )
    });
    steps.step("budget", || {
        let active: std::collections::HashSet<_> = base
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
            .map(|agent| (&agent.kind, &agent.agent_id))
            .collect();
        state
            .budget_checks
            .retain(|(kind, id), _| active.contains(&(kind, id)));
        let mut budget_snapshot = base.clone();
        apply_live_day_spend(&mut budget_snapshot, &spending.workspace);
        crate::harness::budget::enforce(
            |agent| {
                !super::cache::hibernated_check_due(
                    &mut state.budget_checks,
                    (agent.kind.clone(), agent.agent_id.clone()),
                    agent.hibernation.is_some(),
                    Instant::now(),
                )
            },
            &budget_snapshot,
            runtime,
            store.as_ref(),
            config,
        )
    });
    let runs = steps.step("run_timeout", || {
        let runs = crate::harness::run_timeout::enforce(state_paths, runtime, base.now);
        if let Some(runs) = &runs {
            crate::harness::park_notice::notify_parents(runs, &base.agents, runtime);
            crate::harness::stall_notice::notify_parents(
                runs,
                &base.agents,
                runtime,
                base.now,
                config.agents.attention.stalled_after_secs.get(),
            );
        }
        runs
    });
    let now_ms = base.now.as_millisecond().max(0) as u64;
    steps.step("orphan_sweep", || {
        if let Some(runs) = runs
            && orphan_sweep_due(state.orphan_sweep_checked_at_ms, now_ms)
        {
            state.orphan_sweep_checked_at_ms = Some(now_ms);
            crate::harness::orphan_sweep::enforce(
                state_paths,
                runtime,
                &runs,
                base.now,
                &mut state.rollup,
            );
        }
    });
    steps.step("auto_gc", || {
        crate::harness::auto_gc::sweep_if_due(
            state_paths,
            runtime,
            base.project_root.as_deref(),
            &config.gc,
            base.now,
            &mut state.auto_gc,
        );
        crate::harness::auto_gc::tmp_floor_if_due(
            state_paths,
            runtime,
            base.project_root.as_deref(),
            &config.gc,
            base.now,
            &mut state.auto_gc,
        );
    });
    steps.step("diff_stats", || {
        refresh_diff_stats_for(
            base,
            runtime,
            config.sidebar.trunk.as_deref(),
            git_pass,
            &mut state.git,
        )
    });
    steps.step("cohort_spend", || {
        refresh_cohort_spend_for(
            base,
            state_paths,
            runtime,
            config.agents.attention.active_grace_secs.get(),
            unix_now_ms(),
            &mut state.rollup,
            &mut state.cohort_effort,
        )
    });
    steps.step("pipeline", || {
        pipeline::refresh_pipeline_for(base, runtime, config, teams)
    });
    let pr_cache = steps.step("pr_states", || produce_pr_states(base, runtime, state));
    steps.step("trunk", || {
        trunk::produce_trunk_state(base, runtime, config.sidebar.trunk.as_deref())
    });
    state.pass_spawns = steps.into_steps();

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
    use crate::agents::{AgentState, AgentStatus};
    use jiff::Timestamp;

    #[test]
    fn codex_origin_overrides_read_transcript_and_worktree_from_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("rollout.jsonl");
        let worktree = dir.path().join("repo");
        let now = Timestamp::now();
        let agent = AgentState {
            status: AgentStatus::Running,
            worktree_path: Some(worktree.display().to_string()),
            transcript_path: Some(transcript.display().to_string()),
            ..crate::testkit::agent_state("codex", "codex-1", now)
        };
        let snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(&worktree),
            vec![agent],
            now,
        );

        let overrides = codex_origin_overrides(&snapshot);

        assert_eq!(overrides.get(&transcript), Some(&worktree));
    }

    fn runtime_root() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn publish(runtime: &RuntimePaths, scope_hash: &str, usd: f64) {
        let mut cache = WorkspaceSpendingCache {
            scope_hash: scope_hash.to_owned(),
            ..WorkspaceSpendingCache::default()
        };
        cache.tally.year.usd = usd;
        crate::agents::spending::write_workspace_spending_cache(
            &runtime.workspace_spending_path(scope_hash),
            &cache,
        );
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
