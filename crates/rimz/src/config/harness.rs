use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::message::AutoCompact;

/// Harness behavior shared by the immediate and queued agent send paths.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HarnessConfig {
    /// Compact before `steer` and `queue` sends when the agent's context window
    /// has reached this threshold. Unset keeps compact-first sends opt-in.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "smart_auto_compact_serde"
    )]
    pub smart_auto_compact: Option<AutoCompact>,
}

mod smart_auto_compact_serde {
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
        smart_auto_compact: &Option<AutoCompact>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match smart_auto_compact {
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
    fn smart_auto_compact_deserializes_percent() {
        let config: HarnessConfig =
            toml::from_str("smart_auto_compact = \"70%\"").expect("parse harness config");

        assert_eq!(config.smart_auto_compact, Some(AutoCompact::Percent(70)));
    }

    #[test]
    fn smart_auto_compact_deserializes_token_count() {
        let config: HarnessConfig =
            toml::from_str("smart_auto_compact = \"120000\"").expect("parse harness config");

        assert_eq!(
            config.smart_auto_compact,
            Some(AutoCompact::Tokens(120_000))
        );
    }

    #[test]
    fn smart_auto_compact_round_trips() {
        let config = HarnessConfig {
            smart_auto_compact: Some(AutoCompact::Percent(70)),
        };

        let toml = toml::to_string(&config).expect("serialize harness config");
        let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");

        assert_eq!(back, config);
    }
}
