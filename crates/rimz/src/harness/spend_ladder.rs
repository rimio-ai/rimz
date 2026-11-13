//! One provider-evidence spend ladder for launch and redemption decisions.

use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;

use crate::RuntimePaths;
use crate::agents::account::ProviderCapacity;
use crate::agents::credits::read_credits_cache;
use crate::agents::{ExtraCredits, ResetCredits};
use crate::config::{ResumeConfig, SpendOrder};
use crate::harness::auto_redeem::{self, RedeemReason, RedeemStamp};
use crate::ids::LoginKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendStep {
    Subscription,
    PaidCredit,
    ResetCredit,
    Spent,
}

#[derive(Clone)]
pub struct SpendFacts {
    pub(super) capacity: Option<ProviderCapacity>,
    pub(super) extra_credits: Option<ExtraCredits>,
    pub(super) reset_credits: Option<ResetCredits>,
    pub(super) rate_pct_per_day: Option<f64>,
    pub(super) last_attempt: Option<RedeemStamp>,
    supports_reset: bool,
}

impl SpendFacts {
    pub(super) fn from_offer(
        key: &LoginKey,
        offer: &crate::agents::account::ResetCreditOffer,
        rate_pct_per_day: Option<f64>,
    ) -> Self {
        Self {
            capacity: offer.capacity.clone(),
            extra_credits: offer.extra_credits.clone(),
            reset_credits: Some(offer.credits.clone()),
            rate_pct_per_day,
            last_attempt: None,
            supports_reset: auto_redeem::supports_kind(&key.kind),
        }
    }

    pub fn read(runtime: &RuntimePaths, key: &LoginKey, now: Timestamp) -> Self {
        let cache = read_credits_cache(&runtime.shared_credits_path());
        let entry = cache.current(key, now);
        Self {
            capacity: ProviderCapacity::read(runtime, key),
            extra_credits: entry.and_then(|entry| entry.extra_credits.clone()),
            reset_credits: entry.and_then(|entry| entry.reset_credits.clone()),
            rate_pct_per_day: auto_redeem::cached_rate(
                auto_redeem::read_rate_stamp(&runtime.shared_auto_redeem_rate_path(key)).as_ref(),
            ),
            last_attempt: auto_redeem::read_stamp(&runtime.shared_auto_redeem_path(key)),
            supports_reset: auto_redeem::supports_kind(&key.kind),
        }
    }

    pub fn step(&self, config: &ResumeConfig, model: Option<&str>, now: Timestamp) -> SpendStep {
        self.step_on(self.verdict(config, now), config, model, now)
    }

    fn step_on(
        &self,
        verdict: Option<RedeemReason>,
        config: &ResumeConfig,
        model: Option<&str>,
        now: Timestamp,
    ) -> SpendStep {
        let Some(capacity) = &self.capacity else {
            return SpendStep::Subscription;
        };
        if capacity.model_exhausted(now, model) {
            return SpendStep::Spent;
        }
        if !capacity.subscription_exhausted_for_model(now, None) {
            return SpendStep::Subscription;
        }
        let paid = self
            .extra_credits
            .as_ref()
            .is_some_and(ExtraCredits::is_usable);
        if paid && config.spend_order == SpendOrder::CreditsFirst {
            return SpendStep::PaidCredit;
        }
        if verdict.is_some()
            && !auto_redeem::failed_attempt_cooling_down(self.last_attempt.as_ref(), now)
        {
            return SpendStep::ResetCredit;
        }
        if paid {
            SpendStep::PaidCredit
        } else {
            SpendStep::Spent
        }
    }

    pub(super) fn redemption(&self, config: &ResumeConfig, now: Timestamp) -> Option<RedeemReason> {
        self.redemption_on(self.verdict(config, now), config, now)
    }

    fn redemption_on(
        &self,
        verdict: Option<RedeemReason>,
        config: &ResumeConfig,
        now: Timestamp,
    ) -> Option<RedeemReason> {
        let reason = verdict?;
        (reason == RedeemReason::ExpiryRescue
            || self.step_on(Some(reason), config, None, now) == SpendStep::ResetCredit)
            .then_some(reason)
    }

    /// The redemption this ladder would make were auto-redeem on and the
    /// provider served by it, with a credit rescued `expiry_lead` before it
    /// expires: the advice for a login whose credits are the user's to redeem.
    /// Nothing that routes a launch or redeems reads it, so `step` and
    /// `redemption` answer as before.
    pub(super) fn advice(
        &self,
        config: &ResumeConfig,
        expiry_lead: Duration,
        now: Timestamp,
    ) -> Option<RedeemReason> {
        let verdict = auto_redeem::redeem_verdict(
            self.capacity.as_ref(),
            self.reset_credits.as_ref()?,
            self.rate_pct_per_day,
            config.auto_redeem_min_gain(),
            true,
            expiry_lead,
            now,
        );
        self.redemption_on(verdict, config, now)
    }

    /// Whether this provider is served by automatic redemption.
    pub(super) fn supports_reset(&self) -> bool {
        self.supports_reset
    }

    fn verdict(&self, config: &ResumeConfig, now: Timestamp) -> Option<RedeemReason> {
        if !self.supports_reset {
            return None;
        }
        auto_redeem::redeem_verdict(
            self.capacity.as_ref(),
            self.reset_credits.as_ref()?,
            self.rate_pct_per_day,
            config.auto_redeem_min_gain(),
            config.auto_redeem,
            auto_redeem::EXPIRY_RESCUE_LEAD,
            now,
        )
    }
}

#[cfg(test)]
mod tests;
