//! Producer-side Codex reset-credit policy and its detached helper action.
//!
//! The elected producer evaluates cached provider-neutral capacity and credit
//! state, then spawns a hidden CLI helper only when a redemption is useful. The
//! helper serializes account-wide attempts, refreshes both inputs, re-evaluates
//! the same pure verdict, and performs the provider-specific consume request.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::account::ProviderCapacity;
use crate::agents::codex::oauth_usage::{
    ConsumeCode, ResetCreditDetail, consume_reset_credit, fetch_reset_credit_state,
    fetch_usage_with_url, load_configured_credentials, reset_credits_url, usage_url,
};
use crate::agents::{AccountUsageSnapshot, ProviderAccountScope, ResetCredits};
#[cfg(not(test))]
use crate::child_process::detached_rimz_command;
use crate::config::ResumeConfig;
use crate::store::atomic::write_temp_then_rename_cache;
use crate::{RuntimePaths, SidebarProviderPanel};

const CODEX_KIND: &str = "codex";
pub(crate) const EXPIRY_RESCUE_LEAD: Duration = Duration::from_secs(30 * 60);
pub(crate) const MIN_HOLD: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const ATTEMPT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
pub(crate) const POST_SUCCESS_COOLDOWN: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedeemReason {
    ExpiryRescue,
    BlockedGain,
    DoomedCredit,
}

impl RedeemReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExpiryRescue => "expiry_rescue",
            Self::BlockedGain => "blocked_gain",
            Self::DoomedCredit => "doomed_credit",
        }
    }
}

impl FromStr for RedeemReason {
    type Err = AutoRedeemErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "expiry_rescue" => Ok(Self::ExpiryRescue),
            "blocked_gain" => Ok(Self::BlockedGain),
            "doomed_credit" => Ok(Self::DoomedCredit),
            _ => Err(AutoRedeemErr::InvalidReason(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RedeemStamp {
    attempted_at: Timestamp,
    request_id: String,
    reason: RedeemReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AutoRedeemErr {
    #[error("auto-redeem supports only the `codex` provider, not `{0}`")]
    UnsupportedKind(String),
    #[error("invalid auto-redeem reason `{0}`")]
    InvalidReason(String),
    #[error("locking the shared auto-redeem attempt: {0}")]
    Lock(#[from] crate::store::lock::LockErr),
    #[error("writing the shared auto-redeem stamp: {0}")]
    Stamp(#[from] crate::store::atomic::AtomicErr),
    #[error("Codex auto-redeem request failed: {0}")]
    Codex(String),
}

/// Decide whether current provider-neutral capacity and reset credits warrant
/// one consume attempt. Expiry rescue is unconditional; limit redemption
/// follows the user's opt-in.
pub(crate) fn redeem_verdict(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    min_gain: Duration,
    auto_redeem: bool,
    now: Timestamp,
) -> Option<RedeemReason> {
    if credits.count == 0 {
        return None;
    }

    if credits.soonest_expiry.is_some_and(|expiry| {
        expiry > now && expiry.as_second() - now.as_second() <= duration_seconds(EXPIRY_RESCUE_LEAD)
    }) {
        return Some(RedeemReason::ExpiryRescue);
    }
    if !auto_redeem {
        return None;
    }

    let natural_reset = capacity?.latest_spent_window_reset(now)?;
    if credits.soonest_expiry.is_some_and(|expiry| {
        expiry.as_second() - natural_reset.as_second() < duration_seconds(MIN_HOLD)
    }) {
        return Some(RedeemReason::DoomedCredit);
    }
    (natural_reset.as_second() - now.as_second() >= duration_seconds(min_gain))
        .then_some(RedeemReason::BlockedGain)
}

fn duration_seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn stamp_allows_attempt(stamp: Option<&RedeemStamp>, now: Timestamp) -> bool {
    let Some(stamp) = stamp else {
        return true;
    };
    let cooldown = if stamp.outcome.as_deref() == Some("reset") {
        POST_SUCCESS_COOLDOWN
    } else {
        ATTEMPT_COOLDOWN
    };
    now.as_second() - stamp.attempted_at.as_second() >= duration_seconds(cooldown)
}

fn read_stamp(path: &Path) -> Option<RedeemStamp> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_stamp(path: &Path, stamp: &RedeemStamp) -> Result<(), AutoRedeemErr> {
    write_temp_then_rename_cache(path, stamp)?;
    Ok(())
}

/// Evaluate the Codex panel and spawn the account-wide helper when due.
/// Codex is the only provider with reset credits today; keep that provider
/// choice here while the verdict above remains provider-neutral.
pub(crate) fn redeem_credits(
    panels: &[SidebarProviderPanel],
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    now: Timestamp,
) {
    let Some(panel) = panels.iter().find(|panel| panel.kind == CODEX_KIND) else {
        return;
    };
    let Some(credits) = panel.reset_credits.as_ref() else {
        return;
    };
    let capacity = ProviderCapacity::read(runtime, CODEX_KIND);
    let Some(reason) = redeem_verdict(
        capacity.as_ref(),
        credits,
        config.auto_redeem_min_gain(),
        config.auto_redeem,
        now,
    ) else {
        return;
    };

    // Serialize the cooldown check and reservation across room producers. The
    // helper waits on this same lock, so it cannot observe the pre-reservation
    // state after a successful spawn.
    let _guard = match crate::store::lock::WorkspaceLock::try_acquire(
        &runtime.shared_auto_redeem_lock(CODEX_KIND),
    ) {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                tags.operation = "auto_redeem.reserve",
                error = &err as &dyn std::error::Error,
                "sidebar: failed to reserve agent auto-redeem",
            );
            return;
        }
    };
    let stamp_path = runtime.shared_auto_redeem_path(CODEX_KIND);
    if !stamp_allows_attempt(read_stamp(&stamp_path).as_ref(), now) {
        return;
    }
    let request_id = uuid::Uuid::now_v7().to_string();
    if !spawn_auto_redeem(runtime, reason, &request_id) {
        return;
    }
    if let Err(err) = write_stamp(
        &stamp_path,
        &RedeemStamp {
            attempted_at: now,
            request_id,
            reason,
            outcome: None,
        },
    ) {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "auto_redeem.reserve",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record agent auto-redeem reservation",
        );
    }
}

/// Run the provider-specific action behind the hidden helper. Returns `true`
/// after a successful reset was published so the caller can wake renderers.
pub fn execute_auto_redeem(
    runtime: &RuntimePaths,
    kind: &str,
    requested_reason: &str,
    request_id: &str,
    config: &ResumeConfig,
) -> Result<bool, AutoRedeemErr> {
    if kind != CODEX_KIND {
        return Err(AutoRedeemErr::UnsupportedKind(kind.to_owned()));
    }
    let requested_reason = requested_reason.parse::<RedeemReason>()?;
    if crate::agents::credits::oauth_usage_offline() {
        return Ok(false);
    }

    let _guard =
        crate::store::lock::WorkspaceLock::acquire(&runtime.shared_auto_redeem_lock(CODEX_KIND))?;
    let stamp_path = runtime.shared_auto_redeem_path(CODEX_KIND);
    let now = Timestamp::now();
    let prior_stamp = read_stamp(&stamp_path);
    let owns_reservation = prior_stamp
        .as_ref()
        .is_some_and(|stamp| stamp.request_id == request_id && stamp.outcome.is_none());
    if !owns_reservation && !stamp_allows_attempt(prior_stamp.as_ref(), now) {
        return Ok(false);
    }

    let (credentials, base_url) =
        load_configured_credentials().map_err(|err| AutoRedeemErr::Codex(err.to_string()))?;
    let usage = fetch_usage_with_url(&usage_url(base_url.as_deref()), &credentials)
        .map_err(|err| AutoRedeemErr::Codex(err.to_string()))?;
    let (credits, details) =
        fetch_reset_credit_state(&reset_credits_url(base_url.as_deref()), &credentials)
            .map_err(|err| AutoRedeemErr::Codex(err.to_string()))?;
    let capacity = usage
        .rate_limits
        .as_ref()
        .map(|limits| ProviderCapacity::from_windows(limits.windows.clone()));
    let Some(reason) = redeem_verdict(
        capacity.as_ref(),
        &credits,
        config.auto_redeem_min_gain(),
        config.auto_redeem,
        now,
    ) else {
        return Ok(false);
    };

    let credit_id = soonest_credit_id(&details);
    let mut stamp = RedeemStamp {
        attempted_at: now,
        request_id: request_id.to_owned(),
        reason,
        outcome: None,
    };
    write_stamp(&stamp_path, &stamp)?;

    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = CODEX_KIND,
        requested_reason = requested_reason.as_str(),
        reason = reason.as_str(),
        "auto-redeem: consuming reset credit",
    );
    let outcome = consume_reset_credit(&credentials, base_url.as_deref(), request_id, credit_id)
        .map_err(|err| AutoRedeemErr::Codex(err.to_string()))?;
    stamp.outcome = Some(outcome.code.as_str().to_owned());
    write_stamp(&stamp_path, &stamp)?;

    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = CODEX_KIND,
        reason = reason.as_str(),
        outcome = outcome.code.as_str(),
        windows_reset = outcome.windows_reset,
        "auto-redeem: reset-credit outcome",
    );
    if outcome.code != ConsumeCode::Reset {
        return Ok(false);
    }

    let mut refreshed = fetch_usage_with_url(&usage_url(base_url.as_deref()), &credentials)
        .map_err(|err| AutoRedeemErr::Codex(err.to_string()))?;
    refreshed.reset_credits =
        fetch_reset_credit_state(&reset_credits_url(base_url.as_deref()), &credentials)
            .ok()
            .map(|(credits, _)| credits);
    publish_usage(runtime, refreshed);
    Ok(true)
}

fn soonest_credit_id(details: &[ResetCreditDetail]) -> Option<&str> {
    details
        .iter()
        .filter_map(|detail| detail.expires_at.map(|expiry| (expiry, detail)))
        .min_by_key(|(expiry, _)| *expiry)
        .map(|(_, detail)| detail)
        .or_else(|| details.first())
        .and_then(|detail| detail.id.as_deref())
}

fn publish_usage(runtime: &RuntimePaths, snapshot: AccountUsageSnapshot) {
    if let Some(windows) = snapshot.rate_limits.clone() {
        crate::sidebar::refresh::merge_account_rate_limits(
            runtime,
            CODEX_KIND,
            ProviderAccountScope::KindWide,
            windows,
        );
    }
    if snapshot.plan.is_some()
        || snapshot.extra_credits.is_some()
        || snapshot.reset_credits.is_some()
    {
        crate::sidebar::refresh::merge_provider_realtime_usage(
            runtime,
            CODEX_KIND,
            ProviderAccountScope::KindWide,
            snapshot,
        );
    }
}

#[cfg(not(test))]
fn spawn_auto_redeem(runtime: &RuntimePaths, reason: RedeemReason, request_id: &str) -> bool {
    let exe = crate::proc::rimz_exe();
    let mut cmd = detached_rimz_command(exe, runtime);
    cmd.args([
        "agents",
        "auto-redeem",
        "--workspace-id",
        runtime.workspace_id.as_str(),
        "--kind",
        CODEX_KIND,
        "--reason",
        reason.as_str(),
        "--request-id",
        request_id,
    ]);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = CODEX_KIND,
        reason = reason.as_str(),
        "sidebar: auto-redeeming reset credit",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agent-auto-redeem") {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "auto_redeem.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent auto-redeem",
        );
        return false;
    }
    true
}

#[cfg(test)]
fn spawn_auto_redeem(runtime: &RuntimePaths, reason: RedeemReason, request_id: &str) -> bool {
    let _ = (runtime, reason, request_id);
    true
}

#[cfg(test)]
mod tests;
