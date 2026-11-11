//! The duration grammar of workflow definitions and their start limits: `s`, `m`, `h`, or `d`,
//! at most a year.

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
