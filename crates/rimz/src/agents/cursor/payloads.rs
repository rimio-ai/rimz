//! Tolerant Cursor hook payloads.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_string_lossy,
    deserialize_optional_u64_lossy,
};

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ModelParam {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StopOutcome {
    Completed,
    Aborted,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TurnUsage {
    pub fresh_input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[expect(
    dead_code,
    reason = "typed common and per-event fields pin Cursor's wire for future diagnostics"
)]
pub(super) struct CursorHookPayload {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub conversation_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub generation_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub model_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_model_params_lossy")]
    pub model_params: Vec<ModelParam>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub hook_event_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub cursor_version: Option<String>,
    #[serde(default, deserialize_with = "deserialize_strings_lossy")]
    pub workspace_roots: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub transcript_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub tool_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub trigger: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    pub context_usage_percent: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub context_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub context_window_size: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub reason: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub cache_write_tokens: Option<u64>,
}

pub(super) fn parse_payload(payload: &Value) -> CursorHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

impl CursorHookPayload {
    pub(super) fn model_param(&self, id: &str) -> Option<&str> {
        self.model_params.iter().find_map(|param| {
            (param.id.as_deref() == Some(id))
                .then_some(param.value.as_deref())
                .flatten()
        })
    }

    pub(super) fn stop_outcome(&self) -> StopOutcome {
        match self.status.as_deref() {
            Some("completed") => StopOutcome::Completed,
            Some("aborted") => StopOutcome::Aborted,
            _ => StopOutcome::Error,
        }
    }

    pub(super) fn turn_usage(&self) -> Option<TurnUsage> {
        (self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some())
        .then(|| {
            let cache_read = self.cache_read_tokens.unwrap_or(0);
            let cache_write = self.cache_write_tokens.unwrap_or(0);
            TurnUsage {
                fresh_input: self
                    .input_tokens
                    .map(|input| input.saturating_sub(cache_read.saturating_add(cache_write))),
                output: self.output_tokens,
                cache_read: self.cache_read_tokens,
                cache_write: self.cache_write_tokens,
            }
        })
    }
}

fn deserialize_model_params_lossy<'de, D>(deserializer: D) -> Result<Vec<ModelParam>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect())
}

fn deserialize_strings_lossy<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}
