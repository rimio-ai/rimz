//! Versioned canonical JSON envelope spoken by agent plugin shims.

use serde::Deserialize;

use crate::agents::{AgentRateLimits, AskKind};

use super::manifest::PROTOCOL_VERSION;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EnvelopeParseError {
    InvalidJson(String),
    UnsupportedProtocol { found: u32 },
    EventNameMismatch { expected: String, found: String },
}

impl std::fmt::Display for EnvelopeParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "invalid canonical envelope: {error}"),
            Self::UnsupportedProtocol { found } => write!(
                formatter,
                "unsupported protocol {found}; expected {PROTOCOL_VERSION}"
            ),
            Self::EventNameMismatch { expected, found } => write!(
                formatter,
                "hook_event_name `{found}` does not match replay event `{expected}`"
            ),
        }
    }
}

pub(super) const CANONICAL_EVENTS: &[&str] = &[
    "session_start",
    "turn_start",
    "turn_end",
    "tool_use",
    "awaiting_input",
    "compaction_start",
    "compaction_end",
    "subagent_start",
    "subagent_end",
    "session_end",
    "context",
];

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Envelope {
    pub protocol: u32,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context_pct: Option<u64>,
    pub context_window: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub rate_limits: Option<AgentRateLimits>,
    pub transcript_path: Option<String>,
    #[serde(flatten)]
    pub event: CanonicalEvent,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "hook_event_name", rename_all = "snake_case")]
pub(super) enum CanonicalEvent {
    SessionStart,
    TurnStart {
        prompt: Option<String>,
    },
    TurnEnd {
        #[serde(default)]
        errored: bool,
        error_message: Option<String>,
        last_assistant_message: Option<String>,
    },
    ToolUse {
        tool_name: Option<String>,
        #[serde(default)]
        is_error: bool,
    },
    AwaitingInput {
        ask: AskKind,
        question: Option<String>,
    },
    CompactionStart,
    CompactionEnd {
        trigger: Option<CompactionTrigger>,
    },
    SubagentStart,
    SubagentEnd {
        #[serde(default)]
        errored: bool,
    },
    SessionEnd,
    Context,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CompactionTrigger {
    Auto,
    Manual,
}

impl Envelope {
    pub(super) fn parse(event_name: &str, payload: &serde_json::Value) -> Option<Self> {
        Self::parse_diagnostic(event_name, payload).ok()
    }

    pub(super) fn parse_diagnostic(
        event_name: &str,
        payload: &serde_json::Value,
    ) -> Result<Self, EnvelopeParseError> {
        let envelope: Self = serde_json::from_value(payload.clone())
            .map_err(|error| EnvelopeParseError::InvalidJson(error.to_string()))?;
        if envelope.protocol != PROTOCOL_VERSION {
            return Err(EnvelopeParseError::UnsupportedProtocol {
                found: envelope.protocol,
            });
        }
        let payload_event = payload
            .get("hook_event_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| envelope.event.name());
        if payload_event != event_name {
            return Err(EnvelopeParseError::EventNameMismatch {
                expected: event_name.to_owned(),
                found: payload_event.to_owned(),
            });
        }
        Ok(envelope)
    }
}

impl CanonicalEvent {
    pub(super) const fn name(&self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::TurnStart { .. } => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::ToolUse { .. } => "tool_use",
            Self::AwaitingInput { .. } => "awaiting_input",
            Self::CompactionStart => "compaction_start",
            Self::CompactionEnd { .. } => "compaction_end",
            Self::SubagentStart => "subagent_start",
            Self::SubagentEnd { .. } => "subagent_end",
            Self::SessionEnd => "session_end",
            Self::Context => "context",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn diagnostic_parse_reports_serde_version_and_event_name_failures() {
        let malformed = Envelope::parse_diagnostic(
            "awaiting_input",
            &json!({
                "protocol": 1,
                "hook_event_name": "awaiting_input",
                "ask": "not-an-ask-kind"
            }),
        )
        .unwrap_err();
        assert!(matches!(malformed, EnvelopeParseError::InvalidJson(_)));
        assert!(malformed.to_string().contains("unknown variant"));

        assert_eq!(
            Envelope::parse_diagnostic(
                "session_start",
                &json!({ "protocol": 2, "hook_event_name": "session_start" })
            )
            .unwrap_err(),
            EnvelopeParseError::UnsupportedProtocol { found: 2 }
        );
        assert_eq!(
            Envelope::parse_diagnostic(
                "turn_start",
                &json!({ "protocol": 1, "hook_event_name": "turn_end" })
            )
            .unwrap_err(),
            EnvelopeParseError::EventNameMismatch {
                expected: "turn_start".into(),
                found: "turn_end".into(),
            }
        );
    }

    #[test]
    fn diagnostic_parse_accepts_unknown_events_for_forward_compatibility() {
        let envelope = Envelope::parse_diagnostic(
            "future_event",
            &json!({ "protocol": 1, "hook_event_name": "future_event" }),
        )
        .unwrap();
        assert!(matches!(envelope.event, CanonicalEvent::Unknown));
    }
}
