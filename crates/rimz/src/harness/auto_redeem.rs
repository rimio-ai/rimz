//! Producer-side Codex reset-credit policy and its detached helper action.
//!
//! The room host evaluates cached provider-neutral capacity and credit
//! state, gated on an agent of this room stopped on the login's limit, then
//! spawns a hidden CLI helper only when a redemption is useful. The verdict
//! branches on what a redemption does to the window's reset (the credit's
//! `RedeemEffect`), never on the provider. The helper serializes account-wide
//! attempts, refreshes both inputs, re-evaluates the same pure verdict with the
//! producer's limit-paused evidence, and performs the provider-specific consume
//! request.
//! An idle account, one the machine declares and no agent of this room runs
//! on, gets the expiry rescue alone: the producer reads its cached credits
//! from its caller, the helper resolves its login from the machine catalog,
//! and the helper skips a window its fresh read shows unused.
//! Elected-producer and one-shot heavy refreshes may both advance the shared
//! burn-rate cache; atomic replacement plus observation stamps make duplicate
//! folds idempotent.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::account::{
    PreparedRedemption, ProviderCapacity, RedeemHold, RedemptionCode, ResetCreditResult,
    WindowSpan, prepare_reset_credit_redemption,
};
use crate::agents::{
    AccountUsageIdentity, AccountUsageSnapshot, AgentState, ProviderLogin, RateLimitWindow,
    RedeemEffect, ResetCredits, RoomLoginSet,
};
use crate::config::ResumeConfig;
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::harness::assist_log::AssistWindowReset;
use crate::ids::{AgentKind, LoginKey, WorkspaceId};
use crate::store::snapshot::{RedeemForecast, SidebarProviderPanel, SidebarSnapshot};

const CODEX_KIND: &str = "codex";
const EXPIRY_RESCUE_LEAD: Duration = Duration::from_secs(30 * 60);
const MIN_HOLD: Duration = Duration::from_secs(24 * 60 * 60);
const ATTEMPT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const POST_SUCCESS_COOLDOWN: Duration = Duration::from_secs(30 * 60);
const RATE_HALF_LIFE: Duration = Duration::from_secs(3 * 24 * 60 * 60);
const RATE_FLOOR: f64 = 0.5;
const T_MIN: Duration = Duration::from_secs(6 * 60 * 60);
const SECONDS_PER_DAY: f64 = 24.0 * 60.0 * 60.0;

/// The rule a redemption fired under, persisted in assist records and stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedeemReason {
    /// A credit deliberately spent by the user.
    Manual,
    /// A credit within 30 minutes of expiry, with or without the opt-in. The
    /// one rule an idle account is evaluated for, and the helper drops it
    /// there when its fresh read shows the window unused.
    ExpiryRescue,
    /// A spent window that parked an agent resets at least `min_gain` away.
    BlockedGain,
    /// A credit that keeps the window's schedule would die within a day of the
    /// spent window's reset. Never fired for a credit that restarts the window.
    DoomedCredit,
    /// The paced chain deadline passed while a spent window parked an agent.
    ScheduledRedeem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoRedeemRequest {
    pub workspace_id: WorkspaceId,
    pub login: LoginKey,
    pub reason: RedeemReason,
    pub request_id: uuid::Uuid,
    /// Whether the producer saw an agent of this room stopped on this login's
    /// limit. A payload without it reads as not paused, so only a rescue fires.
    #[serde(default)]
    pub limit_paused: bool,
}

/// Whether auto-redeem has anything to act on here: the Codex CLI, the only
/// provider with automated redemption, is installed on this machine.
pub fn provider_located() -> bool {
    crate::agents::spec_by_kind(CODEX_KIND)
        .is_some_and(|spec| crate::agents::locate_binary(spec).is_some())
}

/// Whether this provider is served by the automatic redemption path.
pub fn supports_kind(kind: &AgentKind) -> bool {
    kind.as_str() == CODEX_KIND
}

impl RedeemReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
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
    #[error(
        "another redemption or auto-redeem attempt landed during the preview, or auto-redeem has a pending attempt; rerun `rimz accounts redeem`"
    )]
    RacingAttempt,
    #[error("locking the shared auto-redeem attempt: {0}")]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error("writing the shared auto-redeem stamp: {0}")]
    Stamp(#[from] crate::disk::atomic::AtomicErr),
    #[error("reset-credit request failed: {0}")]
    Provider(String),
    #[error("reset-credit redemption held: {}", .0.reason)]
    Held(RedeemHold),
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
}

/// A redemption result; the caller publishes `usage` before appending the report.
pub struct Redeemed {
    pub report: RedeemReport,
    pub usage: Option<(AccountUsageIdentity, AccountUsageSnapshot)>,
    /// A post-reset refresh failure: manual callers warn; auto-redeem returns an attempted error.
    pub refresh_error: Option<String>,
}

/// A manual redemption read from the provider, armed to consume.
pub struct ManualRedeem {
    pub credits: ResetCredits,
    /// The unscoped 5h and 7d windows projected to the read time.
    pub windows: Vec<RateLimitWindow>,
    pub natural_reset: Option<Timestamp>,
    pub forecast: Option<RedeemForecast>,
    pub min_gain: Duration,
    prepared: PreparedRedemption<RedeemReason>,
    stamp_at_read: Option<RedeemStamp>,
}

/// Read one manual preview without reserving or locking the account.
pub fn prepare_manual_redeem(
    runtime: &RuntimePaths,
    key: &LoginKey,
    login_env: &BTreeMap<String, String>,
    config: &ResumeConfig,
) -> Result<ManualRedeem, AutoRedeemErr> {
    let stamp_at_read = read_stamp(&runtime.shared_auto_redeem_path(key));
    let read_at = Timestamp::now();
    let prepared = prepare_reset_credit_redemption(
        key.kind.as_str(),
        |_, _| Some(RedeemReason::Manual),
        login_env,
    )
    .map_err(AutoRedeemErr::Provider)?
    // The unconditional manual verdict cannot decline an offered action.
    .expect("the manual verdict always returns Some");
    let capacity = prepared.capacity.as_ref();
    let credits = prepared.credits.clone();
    let min_gain = config.auto_redeem_min_gain();
    let rate = cached_rate(read_rate_stamp(&runtime.shared_auto_redeem_rate_path(key)).as_ref());
    let windows = [WindowSpan::FiveHour, WindowSpan::SevenDay]
        .into_iter()
        .filter_map(|span| capacity?.window_of_span(span, read_at))
        .collect();
    Ok(ManualRedeem {
        natural_reset: capacity.and_then(|capacity| capacity.latest_spent_window_reset(read_at)),
        forecast: supports_kind(&key.kind)
            .then(|| {
                redeem_forecast(
                    capacity,
                    &credits,
                    rate,
                    min_gain,
                    config.auto_redeem,
                    read_at,
                )
            })
            .flatten(),
        credits,
        windows,
        min_gain,
        prepared,
        stamp_at_read,
    })
}

impl ManualRedeem {
    pub fn hold(&self) -> Option<&RedeemHold> {
        self.prepared.hold()
    }

    /// Lock, apply the stamp race rule, reserve, consume one credit, stamp the outcome.
    pub fn consume(
        self,
        runtime: &RuntimePaths,
        key: &LoginKey,
        request_id: uuid::Uuid,
    ) -> Result<Redeemed, AutoRedeemErr> {
        if let Some(hold) = self.hold() {
            return Err(AutoRedeemErr::Held(hold.clone()));
        }
        let _guard =
            crate::disk::lock::WorkspaceLock::acquire(&runtime.shared_auto_redeem_lock(key))?;
        let report = redemption_report(RedeemReason::Manual, &self.credits, self.natural_reset);
        let request_id = request_id.to_string();
        consume_manual_redemption(
            runtime,
            key,
            report,
            self.stamp_at_read.as_ref(),
            Timestamp::now(),
            &request_id,
            || self.prepared.consume(&request_id),
        )
    }
}

fn consume_manual_redemption(
    runtime: &RuntimePaths,
    key: &LoginKey,
    report: RedeemReport,
    stamp_at_read: Option<&RedeemStamp>,
    now: Timestamp,
    request_id: &str,
    consume: impl FnOnce() -> Result<ResetCreditResult, String>,
) -> Result<Redeemed, AutoRedeemErr> {
    let stamp_path = runtime.shared_auto_redeem_path(key);
    let stamp = read_stamp(&stamp_path);
    let live_reservation = stamp.as_ref().is_some_and(|stamp| {
        stamp.outcome.is_none()
            && now.as_second() - stamp.attempted_at.as_second() < duration_seconds(ATTEMPT_COOLDOWN)
    });
    if stamp.as_ref() != stamp_at_read || live_reservation {
        return Err(AutoRedeemErr::RacingAttempt);
    }
    finish_redemption(
        key,
        &stamp_path,
        RedeemStamp {
            attempted_at: now,
            request_id: request_id.to_owned(),
            reason: RedeemReason::Manual,
            outcome: None,
        },
        report,
        RedeemReason::Manual,
        consume,
    )
}

/// Decide whether current provider-neutral capacity and reset credits warrant
/// one consume attempt. Expiry rescue is unconditional; every other reason
/// needs the user's opt-in and the gate: a spent window with a future reset
/// and an agent of this room stopped on it. What follows depends on what a
/// redemption does to the window's reset.
fn redeem_verdict(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
    min_gain: Duration,
    auto_redeem: bool,
    limit_paused: bool,
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
    if !auto_redeem || !limit_paused {
        return None;
    }
    let natural_reset = capacity?.latest_spent_window_reset(now)?;
    let blocked_gain = natural_reset.as_second() - now.as_second() >= duration_seconds(min_gain);

    match credits.effect {
        RedeemEffect::RestartsWindow if blocked_gain => Some(RedeemReason::BlockedGain),
        RedeemEffect::RestartsWindow => {
            scheduled_redeem(capacity, credits, rate_pct_per_day, min_gain, now)
        }
        RedeemEffect::KeepsSchedule
            if credits.soonest_expiry.is_some_and(|expiry| {
                expiry.as_second() - natural_reset.as_second() < duration_seconds(MIN_HOLD)
            }) =>
        {
            Some(RedeemReason::DoomedCredit)
        }
        RedeemEffect::KeepsSchedule => blocked_gain.then_some(RedeemReason::BlockedGain),
    }
}

/// The verdict for an idle account, one no agent of this room runs on: the
/// expiry rescue alone.
fn idle_rescue(credits: &ResetCredits, now: Timestamp) -> Option<RedeemReason> {
    redeem_verdict(None, credits, None, Duration::ZERO, false, false, now)
}

/// The helper's verdict on its fresh provider read. An idle account is judged
/// as one whatever evidence the request carried, and is not rescued when its
/// window is known to be unused, where a redemption would only start a clock
/// nobody is spending. Only this fresh read may skip: the producer's cached
/// windows can predate usage from another machine. Unknown capacity is not
/// evidence, so the rescue fires.
fn fresh_verdict(
    idle: bool,
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
    config: &ResumeConfig,
    limit_paused: bool,
    now: Timestamp,
) -> Option<RedeemReason> {
    if idle {
        if capacity.is_some_and(|capacity| capacity.known_unused(now)) {
            return None;
        }
        return idle_rescue(credits, now);
    }
    redeem_verdict(
        capacity,
        credits,
        rate_pct_per_day,
        config.auto_redeem_min_gain(),
        config.auto_redeem,
        limit_paused,
        now,
    )
}

/// The paced chain for a credit whose redemption restarts the window: spend
/// the next credit once its deadline has passed, unless the free reset is near
/// and the credit comfortably outlives it.
fn scheduled_redeem(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
    min_gain: Duration,
    now: Timestamp,
) -> Option<RedeemReason> {
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

/// Forecast what the verdict would do if the longest duration window ran dry
/// now and parked an agent of this room, so the hypothetical opens the gate on
/// its own and reads no live agent (`rimz providers` has none to read).
/// Holding is a `None` verdict on that hypothetical; without a known natural
/// reset there is no hold to prove, so auto mode reads armed.
fn redeem_forecast(
    capacity: Option<&ProviderCapacity>,
    credits: &ResetCredits,
    rate_pct_per_day: Option<f64>,
    min_gain: Duration,
    auto_redeem: bool,
    now: Timestamp,
) -> Option<RedeemForecast> {
    if credits.count == 0 {
        return None;
    }
    if !auto_redeem {
        return Some(RedeemForecast::Manual);
    }
    let Some((capacity, longest)) = capacity.and_then(|capacity| {
        let longest = capacity
            .longest_window_observation(now)
            .filter(|window| window.resets_at.is_some())?;
        Some((capacity, longest))
    }) else {
        return Some(RedeemForecast::Armed);
    };
    let mut dry = capacity.clone();
    dry.windows = capacity
        .projected_windows(now)
        .map(|mut window| {
            if window.scope.is_none() && window.duration_mins == longest.duration_mins {
                window.used_percentage = Some(100);
            }
            window
        })
        .collect();
    Some(
        match redeem_verdict(
            Some(&dry),
            credits,
            rate_pct_per_day,
            min_gain,
            true,
            true,
            now,
        ) {
            Some(_) => RedeemForecast::Armed,
            None => RedeemForecast::Holding,
        },
    )
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
    key: &LoginKey,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) -> Option<f64> {
    let path = runtime.shared_auto_redeem_rate_path(key);
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
    key: &LoginKey,
    reason: RedeemReason,
    now: Timestamp,
    request_id: &str,
) -> bool {
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_auto_redeem_lock(key))
            .ok()
            .flatten()
    else {
        return false;
    };
    let stamp_path = runtime.shared_auto_redeem_path(key);
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

fn cancel_attempt_reservation(runtime: &RuntimePaths, key: &LoginKey, request_id: &str) {
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_auto_redeem_lock(key))
            .ok()
            .flatten()
    else {
        return;
    };
    let stamp_path = runtime.shared_auto_redeem_path(key);
    let owns_reservation = read_stamp(&stamp_path)
        .is_some_and(|stamp| stamp.request_id == request_id && stamp.outcome.is_none());
    if owns_reservation {
        let _ = std::fs::remove_file(stamp_path);
    }
}

/// Whether a live root row of this room is stopped on `key`'s provider limit.
/// A RimZ dollar-cap park is not a provider limit, so it never counts.
fn limit_paused_on(agents: &[AgentState], key: &LoginKey) -> bool {
    agents.iter().any(|agent| {
        agent.ended_at.is_none()
            && !agent.is_provider_subagent()
            && agent.budget_park.is_none()
            && agent.login_key() == *key
            && crate::harness::auto_continue::limit_marker_active(agent)
    })
}

/// The idle accounts: every Codex login the machine declares, `default`
/// included, that no default or live agent of this room uses. An account
/// config that did not load declares none.
pub(crate) fn idle_logins(logins: &RoomLoginSet) -> Vec<ProviderLogin> {
    let in_use = logins.in_use(CODEX_KIND);
    logins
        .declared(CODEX_KIND)
        .into_iter()
        .filter(|login| in_use.iter().all(|used| used.key() != login.key()))
        .collect()
}

/// Evaluate the Codex panel of each in-use login, then the cached credits of
/// each idle account, and spawn the account-wide helper when due.
/// Only Codex supports automated redemption; keep that provider choice here
/// while the verdict above remains provider-neutral.
pub(crate) fn redeem_credits(
    panels: &[SidebarProviderPanel],
    idle_credits: &BTreeMap<LoginKey, ResetCredits>,
    agents: &[AgentState],
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
    config: &ResumeConfig,
    now: Timestamp,
) {
    redeem_credits_with(
        panels,
        idle_credits,
        agents,
        runtime,
        logins,
        config,
        now,
        spawn_auto_redeem,
    );
}

#[allow(clippy::too_many_arguments)]
fn redeem_credits_with(
    panels: &[SidebarProviderPanel],
    idle_credits: &BTreeMap<LoginKey, ResetCredits>,
    agents: &[AgentState],
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
    config: &ResumeConfig,
    now: Timestamp,
    mut spawn: impl FnMut(&RuntimePaths, &LoginKey, RedeemReason, uuid::Uuid, bool) -> bool,
) {
    for login in logins.in_use(CODEX_KIND) {
        let key = login.key();
        let Some(panel) = panels.iter().find(|panel| panel.login_key() == key) else {
            continue;
        };
        let capacity = ProviderCapacity::read(runtime, &key);
        let rate_pct_per_day = update_rate_cache(runtime, &key, capacity.as_ref(), now);
        let Some(credits) = panel.reset_credits.as_ref() else {
            continue;
        };
        let limit_paused = limit_paused_on(agents, &key);
        let Some(reason) = redeem_verdict(
            capacity.as_ref(),
            credits,
            rate_pct_per_day,
            config.auto_redeem_min_gain(),
            config.auto_redeem,
            limit_paused,
            now,
        ) else {
            continue;
        };
        reserve_and_spawn(runtime, &key, reason, limit_paused, now, &mut spawn);
    }
    for (key, credits) in idle_credits {
        if let Some(reason) = idle_rescue(credits, now) {
            reserve_and_spawn(runtime, key, reason, false, now, &mut spawn);
        }
    }
}

fn reserve_and_spawn(
    runtime: &RuntimePaths,
    key: &LoginKey,
    reason: RedeemReason,
    limit_paused: bool,
    now: Timestamp,
    spawn: &mut impl FnMut(&RuntimePaths, &LoginKey, RedeemReason, uuid::Uuid, bool) -> bool,
) {
    let request_id = uuid::Uuid::now_v7();
    // A pending reservation deliberately uses the 10-minute attempt cooldown
    // as its dead-helper lease. Redemption is rare and account-scoped, so the
    // conservative backstop is preferable to a second freshness clock.
    if !reserve_attempt(runtime, key, reason, now, &request_id.to_string()) {
        return;
    }
    if !spawn(runtime, key, reason, request_id, limit_paused) {
        cancel_attempt_reservation(runtime, key, &request_id.to_string());
    }
}

/// Project the auto-redeem forecast onto the Codex panel, reading the same
/// capacity and burn-rate caches `redeem_credits` evaluates without writing
/// either. Only a panel that carries reset credits gets a forecast.
pub(crate) fn project_redeem_forecasts(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    logins: &RoomLoginSet,
) {
    let now = snapshot.now;
    let in_use = logins.keys_in_use();
    for panel in &mut snapshot.providers {
        panel.redeem_forecast = None;
        if panel.kind != CODEX_KIND {
            continue;
        }
        let (Some(credits), Some(key)) = (
            panel.reset_credits.as_ref(),
            in_use
                .contains(&panel.login_key())
                .then(|| panel.login_key()),
        ) else {
            continue;
        };
        let capacity = ProviderCapacity::read(runtime, &key);
        let rate_pct_per_day =
            cached_rate(read_rate_stamp(&runtime.shared_auto_redeem_rate_path(&key)).as_ref());
        panel.redeem_forecast = redeem_forecast(
            capacity.as_ref(),
            credits,
            rate_pct_per_day,
            config.auto_redeem_min_gain(),
            config.auto_redeem,
            now,
        );
    }
}

/// Run the provider-specific action behind the hidden helper. Silent no-ops
/// return `None`; once the consume request starts, its evidence and outcome are
/// retained in a report, including on an attempted error.
pub fn execute_auto_redeem(
    runtime: &RuntimePaths,
    key: &LoginKey,
    requested_reason: RedeemReason,
    request_id: uuid::Uuid,
    limit_paused: bool,
    config: &ResumeConfig,
) -> Result<Option<Redeemed>, AutoRedeemErr> {
    if key.kind.as_str() != CODEX_KIND {
        return Err(AutoRedeemErr::UnsupportedKind(key.kind.to_string()));
    }
    let logins = crate::store::room_logins_in_use(runtime);
    let Some((login, idle)) = redeem_login(runtime, key, &request_id.to_string(), &logins) else {
        return Ok(None);
    };
    if crate::agents::credits::oauth_usage_offline() {
        return Ok(None);
    }
    let request_id = request_id.to_string();

    let _guard = crate::disk::lock::WorkspaceLock::acquire(&runtime.shared_auto_redeem_lock(key))?;
    let stamp_path = runtime.shared_auto_redeem_path(key);
    let now = Timestamp::now();
    let prior_stamp = read_stamp(&stamp_path);
    let owns_reservation = prior_stamp
        .as_ref()
        .is_some_and(|stamp| stamp.request_id == request_id && stamp.outcome.is_none());
    if !owns_reservation && !stamp_allows_attempt(prior_stamp.as_ref(), now) {
        return Ok(None);
    }

    let rate_pct_per_day =
        cached_rate(read_rate_stamp(&runtime.shared_auto_redeem_rate_path(key)).as_ref());
    let action = prepare_reset_credit_redemption(
        CODEX_KIND,
        |capacity, credits| {
            fresh_verdict(
                idle,
                capacity,
                credits,
                rate_pct_per_day,
                config,
                limit_paused,
                now,
            )
        },
        &logins.env(&login),
    );
    let action = action.map_err(AutoRedeemErr::Provider)?;
    let Some(action) = action else {
        return Ok(None);
    };
    let natural_reset = action
        .capacity
        .as_ref()
        .and_then(|capacity| capacity.latest_spent_window_reset(now));
    let report = redemption_report(action.decision, &action.credits, natural_reset);
    let redeemed = finish_redemption(
        key,
        &stamp_path,
        RedeemStamp {
            attempted_at: now,
            request_id: request_id.to_owned(),
            reason: action.decision,
            outcome: None,
        },
        report,
        requested_reason,
        || action.consume(&request_id),
    )?;
    if let Some(error) = &redeemed.refresh_error {
        return Err(attempted_error(
            &redeemed.report,
            AutoRedeemErr::Provider(error.clone()),
        ));
    }
    Ok(Some(redeemed))
}

fn redemption_report(
    reason: RedeemReason,
    credits: &ResetCredits,
    natural_reset: Option<Timestamp>,
) -> RedeemReport {
    RedeemReport {
        reason,
        credits: credits.count,
        soonest_expiry: credits.soonest_expiry,
        natural_reset,
        outcome: None,
        windows_reset: false,
        window_resets: Vec::new(),
    }
}

fn finish_redemption(
    key: &LoginKey,
    stamp_path: &Path,
    mut stamp: RedeemStamp,
    mut report: RedeemReport,
    requested_reason: RedeemReason,
    consume: impl FnOnce() -> Result<ResetCreditResult, String>,
) -> Result<Redeemed, AutoRedeemErr> {
    let action =
        consume_reserved_reset_credit(key, stamp_path, &stamp, &report, requested_reason, consume)?;
    report.outcome = Some(action.outcome);
    report.windows_reset = action.windows_reset > 0;
    stamp.outcome = Some(action.outcome.as_str().to_owned());
    write_stamp(stamp_path, &stamp).map_err(|err| attempted_error(&report, err))?;

    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = key.kind.as_str(),
        reason = report.reason.as_str(),
        outcome = action.outcome.as_str(),
        windows_reset = action.windows_reset,
        "auto-redeem: reset-credit outcome",
    );
    if action.outcome != RedemptionCode::Reset {
        return Ok(Redeemed {
            report,
            usage: None,
            refresh_error: None,
        });
    }

    let Some((usage_identity, refreshed)) = action.refreshed else {
        let error = action
            .refresh_error
            .unwrap_or_else(|| "usage refresh returned no snapshot".to_owned());
        return Ok(Redeemed {
            report,
            usage: None,
            refresh_error: Some(error),
        });
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
    Ok(Redeemed {
        report,
        usage: Some((usage_identity, refreshed)),
        refresh_error: None,
    })
}

/// The login the helper redeems under, and whether it is idle. A login this
/// room uses is read from the room; one it does not, from the machine catalog.
/// A key neither holds cancels the reservation.
fn redeem_login(
    runtime: &RuntimePaths,
    key: &LoginKey,
    request_id: &str,
    logins: &RoomLoginSet,
) -> Option<(ProviderLogin, bool)> {
    let is_key = |login: &ProviderLogin| login.key() == *key;
    let login = logins
        .in_use(CODEX_KIND)
        .into_iter()
        .find(is_key)
        .map(|login| (login, false))
        .or_else(|| {
            let idle = idle_logins(logins).into_iter().find(is_key)?;
            Some((idle, true))
        });
    if login.is_none() {
        cancel_attempt_reservation(runtime, key, request_id);
        tracing::debug!(
            kind = key.kind.as_str(),
            outcome = "login_undeclared",
            "auto-redeem: login neither in use nor declared"
        );
    }
    login
}

fn consume_reserved_reset_credit(
    key: &LoginKey,
    stamp_path: &Path,
    stamp: &RedeemStamp,
    report: &RedeemReport,
    requested_reason: RedeemReason,
    consume: impl FnOnce() -> Result<ResetCreditResult, String>,
) -> Result<ResetCreditResult, AutoRedeemErr> {
    write_stamp(stamp_path, stamp).map_err(|error| attempted_error(report, error))?;
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind = key.kind.as_str(),
        requested_reason = requested_reason.as_str(),
        reason = report.reason.as_str(),
        "auto-redeem: consuming reset credit",
    );
    consume().map_err(|message| attempted_error(report, AutoRedeemErr::Provider(message)))
}

fn attempted_error(report: &RedeemReport, error: AutoRedeemErr) -> AutoRedeemErr {
    AutoRedeemErr::Attempted {
        report: Box::new(report.clone()),
        error: error.to_string(),
    }
}

fn spawn_auto_redeem(
    runtime: &RuntimePaths,
    key: &LoginKey,
    reason: RedeemReason,
    request_id: uuid::Uuid,
    limit_paused: bool,
) -> bool {
    let request = AutoRedeemRequest {
        workspace_id: runtime.workspace_id.clone(),
        login: key.clone(),
        reason,
        request_id,
        limit_paused,
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
