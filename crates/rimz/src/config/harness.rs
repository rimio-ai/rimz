use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    /// Compact before `message` sends when the agent's context window
    /// has reached this threshold. Unset keeps compact-first sends opt-in.
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
    fn smart_compact_deserializes_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_compact = \"120000\"").expect("parse harness config");

        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(120_000)));
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
