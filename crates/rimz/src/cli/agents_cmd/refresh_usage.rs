//! Uniform provider account-usage refresh helper (`rimz agents refresh-usage`).
//!
//! Spawned detached by the sidebar producer for one metered, logged-in credential.
//! Every adapter with a supported usage surface runs the same API-query channel
//! through
//! [`probe_account_usage_for`](rimz::agents::AgentDefinition::probe_account_usage_for),
//! single-flighted and folded into the shared `credits.json`/`rate_limits.v<N>.json`
//! caches. An adapter may expose a pollable realtime account channel; the helper
//! reads it first for the room-default login, then runs the direct channel on its
//! own shared cadence. Other logins use the direct channel alone. When
//! direct-query windows are available, they merge after the realtime fold, so a
//! fresh credential read can replace a stale warm realtime process. Each merged
//! direct read anchors its windows: it supersedes any reading stamped no later
//! than itself, climb or drop, and a best-effort reading overlays it only when
//! stamped later.
//! Best-effort and quiet: every provider-side failure exits successfully with
//! the shared cache recording the retry state.

use anyhow::Result;

use rimz::sidebar::refresh::refresh_claimed_account_usage;
use rimz::sidebar::refresh::usage::AccountUsageRefreshRequest;

use crate::cli::runtime_paths_for;

pub(super) fn run_refresh_usage(request: AccountUsageRefreshRequest) -> Result<()> {
    let runtime = runtime_paths_for(request.workspace_id)?;

    let wrote = refresh_claimed_account_usage(
        &runtime,
        &request.login,
        request.claim_id,
        request.sub_provider.as_deref(),
    );
    if wrote {
        let _ = rimz::wakeup::wake_store_delta(&runtime, None, None);
    }
    Ok(())
}
