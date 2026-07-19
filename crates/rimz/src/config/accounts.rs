use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::harness::DayCap;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountBudgetConfigError {
    #[error(
        "unknown agent kind in `accounts.budget.{kind}`; remove it because no adapter can publish authoritative account-level dollars"
    )]
    UnknownKind { kind: String },
    #[error(
        "unsupported `accounts.budget.{kind}`; remove it because {kind} has no durable account-spend source with authoritative account-level dollars"
    )]
    Unsupported { kind: String },
}

/// Provider-account enrichment preferences.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AccountsConfig {
    /// Local-calendar-day dollar caps by provider login, shared across rooms.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub budget: BTreeMap<String, DayCap>,
    /// Display-only monthly USD ceiling by provider kind. It scales the
    /// extra/API usage bar when no provider limit is available; it is not a
    /// provider-enforced spending limit.
    pub usage_limit_usd: BTreeMap<String, UsageLimitUsd>,
}

impl AccountsConfig {
    pub fn budget(&self, kind: &str) -> Option<DayCap> {
        self.budget.get(kind).copied()
    }

    pub fn usage_limit(&self, kind: &str) -> Option<f64> {
        self.usage_limit_usd.get(kind).map(|limit| limit.as_usd())
    }

    pub fn validate_budgets(&self) -> Result<(), AccountBudgetConfigError> {
        self.budget
            .keys()
            .try_for_each(|kind| Self::validate_budget_kind(kind))
    }

    pub fn validate_budget_kind(kind: &str) -> Result<(), AccountBudgetConfigError> {
        validate_budget_descriptor(kind, crate::agents::spec_by_kind(kind))
    }
}

fn validate_budget_descriptor(
    kind: &str,
    definition: Option<&crate::agents::AgentSpec>,
) -> Result<(), AccountBudgetConfigError> {
    let definition = definition.ok_or_else(|| AccountBudgetConfigError::UnknownKind {
        kind: kind.to_owned(),
    })?;
    if !definition.has_authoritative_account_spend() {
        return Err(AccountBudgetConfigError::Unsupported {
            kind: kind.to_owned(),
        });
    }
    Ok(())
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

    #[test]
    fn account_day_caps_parse_and_round_trip() {
        let config: AccountsConfig = toml::from_str(
            r#"
            [budget]
            claude = "100/day"
            codex = "$25.50/day"
            "#,
        )
        .expect("parse account budgets");
        assert_eq!(config.budget("claude").map(DayCap::as_usd), Some(100.0));
        assert_eq!(config.budget("codex").map(DayCap::as_usd), Some(25.5));
        let rendered = toml::to_string(&config).expect("serialize accounts");
        assert!(rendered.contains("claude = \"100/day\""), "{rendered}");
        assert!(
            toml::from_str::<AccountsConfig>("[budget]\nclaude = \"100\"")
                .unwrap_err()
                .to_string()
                .contains("must end in `/day`")
        );
    }

    #[test]
    fn account_day_caps_require_wired_authoritative_spend() {
        for kind in ["claude", "codex", "opencode", "pi"] {
            let supported: AccountsConfig =
                toml::from_str(&format!("[budget]\n{kind} = \"100/day\"")).unwrap();
            assert_eq!(supported.validate_budgets(), Ok(()), "{kind}");
        }

        for kind in ["antigravity", "amp", "cursor", "kimi"] {
            let unsupported: AccountsConfig =
                toml::from_str(&format!("[budget]\n{kind} = \"100/day\"")).unwrap();
            assert!(matches!(
                unsupported.validate_budgets(),
                Err(AccountBudgetConfigError::Unsupported { kind: rejected }) if rejected == kind
            ));
        }

        let unknown: AccountsConfig = toml::from_str("[budget]\nfuture = \"100/day\"").unwrap();
        assert!(matches!(
            unknown.validate_budgets(),
            Err(AccountBudgetConfigError::UnknownKind { kind }) if kind == "future"
        ));
    }

    #[test]
    fn cursor_usage_limit_remains_display_only_and_valid() {
        let config: AccountsConfig = toml::from_str("[usage_limit_usd]\ncursor = 20").unwrap();
        assert_eq!(config.validate_budgets(), Ok(()));
        assert_eq!(config.usage_limit("cursor"), Some(20.0));
    }

    #[test]
    fn plugin_declaring_a_spend_probe_is_account_budget_eligible() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("spendbot");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("README.md"), "setup").unwrap();
        std::fs::write(plugin.join("spend"), "").unwrap();
        std::fs::write(
            plugin.join("agent.toml"),
            r#"protocol = 1
kind = "spendbot"
display-name = "Spend Bot"
process-names = ["spendbot"]
emits = ["session_start"]
setup-doc = "README.md"
[probes]
spend = ["./spend"]
"#,
        )
        .unwrap();
        let loaded = crate::agents::plugins::load_from_root(root.path());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let definition = loaded.definitions[0].spec();
        assert!(definition.has_authoritative_account_spend());
        assert_eq!(
            validate_budget_descriptor("spendbot", Some(definition)),
            Ok(())
        );
    }
}
