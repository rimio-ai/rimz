use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::disk::retention::{
    DEFAULT_OLDER_THAN, DEFAULT_TMP_FLOOR_MIN, DEFAULT_TMP_FLOOR_PERCENT, DEFAULT_TMP_GRACE,
};
use crate::utils::time::{DurationUnit, parse_duration_units};

const OLDER_THAN_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// Forever-or-duration retention for histories and team archives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryRetention {
    #[default]
    Forever,
    Duration(Duration),
}

impl HistoryRetention {
    pub(super) fn parse(raw: &str, key: &str) -> Result<Self, String> {
        if raw == "forever" {
            return Ok(Self::Forever);
        }
        parse_older_than(raw)
            .map(Self::Duration)
            .map_err(|_| retention_format(key))
    }
}

impl Serialize for HistoryRetention {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Forever => serializer.serialize_str("forever"),
            Self::Duration(duration) => {
                serializer.serialize_str(&crate::utils::time::format_duration_compact(*duration))
            }
        }
    }
}

impl<'de> Deserialize<'de> for HistoryRetention {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_retention(deserializer, "gc.transcript_retention")
    }
}

/// Garbage collection policy: history retention, temp-unit grace and free-space reserve.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct GcConfig {
    /// Sweep once a day from every open room.
    pub auto: bool,
    /// Age cutoff `rimz gc` uses when `--older-than` is absent.
    #[serde(with = "older_than_serde")]
    pub older_than: Duration,
    pub transcript_retention: HistoryRetention,
    #[serde(deserialize_with = "deserialize_team_notes_retention")]
    pub team_notes_retention: HistoryRetention,
    /// Grace after a temp unit's latest owner ends and its latest modification.
    #[serde(
        serialize_with = "older_than_serde::serialize",
        deserialize_with = "deserialize_tmp_grace"
    )]
    pub tmp_grace: Duration,
    #[serde(deserialize_with = "deserialize_tmp_floor_percent")]
    pub tmp_floor_percent: u8,
    #[serde(deserialize_with = "deserialize_tmp_floor_min")]
    pub tmp_floor_min: String,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            auto: true,
            older_than: DEFAULT_OLDER_THAN,
            transcript_retention: HistoryRetention::Forever,
            team_notes_retention: HistoryRetention::Forever,
            tmp_grace: DEFAULT_TMP_GRACE,
            tmp_floor_percent: DEFAULT_TMP_FLOOR_PERCENT,
            tmp_floor_min: DEFAULT_TMP_FLOOR_MIN.to_owned(),
        }
    }
}

fn retention_format(key: &str) -> String {
    format!("{key} must be \"forever\" or a non-zero duration such as 30d or 12h (s/m/h/d)")
}

fn deserialize_retention<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    key: &str,
) -> Result<HistoryRetention, D::Error> {
    let raw = String::deserialize(deserializer)
        .map_err(|_| serde::de::Error::custom(retention_format(key)))?;
    HistoryRetention::parse(&raw, key).map_err(serde::de::Error::custom)
}

fn deserialize_team_notes_retention<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<HistoryRetention, D::Error> {
    deserialize_retention(deserializer, "gc.team_notes_retention")
}

impl GcConfig {
    pub fn tmp_floor_bytes(&self, total: u64) -> Result<u64, String> {
        let minimum = crate::utils::size::parse_byte_size(&self.tmp_floor_min)?;
        Ok(
            ((u128::from(total) * u128::from(self.tmp_floor_percent) / 100)
                .min(u128::from(u64::MAX)) as u64)
                .max(minimum),
        )
    }
}

fn deserialize_tmp_grace<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Duration, D::Error> {
    let raw = String::deserialize(deserializer)?;
    parse_older_than(&raw).map_err(|err| serde::de::Error::custom(format!("gc.tmp_grace {err}")))
}

fn deserialize_tmp_floor_percent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u8, D::Error> {
    let value = i64::deserialize(deserializer)?;
    u8::try_from(value)
        .ok()
        .filter(|value| *value <= 100)
        .ok_or_else(|| serde::de::Error::custom("gc.tmp_floor_percent must be 0-100"))
}

fn deserialize_tmp_floor_min<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let raw = String::deserialize(deserializer)?;
    crate::utils::size::parse_byte_size(&raw)
        .map_err(|err| serde::de::Error::custom(format!("gc.tmp_floor_min {err}")))?;
    Ok(raw)
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
            "auto = true\nolder_than = \"7d\"\ntranscript_retention = \"forever\"\nteam_notes_retention = \"forever\"\ntmp_grace = \"1d\"\ntmp_floor_percent = 10\ntmp_floor_min = \"10GiB\"\n"
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
    fn transcript_retention_accepts_forever_and_durations() {
        for key in ["transcript_retention", "team_notes_retention"] {
            for raw in ["forever", "30d", "12h"] {
                let config: GcConfig = toml::from_str(&format!("{key} = {raw:?}")).unwrap();
                let serialized = serde_json::to_value(config).unwrap();
                assert_eq!(serialized[key], raw);
            }
        }
    }

    #[test]
    fn transcript_retention_refuses_invalid_values_with_the_key() {
        for key in ["transcript_retention", "team_notes_retention"] {
            for raw in ["\"0d\"", "\"never\"", "30"] {
                let result = toml::from_str::<GcConfig>(&format!("{key} = {raw}"));
                assert!(result.is_err(), "accepted {raw}");
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains(&format!("gc.{key}"))
                );
            }
        }
    }

    #[test]
    fn tmp_policy_rejects_invalid_values() {
        for (raw, message) in [
            (
                "tmp_grace = \"0d\"",
                "gc.tmp_grace must be greater than zero",
            ),
            (
                "tmp_floor_percent = 101",
                "gc.tmp_floor_percent must be 0-100",
            ),
            (
                "tmp_floor_min = \"10x\"",
                "gc.tmp_floor_min unknown size unit",
            ),
        ] {
            let result = toml::from_str::<GcConfig>(raw);
            assert!(result.is_err(), "accepted {raw}");
            assert!(result.unwrap_err().to_string().contains(message), "{raw}");
        }
    }

    #[test]
    fn tmp_floor_uses_the_larger_reserve_and_can_be_disabled() {
        let config = GcConfig::default();
        assert_eq!(
            config.tmp_floor_bytes(100).unwrap(),
            10 * 1024 * 1024 * 1024
        );
        assert_eq!(config.tmp_floor_bytes(u64::MAX).unwrap(), u64::MAX / 10);
        let config: GcConfig =
            toml::from_str("tmp_floor_percent = 0\ntmp_floor_min = \"0\"").unwrap();
        assert_eq!(config.tmp_floor_bytes(u64::MAX).unwrap(), 0);
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
