use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Provider-account enrichment preferences.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AccountsConfig {
    /// Display-only monthly USD ceiling by provider kind. It scales the
    /// extra/API usage bar when no provider limit is available; it is not a
    /// provider-enforced spending limit.
    pub usage_limit_usd: BTreeMap<String, UsageLimitUsd>,
}

impl AccountsConfig {
    pub fn usage_limit(&self, kind: &str) -> Option<f64> {
        self.usage_limit_usd.get(kind).map(|limit| limit.as_usd())
    }
}

/// A USD amount stored as integer cents so config structs keep `Eq`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct UsageLimitUsd {
    cents: u64,
}

impl UsageLimitUsd {
    pub fn as_usd(&self) -> f64 {
        self.cents as f64 / 100.0
    }

    #[cfg(test)]
    pub(crate) fn from_usd(value: f64) -> Self {
        Self {
            cents: ((value.max(0.0) * 100.0).round()) as u64,
        }
    }
}

impl Serialize for UsageLimitUsd {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.as_usd())
    }
}

impl<'de> Deserialize<'de> for UsageLimitUsd {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UsageLimitVisitor)
    }
}

struct UsageLimitVisitor;

impl Visitor<'_> for UsageLimitVisitor {
    type Value = UsageLimitUsd;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a non-negative USD number")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(UsageLimitUsd {
            cents: value.saturating_mul(100),
        })
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value < 0 {
            return Err(E::custom("usage limit must be non-negative"));
        }
        self.visit_u64(value as u64)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if !value.is_finite() || value < 0.0 {
            return Err(E::custom(
                "usage limit must be a finite non-negative number",
            ));
        }
        Ok(UsageLimitUsd {
            cents: (value * 100.0).round() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_limit_keeps_cents_exactly() {
        let config: AccountsConfig = toml::from_str(
            r#"
            [usage_limit_usd]
            claude = 50.25
            codex = 12
            "#,
        )
        .unwrap();
        assert_eq!(config.usage_limit("claude"), Some(50.25));
        assert_eq!(config.usage_limit("codex"), Some(12.0));
    }
}
