use std::path::Path;

use serde::Deserialize;

use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::skills::{LaunchSettingsArtifact, LaunchSettingsErr};
use crate::agents::{HeadlessForm, HeadlessRequest, HeadlessResult};

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ExecEvent {
    #[serde(rename = "turn.completed")]
    Completed { usage: Usage },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
}

#[derive(Debug)]
pub(super) struct CodexHeadless;

impl HeadlessForm for CodexHeadless {
    fn render_argv(
        &self,
        extra_args: &[String],
        prompt: Option<&str>,
        request: &HeadlessRequest,
        _cwd: &Path,
        _settings: (&Path, &mut Option<LaunchSettingsArtifact>),
    ) -> Result<Vec<String>, LaunchSettingsErr> {
        let mut argv = vec!["codex".to_owned()];
        argv.extend(
            extra_args
                .iter()
                .filter(|arg| arg.as_str() != "--no-daemon")
                .cloned(),
        );
        argv.extend([
            "-c".into(),
            "features.hooks=false".into(),
            "exec".into(),
            "--json".into(),
            "--output-schema".into(),
            request.schema_file.display().to_string(),
            "-o".into(),
            request.verdict_file.display().to_string(),
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
        request: &HeadlessRequest,
        model: Option<&str>,
        prices: &PriceBook,
    ) -> HeadlessResult {
        let mut result = HeadlessResult::default();
        for line in stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        {
            let Ok(event) = serde_json::from_slice::<ExecEvent>(line) else {
                continue;
            };
            let ExecEvent::Completed { usage } = event else {
                continue;
            };
            result.input_tokens = Some(usage.input_tokens);
            result.output_tokens = Some(usage.output_tokens);
            let split = TokenSplit::new(
                usage.input_tokens.saturating_sub(usage.cached_input_tokens),
                usage.output_tokens,
            )
            .cached(0, usage.cached_input_tokens);
            result.cost_usd = model
                .filter(|model| crate::agents::spending::is_priceable_model_name(model))
                .and_then(|model| prices.price(model))
                .map(|price| price.cost_of(super::spend::codex_billed_split(price, split)));
        }
        let verdict = std::fs::read(&request.verdict_file)
            .map_err(|error| {
                format!(
                    "reading Codex check verdict `{}`: {error}",
                    request.verdict_file.display()
                )
            })
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("invalid Codex check verdict: {error}"))
            });
        match verdict {
            Ok(verdict) => result.verdict = Some(verdict),
            Err(error) => result.error = Some(error),
        }
        result
    }
}
