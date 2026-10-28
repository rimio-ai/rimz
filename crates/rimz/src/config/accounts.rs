use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use unicode_width::UnicodeWidthStr;

use crate::agents::BudgetWindow;
use crate::harness::budget::BudgetSpec;
use crate::ids::{AgentKind, LoginKey, LoginName, RoomLogins};

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
    #[error(
        "`accounts.{kind}.{name}.budget` needs `history = \"standalone\"`: a shared account's spend is counted with {kind}@default; use `[accounts.budget].{kind}` for a shared-pool cap"
    )]
    SharedHistory { kind: String, name: LoginName },
}

/// An account dollar cap, measured over a local day or a UTC calendar month.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountCap {
    cents: u64,
    window: BudgetWindow,
}

impl AccountCap {
    pub(crate) fn as_usd(self) -> f64 {
        self.cents as f64 / 100.0
    }

    pub fn as_spec(self) -> BudgetSpec {
        BudgetSpec {
            cap_usd: self.as_usd(),
            window: self.window,
        }
    }
}

impl std::str::FromStr for AccountCap {
    type Err = AccountCapParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        let (amount, window) = if let Some(amount) = raw.strip_suffix("/day") {
            (amount, BudgetWindow::Day)
        } else if let Some(amount) = raw.strip_suffix("/month") {
            (amount, BudgetWindow::Month)
        } else {
            return Err(AccountCapParseError::WindowRequired(raw.to_owned()));
        };
        let spec = amount
            .parse::<BudgetSpec>()
            .map_err(|_| AccountCapParseError::InvalidAmount(raw.to_owned()))?;
        if spec.window != BudgetWindow::Session {
            return Err(AccountCapParseError::WindowRequired(raw.to_owned()));
        }
        let cents = (spec.cap_usd * 100.0).round();
        if cents > u64::MAX as f64 {
            return Err(AccountCapParseError::TooLarge(raw.to_owned()));
        }
        Ok(Self {
            cents: cents as u64,
            window,
        })
    }
}

impl fmt::Display for AccountCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollars = self.cents / 100;
        let cents = self.cents % 100;
        if cents == 0 {
            write!(f, "{dollars}/{}", self.window.label())
        } else {
            write!(f, "{dollars}.{cents:02}/{}", self.window.label())
        }
    }
}

impl Serialize for AccountCap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AccountCap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountCapParseError {
    #[error(
        "invalid account budget `{0}`; use a non-negative dollar amount ending in `/day` or `/month`, such as `50/day` or `300/month`"
    )]
    InvalidAmount(String),
    #[error("account budget `{0}` must end in `/day` or `/month`; use `50/day` or `300/month`")]
    WindowRequired(String),
    #[error("account budget `{0}` is too large")]
    TooLarge(String),
}

/// Provider-account enrichment preferences.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AccountsConfig {
    /// Machine account selections for new rooms, below explicit and project selections.
    #[serde(rename = "use", skip_serializing_if = "BTreeMap::is_empty")]
    pub use_accounts: RoomLogins,
    /// Day or month dollar caps by provider kind, shared across rooms and
    /// applied once per history pool of the kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub budget: BTreeMap<String, AccountCap>,
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
    /// Named Pi accounts: each one a standalone `PI_CODING_AGENT_DIR`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pi: BTreeMap<LoginName, NamedAccount>,
}

/// One declared account of a provider kind. An empty table takes the default
/// home under the RimZ data root. Unknown fields are refused because a mistyped
/// `home` must not silently launch the account into a different home.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NamedAccount {
    /// Display text only; the name remains the account's identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The provider home this account launches into. `None` means the default
    /// location RimZ derives from the kind and the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<PathBuf>,
    /// Absent means shared; only `standalone` is written back.
    #[serde(default, skip_serializing_if = "AccountHistory::is_shared")]
    pub history: AccountHistory,
    /// Overrides the kind cap only when this account has standalone history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<AccountCap>,
}

/// A display label's broken rule, independent of account usability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountLabelError {
    #[error("must not be empty")]
    Empty,
    #[error("must be at most 32 columns wide")]
    TooLong,
    #[error("must not contain control characters")]
    Control,
    #[error("must not have leading or trailing whitespace")]
    Whitespace,
}

impl NamedAccount {
    /// Validate a label on write; readers ignore invalid labels instead.
    pub fn validate_label(label: &str) -> Result<(), AccountLabelError> {
        if label.chars().any(char::is_control) {
            return Err(AccountLabelError::Control);
        }
        let width = UnicodeWidthStr::width(label);
        if width == 0 {
            return Err(AccountLabelError::Empty);
        }
        if width > 32 {
            return Err(AccountLabelError::TooLong);
        }
        if label.trim() != label {
            return Err(AccountLabelError::Whitespace);
        }
        Ok(())
    }
}

/// Whether a named account keeps its provider history to itself.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountHistory {
    /// Everything but credentials is the default provider home's: the
    /// account home links there, sessions and transcripts included.
    #[default]
    Shared,
    /// The account home holds its own sessions and transcripts and links
    /// its settings alone.
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
    /// Borrow core account declarations, with the full lenient load's defaults and file guard.
    pub(crate) fn with_lenient<T>(read: impl FnOnce(&Self) -> T) -> T {
        Self::with_lenient_from(&super::MachineConfig::config_path(), read)
    }

    fn with_lenient_from<T>(path: &std::path::Path, read: impl FnOnce(&Self) -> T) -> T {
        read(&super::read_core_lenient(path).accounts)
    }

    /// The valid display label of a declared named account, never an identity.
    pub fn label(&self, key: &LoginKey) -> Option<&str> {
        if key.name.is_default() {
            return None;
        }
        let label = self.named(&key.kind)?.get(&key.name)?.label.as_deref()?;
        NamedAccount::validate_label(label).ok()?;
        Some(label)
    }

    /// Dashboard title using the theme-resolved product and the label or name.
    pub fn provider_title(&self, product: &str, key: &LoginKey) -> String {
        if key.name.is_default() {
            return product.to_owned();
        }
        format!(
            "{product} · {}",
            self.label(key).unwrap_or(key.name.as_str())
        )
    }

    /// Invalid labels and their fixes for room-start and account-list warnings.
    pub fn label_warnings(&self) -> Vec<(String, String)> {
        let mut warnings = Vec::new();
        for kind in crate::agents::known_kinds() {
            let Some(accounts) = self.named(&AgentKind::new_unchecked(kind)) else {
                continue;
            };
            for (name, account) in accounts {
                let Some(error) = account
                    .label
                    .as_deref()
                    .and_then(|label| NamedAccount::validate_label(label).err())
                else {
                    continue;
                };
                let key = format!("accounts.{kind}.{name}.label");
                warnings.push((key.clone(), format!("{key} {error}; the dashboard shows the account name instead; set it with `rimz accounts add {kind} {name} --label <TEXT>`, or clear it with `--label ''`")));
            }
        }
        warnings
    }

    pub fn budget(&self, kind: &str) -> Option<AccountCap> {
        self.budget.get(kind).copied()
    }

    /// The declared accounts of a kind, or `None` for a kind that cannot carry
    /// named accounts. This and [`Self::named_mut`] are the only places the
    /// supported kinds are listed.
    pub fn named(&self, kind: &AgentKind) -> Option<&BTreeMap<LoginName, NamedAccount>> {
        match kind.as_str() {
            "claude" => Some(&self.claude),
            "codex" => Some(&self.codex),
            "pi" => Some(&self.pi),
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
            "pi" => Some(&mut self.pi),
            _ => None,
        }
    }

    pub(crate) fn usage_limit(&self, kind: &str) -> Option<f64> {
        self.usage_limit_usd.get(kind).map(|limit| limit.as_usd())
    }

    pub fn validate_budgets(&self) -> Result<(), AccountBudgetConfigError> {
        self.budget_errors()
            .into_iter()
            .next()
            .map_or(Ok(()), |(_, error)| Err(error))
    }

    pub fn budget_errors(&self) -> Vec<(String, AccountBudgetConfigError)> {
        let mut errors = Vec::new();
        for kind in self.budget.keys() {
            if let Err(error) = Self::validate_budget_kind(kind) {
                errors.push((format!("accounts.budget.{kind}"), error));
            }
        }
        for kind in crate::agents::known_kinds() {
            let Some(accounts) = self.named(&AgentKind::new_unchecked(kind)) else {
                continue;
            };
            for (name, account) in accounts {
                if account.budget.is_some()
                    && let Err(error) = self.validate_budget_for_login(&LoginKey {
                        kind: AgentKind::new_unchecked(kind),
                        name: name.clone(),
                    })
                {
                    errors.push((format!("accounts.{kind}.{name}.budget"), error));
                }
            }
        }
        errors
    }

    pub fn validate_budget_for_login(
        &self,
        login: &LoginKey,
    ) -> Result<(), AccountBudgetConfigError> {
        let kind = login.kind.as_str();
        if self.budget.contains_key(kind) {
            Self::validate_budget_kind(kind)?;
        }
        if let Some(account) = self
            .named(&login.kind)
            .and_then(|accounts| accounts.get(&login.name))
            && account.budget.is_some()
        {
            Self::validate_budget_kind(kind)?;
            if account.history != AccountHistory::Standalone {
                return Err(AccountBudgetConfigError::SharedHistory {
                    kind: kind.to_owned(),
                    name: login.name.clone(),
                });
            }
        }
        Ok(())
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
    fn narrow_accounts_match_full_lenient_config() {
        for core in [
            Some("[accounts.codex.work]\nhome = '/srv/work'\n"),
            Some("[harness]\nsmart_compact = 'off'\n"),
            Some("[accounts\n"),
            None,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let core_path = dir.path().join("config.toml");
            if let Some(core) = core {
                std::fs::write(&core_path, core).unwrap();
            }
            let full = crate::config::MachineConfig::load_lenient_from(
                &core_path,
                &dir.path().join("agents"),
                &BTreeMap::new(),
            );
            AccountsConfig::with_lenient_from(&core_path, |accounts| {
                assert_eq!(accounts, &full.accounts, "core contents: {core:?}");
            });
        }
    }

    #[test]
    fn account_month_caps_and_standalone_overrides_round_trip() {
        let parsed = toml::from_str::<AccountsConfig>(
            "[budget]\npi = '100/month'\n[claude.work]\nhistory = 'standalone'\nbudget = '25/day'\n[codex.work]\nhistory = 'standalone'\nbudget = '$30.50/month'\n",
        );
        assert!(
            parsed.is_ok(),
            "account day/month caps must parse: {parsed:?}"
        );
        let config = parsed.unwrap();
        assert_eq!(config.validate_budgets(), Ok(()));
        let value = toml::Value::try_from(&config).unwrap();
        assert_eq!(value["budget"]["pi"].as_str(), Some("100/month"));
        assert_eq!(value["claude"]["work"]["budget"].as_str(), Some("25/day"));
        assert_eq!(
            value["codex"]["work"]["budget"].as_str(),
            Some("30.50/month")
        );
        assert_eq!(
            toml::from_str::<AccountsConfig>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
    }

    #[test]
    fn shared_account_budget_refuses_with_pool_and_standalone_fix() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        for window in ["day", "month"] {
            std::fs::write(
                &path,
                format!("[accounts.claude.work]\nbudget = '25/{window}'\n"),
            )
            .unwrap();
            let error = crate::config::MachineConfig::load_from(&path, root.path()).unwrap_err();
            let error = error.to_string();
            assert!(
                error.contains("accounts.claude.work.budget")
                    && error.contains("history = \"standalone\"")
                    && error.contains("claude@default")
                    && error.contains("[accounts.budget].claude"),
                "{error}"
            );
        }
    }

    #[test]
    fn account_labels_parse_and_unset_labels_stay_absent() {
        let parsed = toml::from_str::<AccountsConfig>("[codex.team]\nlabel = 'work'\n");
        assert!(parsed.is_ok(), "a string label must parse: {parsed:?}");
        assert_eq!(parsed.unwrap().codex["team"].label.as_deref(), Some("work"));
        let unset: AccountsConfig = toml::from_str("[codex.team]\n").unwrap();
        assert!(!toml::to_string(&unset).unwrap().contains("label"));
        assert!(toml::from_str::<AccountsConfig>("[codex.team]\nlabel = 5\n").is_err());
    }

    #[test]
    fn account_labels_count_display_columns() {
        for label in ["x".repeat(32), "界".repeat(16), "e\u{301}".repeat(32)] {
            assert_eq!(NamedAccount::validate_label(&label), Ok(()), "{label}");
        }
        for label in ["x".repeat(33), "界".repeat(17)] {
            assert_eq!(
                NamedAccount::validate_label(&label),
                Err(AccountLabelError::TooLong),
                "{label}"
            );
        }
    }

    #[test]
    fn account_labels_refuse_empty_values() {
        for label in ["", "\u{200b}", "\u{301}"] {
            assert_eq!(
                NamedAccount::validate_label(label),
                Err(AccountLabelError::Empty)
            );
        }
    }

    #[test]
    fn account_labels_refuse_control_characters() {
        for label in ["\n", "\0", "wo\nrk", "wo\trk", "work\u{7f}"] {
            assert_eq!(
                NamedAccount::validate_label(label),
                Err(AccountLabelError::Control)
            );
        }
    }

    #[test]
    fn account_labels_refuse_edge_whitespace() {
        for label in [" work", "work ", "\u{2003}work", "work\u{a0}"] {
            assert_eq!(
                NamedAccount::validate_label(label),
                Err(AccountLabelError::Whitespace)
            );
        }
        assert_eq!(NamedAccount::validate_label("work team"), Ok(()));
    }

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
        assert_eq!(config.budget("claude").map(AccountCap::as_usd), Some(100.0));
        assert_eq!(config.budget("codex").map(AccountCap::as_usd), Some(25.5));
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

            [pi.work]
            home = "/srv/pi/work"
            label = "Work seat"
            history = "standalone"
            "#,
        )
        .expect("parse named accounts");
        let claude = config.named(&AgentKind::new_unchecked("claude")).unwrap();
        assert_eq!(claude["work"].home, Some(PathBuf::from("/srv/homes/work")));
        let codex = config.named(&AgentKind::new_unchecked("codex")).unwrap();
        assert_eq!(codex["personal"].home, None);
        let pi = config.named(&AgentKind::new_unchecked("pi"));
        assert!(pi.is_some(), "Pi supports declared accounts");
        let pi = &pi.unwrap()["work"];
        assert_eq!(pi.home, Some(PathBuf::from("/srv/pi/work")));
        assert_eq!(pi.label.as_deref(), Some("Work seat"));
        assert_eq!(pi.history, AccountHistory::Standalone);
        assert_eq!(
            toml::from_str::<AccountsConfig>(&toml::to_string(&config).unwrap()).unwrap(),
            config,
        );
        assert!(config.named(&AgentKind::new_unchecked("grok")).is_none());
        assert_eq!(config.budget("claude").map(AccountCap::as_usd), Some(100.0));
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
        let refused = ["claude", "pi"].map(|kind| {
            [
                toml::from_str::<AccountsConfig>(&format!("[{kind}.work]\npath = \"/srv/work\""))
                    .is_err_and(|error| error.to_string().contains("unknown field")),
                toml::from_str::<AccountsConfig>(&format!("[{kind}.Work]\n")).is_err(),
            ]
        });
        assert_eq!(refused, [[true, true]; 2]);
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
