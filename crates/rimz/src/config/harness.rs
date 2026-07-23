use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::harness::budget::{BudgetSpec, BudgetWindow};

/// A local-calendar-day dollar cap stored as cents so machine config keeps
/// exact equality while reusing the public budget grammar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DayCap {
    cents: u64,
}

impl DayCap {
    pub fn as_usd(self) -> f64 {
        self.cents as f64 / 100.0
    }

    pub fn as_spec(self) -> BudgetSpec {
        BudgetSpec {
            cap_usd: self.as_usd(),
            window: BudgetWindow::Day,
        }
    }
}

impl FromStr for DayCap {
    type Err = DayCapParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let spec = raw
            .parse::<BudgetSpec>()
            .map_err(DayCapParseError::Budget)?;
        if spec.window != BudgetWindow::Day {
            return Err(DayCapParseError::DayRequired(raw.trim().to_owned()));
        }
        let cents = (spec.cap_usd * 100.0).round();
        if cents > u64::MAX as f64 {
            return Err(DayCapParseError::TooLarge(raw.trim().to_owned()));
        }
        Ok(Self {
            cents: cents as u64,
        })
    }
}

impl fmt::Display for DayCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollars = self.cents / 100;
        let cents = self.cents % 100;
        if cents == 0 {
            write!(f, "{dollars}/day")
        } else {
            write!(f, "{dollars}.{cents:02}/day")
        }
    }
}

impl Serialize for DayCap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for DayCap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DayCapParseError {
    #[error(transparent)]
    Budget(#[from] crate::harness::budget::BudgetParseError),
    #[error("daily budget `{0}` must end in `/day`; use an amount such as `50/day`")]
    DayRequired(String),
    #[error("daily budget `{0}` is too large")]
    TooLarge(String),
}

/// A per-turn dollar cap stored as cents so machine config keeps exact
/// equality while reusing the public budget grammar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TurnCap {
    cents: u64,
}

impl TurnCap {
    pub fn as_usd(self) -> f64 {
        self.cents as f64 / 100.0
    }
}

impl FromStr for TurnCap {
    type Err = TurnCapParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let spec = raw
            .parse::<BudgetSpec>()
            .map_err(TurnCapParseError::Budget)?;
        if spec.window != BudgetWindow::Session {
            return Err(TurnCapParseError::PlainAmountRequired(
                raw.trim().to_owned(),
            ));
        }
        let cents = (spec.cap_usd * 100.0).round();
        if cents > u64::MAX as f64 {
            return Err(TurnCapParseError::TooLarge(raw.trim().to_owned()));
        }
        Ok(Self {
            cents: cents as u64,
        })
    }
}

impl fmt::Display for TurnCap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dollars = self.cents / 100;
        let cents = self.cents % 100;
        if cents == 0 {
            write!(f, "{dollars}")
        } else {
            write!(f, "{dollars}.{cents:02}")
        }
    }
}

impl Serialize for TurnCap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TurnCap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TurnCapParseError {
    #[error(transparent)]
    Budget(#[from] crate::harness::budget::BudgetParseError),
    #[error(
        "turn budget `{0}` must be a plain dollar amount; use an amount such as `3` or `$2.50`"
    )]
    PlainAmountRequired(String),
    #[error("turn budget `{0}` is too large")]
    TooLarge(String),
}

use crate::message::AutoCompact;

/// rtk output compression mode for agent-run cargo commands in `cargo xtask`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RtkMode {
    /// Wrap agent cargo runs through `rtk` when the binary is on PATH.
    #[default]
    Auto,
    /// Always wrap; xtask warns and runs plain when `rtk` is missing.
    On,
    /// Never wrap.
    Off,
}

impl RtkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// Harness behavior shared by immediate and parked message send paths.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HarnessConfig {
    /// Default local-calendar-day cap for one room's whole agent fleet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<DayCap>,
    /// Default per-turn dollar cap for every agent in the room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_budget: Option<TurnCap>,
    /// Compact before messages and scheduled loop wakes when the agent's
    /// context window has reached this threshold. Unset keeps compaction opt-in.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "smart_compact_serde"
    )]
    pub smart_compact: Option<AutoCompact>,
    /// rtk output compression for agent-run cargo commands in `cargo xtask`.
    #[serde(default)]
    pub rtk: RtkMode,
}

mod smart_compact_serde {
    use super::*;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<AutoCompact>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer)?
            .map(|raw| AutoCompact::parse(&raw).map_err(serde::de::Error::custom))
            .transpose()
    }

    pub fn serialize<S>(
        smart_compact: &Option<AutoCompact>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match smart_compact {
            Some(AutoCompact::Percent(pct)) => serializer.serialize_str(&format!("{pct}%")),
            Some(AutoCompact::Tokens(tokens)) => serializer.serialize_str(&tokens.to_string()),
            None => serializer.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_compact_deserializes_percent() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"70%\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Percent(70)));
    }

    #[test]
    fn day_cap_requires_day_and_round_trips() {
        let config: HarnessConfig =
            toml::from_str("budget = \"50.25/day\"").expect("parse day cap");
        assert_eq!(config.budget.map(DayCap::as_usd), Some(50.25));
        let rendered = toml::to_string(&config).expect("serialize harness");
        assert!(rendered.contains("budget = \"50.25/day\""), "{rendered}");
        assert!(
            toml::from_str::<HarnessConfig>("budget = \"50\"")
                .unwrap_err()
                .to_string()
                .contains("must end in `/day`")
        );
    }

    #[test]
    fn turn_cap_requires_plain_amount_and_round_trips() {
        let config: HarnessConfig =
            toml::from_str("turn_budget = \"$2.50\"").expect("parse turn cap");
        assert_eq!(config.turn_budget.map(TurnCap::as_usd), Some(2.5));
        let rendered = toml::to_string(&config).expect("serialize harness");
        assert!(rendered.contains("turn_budget = \"2.50\""), "{rendered}");
        assert!(
            toml::from_str::<HarnessConfig>("turn_budget = \"3/day\"")
                .unwrap_err()
                .to_string()
                .contains("must be a plain dollar amount")
        );
    }

    #[test]
    fn smart_compact_deserializes_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"120000\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(120_000)));
    }

    #[test]
    fn smart_compact_deserializes_suffixed_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"180k\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(180_000)));
    }

    #[test]
    fn smart_compact_round_trips() {
        let config = HarnessConfig {
            smart_compact: Some(AutoCompact::Percent(70)),
            ..Default::default()
        };

        let toml = toml::to_string(&config).expect("serialize harness config");
        let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");

        assert_eq!(back, config);
    }

    #[test]
    fn rtk_defaults_to_auto() {
        let config: HarnessConfig = toml::from_str("").expect("parse harness config");

        assert_eq!(config.rtk, RtkMode::Auto);
    }

    #[test]
    fn rtk_deserializes_modes() {
        let on: HarnessConfig = toml::from_str("rtk = \"on\"").expect("parse rtk on");
        let off: HarnessConfig = toml::from_str("rtk = \"off\"").expect("parse rtk off");

        assert_eq!(on.rtk, RtkMode::On);
        assert_eq!(off.rtk, RtkMode::Off);
    }

    #[test]
    fn rtk_round_trips() {
        let config = HarnessConfig {
            rtk: RtkMode::On,
            ..Default::default()
        };

        let toml = toml::to_string(&config).expect("serialize harness config");
        let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");

        assert_eq!(back, config);
    }
}
