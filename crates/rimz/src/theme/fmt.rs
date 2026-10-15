//! Shared value vocabulary for human renderers.

use jiff::Timestamp;

use crate::agents::RateLimitWindow;

/// Format bytes for human reports with 1024-based units.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value.fract() == 0.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

pub fn command_preview(command: &str) -> std::borrow::Cow<'_, str> {
    let count = command.chars().count();
    if count <= 120 {
        return std::borrow::Cow::Borrowed(command);
    }
    let start = command.chars().take(60).collect::<String>();
    let end = command.chars().skip(count - 59).collect::<String>();
    std::borrow::Cow::Owned(format!("{start}…{end}"))
}

/// A reset-credit expiry floored to one unit: days from a day out, hours from
/// an hour out, minutes below.
pub fn expiry_label(remaining: jiff::SignedDuration) -> String {
    let seconds = remaining.as_secs().max(0);
    if seconds >= 86_400 {
        format!("{}d", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}m", seconds / 60)
    }
}

/// A budget reset countdown in two units.
pub fn reset_countdown(deadline: Timestamp, now: Timestamp) -> String {
    reset_secs(deadline.duration_since(now).as_secs())
}

/// A run or stage duration: one unit below an hour, two scaled units above.
pub(crate) fn run_duration(seconds: i64) -> String {
    if seconds < 3_600 {
        crate::utils::time::format_duration_coarse(seconds)
    } else {
        reset_secs(seconds)
    }
}

fn reset_secs(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 86_400 {
        format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600)
    } else {
        format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60)
    }
}

/// A CLI-width budget window label. Sidebar fixed-width labels keep their
/// renderer-local clipping and rounding policy.
pub fn window_label(window: &RateLimitWindow) -> String {
    if let Some(scope) = &window.scope {
        return scope.label.clone();
    }
    match window.duration_mins {
        Some(mins) => duration_label(mins.into()),
        None => "usage".to_owned(),
    }
}

pub fn duration_label(mins: u64) -> String {
    if mins.is_multiple_of(24 * 60) {
        format!("{}d", mins / (24 * 60))
    } else if mins.is_multiple_of(60) {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
    }
}

pub fn dollars2(usd: f64) -> String {
    let cents = (usd.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", group_thousands(cents / 100), cents % 100)
}

pub(crate) fn dollars_cap(usd: f64) -> String {
    let cents = (usd.max(0.0) * 100.0).round() as u64;
    if cents.is_multiple_of(100) {
        format!("${}", group_thousands(cents / 100))
    } else {
        format!("${}.{:02}", group_thousands(cents / 100), cents % 100)
    }
}

/// Dollars left, floored at the display tier and compact enough for a budget row.
pub(crate) fn dollars_left(usd: f64) -> String {
    let usd = usd.max(0.0);
    if usd < 10.0 {
        // Decimal cents such as 0.29 can multiply to just below an integer.
        let cents = (usd * 100.0 + 1e-9).floor() as u64;
        if cents.is_multiple_of(100) {
            return format!("${}", cents / 100);
        }
        return format!("${}.{:02}", cents / 100, cents % 100);
    }
    if usd < 100_000.0 {
        return format!("${}", group_thousands(usd.floor() as u64));
    }
    if usd < 1_000_000.0 {
        return format!("${:.0}k", (usd / 1_000.0).floor());
    }
    format!("${:.1}M", (usd / 100_000.0).floor() / 10.0)
}

pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Whole-unit token count shared by CLI and sidebar: `523`, `76k`, `1M`, `2B`.
pub fn compact_count(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{}k", value / 1_000)
    } else if value < 1_000_000_000 {
        format!("{}M", value / 1_000_000)
    } else {
        format!("{}B", value / 1_000_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_count_uses_uppercase_millions() {
        assert_eq!(compact_count(1_200_000), "1M");
        for (value, expected) in [
            (0, "0"),
            (523, "523"),
            (999, "999"),
            (1_000, "1k"),
            (76_500, "76k"),
            (999_999, "999k"),
            (1_000_000, "1M"),
            (999_999_999, "999M"),
        ] {
            assert_eq!(compact_count(value), expected);
        }
    }

    #[test]
    fn compact_count_uses_billions() {
        for (value, expected) in [
            (1_000_000_000, "1B"),
            (2_500_000_000, "2B"),
            (u64::MAX, "18446744073B"),
        ] {
            assert_eq!(compact_count(value), expected);
        }
    }

    #[test]
    fn fmt_bytes_uses_binary_units_and_short_decimals() {
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1024), "1 KB");
        assert_eq!(fmt_bytes(13_018), "13 KB");
        assert_eq!(fmt_bytes(1_503_238_553), "1.4 GB");
        assert_eq!(fmt_bytes(18 * 1024 * 1024), "18 MB");
    }

    #[test]
    fn command_preview_bounds_unicode_without_changing_short_commands() {
        assert!(matches!(
            command_preview("echo short"),
            std::borrow::Cow::Borrowed("echo short")
        ));
        let long = format!("start{}end", "é".repeat(130));
        let preview = command_preview(&long);
        assert_eq!(preview.chars().count(), 120);
        assert!(preview.starts_with("start"));
        assert!(preview.ends_with("end"));
        assert!(preview.contains('…'));
    }

    #[test]
    fn money_groups_and_rounds() {
        assert_eq!(dollars2(1_240.567), "$1,240.57");
        assert_eq!(dollars_cap(1_240.0), "$1,240");
    }

    #[test]
    fn money_left_floors_at_each_display_tier() {
        for (amount, expected) in [
            (-1.0, "$0"),
            (f64::NAN, "$0"),
            (0.0, "$0"),
            (0.29, "$0.29"),
            (1.13, "$1.13"),
            (7.5, "$7.50"),
            (7.0, "$7"),
            (9.999, "$9.99"),
            (10.0, "$10"),
            (42.99, "$42"),
            (2414.19, "$2,414"),
            (2600.0, "$2,600"),
            (99_999.99, "$99,999"),
            (100_000.0, "$100k"),
            (999_999.0, "$999k"),
            (1_000_000.0, "$1.0M"),
            (1_299_999.0, "$1.2M"),
            (999_900_000.0, "$999.9M"),
        ] {
            let value = dollars_left(amount);
            assert_eq!(value, expected, "{amount}");
            assert!(value.len() <= 8);
        }
    }

    #[test]
    fn expiry_label_floors_to_one_unit() {
        let label = |seconds| expiry_label(jiff::SignedDuration::from_secs(seconds));
        assert_eq!(label(6 * 86_400 + 23 * 3_600), "6d");
        assert_eq!(label(23 * 3_600 + 59 * 60), "23h");
        assert_eq!(label(59 * 60), "59m");
        assert_eq!(label(-60), "0m");
    }

    #[test]
    fn countdown_uses_two_scaled_units() {
        assert_eq!(reset_secs(90_000), "1d01h");
        assert_eq!(reset_secs(18_000), "5h00m");
        assert_eq!(reset_secs(-1), "0h00m");
        for (seconds, expected) in [
            (4 * 3_600 + 20 * 60, "4h20m"),
            (45 * 60, "0h45m"),
            (86_400, "1d00h"),
            (6 * 86_400 + 23 * 3_600, "6d23h"),
            (30 * 86_400 + 10 * 3_600, "30d10h"),
            (-10, "0h00m"),
        ] {
            assert_eq!(reset_secs(seconds), expected);
        }
        assert_eq!(reset_secs(45 * 60).chars().count(), 5);
        assert_eq!(reset_secs(5 * 86_400).chars().count(), 5);
    }

    #[test]
    fn run_duration_scales_from_one_unit_to_two() {
        for (seconds, expected) in [
            (45, "45s"),
            (18 * 60, "18m"),
            (3_600, "1h00m"),
            (4 * 3_600 + 12 * 60, "4h12m"),
            (26 * 3_600, "1d02h"),
        ] {
            assert_eq!(run_duration(seconds), expected);
        }
    }

    #[test]
    fn duration_uses_largest_exact_unit() {
        assert_eq!(duration_label(0), "0d");
        assert_eq!(duration_label(59), "59m");
        assert_eq!(duration_label(60), "1h");
        assert_eq!(duration_label(1_440), "1d");
    }
}
