//! Tolerant Kiro v3 hook payload parsing.
//!
//! Kiro documents JSON stdin for command hooks but does not publish its
//! object schema. Every field stays optional and accepts plausible casing
//! aliases until live fixtures can pin the wire.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct KiroHookPayload {
    #[serde(
        alias = "sessionId",
        alias = "conversation_id",
        alias = "conversationId"
    )]
    pub session_id: Option<String>,
    #[serde(alias = "user_prompt", alias = "userPrompt")]
    pub prompt: Option<String>,
    #[serde(alias = "toolName")]
    pub tool_name: Option<String>,
}

pub(crate) fn parse_payload(payload: &Value) -> KiroHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}
