//! Producer-published files read by consumer folds.

use std::path::PathBuf;

use crate::RuntimePaths;

pub(in crate::sidebar) fn published_lane_inputs(runtime: &RuntimePaths) -> [PathBuf; 8] {
    [
        runtime.diff_stats_path(),
        runtime.cohort_spend_path(),
        runtime.pr_state_path(),
        runtime.shared_accounts_path(),
        runtime.shared_rate_limits_path(),
        runtime.shared_credits_path(),
        runtime.shared_provider_spending_path(),
        super::daemon_reap::codex_daemon_reap_path(runtime),
    ]
}

pub(in crate::sidebar) fn is_workspace_spending_file(name: &str) -> bool {
    name.starts_with("workspace-spending.") && name.ends_with(".json")
}
