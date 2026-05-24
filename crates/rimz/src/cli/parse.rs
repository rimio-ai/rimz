//! Shared CLI value parsers.

use std::time::Duration;

/// Parse `<n><unit>` against an allowed-units slice. Each entry is
/// `(unit_str, multiplier_in_seconds)`. Returns a human-readable error string
/// suitable for `clap`'s `value_parser`.
pub(crate) fn parse_duration_units(raw: &str, allowed: &[(&str, u64)]) -> Result<Duration, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("duration is empty".to_owned());
    }
    let (digits, unit) = trimmed
        .split_at_checked(trimmed.len() - 1)
        .ok_or_else(|| format!("unrecognised duration `{raw}`"))?;
    let factor = allowed
        .iter()
        .find_map(|(name, mult)| (*name == unit).then_some(*mult))
        .ok_or_else(|| {
            let units = allowed
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join("/");
            format!("unknown duration unit `{unit}`; use {units}")
        })?;
    let n: u64 = digits
        .parse()
        .map_err(|e| format!("duration `{raw}` is not an integer: {e}"))?;
    Ok(Duration::from_secs(n.saturating_mul(factor)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMHD: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];
    const SMH: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600)];

    #[test]
    fn parses_each_unit() {
        assert_eq!(
            parse_duration_units("30s", SMH).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            parse_duration_units("5m", SMH).unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            parse_duration_units("1h", SMH).unwrap(),
            Duration::from_secs(3600)
        );
        assert_eq!(
            parse_duration_units("7d", SMHD).unwrap(),
            Duration::from_secs(7 * 86_400)
        );
    }

    #[test]
    fn rejects_unit_outside_allowed_set() {
        assert!(parse_duration_units("30d", SMH).is_err());
        assert!(parse_duration_units("30y", SMHD).is_err());
    }

    #[test]
    fn rejects_missing_unit_or_empty() {
        assert!(parse_duration_units("30", SMH).is_err());
        assert!(parse_duration_units("", SMH).is_err());
    }
}
