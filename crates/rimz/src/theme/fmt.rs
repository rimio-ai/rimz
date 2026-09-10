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

/// A budget reset countdown in two units.
pub fn reset_countdown(deadline: Timestamp, now: Timestamp) -> String {
    reset_secs(deadline.duration_since(now).as_secs())
}

pub(crate) fn reset_secs(seconds: i64) -> String {
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

pub fn dollars_cap(usd: f64) -> String {
    let cents = (usd.max(0.0) * 100.0).round() as u64;
    if cents.is_multiple_of(100) {
        format!("${}", group_thousands(cents / 100))
    } else {
        format!("${}.{:02}", group_thousands(cents / 100), cents % 100)
    }
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

/// Whole-unit compact token count for CLI surfaces.
pub fn compact_count(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{}k", value / 1_000)
    } else {
        format!("{}m", value / 1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn countdown_uses_two_scaled_units() {
        assert_eq!(reset_secs(90_000), "1d01h");
        assert_eq!(reset_secs(18_000), "5h00m");
        assert_eq!(reset_secs(-1), "0h00m");
    }

    #[test]
    fn duration_uses_largest_exact_unit() {
        assert_eq!(duration_label(0), "0d");
        assert_eq!(duration_label(59), "59m");
        assert_eq!(duration_label(60), "1h");
        assert_eq!(duration_label(1_440), "1d");
    }
}
