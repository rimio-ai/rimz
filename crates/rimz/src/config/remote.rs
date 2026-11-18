use serde::{Deserialize, Serialize};

const DEFAULT_PASTE_MAX: &str = "1GiB";

/// Laptop-side `rimz remote connect` settings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteConfig {
    /// Relay the keyboard through rimz so Ctrl+V and file drops upload into
    /// the room; off leaves ssh reading the terminal itself.
    pub paste: bool,
    /// Total bytes of one upload, written like `gc.tmp_floor_min`.
    #[serde(deserialize_with = "deserialize_paste_max")]
    pub paste_max: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            paste: true,
            paste_max: DEFAULT_PASTE_MAX.to_owned(),
        }
    }
}

impl RemoteConfig {
    pub fn paste_max_bytes(&self) -> u64 {
        // Deserialization refused any value this parse would refuse.
        crate::utils::size::parse_byte_size(&self.paste_max).unwrap_or(u64::MAX)
    }
}

fn deserialize_paste_max<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let raw = String::deserialize(deserializer)?;
    crate::utils::size::parse_byte_size(&raw)
        .map_err(|err| serde::de::Error::custom(format!("remote.paste_max {err}")))?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_relay_paste_with_a_one_gib_limit() {
        let config: RemoteConfig = toml::from_str("").expect("empty section");
        assert!(config.paste);
        assert_eq!(config.paste_max_bytes(), 1 << 30);
        assert_eq!(
            toml::to_string(&config).expect("serialize"),
            "paste = true\npaste_max = \"1GiB\"\n"
        );
    }

    #[test]
    fn both_keys_parse_and_a_bad_size_names_its_key() {
        let config: RemoteConfig =
            toml::from_str("paste = false\npaste_max = \"64MB\"").expect("both keys");
        assert!(!config.paste);
        assert_eq!(config.paste_max_bytes(), 64_000_000);
        let err = toml::from_str::<RemoteConfig>("paste_max = \"10x\"").expect_err("bad unit");
        assert!(
            err.to_string()
                .contains("remote.paste_max unknown size unit"),
            "{err}"
        );
    }
}
