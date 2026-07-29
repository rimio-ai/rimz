//! Producer-side Codex reset-credit policy and its detached helper action.
//!
//! The elected producer evaluates cached provider-neutral capacity and credit
//! state, then spawns a hidden CLI helper only when a redemption is useful. The
//! helper serializes account-wide attempts, refreshes both inputs, re-evaluates
//! the same pure verdict, and performs the provider-specific consume request.
//! Elected-producer and one-shot heavy refreshes may both advance the shared
//! burn-rate cache; atomic replacement plus observation stamps make duplicate
//! folds idempotent.

use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::agents::account::{
    ProviderCapacity, RedemptionCode, ResetCreditResult, prepare_reset_credit_redemption,
};
use crate::agents::{AccountUsageSnapshot, RateLimitWindow, ResetCredits};
use crate::config::ResumeConfig;
use crate::harness::assist_log::AssistWindowReset;
use crate::ids::{AgentKind, WorkspaceId};
use crate::store::atomic::write_temp_then_rename_cache;
use crate::{RuntimePaths, SidebarProviderPanel};

const CODEX_KIND: &str = "codex";
pub(crate) const EXPIRY_RESCUE_LEAD: Duration = Duration::from_secs(30 * 60);
pub(crate) const MIN_HOLD: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const ATTEMPT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
pub(crate) const POST_SUCCESS_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const RATE_HALF_LIFE: Duration = Duration::from_secs(3 * 24 * 60 * 60);
pub(crate) const RATE_FLOOR: f64 = 0.5;
pub(crate) const T_MIN: Duration = Duration::from_secs(6 * 60 * 60);
const SECONDS_PER_DAY: f64 = 24.0 * 60.0 * 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedeemReason {
    ExpiryRescue,
    BlockedGain,
    DoomedCredit,
    ScheduledRedeem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRedeemRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub reason: RedeemReason,
    pub request_id: uuid::Uuid,
}

impl RedeemReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExpiryRescue => "expiry_rescue",
            Self::BlockedGain => "blocked_gain",
            Self::DoomedCredit => "doomed_credit",
            Self::ScheduledRedeem => "scheduled_redeem",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RateStamp {
    window_resets_at: Timestamp,
    last_used_pct: u8,
    last_observed_at: Timestamp,
    rate_pct_per_day: f64,
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
    #[error("locking the shared auto-redeem attempt: {0}")]
    Lock(#[from] crate::store::lock::LockErr),
    #[error("writing the shared auto-redeem stamp: {0}")]
    Stamp(#[from] crate::store::atomic::AtomicErr),
    #[error("Codex auto-redeem request failed: {0}")]
    Codex(String),
    #[error("{error}")]
    Attempted {
        report: Box<RedeemReport>,
        error: String,
    },
}

impl AutoRedeemErr {
    pub fn attempted_report(&self) -> Option<&RedeemReport> {
        match self {
            Self::Attempted { report, .. } => Some(report),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemReport {
    pub reason: RedeemReason,
    pub credits: u32,
    pub soonest_expiry: Option<Timestamp>,
    pub natural_reset: Option<Timestamp>,
    pub outcome: Option<RedemptionCode>,
    pub windows_reset: bool,
    pub window_resets: Vec<AssistWindowReset>,
    pub reset: bool,
}

/// Decide whether current provider-neutral capacity and reset credits warrant
/// one consume attempt. Expiry rescue is unconditional; limit redemption
/// follows the user's opt-in.
pub(crate) fn redeem_verdict(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
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

    if let Some(natural_reset) = capacity.and_then(|value| value.latest_spent_window_reset(now)) {
        if credits.soonest_expiry.is_some_and(|expiry| {
            expiry.as_second() - natural_reset.as_second() < duration_seconds(MIN_HOLD)
        }) {
            return Some(RedeemReason::DoomedCredit);
        }
        if natural_reset.as_second() - now.as_second() >= duration_seconds(min_gain) {
            return Some(RedeemReason::BlockedGain);
        }
    }

    let expiry_count = usize::try_from(credits.count).unwrap_or(usize::MAX);
    let expiries = credits
        .expiries
        .iter()
        .copied()
        .filter(|expiry| *expiry > now)
        .take(expiry_count)
        .collect::<Vec<_>>();
    let first_expiry = *expiries.first()?;
    if now < paced_chain_deadline(capacity, &expiries, rate_pct_per_day, now)? {
        return None;
    }
    if free_reset_defers(capacity, first_expiry, min_gain, now) {
        return None;
    }
    Some(RedeemReason::ScheduledRedeem)
}

fn paced_chain_deadline(
    capacity: Option<&ProviderCapacity>,
    expiries: &[Timestamp],
    rate_pct_per_day: Option<f64>,
    now: Timestamp,
) -> Option<Timestamp> {
    let refill = refill_interval(rate_pct_per_day)?;
    let chain_deadline = chain_deadline(expiries, rate_pct_per_day)?;
    let window = capacity?.longest_window_observation(now)?;
    let resets_at = window.resets_at?;
    let duration_mins = window.duration_mins.filter(|mins| *mins > 0)?;
    let window_start = resets_at
        .checked_sub(SignedDuration::from_secs(i64::from(duration_mins) * 60))
        .ok()?;
    let paced_deadline = window_start.checked_add(refill).ok()?;
    Some(chain_deadline.max(paced_deadline))
}

fn chain_deadline(expiries: &[Timestamp], rate_pct_per_day: Option<f64>) -> Option<Timestamp> {
    let lead = SignedDuration::from_secs(duration_seconds(EXPIRY_RESCUE_LEAD));
    let mut deadline = expiries.last()?.checked_sub(lead).ok()?;
    let Some(refill) = refill_interval(rate_pct_per_day) else {
        return expiries.first()?.checked_sub(lead).ok();
    };
    for expiry in expiries[..expiries.len() - 1].iter().rev() {
        let rescue_deadline = expiry.checked_sub(lead).ok()?;
        let chain_deadline = deadline.checked_sub(refill).ok()?;
        deadline = rescue_deadline.min(chain_deadline);
    }
    Some(deadline)
}

fn refill_interval(rate_pct_per_day: Option<f64>) -> Option<SignedDuration> {
    let rate = rate_pct_per_day.filter(|rate| rate.is_finite() && *rate >= RATE_FLOOR)?;
    let seconds = (100.0 / rate * SECONDS_PER_DAY)
        .max(T_MIN.as_secs_f64())
        .ceil() as i64;
    Some(SignedDuration::from_secs(seconds))
}

fn free_reset_defers(
    capacity: Option<&ProviderCapacity>,
    first_expiry: Timestamp,
    min_gain: Duration,
    now: Timestamp,
) -> bool {
    let Some(reset) = capacity
        .and_then(|value| value.longest_window_observation(now))
        .and_then(|window| window.resets_at)
        .filter(|reset| *reset > now)
    else {
        return false;
    };
    reset.as_second() - now.as_second() < duration_seconds(min_gain)
        && first_expiry.as_second() - reset.as_second() >= duration_seconds(MIN_HOLD)
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

fn read_rate_stamp(path: &Path) -> Option<RateStamp> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn update_rate_stamp(prior: Option<&RateStamp>, window: &RateLimitWindow) -> Option<RateStamp> {
    let (window_resets_at, last_used_pct, last_observed_at) = (
        window.resets_at?,
        window.used_percentage?,
        window.observed_at?,
    );
    let Some(prior) = prior else {
        return Some(RateStamp {
            window_resets_at,
            last_used_pct,
            last_observed_at,
            rate_pct_per_day: 0.0,
        });
    };
    if last_observed_at <= prior.last_observed_at {
        return Some(prior.clone());
    }

    let mut rate_pct_per_day = prior.rate_pct_per_day.max(0.0);
    if window_resets_at == prior.window_resets_at && last_used_pct >= prior.last_used_pct {
        let elapsed_secs = last_observed_at
            .duration_since(prior.last_observed_at)
            .as_secs_f64();
        let sample_rate =
            f64::from(last_used_pct - prior.last_used_pct) * SECONDS_PER_DAY / elapsed_secs;
        if rate_pct_per_day == 0.0 && elapsed_secs < T_MIN.as_secs_f64() {
            return Some(prior.clone());
        }
        let alpha = 1.0 - 0.5_f64.powf(elapsed_secs / RATE_HALF_LIFE.as_secs_f64());
        rate_pct_per_day = if rate_pct_per_day > 0.0 {
            rate_pct_per_day + alpha * (sample_rate - rate_pct_per_day)
        } else {
            sample_rate
        };
    }
    Some(RateStamp {
        window_resets_at,
        last_used_pct,
        last_observed_at,
        rate_pct_per_day,
    })
}

fn cached_rate(stamp: Option<&RateStamp>) -> Option<f64> {
    stamp
        .map(|stamp| stamp.rate_pct_per_day)
        .filter(|rate| rate.is_finite() && *rate >= RATE_FLOOR)
}

fn update_rate_cache(
    runtime: &RuntimePaths,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) -> Option<f64> {
    let path = runtime.shared_auto_redeem_rate_path(CODEX_KIND);
    let prior = read_rate_stamp(&path);
    let Some(next) = capacity
        .and_then(|value| value.longest_window_observation(now))
        .as_ref()
        .and_then(|window| update_rate_stamp(prior.as_ref(), window))
    else {
        return cached_rate(prior.as_ref());
    };
    if prior.as_ref() != Some(&next)
        && let Err(error) = write_temp_then_rename_cache(&path, &next)
    {
        tracing::debug!(
            tags.operation = "auto_redeem.rate_cache",
            error = &error as &dyn std::error::Error,
            "auto-redeem: failed to publish burn-rate cache",
        );
        return cached_rate(prior.as_ref());
    }
    cached_rate(Some(&next))
}

fn write_stamp(path: &Path, stamp: &RedeemStamp) -> Result<(), AutoRedeemErr> {
    write_temp_then_rename_cache(path, stamp)?;
    Ok(())
}

fn reserve_attempt(
    runtime: &RuntimePaths,
    reason: RedeemReason,
    now: Timestamp,
    request_id: &str,
) -> bool {
    let Some(_guard) = crate::store::lock::WorkspaceLock::try_acquire(
        &runtime.shared_auto_redeem_lock(CODEX_KIND),
    )
    .ok()
    .flatten() else {
        return false;
    };
    let stamp_path = runtime.shared_auto_redeem_path(CODEX_KIND);
    if !stamp_allows_attempt(read_stamp(&stamp_path).as_ref(), now) {
        return false;
    }
    write_stamp(
        &stamp_path,
        &RedeemStamp {
            attempted_at: now,
            request_id: request_id.to_owned(),
            reason,
            outcome: None,
        },
    )
    .is_ok()
}

fn cancel_attempt_reservation(runtime: &RuntimePaths, request_id: &str) {
    let Some(_guard) = crate::store::lock::WorkspaceLock::try_acquire(
        &runtime.shared_auto_redeem_lock(CODEX_KIND),
    )
    .ok()
    .flatten() else {
        return;
    };
    let stamp_path = runtime.shared_auto_redeem_path(CODEX_KIND);
    let owns_reservation = read_stamp(&stamp_path)
        .is_some_and(|stamp| stamp.request_id == request_id && stamp.outcome.is_none());
    if owns_reservation {
        let _ = std::fs::remove_file(stamp_path);
    }
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
    let capacity = ProviderCapacity::read(runtime, CODEX_KIND);
    let rate_pct_per_day = update_rate_cache(runtime, capacity.as_ref(), now);
    let Some(credits) = panel.reset_credits.as_ref() else {
        return;
    };
    let Some(reason) = redeem_verdict(
        capacity.as_ref(),
        credits,
        rate_pct_per_day,
        config.auto_redeem_min_gain(),
        config.auto_redeem,
        now,
    ) else {
        return;
    };

    let request_id = uuid::Uuid::now_v7();
    // A pending reservation deliberately uses the 10-minute attempt cooldown
    // as its dead-helper lease. Redemption is rare and account-scoped, so the
    // conservative backstop is preferable to a second freshness clock.
    if !reserve_attempt(runtime, reason, now, &request_id.to_string()) {
        return;
    }
    if !spawn_auto_redeem(runtime, reason, request_id) {
        cancel_attempt_reservation(runtime, &request_id.to_string());
    }
}

/// Run the provider-specific action behind the hidden helper. Silent no-ops
/// return `None`; once the consume request starts, its evidence and outcome are
/// retained in a report, including on an attempted error.
pub fn execute_auto_redeem(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    requested_reason: RedeemReason,
    request_id: uuid::Uuid,
    config: &ResumeConfig,
) -> Result<Option<RedeemReport>, AutoRedeemErr> {
    if kind.as_str() != CODEX_KIND {
        return Err(AutoRedeemErr::UnsupportedKind(kind.to_string()));
    }
    if crate::agents::credits::oauth_usage_offline() {
        return Ok(None);
    }
    let request_id = request_id.to_string();

    let _guard =
        crate::store::lock::WorkspaceLock::acquire(&runtime.shared_auto_redeem_lock(CODEX_KIND))?;
    let stamp_path = runtime.shared_auto_redeem_path(CODEX_KIND);
    let now = Timestamp::now();
    let prior_stamp = read_stamp(&stamp_path);
    let owns_reservation = prior_stamp
        .as_ref()
        .is_some_and(|stamp| stamp.request_id == request_id && stamp.outcome.is_none());
    if !owns_reservation && !stamp_allows_attempt(prior_stamp.as_ref(), now) {
        return Ok(None);
    }

    let rate_pct_per_day =
        cached_rate(read_rate_stamp(&runtime.shared_auto_redeem_rate_path(CODEX_KIND)).as_ref());
    let action = prepare_reset_credit_redemption(CODEX_KIND, |capacity, credits| {
        redeem_verdict(
            capacity,
            credits,
            rate_pct_per_day,
            config.auto_redeem_min_gain(),
            config.auto_redeem,
            now,
        )
    });
    let action = action.map_err(AutoRedeemErr::Codex)?;
    let Some(action) = action else {
        return Ok(None);
    };
    let reason = action.decision;
    let capacity = action.capacity.clone();
    let credits = action.credits.clone();

    let natural_reset = capacity
        .as_ref()
        .and_then(|capacity| capacity.latest_spent_window_reset(now));
    let mut report = RedeemReport {
        reason,
        credits: credits.count,
        soonest_expiry: credits.soonest_expiry,
        natural_reset,
        outcome: None,
        windows_reset: false,
        window_resets: Vec::new(),
        reset: false,
    };

    let mut stamp = RedeemStamp {
        attempted_at: now,
        request_id: request_id.to_owned(),
        reason,
        outcome: None,
    };
    let action =
        consume_reserved_reset_credit(&stamp_path, &stamp, &report, requested_reason, || {
            action.consume(&request_id)
        })?;
    report.outcome = Some(action.outcome);
    report.windows_reset = action.windows_reset > 0;
    report.reset = action.outcome == RedemptionCode::Reset;
    stamp.outcome = Some(action.outcome.as_str().to_owned());
    write_stamp(&stamp_path, &stamp).map_err(|err| attempted_error(&report, err))?;

    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = CODEX_KIND,
        reason = reason.as_str(),
        outcome = action.outcome.as_str(),
        windows_reset = action.windows_reset,
        "auto-redeem: reset-credit outcome",
    );
    if action.outcome != RedemptionCode::Reset {
        return Ok(Some(report));
    }

    let Some((usage_identity, refreshed)) = action.refreshed else {
        let error = action
            .refresh_error
            .unwrap_or_else(|| "usage refresh returned no snapshot".to_owned());
        return Err(attempted_error(&report, AutoRedeemErr::Codex(error)));
    };
    report.window_resets = refreshed
        .rate_limits
        .as_ref()
        .map(|limits| {
            limits
                .windows
                .iter()
                .filter(|window| window.scope.is_none())
                .map(|window| AssistWindowReset {
                    duration_mins: window.duration_mins.map(u64::from),
                    resets_at: window.resets_at,
                })
                .collect()
        })
        .unwrap_or_default();
    publish_usage(runtime, usage_identity, refreshed);
    Ok(Some(report))
}

fn consume_reserved_reset_credit(
    stamp_path: &Path,
    stamp: &RedeemStamp,
    report: &RedeemReport,
    requested_reason: RedeemReason,
    consume: impl FnOnce() -> Result<ResetCreditResult, String>,
) -> Result<ResetCreditResult, AutoRedeemErr> {
    write_stamp(stamp_path, stamp).map_err(|error| attempted_error(report, error))?;
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = CODEX_KIND,
        requested_reason = requested_reason.as_str(),
        reason = report.reason.as_str(),
        "auto-redeem: consuming reset credit",
    );
    consume().map_err(|message| attempted_error(report, AutoRedeemErr::Codex(message)))
}

fn attempted_error(report: &RedeemReport, error: AutoRedeemErr) -> AutoRedeemErr {
    AutoRedeemErr::Attempted {
        report: Box::new(report.clone()),
        error: error.to_string(),
    }
}

fn publish_usage(
    runtime: &RuntimePaths,
    identity: crate::agents::AccountUsageIdentity,
    snapshot: AccountUsageSnapshot,
) {
    let scope = identity.scope.clone();
    if let Some(windows) = snapshot.rate_limits.clone() {
        crate::sidebar::refresh::merge_account_rate_limits(runtime, CODEX_KIND, identity, windows);
    }
    if snapshot.plan.is_some()
        || snapshot.extra_credits.is_some()
        || snapshot.reset_credits.is_some()
    {
        crate::sidebar::refresh::merge_provider_realtime_usage(
            runtime, CODEX_KIND, scope, snapshot,
        );
    }
}

fn spawn_auto_redeem(runtime: &RuntimePaths, reason: RedeemReason, request_id: uuid::Uuid) -> bool {
    let request = AutoRedeemRequest {
        workspace_id: runtime.workspace_id.clone(),
        kind: AgentKind::new_unchecked(CODEX_KIND),
        reason,
        request_id,
    };
    let args = crate::child_process::agent_helper_argv("auto-redeem", &request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = CODEX_KIND,
        reason = reason.as_str(),
        "sidebar: auto-redeeming reset credit",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-auto-redeem")
    {
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
mod tests;
