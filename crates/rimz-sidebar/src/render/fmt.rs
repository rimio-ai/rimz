//! Time and text formatting helpers shared by the renderer.

use jiff::Timestamp;

pub(super) fn age_short(at: Timestamp) -> String {
    let seconds = Timestamp::now().duration_since(at).as_secs();
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

pub(super) fn time_remaining(deadline: Timestamp) -> String {
    let seconds = deadline.duration_since(Timestamp::now()).as_secs();
    if seconds <= 0 {
        "0s".to_owned()
    } else if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m", seconds / 60)
    }
}

pub(super) fn clip(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

/// Compact countdown to a deadline as its two highest non-zero units — `3d12h`,
/// `3h12m`, `2m30s`, `45s`. Skipping zero units keeps it short, so a window with
/// no minutes left reads `3h3s` rather than padding a `0m`. A passed deadline is
/// `now` (the window already reset).
pub(super) fn duration_compact(deadline: Timestamp) -> String {
    compact_seconds(deadline.duration_since(Timestamp::now()).as_secs())
}

/// The pure core of [`duration_compact`], split out so the formatting is tested
/// without a wall-clock read.
fn compact_seconds(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_owned();
    }
    let parts = [
        (seconds / 86_400, 'd'),
        (seconds % 86_400 / 3_600, 'h'),
        (seconds % 3_600 / 60, 'm'),
        (seconds % 60, 's'),
    ];
    parts
        .into_iter()
        .filter(|(value, _)| *value > 0)
        .take(2)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect()
}

/// Like [`duration_compact`] but never finer than minutes — for the 5-hour
/// usage window, where a ticking seconds field is just noise. A sub-minute
/// remainder rounds up to `1m`, so a live window never reads `now` before it
/// has actually reset.
pub(super) fn duration_compact_minutes(deadline: Timestamp) -> String {
    compact_minutes(deadline.duration_since(Timestamp::now()).as_secs())
}

/// The pure core of [`duration_compact_minutes`]. Rounds up to the next whole
/// minute, then takes the two highest non-zero units from `d`/`h`/`m`.
fn compact_minutes(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_owned();
    }
    let minutes = (seconds + 59) / 60;
    let parts = [
        (minutes / 1_440, 'd'),
        (minutes % 1_440 / 60, 'h'),
        (minutes % 60, 'm'),
    ];
    parts
        .into_iter()
        .filter(|(value, _)| *value > 0)
        .take(2)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect()
}

/// Session spend as a fixed one-decimal dollar amount — `$0.0`, `$3.3`, `$21.0`,
/// `$124.0`. The single decimal never varies, so the cost column aligns across
/// rows instead of jittering between a cents and a whole-dollar shape.
pub(super) fn dollars(usd: f64) -> String {
    format!("${usd:.1}")
}

/// Shorten a model's statusline display name for the capability line: drop the
/// `context` qualifier from an extended-window suffix so `Opus 4.8 (1M context)`
/// reads `Opus 4.8 (1M)`. A name without that suffix passes through unchanged.
pub(super) fn model_label(display: &str) -> String {
    display.replace(" context)", ")")
}

/// A token count as a thin magnitude with no unit suffix — `523`, `76.5k`,
/// `1.2M` — so callers compose it into a label (`{} tok`) or a split line.
pub(super) fn tokens_short(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// The context bar's right-hand value, sized to fit the bar's 5-cell value
/// column. Prefers a one-decimal precise fraction (`78.2%`) when the caller can
/// derive it from the current-message token composition; otherwise the agent's
/// integer `used_percentage` (`38%`). Clamps to `100%` so it never spills past
/// five cells (`100.0%` would).
pub(super) fn pct_label(precise: Option<f64>, whole: u8) -> String {
    match precise {
        Some(fraction) if fraction >= 99.95 => "100%".to_owned(),
        Some(fraction) => format!("{:.1}%", fraction.clamp(0.0, 100.0)),
        None => format!("{}%", whole.min(100)),
    }
}

/// A worked-time span (`12m`, `1h12m`, `3d4h`) from a millisecond duration — the
/// session's `total_duration_ms`. Reuses the two-highest-units core that
/// [`duration_compact`] uses; a zero span reads `0s` rather than the core's
/// `now`, which is a countdown idiom that misreads as elapsed work.
pub(super) fn duration_worked(ms: u64) -> String {
    let seconds = (ms / 1_000) as i64;
    if seconds <= 0 {
        return "0s".to_owned();
    }
    compact_seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_compact_takes_two_highest_nonzero_units() {
        assert_eq!(compact_seconds(3 * 86_400 + 12 * 3_600), "3d12h");
        assert_eq!(compact_seconds(3 * 3_600 + 12 * 60), "3h12m");
        assert_eq!(compact_seconds(2 * 60 + 30), "2m30s");
        assert_eq!(compact_seconds(45), "45s");
        // Zero minutes between non-zero hours and seconds collapses out.
        assert_eq!(compact_seconds(3 * 3_600 + 3), "3h3s");
    }

    #[test]
    fn duration_compact_past_deadline_is_now() {
        assert_eq!(compact_seconds(0), "now");
        assert_eq!(compact_seconds(-5), "now");
    }

    #[test]
    fn duration_compact_minutes_never_shows_seconds() {
        assert_eq!(compact_minutes(3 * 3_600 + 12 * 60), "3h12m");
        assert_eq!(compact_minutes(3 * 86_400 + 4 * 3_600), "3d4h");
        // A sub-minute remainder rounds up to 1m, never collapsing to "now".
        assert_eq!(compact_minutes(45), "1m");
        assert_eq!(compact_minutes(2 * 60 + 30), "3m");
        assert_eq!(compact_minutes(0), "now");
    }

    #[test]
    fn dollars_is_always_one_decimal() {
        assert_eq!(dollars(0.0), "$0.0");
        assert_eq!(dollars(0.04), "$0.0");
        assert_eq!(dollars(3.27), "$3.3");
        assert_eq!(dollars(20.4), "$20.4");
        assert_eq!(dollars(124.0), "$124.0");
    }

    #[test]
    fn model_label_drops_context_qualifier() {
        assert_eq!(model_label("Opus 4.8 (1M context)"), "Opus 4.8 (1M)");
        assert_eq!(model_label("Opus 4.8"), "Opus 4.8");
        assert_eq!(model_label("GPT-5.5"), "GPT-5.5");
    }

    #[test]
    fn tokens_short_scales_by_magnitude() {
        assert_eq!(tokens_short(523), "523");
        assert_eq!(tokens_short(76_500), "76.5k");
        assert_eq!(tokens_short(1_200_000), "1.2M");
    }

    #[test]
    fn pct_label_prefers_precise_decimal_then_clamps() {
        assert_eq!(pct_label(Some(78.23), 78), "78.2%");
        assert_eq!(pct_label(Some(9.9), 9), "9.9%");
        // A precise value within rounding of full reads `100%`, never `100.0%`.
        assert_eq!(pct_label(Some(99.96), 99), "100%");
        assert_eq!(pct_label(Some(100.0), 100), "100%");
        // No breakdown: the integer gauge value, also clamped.
        assert_eq!(pct_label(None, 38), "38%");
        assert_eq!(pct_label(None, 200), "100%");
        // Every rendering fits the 5-cell value column.
        for s in [
            pct_label(Some(78.23), 78),
            pct_label(Some(100.0), 100),
            pct_label(None, 38),
        ] {
            assert!(s.chars().count() <= 5, "{s:?} exceeds 5 cells");
        }
    }

    #[test]
    fn duration_worked_spans_two_units_and_floors_zero() {
        assert_eq!(duration_worked(720_000), "12m"); // 12 minutes
        assert_eq!(duration_worked(4_320_000), "1h12m"); // 1h12m
        assert_eq!(duration_worked(0), "0s");
        assert_eq!(duration_worked(500), "0s"); // sub-second floors to 0s, not "now"
    }
}
