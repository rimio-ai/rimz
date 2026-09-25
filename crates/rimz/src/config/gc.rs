use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::disk::retention::DEFAULT_OLDER_THAN;
use crate::utils::time::{DurationUnit, parse_duration_units};

const OLDER_THAN_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// Garbage collection policy: the retention cutoff and the daily automatic sweep.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct GcConfig {
    /// Sweep once a day from every open room.
    pub auto: bool,
    /// Age cutoff `rimz gc` uses when `--older-than` is absent.
    #[serde(with = "older_than_serde")]
    pub older_than: Duration,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            auto: true,
            older_than: DEFAULT_OLDER_THAN,
        }
    }
}

/// Parse a gc retention span (`30s`, `5m`, `8h`, `3d`); zero is refused.
pub fn parse_older_than(raw: &str) -> Result<Duration, String> {
    let duration = parse_duration_units(raw, OLDER_THAN_UNITS).map_err(|err| err.to_string())?;
    if duration.is_zero() {
        return Err("must be greater than zero".to_owned());
    }
    Ok(duration)
}

mod older_than_serde {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        super::parse_older_than(&raw)
            .map_err(|err| serde::de::Error::custom(format!("gc.older_than {err}")))
    }

    pub fn serialize<S>(older_than: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&crate::utils::time::format_duration_compact(*older_than))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_sweep_automatically_with_a_week_cutoff() {
        let config: GcConfig = toml::from_str("").expect("empty section");

        assert!(config.auto);
        assert_eq!(config.older_than, Duration::from_secs(7 * 86_400));
        assert_eq!(
            toml::to_string(&config).expect("serialize"),
            "auto = true\nolder_than = \"7d\"\n"
        );
    }

    #[test]
    fn older_than_accepts_every_compact_unit() {
        for (raw, secs) in [("30s", 30), ("5m", 300), ("8h", 28_800), ("3d", 259_200)] {
            assert_eq!(
                parse_older_than(raw),
                Ok(Duration::from_secs(secs)),
                "{raw}"
            );
        }
    }

    #[test]
    fn older_than_rejects_zero_and_unknown_units() {
        assert_eq!(
            parse_older_than("0d").expect_err("zero"),
            "must be greater than zero"
        );
        for raw in ["7x", "abc", ""] {
            assert!(parse_older_than(raw).is_err(), "{raw}");
        }
        let err = toml::from_str::<GcConfig>("older_than = \"0h\"").expect_err("zero in file");
        assert!(
            err.to_string()
                .contains("gc.older_than must be greater than zero")
        );
    }
}
