//! Versioned canonical JSON envelope spoken by agent plugin shims.

use serde::Deserialize;

use crate::agents::{AgentHookClass, AgentRateLimits, AskKind, LifecycleSignal};
use crate::transcript::AskQuestion;

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

    pub(super) fn normalize(&self, mutates: bool, edits: bool) -> NormalizedCanonicalEvent {
        match self {
            Self::SessionStart => {
                NormalizedCanonicalEvent::lifecycle(LifecycleSignal::Registered, true)
            }
            Self::TurnStart { prompt } => NormalizedCanonicalEvent {
                signal: Some(LifecycleSignal::TurnStarted),
                prompt: prompt.clone(),
                progress: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::TurnEnd {
                errored,
                error_message,
                last_assistant_message,
            } => NormalizedCanonicalEvent {
                signal: Some(LifecycleSignal::TurnEnded {
                    errored: *errored,
                    parked_on_background: false,
                }),
                turn_error: errored.then(|| error_message.clone()).flatten(),
                final_message: last_assistant_message.clone(),
                progress: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::ToolUse { .. } => NormalizedCanonicalEvent::lifecycle(
                LifecycleSignal::ToolUsed {
                    mutates,
                    edits,
                    native_key: None,
                },
                true,
            ),
            Self::AwaitingInput { ask, question } => {
                let questions = question
                    .as_deref()
                    .map(str::trim)
                    .filter(|question| !question.is_empty())
                    .map(|question| {
                        vec![AskQuestion {
                            question: question.to_owned(),
                            options: Vec::new(),
                            multi_select: false,
                            has_option_previews: false,
                        }]
                    })
                    .unwrap_or_default();
                let detail = questions.first().map(|question| question.question.clone());
                NormalizedCanonicalEvent {
                    class: AgentHookClass::AwaitingUser,
                    ask_kind: Some(*ask),
                    signal: Some(LifecycleSignal::AwaitingInput {
                        kind: *ask,
                        ask_id: None,
                        detail: detail.clone(),
                        native_key: None,
                    }),
                    questions,
                    ask_detail: detail,
                    ..NormalizedCanonicalEvent::default()
                }
            }
            Self::CompactionStart => {
                NormalizedCanonicalEvent::lifecycle(LifecycleSignal::Compacting, true)
            }
            Self::CompactionEnd { trigger } => NormalizedCanonicalEvent::lifecycle(
                LifecycleSignal::CompactionEnded {
                    auto: trigger.map(|trigger| matches!(trigger, CompactionTrigger::Auto)),
                },
                true,
            ),
            Self::SubagentStart => NormalizedCanonicalEvent {
                signal: Some(LifecycleSignal::SubagentStarted),
                is_subagent: true,
                progress: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::SubagentEnd { errored } => NormalizedCanonicalEvent {
                signal: Some(LifecycleSignal::SubagentStopped { errored: *errored }),
                is_subagent: true,
                progress: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::SessionEnd => NormalizedCanonicalEvent {
                signal: Some(LifecycleSignal::Ended),
                session_ended: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::Context => NormalizedCanonicalEvent {
                context: true,
                ..NormalizedCanonicalEvent::default()
            },
            Self::Unknown => NormalizedCanonicalEvent {
                class: AgentHookClass::Unknown,
                ..NormalizedCanonicalEvent::default()
            },
        }
    }

    pub(super) fn tool(&self) -> Option<(&str, bool)> {
        match self {
            Self::ToolUse {
                tool_name: Some(tool_name),
                is_error,
            } => Some((tool_name, *is_error)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct NormalizedCanonicalEvent {
    pub class: AgentHookClass,
    pub ask_kind: Option<AskKind>,
    pub signal: Option<LifecycleSignal>,
    pub questions: Vec<AskQuestion>,
    pub ask_detail: Option<String>,
    pub turn_error: Option<String>,
    pub final_message: Option<String>,
    pub prompt: Option<String>,
    pub progress: bool,
    pub session_ended: bool,
    pub is_subagent: bool,
    pub context: bool,
}

impl Default for NormalizedCanonicalEvent {
    fn default() -> Self {
        Self {
            class: AgentHookClass::Lifecycle,
            ask_kind: None,
            signal: None,
            questions: Vec::new(),
            ask_detail: None,
            turn_error: None,
            final_message: None,
            prompt: None,
            progress: false,
            session_ended: false,
            is_subagent: false,
            context: false,
        }
    }
}

impl NormalizedCanonicalEvent {
    fn lifecycle(signal: LifecycleSignal, progress: bool) -> Self {
        Self {
            signal: Some(signal),
            progress,
            ..Self::default()
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
