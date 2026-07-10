//! Producer-owned heavy lane refresh for the sidebar data plane.
//!
//! The elected producer folds pane/sidecar truth first, then this module runs
//! the TTL-gated probes and cache publishes that are too expensive for every
//! renderer. The returned lane values feed a final fold when a caller needs the
//! freshest view in the same process.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::AgentAccount;
use crate::agents::AgentState;
use crate::agents::spending::{SpendingCaches, SpendingWalker};
use crate::config::MachineConfig;
use crate::{RuntimePaths, SidebarSnapshot};

pub mod accounts;
pub mod credits;
pub mod daemon_reap;
mod git_refs;
pub mod git_stats;
pub mod live_spend;
pub mod pr;
pub mod rate_limits;
pub mod sessions;
pub mod spending;
pub mod usage;

pub use accounts::AccountsCache;
pub use credits::{
    CreditsCache, ProviderCreditsEntry, merge_provider_credits, merge_provider_credits_entry_if_due,
};
pub use daemon_reap::{CodexDaemonReap, read_codex_daemon_reap, write_codex_daemon_reap};
pub use live_spend::{apply_live_day_spend, apply_live_today_spend};
pub use pr::{PrLink, PrStateCache};
pub use rate_limits::{drop_kind_rate_limits, merge_account_rate_limits};
pub use sessions::{
    ForcedSessionRefresh, force_refresh_session_context, refresh_session_transcript_context,
};
pub use usage::merge_oauth_usage_if_due;

use self::accounts::produce_accounts;
use self::daemon_reap::refresh_codex_daemon_reap_cache;
use self::git_stats::refresh_diff_stats_for;
use self::pr::produce_pr_states;
use self::rate_limits::apply_rate_limit_cache;
use self::sessions::refresh_live_sessions;
use self::usage::refresh_account_usage;
use super::enrich::{fold_machine_config_with, read_auto_continue_resume_messages};
use super::timing::unix_now_ms;

#[derive(Clone, Debug)]
pub struct RefreshedLanes {
    pub spending: SpendingCaches,
    pub accounts: BTreeMap<String, AgentAccount>,
    pub pr_states: BTreeMap<String, PrLink>,
}

pub fn refresh_heavy_lanes(
    base: &SidebarSnapshot,
    daemon_probe_agents: &[AgentState],
    state_messages_dir: &Path,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    walker: &mut SpendingWalker,
) -> RefreshedLanes {
    refresh_codex_daemon_reap_cache(daemon_probe_agents, runtime, unix_now_ms());

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
        config,
        lanes.accounts.clone(),
        &lanes.spending.provider.spending.by_provider,
    );
    apply_rate_limit_cache(&mut panels, runtime, true);

    refresh_live_sessions(base, runtime);
    refresh_account_usage(base, runtime);
    let resume_messages = read_auto_continue_resume_messages(
        Some(state_messages_dir),
        &config.resume,
        base.resume_outcomes.as_deref().unwrap_or_default(),
    );
    crate::harness::auto_continue::resume_parked(base, runtime, &config.resume, &resume_messages);
    let mut budget_snapshot = base.clone();
    apply_live_day_spend(&mut budget_snapshot, &lanes.spending.workspace);
    crate::harness::budget::enforce(&budget_snapshot, runtime, state_messages_dir, config);
    refresh_diff_stats_for(base, runtime, config.sidebar.trunk.as_deref());

    lanes
}
