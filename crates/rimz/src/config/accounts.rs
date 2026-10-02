use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::harness::DayCap;
use crate::ids::{AgentKind, LoginName, RoomLogins};

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
    /// Machine account selections for new rooms, below explicit and project selections.
    #[serde(rename = "use", skip_serializing_if = "BTreeMap::is_empty")]
    pub use_accounts: RoomLogins,
    /// Local-calendar-day dollar caps by provider login, shared across rooms.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub budget: BTreeMap<String, DayCap>,
    /// Display-only monthly USD ceiling by provider kind. It scales the
    /// extra/API usage bar when no provider limit is available; it is not a
    /// provider-enforced spending limit.
    pub usage_limit_usd: BTreeMap<String, UsageLimitUsd>,
    /// Named Claude accounts: each one a standalone `CLAUDE_CONFIG_DIR`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claude: BTreeMap<LoginName, NamedAccount>,
    /// Named Codex accounts: each one a standalone `CODEX_HOME`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub codex: BTreeMap<LoginName, NamedAccount>,
}

/// One declared account of a provider kind. An empty table takes the default
/// home under the RimZ data root.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NamedAccount {
    /// The provider home this account launches into. `None` means the default
    /// location RimZ derives from the kind and the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<PathBuf>,
    /// Absent means shared; only `standalone` is written back.
    #[serde(default, skip_serializing_if = "AccountHistory::is_shared")]
    pub history: AccountHistory,
}

/// Whether a named account keeps its provider history to itself.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountHistory {
    /// Everything but credentials is the default provider home's.
    #[default]
    Shared,
    /// The account home holds its own sessions and transcripts.
    Standalone,
}

impl AccountHistory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Standalone => "standalone",
        }
    }

    fn is_shared(&self) -> bool {
        *self == Self::Shared
    }
}

impl std::str::FromStr for AccountHistory {
    type Err = de::value::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::deserialize(de::value::StrDeserializer::new(value))
    }
}

impl AccountsConfig {
    pub fn budget(&self, kind: &str) -> Option<DayCap> {
        self.budget.get(kind).copied()
    }

    /// The declared accounts of a kind, or `None` for a kind that cannot carry
    /// named accounts. This and [`Self::named_mut`] are the only places the
    /// supported kinds are listed.
    pub fn named(&self, kind: &AgentKind) -> Option<&BTreeMap<LoginName, NamedAccount>> {
        match kind.as_str() {
            "claude" => Some(&self.claude),
            "codex" => Some(&self.codex),
            _ => None,
        }
    }

    pub fn named_mut(
        &mut self,
        kind: &AgentKind,
    ) -> Option<&mut BTreeMap<LoginName, NamedAccount>> {
        match kind.as_str() {
            "claude" => Some(&mut self.claude),
            "codex" => Some(&mut self.codex),
            _ => None,
        }
    }

    pub(crate) fn usage_limit(&self, kind: &str) -> Option<f64> {
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
    fn as_usd(&self) -> f64 {
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
    fn named_accounts_parse_beside_the_kind_keyed_tables() {
        let config: AccountsConfig = toml::from_str(
            r#"
            [budget]
            claude = "100/day"

            [use]
            codex = "personal"

            [usage_limit_usd]
            codex = 25

            [claude.work]
            home = "/srv/homes/work"

            [codex.personal]
            "#,
        )
        .expect("parse named accounts");
        let claude = config.named(&AgentKind::new_unchecked("claude")).unwrap();
        assert_eq!(claude["work"].home, Some(PathBuf::from("/srv/homes/work")));
        let codex = config.named(&AgentKind::new_unchecked("codex")).unwrap();
        assert_eq!(codex["personal"].home, None);
        assert!(config.named(&AgentKind::new_unchecked("grok")).is_none());
        assert_eq!(config.budget("claude").map(DayCap::as_usd), Some(100.0));
        let serialized = toml::Value::try_from(&config).unwrap();
        assert_eq!(
            serialized
                .get("use")
                .and_then(|v| v.get("codex"))
                .and_then(toml::Value::as_str),
            Some("personal")
        );
    }

    #[test]
    fn machine_selection_allows_undeclared_names_at_strict_load() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, "[accounts.use]\ncodex = \"missing\"\n").unwrap();
        let config = crate::config::MachineConfig::load_from(&path, root.path()).unwrap();
        let serialized = toml::Value::try_from(&config.accounts).unwrap();
        assert_eq!(
            serialized
                .get("use")
                .and_then(|v| v.get("codex"))
                .and_then(toml::Value::as_str),
            Some("missing")
        );
    }

    #[test]
    fn named_account_refuses_an_unknown_field_and_a_bad_name() {
        assert!(
            toml::from_str::<AccountsConfig>("[claude.work]\npath = \"/srv/work\"")
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
        assert!(toml::from_str::<AccountsConfig>("[claude.Work]\n").is_err());
    }

    #[test]
    fn account_history_is_shared_unless_declared_standalone() {
        let config: AccountsConfig = toml::from_str(
            "[claude.work]\n[claude.solo]\nhistory = \"standalone\"\n[codex.team]\nhistory = \"shared\"\n",
        )
        .expect("parse account history");
        assert_eq!(config.claude["work"].history, AccountHistory::Shared);
        assert_eq!(config.claude["solo"].history, AccountHistory::Standalone);
        assert_eq!(config.codex["team"].history, AccountHistory::Shared);
        let rendered = toml::to_string(&config).expect("serialize accounts");
        assert_eq!(rendered.matches("history").count(), 1, "{rendered}");
        assert!(rendered.contains("history = \"standalone\""), "{rendered}");
    }

    #[test]
    fn account_history_refuses_an_unknown_value_at_strict_load() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, "[accounts.claude.work]\nhistory = \"mine\"\n").unwrap();
        let error = crate::config::MachineConfig::load_from(&path, root.path())
            .expect_err("an unknown history value");
        let error = format!("{:#}", anyhow::Error::from(error));
        assert!(
            error.contains("`shared`") && error.contains("`standalone`"),
            "{error}"
        );
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
