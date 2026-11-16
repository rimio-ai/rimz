//! The verdict loop judges answered before outcome words, kept to read old loop run logs.

use serde::{Deserialize, Serialize};

/// The `{pass, reason}` verdict loop judges answered before outcome words, kept only
/// to read legacy `AgentCheckRecord` rows in the loop run log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessVerdict {
    pub pass: bool,
    #[serde(deserialize_with = "bounded_reason")]
    pub reason: String,
}

fn bounded_reason<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let mut reason = String::deserialize(deserializer)?;
    let mut start = reason.len().saturating_sub(4 * 1024);
    while !reason.is_char_boundary(start) {
        start += 1;
    }
    reason.drain(..start);
    Ok(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_reader_bounds_the_reason_at_a_utf8_boundary() {
        let verdict: HeadlessVerdict = serde_json::from_value(serde_json::json!({
            "pass": false, "reason": format!("{}tail", "界".repeat(5000))
        }))
        .unwrap();
        assert!(
            verdict.reason.len() <= 4096 && verdict.reason.ends_with("tail"),
            "verdict evidence must be a bounded UTF-8 tail: {} bytes",
            verdict.reason.len()
        );
    }
}
