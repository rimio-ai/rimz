//! Consumed fields on Qoder's Claude-shaped native hook envelopes.

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{CompactTrigger, HookEventCommon, SessionSource};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct QoderHook {
    #[serde(flatten)]
    pub(super) common: HookEventCommon,
    pub(super) source: SessionSource,
    pub(super) trigger: CompactTrigger,
    pub(super) reason: Option<String>,
    pub(super) model: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) tool_use_id: Option<String>,
    pub(super) agent_id: Option<String>,
    pub(super) agent_type: Option<String>,
    pub(super) last_assistant_message: Option<String>,
    pub(super) error: QoderStopError,
}

impl QoderHook {
    pub(super) fn parse(payload: &Value) -> Self {
        serde_json::from_value(payload.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum QoderStopError {
    RateLimit,
    ServerError,
    Overloaded,
    #[default]
    #[serde(other)]
    Other,
}
