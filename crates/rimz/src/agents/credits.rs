//! Provider paid-usage enrichment.
//!
//! Rate-limit windows answer "how much included subscription budget is left".
//! Extra credits answer "how much paid usage beyond those windows is available
//! or has been spent". The source may be a provider account surface, a local
//! API-key spend projection, or a future admin API, so the type keeps each
//! figure optional and lets the renderer state only what is known.

use serde::{Deserialize, Serialize};

use crate::agents::context::AgentRateLimits;

/// `true` when direct provider account-usage fetches are disabled for this
/// process (tests, CI, air-gapped runs).
pub fn oauth_usage_offline() -> bool {
    std::env::var_os("RIMZ_OAUTH_USAGE_OFFLINE").is_some()
}

/// A provider account-usage reading normalized from a local out-of-band source:
/// included subscription windows plus the optional paid extra/API balance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountUsageSnapshot {
    pub rate_limits: Option<AgentRateLimits>,
    pub extra_credits: Option<ExtraCredits>,
}

/// Paid usage beyond the subscription windows: Claude extra usage, Codex
/// credits, or API-key spend against a display ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraCredits {
    /// The provider says this account cannot use extra paid usage.
    Disabled,
    /// The source supplied some combination of usage, remaining balance, and
    /// limit. Missing values stay missing; callers may fill a display-only
    /// ceiling when a real limit is absent.
    Known {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        used_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_usd: Option<f64>,
    },
}

impl ExtraCredits {
    pub fn known(
        used_usd: Option<f64>,
        remaining_usd: Option<f64>,
        limit_usd: Option<f64>,
    ) -> Self {
        Self::Known {
            used_usd: clean_usd(used_usd),
            remaining_usd: clean_usd(remaining_usd),
            limit_usd: clean_usd(limit_usd),
        }
    }

    pub fn with_limit_if_missing(self, fallback_limit_usd: Option<f64>) -> Self {
        match self {
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => Self::Known {
                used_usd,
                remaining_usd,
                limit_usd: limit_usd.or_else(|| clean_usd(fallback_limit_usd)),
            },
            Self::Disabled => Self::Disabled,
        }
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Whether this extra paid budget is known to be exhausted.
    pub fn is_exhausted(&self) -> bool {
        match self {
            Self::Disabled => true,
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => {
                remaining_usd.is_some_and(|remaining| remaining <= 0.0)
                    || used_usd
                        .zip(*limit_usd)
                        .is_some_and(|(used, limit)| limit > 0.0 && used >= limit)
            }
        }
    }

    /// Whether extra paid usage may be available. Unknown values count as usable
    /// because no source has proven the budget is exhausted.
    pub fn is_usable(&self) -> bool {
        !self.is_disabled() && !self.is_exhausted()
    }

    /// Remaining percentage for a draining bar, when enough data is known.
    pub fn remaining_percentage(&self) -> Option<u8> {
        match self {
            Self::Disabled => Some(0),
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => {
                let limit = limit_usd.filter(|limit| *limit > 0.0)?;
                let remaining = if let Some(remaining) = remaining_usd {
                    *remaining
                } else {
                    limit - (*used_usd)?
                };
                Some(((remaining.max(0.0).min(limit) / limit) * 100.0).round() as u8)
            }
        }
    }

    pub fn used_usd(&self) -> Option<f64> {
        match self {
            Self::Known { used_usd, .. } => *used_usd,
            Self::Disabled => None,
        }
    }

    pub fn remaining_usd(&self) -> Option<f64> {
        match self {
            Self::Known { remaining_usd, .. } => *remaining_usd,
            Self::Disabled => Some(0.0),
        }
    }

    pub fn limit_usd(&self) -> Option<f64> {
        match self {
            Self::Known { limit_usd, .. } => *limit_usd,
            Self::Disabled => Some(0.0),
        }
    }
}

fn clean_usd(value: Option<f64>) -> Option<f64> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_credits_exhaustion_and_remaining_percentage() {
        // Exhausted when disabled, when the remaining balance is zero, or when
        // used has reached the limit; unknown values count as still usable.
        assert!(ExtraCredits::Disabled.is_exhausted());
        assert!(ExtraCredits::known(None, Some(0.0), None).is_exhausted());
        assert!(ExtraCredits::known(Some(50.0), None, Some(50.0)).is_exhausted());
        assert!(!ExtraCredits::known(Some(12.0), None, Some(50.0)).is_exhausted());
        assert!(ExtraCredits::known(None, None, None).is_usable());

        // Remaining percentage works from either a used or a remaining figure,
        // and is None when neither pins the balance against the limit.
        assert_eq!(
            ExtraCredits::known(Some(12.0), None, Some(50.0)).remaining_percentage(),
            Some(76)
        );
        assert_eq!(
            ExtraCredits::known(None, Some(7.5), Some(30.0)).remaining_percentage(),
            Some(25)
        );
        assert_eq!(
            ExtraCredits::known(Some(12.0), None, None).remaining_percentage(),
            None
        );
    }
}
