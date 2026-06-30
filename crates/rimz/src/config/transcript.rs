use serde::{Deserialize, Serialize};

pub const DEFAULT_FILE_DAYS: u32 = 7;

/// Rolling transcript log settings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptConfig {
    /// Days covered by one transcript JSONL bucket. This controls file size,
    /// not deletion; transcript buckets are never pruned.
    pub file_days: u32,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        Self {
            file_days: DEFAULT_FILE_DAYS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_days_defaults_to_seven() {
        let config: TranscriptConfig = toml::from_str("").expect("parse transcript config");

        assert_eq!(config.file_days, DEFAULT_FILE_DAYS);
    }

    #[test]
    fn file_days_round_trips() {
        let config = TranscriptConfig { file_days: 30 };

        let toml = toml::to_string(&config).expect("serialize transcript config");
        let back: TranscriptConfig = toml::from_str(&toml).expect("parse transcript config");

        assert_eq!(back, config);
    }
}
