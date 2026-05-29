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

/// Session spend as a terse dollar amount: cents-precise under `$10`
/// (`$0.04`, `$3.27`), whole dollars above (`$20`, `$124`). The threshold keeps
/// a long session's cost from carrying noisy trailing cents.
pub(super) fn dollars(usd: f64) -> String {
    if usd < 10.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.0}")
    }
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
    fn dollars_switches_precision_at_ten() {
        assert_eq!(dollars(0.0), "$0.00");
        assert_eq!(dollars(0.04), "$0.04");
        assert_eq!(dollars(3.27), "$3.27");
        assert_eq!(dollars(9.99), "$9.99");
        assert_eq!(dollars(20.4), "$20");
        assert_eq!(dollars(124.0), "$124");
    }

    #[test]
    fn tokens_short_scales_by_magnitude() {
        assert_eq!(tokens_short(523), "523");
        assert_eq!(tokens_short(76_500), "76.5k");
        assert_eq!(tokens_short(1_200_000), "1.2M");
    }
}
