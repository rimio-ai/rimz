use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::utils::size::parse_byte_size;
use crate::utils::time::{DurationUnit, parse_duration_units};

const THROTTLE_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
];

/// Machine-only `[throttle]` in `config.toml`: open-agent caps, launch stagger,
/// and resource protection. Values are validated at deserialize.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThrottleConfig {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_check"
    )]
    pub check: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_duration"
    )]
    pub check_timeout: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_duration"
    )]
    pub stagger: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_cap"
    )]
    pub max_parallel_agents: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_cap"
    )]
    pub max_parallel_automation: Option<u32>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub cpu_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub io_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_pressure"
    )]
    pub memory_pressure: Option<u8>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_size"
    )]
    pub min_memory: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_throttle_size"
    )]
    pub min_disk: Option<String>,
}

impl ThrottleConfig {
    pub fn check_timeout(&self) -> Duration {
        throttle_duration(self.check_timeout.as_deref().unwrap_or("30s"))
            .unwrap_or(Duration::from_secs(30))
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn stagger(&self) -> Option<Duration> {
        self.stagger
            .as_deref()
            .and_then(|raw| throttle_duration(raw).ok())
    }

    pub fn min_memory_bytes(&self) -> Option<u64> {
        parse_byte_size(self.min_memory.as_deref()?).ok()
    }

    pub fn min_disk_bytes(&self) -> Option<u64> {
        parse_byte_size(self.min_disk.as_deref()?).ok()
    }
}

fn throttle_duration(raw: &str) -> crate::utils::time::Result<Duration> {
    parse_duration_units(raw, THROTTLE_UNITS)
}

fn deserialize_throttle_check<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if raw
        .as_deref()
        .is_some_and(|command| command.trim().is_empty())
    {
        return Err(D::Error::custom(
            "must be a non-empty command; remove the key to disable it",
        ));
    }
    Ok(raw)
}

fn deserialize_throttle_duration<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref()
        && throttle_duration(value)
            .map_err(D::Error::custom)?
            .is_zero()
    {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(raw)
}

fn deserialize_throttle_cap<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let cap = Option::<u32>::deserialize(deserializer)?;
    if cap == Some(0) {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(cap)
}

fn deserialize_throttle_pressure<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    let percent = Option::<u8>::deserialize(deserializer)?;
    if percent.is_some_and(|percent| !(1..=100).contains(&percent)) {
        return Err(D::Error::custom("must be a percentage between 1 and 100"));
    }
    Ok(percent)
}

fn deserialize_throttle_size<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = raw.as_deref()
        && parse_byte_size(value).map_err(D::Error::custom)? == 0
    {
        return Err(D::Error::custom("must be greater than zero"));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineConfig;

    #[test]
    fn capacity_check_config_parses_and_rejects_empty_commands_and_invalid_timeouts() {
        let configured: ThrottleConfig =
            toml::from_str("check = 'capacity'\ncheck-timeout = '2m'").unwrap();
        assert_eq!(configured.check.as_deref(), Some("capacity"));
        assert_eq!(configured.check_timeout(), Duration::from_secs(120));
        assert!(!configured.is_empty());
        let rejected: Vec<_> = [
            "check = '  '",
            "check-timeout = '0s'",
            "check-timeout = '1d'",
            "check-timeout = 'soon'",
        ]
        .into_iter()
        .map(|text| toml::from_str::<ThrottleConfig>(text).is_err())
        .collect();
        assert_eq!(rejected, [true; 4]);
    }

    #[test]
    fn throttle_table_parses_with_defaults_and_typed_values() {
        let defaults = MachineConfig::default().throttle;
        assert!(defaults.is_empty());
        assert_eq!(defaults.stagger(), None);
        assert_eq!(defaults.min_memory_bytes(), None);

        let config: MachineConfig = toml::from_str(
            "[throttle]\nstagger = \"30s\"\nmax-parallel-agents = 12\n\
             max-parallel-automation = 4\ncpu-pressure = 60\nio-pressure = 40\n\
             memory-pressure = 100\nmin-memory = \"8GB\"\nmin-disk = \"20GB\"\n",
        )
        .expect("valid throttle table");
        let throttle = &config.throttle;
        assert!(!throttle.is_empty());
        assert_eq!(throttle.stagger(), Some(Duration::from_secs(30)));
        assert_eq!(throttle.max_parallel_agents, Some(12));
        assert_eq!(throttle.max_parallel_automation, Some(4));
        assert_eq!(
            (
                throttle.cpu_pressure,
                throttle.io_pressure,
                throttle.memory_pressure
            ),
            (Some(60), Some(40), Some(100))
        );
        assert_eq!(throttle.min_memory_bytes(), Some(8_000_000_000));
        assert_eq!(throttle.min_disk_bytes(), Some(20_000_000_000));
        let round_trip: MachineConfig =
            toml::from_str(&toml::to_string(&config).expect("serialize")).expect("reparse");
        assert_eq!(round_trip, config);
    }

    #[test]
    fn throttle_table_rejects_values_that_could_never_admit() {
        for (text, expected) in [
            ("check-timeout = \"0s\"", "must be greater than zero"),
            ("check-timeout = \"1d\"", "unit"),
            ("stagger = \"soon\"", "duration"),
            ("max-parallel-agents = 0", "must be greater than zero"),
            ("max-parallel-automation = 0", "must be greater than zero"),
            ("cpu-pressure = 0", "between 1 and 100"),
            ("io-pressure = 101", "between 1 and 100"),
            ("min-memory = \"lots\"", "size"),
            ("min-disk = \"0\"", "must be greater than zero"),
        ] {
            let err =
                toml::from_str::<MachineConfig>(&format!("[throttle]\n{text}\n")).expect_err(text);
            assert!(err.to_string().contains(expected), "{text}: {err}");
        }
    }
}
