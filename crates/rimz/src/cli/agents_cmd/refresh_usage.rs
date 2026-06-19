//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in provider.
//! Every kind runs the same API-query channel — a direct OAuth read of its own
//! quota surface through
//! [`AgentAdapter::probe_oauth_usage`](rimz::agents::AgentAdapter::probe_oauth_usage),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.json`
//! caches. Codex additionally polls its app-server first (its realtime channel,
//! pollable while idle) and falls back to OAuth only for the fields the
//! app-server did not return. Best-effort and quiet: every provider-side failure
//! exits successfully with the shared cache recording the retry state.

use anyhow::{Context, Result};
use clap::Args;

use rimz::ids::WorkspaceId;
use rimz::sidebar::enrich::merge_oauth_usage_if_due;
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

    let wrote = match args.kind.as_str() {
        "codex" => refresh_codex(&runtime),
        kind => merge_oauth_usage_if_due(&runtime, kind, args.merge_windows),
    };
    if wrote {
        let _ = rimz::ledger::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}

/// Codex's realtime channel is its app-server, pollable while idle: read it
/// first, then fall back to the OAuth channel only for the fields it did not
/// return. Mirrors the live-session refresh's app-server-first precedence so the
/// idle and active dashboards agree.
fn refresh_codex(runtime: &RuntimePaths) -> bool {
    let broker_socket = runtime.codex_app_server_socket_path();
    let Some(enrichment) =
        agents::codex::refresh_app_server_enrichment(None, None, Some(&broker_socket))
    else {
        // App-server unreachable: the OAuth channel owns both windows and credits.
        return merge_oauth_usage_if_due(runtime, "codex", true);
    };
    let mut wrote = false;
    let app_windows_missing = enrichment.context.rate_limits.is_none();
    let app_credits_missing = enrichment.extra_credits.is_none();
    if let Some(extra_credits) = enrichment.extra_credits.clone() {
        rimz::sidebar::enrich::merge_provider_credits(runtime, "codex", Some(extra_credits));
        wrote = true;
    }
    if let Some(rate_limits) = enrichment.context.rate_limits.clone() {
        rimz::sidebar::enrich::merge_account_rate_limits(runtime, "codex", rate_limits);
        wrote = true;
    }
    if app_windows_missing || app_credits_missing {
        wrote |= merge_oauth_usage_if_due(runtime, "codex", app_windows_missing);
    }
    wrote
}
