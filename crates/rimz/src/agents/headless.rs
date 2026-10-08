//! Provider-neutral schema, paths, and results for bounded headless checks.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::pricing::PriceBook;
use super::skills::{LaunchSettingsArtifact, LaunchSettingsErr};

pub const CHECK_VERDICT_SCHEMA: &str = r#"{"type":"object","properties":{"pass":{"type":"boolean"},"reason":{"type":"string"}},"required":["pass","reason"],"additionalProperties":false}"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadlessRequest {
    pub schema: String,
    pub schema_file: PathBuf,
    pub verdict_file: PathBuf,
}

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

#[derive(Debug, Default, PartialEq)]
pub struct HeadlessResult {
    pub verdict: Option<HeadlessVerdict>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub error: Option<String>,
}

pub trait HeadlessForm: std::fmt::Debug + Sync {
    fn render_argv(
        &self,
        extra_args: &[String],
        prompt: Option<&str>,
        request: &HeadlessRequest,
        cwd: &Path,
        settings: (&Path, &mut Option<LaunchSettingsArtifact>),
    ) -> Result<Vec<String>, LaunchSettingsErr>;

    fn read_result(
        &self,
        stdout: &[u8],
        stderr: &[u8],
        request: &HeadlessRequest,
        model: Option<&str>,
        prices: &PriceBook,
    ) -> HeadlessResult;
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
