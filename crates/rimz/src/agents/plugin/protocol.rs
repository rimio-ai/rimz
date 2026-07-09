//! Versioned canonical JSON envelope spoken by agent plugin shims.

use serde::Deserialize;

use crate::agents::{AgentRateLimits, AskKind};

use super::manifest::PROTOCOL_VERSION;

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
        let envelope: Self = serde_json::from_value(payload.clone()).ok()?;
        (envelope.protocol == PROTOCOL_VERSION && envelope.event.name() == event_name)
            .then_some(envelope)
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
