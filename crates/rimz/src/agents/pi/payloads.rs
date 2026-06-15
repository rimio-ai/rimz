//! Typed Pi hook wire structs.
//!
//! One tolerant struct covers every event the Rimz extension forwards — the
//! fields are sparse per event, so a single optional-field shape beats a set
//! of near-empty ones. The wire is Rimz-authored ([`extension.ts`](./extension.ts)
//! flattens pi's in-process payloads to snake_case), so drift is a Rimz bug,
//! not an upstream one; the upstream shapes are mirrored in
//! `docs/internals/adapter/pi-reference.md`.

use serde::Deserialize;
use serde_json::Value;

/// The flattened payload the Rimz pi extension posts for every event.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PiHookPayload {
    /// `before_agent_start`: the user's prompt.
    pub prompt: Option<String>,
    /// `agent_end`: the last assistant message's `stopReason`
    /// (`stop` | `length` | `toolUse` | `error` | `aborted`).
    pub stop_reason: Option<String>,
    /// `agent_end`: present when the turn died — the in-band death
    /// certificate (no transcript forensics needed, unlike Claude).
    pub error_message: Option<String>,
    /// Every event: the session's model id (`ctx.model.id`); `agent_end`
    /// overrides it with the last assistant message's model when present.
    pub model: Option<String>,
    /// Every event: pi's thinking level (`off` | `minimal` | … | `highest`),
    /// the closest pi has to Claude's effort.
    pub effort: Option<String>,
    /// Every event: the model's context window in tokens, from
    /// `ctx.getContextUsage()`.
    pub context_window: Option<u64>,
    /// Every event: cumulative session tokens from `ctx.getContextUsage()`
    /// (rounded on the wire); `agent_end` overrides with the last assistant
    /// message's `usage.totalTokens` when present.
    pub total_tokens: Option<u64>,
    /// Latest provider call: fresh input, excluding cache reads and cache writes.
    pub input_tokens: Option<u64>,
    /// Latest provider call: generated output.
    pub output_tokens: Option<u64>,
    /// Latest provider call: cache-read input.
    pub cache_read_input_tokens: Option<u64>,
    /// Latest provider call: cache-write/cache-creation input.
    pub cache_write_input_tokens: Option<u64>,
}

/// Tolerant parse: any non-conforming payload reads as the empty default —
/// enrichment, never an error.
pub(crate) fn parse_payload(payload: &Value) -> PiHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

/// Whether an `agent_end` payload reports a dead turn: an explicit error or
/// abort `stopReason`, or any `errorMessage` riding the last assistant
/// message.
pub(crate) fn agent_end_errored(parsed: &PiHookPayload) -> bool {
    matches!(parsed.stop_reason.as_deref(), Some("error" | "aborted"))
        || parsed
            .error_message
            .as_deref()
            .is_some_and(|message| !message.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_end_error_signals() {
        let errored = parse_payload(&json!({ "stop_reason": "error" }));
        assert!(agent_end_errored(&errored));
        let aborted = parse_payload(&json!({ "stop_reason": "aborted" }));
        assert!(agent_end_errored(&aborted));
        let message_only =
            parse_payload(&json!({ "stop_reason": "stop", "error_message": "boom" }));
        assert!(agent_end_errored(&message_only));
        let clean = parse_payload(&json!({ "stop_reason": "stop" }));
        assert!(!agent_end_errored(&clean));
        assert!(!agent_end_errored(&parse_payload(&json!({}))));
    }

    #[test]
    fn tolerant_parse_degrades_to_the_empty_default() {
        let parsed = parse_payload(&json!("not an object"));
        assert!(parsed.prompt.is_none());
        // A type mismatch anywhere degrades the whole payload to default
        // rather than erroring — enrichment, never correctness.
        let typed = parse_payload(&json!({ "total_tokens": "not a number", "prompt": "p" }));
        assert!(typed.total_tokens.is_none());
        assert!(typed.prompt.is_none());
    }
}
