//! The surplus gate: a task's `surplus` and `surplus-after` values, and why
//! they hold an agent on an account back.

use std::time::Duration;

use crate::agents::WindowSurplus;
use crate::utils::time::{DurationUnit, parse_duration_units};

/// Parse a positive forward-headroom ratio such as `1.5x`.
pub fn parse_surplus(raw: &str) -> Result<f64, String> {
    let trimmed = raw.trim();
    let number = trimmed.strip_suffix(['x', 'X']).unwrap_or(trimmed).trim();
    let ratio = number
        .parse::<f64>()
        .map_err(|err| format!("surplus ratio `{raw}` is not a number: {err}"))?;
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err(format!(
            "surplus ratio `{raw}` must be finite and greater than zero"
        ));
    }
    Ok(ratio)
}

/// Parse the elapsed floor for a provider budget-window surplus gate.
pub fn parse_surplus_after(raw: &str) -> Result<Duration, String> {
    parse_duration_units(
        raw,
        &[DurationUnit::Minute, DurationUnit::Hour, DurationUnit::Day],
    )
    .map_err(|err| err.to_string())
}

/// Why a task's `surplus` and `surplus-after` values, as written on its row, hold an agent on
/// `kind`'s account back, from that account's window `reading`, read only when either is set:
/// a fire's admission passes the live row's values, and a workflow judge's spawn the values
/// its run's record kept from the row when the run started.
pub(in crate::harness) fn surplus_refusal(
    surplus: Option<&str>,
    surplus_after: Option<&str>,
    kind: &str,
    reading: impl FnOnce() -> Option<WindowSurplus>,
) -> Option<String> {
    if surplus.is_none() && surplus_after.is_none() {
        return None;
    }
    surplus_gate_in(surplus, surplus_after, kind, reading())
}

fn surplus_gate_in(
    surplus: Option<&str>,
    surplus_after: Option<&str>,
    kind: &str,
    reading: Option<WindowSurplus>,
) -> Option<String> {
    if surplus.is_none() && surplus_after.is_none() {
        return None;
    }
    let Some(reading) = reading else {
        return Some(format!(
            "no {kind} budget-window reading; surplus gate stays closed"
        ));
    };
    let after = match surplus_after.map(parse_surplus_after) {
        Some(Ok(after)) => Some(after),
        Some(Err(_)) => {
            return Some("invalid surplus-after gate; surplus gate stays closed".to_owned());
        }
        None => None,
    };
    if let Some(after) = after
        && (reading.elapsed.as_secs().max(0) as u64) < after.as_secs()
    {
        return Some(format!(
            "{kind} {} window {} elapsed; fires after {}",
            crate::theme::fmt::window_duration_label(reading.duration_mins),
            elapsed_label(reading.elapsed),
            surplus_after.unwrap_or_default().trim(),
        ));
    }
    let threshold = match surplus.map(parse_surplus) {
        Some(Ok(threshold)) => threshold,
        Some(Err(_)) => return Some("invalid surplus gate; surplus gate stays closed".to_owned()),
        None => 1.0,
    };
    (reading.headroom < threshold).then(|| {
        format!(
            "{kind} {} window surplus {:.1}x below {threshold:.1}x",
            crate::theme::fmt::window_duration_label(reading.duration_mins),
            reading.headroom,
        )
    })
}

fn elapsed_label(elapsed: jiff::SignedDuration) -> String {
    let total_mins = elapsed.as_secs().max(0) / 60;
    let days = total_mins / (24 * 60);
    let hours = total_mins % (24 * 60) / 60;
    let mins = total_mins % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h{mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{mins}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
        WindowSurplus {
            duration_mins: 7 * 24 * 60,
            elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
            headroom,
        }
    }

    #[test]
    fn surplus_gate_values_parse_and_reject_unsafe_inputs() {
        assert_eq!(parse_surplus("1.5x"), Ok(1.5));
        assert_eq!(parse_surplus(" 2X "), Ok(2.0));
        assert_eq!(
            parse_surplus_after("3d"),
            Ok(Duration::from_secs(3 * 86_400))
        );
        for raw in ["", "0", "-1x", "NaN", "inf", "many"] {
            assert!(parse_surplus(raw).is_err(), "{raw}");
        }
        for raw in ["3", "2w", "1.5d"] {
            assert!(parse_surplus_after(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn surplus_gate_covers_every_branch() {
        // Reason `surplus`/`surplus-after` gave for holding a fire back, if any.
        let gate = |surplus, after, reading| surplus_gate_in(surplus, after, "claude", reading);

        assert_eq!(
            surplus_gate_in(None, None, "claude", None),
            None,
            "an ungated task never consults the window"
        );
        assert_eq!(
            gate(Some("1.5x"), None, None).as_deref(),
            Some("no claude budget-window reading; surplus gate stays closed"),
            "a gate with no reading fails closed"
        );
        assert_eq!(
            gate(Some("1.5x"), Some("3d"), Some(reading(2, 2.0))).as_deref(),
            Some("claude 7d window 2d elapsed; fires after 3d"),
            "ample headroom still waits for the elapsed floor"
        );
        assert_eq!(
            gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.4))).as_deref(),
            Some("claude 7d window surplus 1.4x below 1.5x")
        );
        assert_eq!(
            gate(Some("1.5x"), Some("3d"), Some(reading(4, 1.5))),
            None,
            "headroom exactly at the threshold fires"
        );
        assert_eq!(
            surplus_gate_in(None, Some("3d"), "codex", Some(reading(4, 0.9))).as_deref(),
            Some("codex 7d window surplus 0.9x below 1.0x"),
            "a bare elapsed floor still demands sustainable headroom"
        );
    }

    #[test]
    fn month_surplus_reasons_use_the_window_label() {
        let reasons: Vec<_> = [(None, Some("3d")), (Some("1.5x"), None)]
            .into_iter()
            .map(|(surplus, after)| {
                surplus_gate_in(
                    surplus,
                    after,
                    "qwen",
                    Some(WindowSurplus {
                        duration_mins: 43_200,
                        ..reading(2, 0.9)
                    }),
                )
            })
            .collect();
        assert_eq!(
            reasons,
            [
                Some("qwen mo window 2d elapsed; fires after 3d".to_owned()),
                Some("qwen mo window surplus 0.9x below 1.5x".to_owned())
            ]
        );
    }
}
