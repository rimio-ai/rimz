//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in provider.
//! Every adapter with a supported usage surface runs the same API-query channel
//! through
//! [`AgentDefinition::probe_account_usage`](rimz::agents::AgentDefinition::probe_account_usage),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.json`
//! caches. An adapter may expose a pollable realtime account channel; the helper
//! reads it first, then runs the direct channel on its own shared cadence. When
//! direct-query windows are available, they merge after the realtime fold:
//! climbs land immediately, while a same-epoch drop contested by stamped
//! realtime truth waits for confirmation.
//! Best-effort and quiet: every provider-side failure exits successfully with
//! the shared cache recording the retry state.

use anyhow::{Context, Result};
use clap::Args;

use rimz::ids::WorkspaceId;
use rimz::sidebar::refresh::refresh_claimed_account_usage;

use crate::cli::{GlobalFlags, runtime_paths_for};

#[derive(Debug, Args)]
pub(super) struct RefreshUsageArgs {
    /// The provider kind whose account usage is refreshed (`claude`, `codex`,
    /// `copilot`, `antigravity`, `kimi`, `pi`, `opencode`, `qwen`).
    #[arg(long)]
    kind: String,
    /// Workspace whose runtime cache the account usage is written into.
    #[arg(long)]
    workspace_id: String,
    /// Internal nonce of the producer's durable refresh claim.
    #[arg(long, hide = true)]
    claim_id: uuid::Uuid,
}

pub(super) fn run_refresh_usage(args: RefreshUsageArgs, _globals: &GlobalFlags) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let runtime = runtime_paths_for(workspace_id)?;

    let wrote = refresh_claimed_account_usage(&runtime, &args.kind, args.claim_id);
    if wrote {
        let _ = rimz::store::wakeup::wake_sidebars(&runtime);
    }
    Ok(())
}
