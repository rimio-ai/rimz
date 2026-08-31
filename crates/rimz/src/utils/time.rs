//! Parsers for compact, human-authored time values used across RimZ domains.

use std::num::ParseIntError;
use std::str::FromStr;
use std::time::Duration;

/// A duration suffix RimZ accepts at a particular input boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurationUnit {
    UnitlessSecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
}

impl DurationUnit {
    const fn suffix(self) -> &'static str {
        match self {
            Self::UnitlessSecond => "",
            Self::Millisecond => "ms",
            Self::Second => "s",
            Self::Minute => "m",
            Self::Hour => "h",
            Self::Day => "d",
        }
    }

    const fn duration(self, amount: u64) -> Duration {
        match self {
            Self::UnitlessSecond | Self::Second => Duration::from_secs(amount),
            Self::Millisecond => Duration::from_millis(amount),
            Self::Minute => Duration::from_secs(amount.saturating_mul(60)),
            Self::Hour => Duration::from_secs(amount.saturating_mul(3_600)),
            Self::Day => Duration::from_secs(amount.saturating_mul(86_400)),
        }
    }
}

/// A validated local wall-clock hour and minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockTime {
    hour: u8,
    minute: u8,
}

impl ClockTime {
    pub const fn hour(self) -> u8 {
        self.hour
    }

    pub const fn minute(self) -> u8 {
        self.minute
    }
}

impl FromStr for ClockTime {
    type Err = TimeInputErr;

    fn from_str(value: &str) -> Result<Self> {
        let invalid = || TimeInputErr::InvalidClockTime {
            value: value.to_owned(),
        };
        let (hh, mm) = value.trim().split_once(':').ok_or_else(invalid)?;
        let hour: u8 = hh.parse().map_err(|_| invalid())?;
        let minute: u8 = mm.parse().map_err(|_| invalid())?;
        if hour > 23 || minute > 59 {
            return Err(invalid());
        }
        Ok(Self { hour, minute })
    }
}

/// Failures while parsing RimZ's compact duration and wall-clock inputs.
#[derive(Debug, thiserror::Error)]
pub enum TimeInputErr {
    #[error("duration is empty")]
    EmptyDuration,
    #[error("unknown duration unit `{unit}`; use {allowed}")]
    UnknownDurationUnit { unit: String, allowed: String },
    #[error("duration `{value}` is not an integer: {source}")]
    InvalidDurationInteger {
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("invalid 24-hour time `{value}`; use HH:MM")]
    InvalidClockTime { value: String },
}

pub type Result<T> = std::result::Result<T, TimeInputErr>;

/// Parse `<n><unit>` against the units accepted by one input boundary.
pub fn parse_duration_units(raw: &str, allowed: &[DurationUnit]) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(TimeInputErr::EmptyDuration);
    }
    let suffix_start = trimmed
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(suffix_start);
    let unit = allowed
        .iter()
        .copied()
        .find(|unit| unit.suffix() == suffix)
        .ok_or_else(|| TimeInputErr::UnknownDurationUnit {
            unit: suffix.to_owned(),
            allowed: allowed
                .iter()
                .map(|unit| unit.suffix())
                .collect::<Vec<_>>()
                .join("/"),
        })?;
    let amount = digits
        .parse::<u64>()
        .map_err(|source| TimeInputErr::InvalidDurationInteger {
            value: raw.to_owned(),
            source,
        })?;
    Ok(unit.duration(amount))
}

pub fn format_compact_duration(duration: Duration) -> String {
    let mut seconds = duration.as_secs();
    let mut rendered = String::new();
    for (unit_seconds, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        let amount = seconds / unit_seconds;
        if amount > 0 {
            rendered.push_str(&format!("{amount}{suffix}"));
            seconds %= unit_seconds;
        }
    }
    if seconds > 0 || rendered.is_empty() {
        rendered.push_str(&format!("{seconds}s"));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMH: &[DurationUnit] = &[
        DurationUnit::Second,
        DurationUnit::Minute,
        DurationUnit::Hour,
    ];

    #[test]
    fn duration_units_parse_and_reject_by_allowed_set() {
        assert_eq!(parse_duration_units("30s", SMH).unwrap().as_secs(), 30);
        assert_eq!(parse_duration_units("5m", SMH).unwrap().as_secs(), 300);
        assert_eq!(parse_duration_units("1h", SMH).unwrap().as_secs(), 3_600);
        assert_eq!(
            parse_duration_units("7d", &[DurationUnit::Day])
                .unwrap()
                .as_secs(),
            7 * 86_400
        );
        for raw in ["30d", "30", ""] {
            assert!(parse_duration_units(raw, SMH).is_err(), "{raw}");
        }
    }

    #[test]
    fn unitless_seconds_are_accepted_only_when_enabled() {
        let allowed = [DurationUnit::Second, DurationUnit::UnitlessSecond];
        assert_eq!(parse_duration_units("30", &allowed).unwrap().as_secs(), 30);
        assert!(parse_duration_units("30", &[DurationUnit::Second]).is_err());
    }

    #[test]
    fn multi_character_units_are_parsed_without_caller_special_cases() {
        assert_eq!(
            parse_duration_units("500ms", &[DurationUnit::Millisecond]).unwrap(),
            Duration::from_millis(500)
        );
        assert!(parse_duration_units("500ms", &[DurationUnit::Second]).is_err());
    }

    #[test]
    fn clock_time_is_typed_and_range_checked() {
        let time = " 08:30 ".parse::<ClockTime>().unwrap();
        assert_eq!((time.hour(), time.minute()), (8, 30));
        for raw in ["", "8", "24:00", "23:60", "noon"] {
            assert!(raw.parse::<ClockTime>().is_err(), "{raw}");
        }
    }

    #[test]
    fn compact_duration_uses_each_nonzero_unit() {
        assert_eq!(format_compact_duration(Duration::ZERO), "0s");
        assert_eq!(format_compact_duration(Duration::from_secs(252)), "4m12s");
        assert_eq!(format_compact_duration(Duration::from_secs(3_660)), "1h1m");
    }
}
