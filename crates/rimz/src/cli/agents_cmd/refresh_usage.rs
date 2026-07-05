//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in provider.
//! Every kind runs the same API-query channel — a direct OAuth read of its own
//! quota surface through
//! [`AgentAdapter::probe_oauth_usage`](rimz::agents::AgentAdapter::probe_oauth_usage),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.json`
//! caches. An adapter may expose a pollable realtime account channel; the helper
//! reads it first, then runs the OAuth channel on its own shared cadence.
//! Best-effort and quiet: every provider-side failure exits successfully with
//! the shared cache recording the retry state.

use anyhow::{Context, Result};
use clap::Args;

use rimz::ids::WorkspaceId;
use rimz::sidebar::refresh::{
    merge_account_rate_limits, merge_oauth_usage_if_due, merge_provider_credits,
};
use rimz::{RuntimePaths, agents};

use crate::cli::GlobalFlags;

#[derive(Debug, Args)]
pub(super) struct RefreshUsageArgs {
    /// The provider kind whose account usage is refreshed (`claude`, `codex`,
    /// `pi`, `opencode`).
    #[arg(long)]
    kind: String,
    /// Workspace whose runtime cache the account usage is written into.
    #[arg(long)]
    workspace_id: String,
    /// Also merge included-budget windows from the OAuth read into the shared
    /// rate-limit cache. Unset when a fresh realtime reading already owns them.
    #[arg(long)]
    merge_windows: bool,
}

pub(super) fn run_refresh_usage(args: RefreshUsageArgs, _globals: &GlobalFlags) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    if agents::credits::oauth_usage_offline() {
        return Ok(());
    }

    let wrote = refresh_usage(&runtime, &args.kind, args.merge_windows);
    if wrote {
        let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

fn refresh_usage(runtime: &RuntimePaths, kind: &str, merge_windows: bool) -> bool {
    let Some(adapter) = agents::find_adapter(kind) else {
        return false;
    };
    let Some(usage) = adapter.probe_realtime_account_usage(runtime) else {
        let fallback_merge_windows = merge_windows
            || adapter
                .descriptor()
                .capabilities
                .realtime_usage
                .covers_account_while_live;
        return merge_oauth_usage_if_due(runtime, kind, fallback_merge_windows);
    };
    let mut wrote = false;
    let windows_missing = usage.rate_limits.is_none();
    if let Some(extra_credits) = usage.extra_credits {
        merge_provider_credits(runtime, kind, Some(extra_credits));
        wrote = true;
    }
    if let Some(rate_limits) = usage.rate_limits {
        merge_account_rate_limits(runtime, kind, rate_limits);
        wrote = true;
    }
    wrote |= merge_oauth_usage_if_due(runtime, kind, windows_missing);
    wrote
}
