use std::path::Path;

use serde::Deserialize;

use crate::agents::pricing::PriceBook;
use crate::agents::skills::{LaunchSettingsArtifact, LaunchSettingsErr};
use crate::agents::{HeadlessForm, HeadlessRequest, HeadlessResult};

#[derive(Deserialize)]
struct ClaudeOutput {
    structured_output: Option<serde_json::Value>,
    total_cost_usd: Option<f64>,
    usage: Option<Usage>,
    #[serde(default)]
    is_error: bool,
    result: Option<String>,
    subtype: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Debug)]
pub(super) struct ClaudeHeadless;

impl HeadlessForm for ClaudeHeadless {
    fn render_argv(
        &self,
        extra_args: &[String],
        prompt: Option<&str>,
        request: &HeadlessRequest,
        cwd: &Path,
        (artifact_dir, artifact): (&Path, &mut Option<LaunchSettingsArtifact>),
    ) -> Result<Vec<String>, LaunchSettingsErr> {
        let mut args = extra_args.to_vec();
        *artifact = super::merge_settings(
            cwd,
            artifact_dir,
            &mut args,
            artifact.as_ref(),
            false,
            |object| {
                object.insert("disableAllHooks".into(), serde_json::Value::Bool(true));
                Ok(())
            },
        )?;
        let mut argv = vec!["claude".to_owned()];
        argv.extend(args);
        argv.extend([
            "-p".into(),
            "--output-format".into(),
            "json".into(),
            "--json-schema".into(),
            request.schema.clone(),
        ]);
        if let Some(prompt) = prompt {
            argv.extend(["--".into(), prompt.to_owned()]);
        }
        Ok(argv)
    }

    fn read_result(
        &self,
        stdout: &[u8],
        _stderr: &[u8],
        _request: &HeadlessRequest,
        _model: Option<&str>,
        _prices: &PriceBook,
    ) -> HeadlessResult {
        let line = stdout
            .rsplit(|byte| *byte == b'\n')
            .find(|line| !line.iter().all(u8::is_ascii_whitespace))
            .unwrap_or_default();
        let output: ClaudeOutput = match serde_json::from_slice(line) {
            Ok(output) => output,
            Err(error) => {
                return HeadlessResult {
                    error: Some(format!("invalid Claude check output: {error}")),
                    ..Default::default()
                };
            }
        };
        let mut result = HeadlessResult {
            cost_usd: output
                .total_cost_usd
                .filter(|cost| cost.is_finite() && *cost >= 0.0),
            input_tokens: output.usage.as_ref().map(|usage| {
                usage
                    .input_tokens
                    .saturating_add(usage.cache_creation_input_tokens)
                    .saturating_add(usage.cache_read_input_tokens)
            }),
            output_tokens: output.usage.as_ref().map(|usage| usage.output_tokens),
            ..Default::default()
        };
        if output.is_error {
            result.error = Some(format!(
                "Claude check failed: {}",
                output
                    .result
                    .or(output.subtype)
                    .unwrap_or_else(|| "provider error".into())
            ));
            return result;
        }
        let verdict = output
            .structured_output
            .ok_or_else(|| "Claude check output has no structured_output".to_owned())
            .and_then(|value| {
                serde_json::from_value(value)
                    .map_err(|error| format!("invalid Claude check verdict: {error}"))
            });
        match verdict {
            Ok(verdict) => result.verdict = Some(verdict),
            Err(error) => result.error = Some(error),
        }
        result
    }
}
