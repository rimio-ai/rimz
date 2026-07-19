use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// Opt-in remote-control auto-launch policy keyed by registered agent kind.
///
/// Flattening preserves the existing `[remote_control] claude = true` and
/// `codex = true` TOML while allowing a capable integration to add its key
/// without extending this config type.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RemoteControlConfig {
    #[serde(flatten)]
    enabled: BTreeMap<String, bool>,
}

impl Default for RemoteControlConfig {
    fn default() -> Self {
        Self {
            enabled: [("claude".to_owned(), false), ("codex".to_owned(), false)]
                .into_iter()
                .collect(),
        }
    }
}

impl RemoteControlConfig {
    pub fn enabled_for(&self, kind: &str) -> bool {
        self.enabled.get(kind).copied().unwrap_or(false)
    }
}

impl<'de> Deserialize<'de> for RemoteControlConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(flatten)]
            values: BTreeMap<String, serde_json::Value>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut config = Self::default();
        for (kind, value) in wire.values {
            let known = crate::agents::find_definition(&kind).is_some();
            let Some(enabled) = value.as_bool() else {
                // Keep machine config forward-compatible with unrelated future
                // fields while treating boolean keys as agent toggles.
                if known {
                    return Err(serde::de::Error::custom(format!(
                        "remote-control agent kind `{kind}` must be a boolean"
                    )));
                }
                continue;
            };
            if !known {
                return Err(serde::de::Error::custom(format!(
                    "unknown remote-control agent kind `{kind}`"
                )));
            }
            config.enabled.insert(kind, enabled);
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_boolean_toggle_keys_and_ignores_extension_values() {
        let config: RemoteControlConfig =
            toml::from_str("claude = true\ncapacity = 16").expect("known toggle");
        assert!(config.enabled_for("claude"));
        assert!(!config.enabled_for("capacity"));

        let error = toml::from_str::<RemoteControlConfig>("future_agent = true")
            .expect_err("unknown toggle must fail");
        assert!(error.to_string().contains("future_agent"));
    }
}
