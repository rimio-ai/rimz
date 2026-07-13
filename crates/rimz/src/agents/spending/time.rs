//! UTC timestamp and civil-date helpers shared by spending parsers and rollups.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp in seconds as a UTC date: `"YYYY-MM-DD"`.
pub fn utc_date(secs: u64) -> String {
    civil_date_from_epoch_days((secs / 86_400) as i64)
}

/// Parse an ISO-8601 timestamp (`YYYY-MM-DDTHH:MM:SS…`, e.g. a JSONL
/// `timestamp`) to Unix seconds. Reads fixed offsets; the time-of-day is optional
/// (a bare `YYYY-MM-DD` parses to midnight). An explicit `±HH:MM` UTC offset is
/// applied (`UTC = local − offset`); a `Z` or absent zone is treated as UTC.
/// Returns `None` when the date prefix is malformed — the same guard the parsers
/// applied to the old date slice.
pub(crate) fn iso_to_unix_secs(ts: &str) -> Option<u64> {
    let bytes = ts.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: i64 = ts.get(5..7)?.parse().ok()?;
    let day: i64 = ts.get(8..10)?.parse().ok()?;
    let hour: i64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
    let min: i64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sec: i64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + min * 60 + sec
        - timezone_offset_secs(bytes);
    u64::try_from(secs).ok()
}

/// Seconds to subtract from a civil timestamp to reach UTC, read from a trailing
/// `±HH:MM` / `±HHMM` designator. `Z`, `z`, or no zone yields `0`. Scans past the
/// date/time and any fractional seconds for the first sign, so the `-` in the
/// date never matches.
fn timezone_offset_secs(bytes: &[u8]) -> i64 {
    let Some(pos) = bytes
        .iter()
        .skip(19)
        .position(|&b| matches!(b, b'+' | b'-' | b'Z' | b'z'))
    else {
        return 0;
    };
    let idx = 19 + pos;
    let sign = match bytes[idx] {
        b'-' => -1,
        b'+' => 1,
        _ => return 0, // Z / z
    };
    let two = |start: usize| -> i64 {
        match (bytes.get(start), bytes.get(start + 1)) {
            (Some(h), Some(l)) if h.is_ascii_digit() && l.is_ascii_digit() => {
                i64::from((h - b'0') * 10 + (l - b'0'))
            }
            _ => 0,
        }
    };
    let hours = two(idx + 1);
    let minute_start = if bytes.get(idx + 3) == Some(&b':') {
        idx + 4
    } else {
        idx + 3
    };
    let minutes = two(minute_start);
    sign * (hours * 3_600 + minutes * 60)
}

/// Days since the Unix epoch (1970-01-01 = 0) for a civil date — the inverse of
/// [`civil_date_from_epoch_days`]. Howard Hinnant's algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Convert days since the Unix epoch (1970-01-01 = 0) to `"YYYY-MM-DD"`.
///
/// Uses Howard Hinnant's civil-from-days algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn civil_date_from_epoch_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::iso_to_unix_secs;

    #[test]
    fn applies_explicit_timezone_offsets_and_defaults_to_utc() {
        let utc = iso_to_unix_secs("2026-01-01T10:00:00.000Z").unwrap();
        // `+02:00` is two hours ahead of UTC, so the same wall clock is earlier
        // in UTC by two hours.
        assert_eq!(iso_to_unix_secs("2026-01-01T12:00:00+02:00").unwrap(), utc);
        // `-05:00` is five hours behind UTC.
        assert_eq!(iso_to_unix_secs("2026-01-01T05:00:00-05:00").unwrap(), utc);
        // Compact offset without a colon.
        assert_eq!(iso_to_unix_secs("2026-01-01T12:00:00+0200").unwrap(), utc);
        // Fractional seconds ahead of the offset.
        assert_eq!(
            iso_to_unix_secs("2026-01-01T12:00:00.500+02:00").unwrap(),
            utc
        );
        // A `Z`, a missing zone, and a bare date all read as UTC.
        assert_eq!(iso_to_unix_secs("2026-01-01T10:00:00").unwrap(), utc);
        assert_eq!(
            iso_to_unix_secs("2026-01-01T00:00:00Z").unwrap(),
            iso_to_unix_secs("2026-01-01").unwrap()
        );
    }
}
