use super::*;

pub(super) fn agent_display_name(kind: &str) -> String {
    rimz::agents::spec_by_kind(kind)
        .map(|definition| definition.display_name.to_owned())
        .unwrap_or_else(|| agent_kind_label(kind))
}

pub(super) fn agent_kind_label(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

pub(super) fn fmt_tokens(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.0}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Tokens at one-decimal precision in lowercase units (`61.0m`, `1.2b`), the
/// finer register the per-model In/Out lines read in.
pub(super) fn fmt_tokens_lower(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}b", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}m", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Dollars as `$8,666` — rounded, thousands grouped.
pub(super) fn fmt_usd(v: f64) -> String {
    let whole = v.round() as i64;
    let sign = if whole < 0 { "-" } else { "" };
    format!("{sign}${}", group_thousands(whole.unsigned_abs()))
}

pub(super) fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped
}

/// A day key (days since the epoch) as `May 29`.
pub(super) fn fmt_day(day: i64) -> String {
    let date = utc_date(day.max(0) as u64 * DAY_SECS as u64);
    let month = date
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .filter(|m| (1..=12).contains(m));
    let dom = date.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, dom) {
        (Some(m), Some(d)) => format!("{} {d}", MONTHS[m - 1]),
        _ => date,
    }
}

/// Heatmap span from the terminal width: wider screens show more weeks.
pub(super) fn weeks_for_terminal(cols: usize) -> usize {
    (cols.saturating_sub(GUTTER) / 2).clamp(MIN_WEEKS, MAX_WEEKS)
}
