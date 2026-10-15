//! OMP's RimZ-authored hook envelopes.

use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{
    AgentContext, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, RateLimitWindow,
    WindowSource,
};
use crate::agents::{ContextObservation, non_empty_trimmed};
use crate::transcript::{AskOption, AskQuestion};

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Payload {
    pub session_id: Option<String>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
    pub last_assistant_message: Option<String>,
    pub will_continue: Option<bool>,
    pub context_window: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub aborted: Option<bool>,
    pub skipped: Option<bool>,
}

pub(super) fn parse(payload: &Value) -> Payload {
    serde_json::from_value(payload.clone()).unwrap_or_else(|_| Payload {
        session_id: payload
            .get("session_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        ..Payload::default()
    })
}

#[derive(Deserialize)]
struct QuestionInput {
    questions: Vec<Question>,
}

#[derive(Deserialize)]
struct Question {
    question: String,
    #[serde(default)]
    options: Vec<AskOption>,
    #[serde(default)]
    multi: bool,
}

pub(super) fn questions(input: &Value) -> Vec<AskQuestion> {
    let Ok(input) = serde_json::from_value::<QuestionInput>(input.clone()) else {
        return Vec::new();
    };
    input
        .questions
        .into_iter()
        .filter_map(|q| {
            Some(AskQuestion {
                question: non_empty_trimmed(&q.question)?,
                options: q.options,
                multi_select: q.multi,
                has_option_previews: false,
            })
        })
        .collect()
}

pub(super) fn context(source: &str, payload: &Value) -> Option<ContextObservation> {
    let parsed = parse(payload);
    let id = parsed.session_id?;
    let current = AgentCurrentUsage {
        input_tokens: parsed.input_tokens,
        output_tokens: parsed.output_tokens,
        cache_creation_input_tokens: parsed.cache_write_input_tokens,
        cache_read_input_tokens: parsed.cache_read_input_tokens,
    };
    let tokens = AgentTokenUsage {
        context_window_size: parsed.context_window,
        used_percentage: crate::agents::observation::payload_context_pct(payload, None),
        current_context_tokens: parsed.total_tokens,
        current_usage: (!current.is_zero()).then_some(current),
        ..AgentTokenUsage::default()
    };
    let has_tokens = tokens.context_window_size.is_some()
        || tokens.used_percentage.is_some()
        || tokens.current_context_tokens.is_some()
        || tokens.current_usage.is_some();
    let windows = payload
        .get("rate_limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(window)
        .collect::<Vec<_>>();
    if parsed.model.is_none() && parsed.effort.is_none() && !has_tokens && windows.is_empty() {
        return None;
    }
    ContextObservation::new(
        id,
        AgentContext {
            model_id: parsed.model,
            effort: parsed.effort,
            tokens: has_tokens.then_some(tokens),
            rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
            ..AgentContext::new(source, jiff::Timestamp::now())
        },
    )
}

fn window(value: &Value) -> Option<RateLimitWindow> {
    let object = value.as_object()?;
    let used_percentage = object
        .get("used_percentage")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .map(|v| v.round().clamp(0.0, 100.0) as u8);
    let timestamp = |key| {
        object
            .get(key)
            .and_then(Value::as_i64)
            .and_then(|t| jiff::Timestamp::from_second(t).ok())
    };
    let resets_at = timestamp("resets_at");
    let duration_mins = object
        .get("duration_mins")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    if used_percentage.is_none() && resets_at.is_none() && duration_mins.is_none() {
        return None;
    }
    Some(RateLimitWindow {
        used_percentage,
        resets_at,
        duration_mins,
        observed_at: timestamp("observed_at"),
        source: WindowSource::BestEffort,
        ..RateLimitWindow::default()
    })
}
