//! Shared CLI value parsers.

use std::time::Duration;

/// Parse `<n><unit>` against an allowed-units slice. Each entry is
/// `(unit_str, multiplier_in_seconds)`. Returns a human-readable error string
/// suitable for `clap`'s `value_parser`.
pub(crate) fn parse_duration_units(raw: &str, allowed: &[(&str, u64)]) -> Result<Duration, String> {
    rimz::harness::schedule::parse_duration_units(raw, allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMHD: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];
    const SMH: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600)];

    #[test]
    fn duration_units_parse_and_reject_by_allowed_set() {
        for (raw, allowed, expected) in [
            ("30s", SMH, Duration::from_secs(30)),
            ("5m", SMH, Duration::from_secs(300)),
            ("1h", SMH, Duration::from_secs(3600)),
            ("7d", SMHD, Duration::from_secs(7 * 86_400)),
        ] {
            assert_eq!(
                parse_duration_units(raw, allowed).unwrap(),
                expected,
                "{raw}"
            );
        }

        for (raw, allowed) in [("30d", SMH), ("30y", SMHD), ("30", SMH), ("", SMH)] {
            assert!(parse_duration_units(raw, allowed).is_err(), "{raw}");
        }
    }
}
