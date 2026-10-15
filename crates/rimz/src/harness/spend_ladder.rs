//! One provider-evidence spend ladder for launch and redemption decisions.

use jiff::Timestamp;
use serde::Serialize;

use crate::RuntimePaths;
use crate::agents::account::ProviderCapacity;
use crate::agents::credits::{entry_is_displayable, read_credits_cache};
use crate::agents::{Entitlement, ExtraCredits, ResetCredits};
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
        let entry = cache.logins.get(key).filter(|entry| {
            entry_is_displayable(entry, now.as_millisecond().max(0) as u64)
                && entry.scope.is_kind_wide()
                && !matches!(entry.entitlement, Entitlement::Lapsed { .. })
        });
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
        if self.verdict(config, now).is_some()
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
        let reason = self.verdict(config, now)?;
        (reason == RedeemReason::ExpiryRescue
            || self.step(config, None, now) == SpendStep::ResetCredit)
            .then_some(reason)
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
            now,
        )
    }
}

#[cfg(test)]
mod tests;
