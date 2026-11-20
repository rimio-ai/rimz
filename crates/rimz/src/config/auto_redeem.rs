//! `[harness.auto_redeem]`: what RimZ spends at the limit, as table defaults and
//! per-provider subtables that set only what differs.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::ids::AgentKind;
use crate::utils::time::{DurationUnit, parse_duration_units};

/// Minimum blocked time an automatic reset-credit redemption buys by default.
const DEFAULT_MIN_GAIN: &str = "72h";

const MIN_GAIN_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// Which capacity follows the included subscription windows.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpendOrder {
    #[default]
    CreditsFirst,
    ResetsFirst,
}

/// How far automatic redemption goes for a provider, each level including the
/// one before it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RedeemLevel {
    /// Spend a credit only to save it from expiring.
    #[default]
    Rescue,
    /// Also spend into a spent window whose natural reset is at least
    /// `min_gain` away, and a credit that keeps the window's schedule and would
    /// die within a day of that reset.
    Gain,
    /// Also pace credits that restart the window through the week.
    Paced,
}

/// The table: defaults for every provider, then each provider's overrides.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AutoRedeemConfig {
    pub level: RedeemLevel,
    /// A duration with an s, m, h, or d unit.
    pub min_gain: String,
    pub spend_order: SpendOrder,
    #[serde(flatten)]
    providers: BTreeMap<String, ProviderRedeem>,
}

/// One provider's subtable; an unset key falls through to the table default.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
struct ProviderRedeem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<RedeemLevel>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_min_gain"
    )]
    min_gain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spend_order: Option<SpendOrder>,
}

/// The policy one provider runs under, resolved from the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedeemPolicy {
    pub level: RedeemLevel,
    pub min_gain: Duration,
    pub spend_order: SpendOrder,
}

impl Default for AutoRedeemConfig {
    fn default() -> Self {
        Self {
            level: RedeemLevel::default(),
            min_gain: DEFAULT_MIN_GAIN.to_owned(),
            spend_order: SpendOrder::default(),
            providers: BTreeMap::new(),
        }
    }
}

impl AutoRedeemConfig {
    /// The policy for `kind`: its subtable's keys over the table defaults. The
    /// subtable of a provider auto-redeem does not serve is ignored, as its
    /// room-entry warning says, and `paced` runs as `gain` for a provider that
    /// cannot pace.
    pub fn policy(&self, kind: &AgentKind) -> RedeemPolicy {
        let provider = self
            .providers
            .get(kind.as_str())
            .filter(|_| crate::harness::auto_redeem::supports_kind(kind));
        let min_gain = provider
            .and_then(|provider| provider.min_gain.as_deref())
            .unwrap_or(&self.min_gain);
        let level = provider
            .and_then(|provider| provider.level)
            .unwrap_or(self.level);
        RedeemPolicy {
            level: if level == RedeemLevel::Paced && !crate::harness::auto_redeem::can_pace(kind) {
                RedeemLevel::Gain
            } else {
                level
            },
            min_gain: parse_min_gain(min_gain).unwrap_or_else(|_| default_min_gain()),
            spend_order: provider
                .and_then(|provider| provider.spend_order)
                .unwrap_or(self.spend_order),
        }
    }

    /// One room-entry warning per provider subtable whose provider has no
    /// automatic redemption, and per subtable that sets `paced` for a provider
    /// that cannot pace. A `paced` default such a provider inherits runs as
    /// `gain` silently.
    pub(crate) fn provider_warnings(&self) -> Vec<(String, String)> {
        self.providers
            .iter()
            .filter_map(|(kind, provider)| {
                let agent = AgentKind::new_unchecked(kind.as_str());
                if !crate::harness::auto_redeem::supports_kind(&agent) {
                    return Some((
                        format!("harness.auto_redeem.{kind}"),
                        format!(
                            "`[harness.auto_redeem.{kind}]` is ignored: {kind} has no automatic \
                             reset-credit redemption; remove the table"
                        ),
                    ));
                }
                (provider.level == Some(RedeemLevel::Paced)
                    && !crate::harness::auto_redeem::can_pace(&agent))
                .then(|| {
                    (
                        format!("harness.auto_redeem.{kind}.level"),
                        format!(
                            "`harness.auto_redeem.{kind}.level = \"paced\"` runs as `gain`: \
                                 a {kind} reset credit keeps the window's schedule, so there is \
                                 nothing to pace"
                        ),
                    )
                })
            })
            .collect()
    }
}

impl<'de> Deserialize<'de> for AutoRedeemConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Table;

        impl<'de> Visitor<'de> for Table {
            type Value = AutoRedeemConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the [harness.auto_redeem] table")
            }

            // A key that is neither a setting nor a provider kind is consumed as
            // `IgnoredAny`, so the unknown-key notice reports it.
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut config = AutoRedeemConfig::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "level" => config.level = map.next_value()?,
                        "min_gain" => {
                            let raw = map.next_value::<String>()?;
                            parse_min_gain(&raw).map_err(serde::de::Error::custom)?;
                            config.min_gain = raw;
                        }
                        "spend_order" => config.spend_order = map.next_value()?,
                        kind if crate::agents::known_kinds().any(|known| known == kind) => {
                            let provider = map.next_value()?;
                            config.providers.insert(key, provider);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(config)
            }
        }

        deserializer.deserialize_map(Table)
    }
}

fn deserialize_optional_min_gain<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    parse_min_gain(&raw).map_err(serde::de::Error::custom)?;
    Ok(Some(raw))
}

fn default_min_gain() -> Duration {
    // This in-source default is validated by `min_gain_defaults_and_parses_supported_units`.
    parse_min_gain(DEFAULT_MIN_GAIN).expect("DEFAULT_MIN_GAIN must be a supported duration")
}

pub(super) fn parse_min_gain(value: &str) -> Result<Duration, String> {
    parse_duration_units(value, MIN_GAIN_UNITS).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests;
