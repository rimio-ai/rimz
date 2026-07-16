//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in provider.
//! Every adapter with a supported usage surface runs the same API-query channel
//! through
//! [`AgentAdapter::probe_account_usage`](rimz::agents::AgentAdapter::probe_account_usage),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.json`
//! caches. An adapter may expose a pollable realtime account channel; the helper
//! reads it first, then runs the direct channel on its own shared cadence. When
//! direct-query windows are requested, they merge after the realtime fold so a
//! fresh credential read can replace a stale warm realtime process.
//! Best-effort and quiet: every provider-side failure exits successfully with
//! the shared cache recording the retry state.

use anyhow::{Context, Result};
use clap::Args;

use rimz::RuntimePaths;
use rimz::ids::WorkspaceId;
use rimz::sidebar::refresh::refresh_claimed_account_usage;

use crate::cli::GlobalFlags;

#[derive(Debug, Args)]
pub(super) struct RefreshUsageArgs {
    /// The provider kind whose account usage is refreshed (`claude`, `codex`,
    /// `copilot`, `antigravity`, `kimi`, `pi`, `opencode`, `qwen`).
    #[arg(long)]
    kind: String,
    /// Workspace whose runtime cache the account usage is written into.
    #[arg(long)]
    workspace_id: String,
    /// Also merge included-budget windows from the OAuth read into the shared
    /// rate-limit cache. Unset when a fresh realtime reading already owns them.
    #[arg(long)]
    merge_windows: bool,
    /// Internal nonce of the producer's durable refresh claim.
    #[arg(long, hide = true)]
    claim_id: uuid::Uuid,
}

pub(super) fn run_refresh_usage(args: RefreshUsageArgs, _globals: &GlobalFlags) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    runtime.ensure_dirs().context("preparing runtime dirs")?;

    let wrote =
        refresh_claimed_account_usage(&runtime, &args.kind, args.claim_id, args.merge_windows);
    if wrote {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}
