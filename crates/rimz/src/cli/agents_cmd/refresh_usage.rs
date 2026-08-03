//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in provider.
//! Every adapter with a supported usage surface runs the same API-query channel
//! through
//! [`AgentDefinition::probe_account_usage`](rimz::agents::AgentDefinition::probe_account_usage),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.json`
//! caches. An adapter may expose a pollable realtime account channel; the helper
//! reads it first, then runs the direct channel on its own shared cadence. When
//! direct-query windows are available, they merge after the realtime fold, so a
//! fresh credential read can replace a stale warm realtime process. Climbs land
//! immediately, while a same-epoch drop contested by stamped statusline truth
//! waits for confirmation.
//! Best-effort and quiet: every provider-side failure exits successfully with
//! the shared cache recording the retry state.

use anyhow::Result;

use rimz::sidebar::refresh::refresh_claimed_account_usage;
use rimz::sidebar::refresh::usage::AccountUsageRefreshRequest;

use crate::cli::runtime_paths_for;

pub(super) fn run_refresh_usage(request: AccountUsageRefreshRequest) -> Result<()> {
    let runtime = runtime_paths_for(request.workspace_id)?;

    let wrote = refresh_claimed_account_usage(&runtime, request.kind.as_str(), request.claim_id);
    if wrote {
        let _ = rimz::sidebar::wakeup::wake_store_delta(&runtime, None, None);
    }
    Ok(())
}
