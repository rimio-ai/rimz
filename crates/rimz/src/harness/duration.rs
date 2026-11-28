//! The duration grammar of workflow definitions and their start limits: `s`, `m`, `h`, or `d`,
//! at most a year; and the workflow time a deadline or a give-up takes, which may be one.

pub(in crate::harness) fn duration(
    raw: &str,
) -> Result<std::time::Duration, crate::utils::time::TimeInputErr> {
    use crate::utils::time::{
        DurationUnit, TimeInputErr, format_duration_compact, parse_duration_units,
    };
    let duration = parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )?;
    let maximum = std::time::Duration::from_secs(365 * 86_400);
    if duration > maximum {
        return Err(TimeInputErr::DurationTooLong {
            value: raw.to_owned(),
            maximum: format_duration_compact(maximum),
        });
    }
    Ok(duration)
}

/// A workflow time, checked without a clock: what `[workflow].deadline`, `workflow run
/// --deadline` and `--give-up`, and `extend --deadline` take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct When(Form);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Instant(jiff::Timestamp),
    Civil(jiff::civil::DateTime),
    Calendar(crate::utils::time::CalendarAt),
    After(std::time::Duration),
}

/// Check a workflow time's spelling; the refusal names the value and the accepted forms.
pub fn parse_when(raw: &str) -> Result<When, String> {
    use crate::utils::time::CalendarAt;
    let form = if let Ok(at) = raw.parse::<jiff::Timestamp>() {
        Form::Instant(at)
    } else if let Ok(civil) = raw.parse::<jiff::civil::DateTime>() {
        Form::Civil(civil)
    } else if let Ok(date) = raw.parse::<jiff::civil::Date>() {
        Form::Civil(date.at(0, 0, 0, 0))
    } else if let Some(at) = CalendarAt::parse(raw) {
        Form::Calendar(at)
    } else if let Ok(delay) = duration(raw) {
        Form::After(delay)
    } else {
        return Err(format!(
            "{raw:?} is not a workflow time; give a duration (8h), HH:MM, <weekday> HH:MM, a \
             date (2026-11-14), a date-time (2026-11-14 09:00), or a timestamp \
             (2026-11-13T21:00:00Z)"
        ));
    };
    Ok(When(form))
}

impl When {
    /// The instant this time names from `now`: a duration after it, `HH:MM` and `<weekday>
    /// HH:MM` at their first instant strictly after it, and a date or date-time as written in
    /// `zone`, even when it has passed.
    pub fn resolve(
        self,
        now: jiff::Timestamp,
        zone: &jiff::tz::TimeZone,
    ) -> Result<jiff::Timestamp, String> {
        let at = match self.0 {
            Form::Instant(at) => Some(at),
            Form::Civil(civil) => civil
                .to_zoned_datetime(zone.clone())
                .ok()
                .map(|at| at.timestamp()),
            Form::Calendar(at) => at.resolve(&now.to_zoned_datetime(zone.clone())),
            Form::After(delay) => now.checked_add(delay).ok(),
        };
        at.ok_or_else(|| "the time lies outside the calendar".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn workflow_duration_limit_is_one_year_inclusive() {
        for raw in ["365d", "8760h"] {
            assert_eq!(duration(raw).unwrap(), Duration::from_secs(365 * 86_400));
        }
        for raw in ["366d", "8761h", "3000000d"] {
            let result = duration(raw);
            assert!(
                result.is_err(),
                "{raw} must exceed the workflow duration maximum"
            );
            assert_eq!(
                result.unwrap_err().to_string(),
                format!("duration `{raw}` is longer than the 365d maximum")
            );
        }
    }
}
