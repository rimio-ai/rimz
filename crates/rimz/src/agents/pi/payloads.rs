//! Typed Pi hook wire structs.
//!
//! One tolerant struct covers every event the Rimz extension forwards — the
//! fields are sparse per event, so a single optional-field shape beats a set
//! of near-empty ones. The wire is Rimz-authored ([`extension.ts`](./extension.ts)
//! flattens pi's in-process payloads to snake_case), so drift is a Rimz bug,
//! not an upstream one; the upstream shapes are mirrored in
//! `docs/externals/agent-adapter/pi-reference.md`.

use serde::Deserialize;
use serde_json::Value;

/// Pi's compaction cause, added to extension events in 0.79.10.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PiCompactionReason {
    Manual,
    Threshold,
    Overflow,
    #[serde(other)]
    Unknown,
}

impl PiCompactionReason {
    pub(crate) const fn auto_flag(&self) -> Option<bool> {
        match self {
            Self::Manual => Some(false),
            Self::Threshold | Self::Overflow => Some(true),
            Self::Unknown => None,
        }
    }
}

/// The flattened payload the Rimz pi extension posts for every event.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct PiHookPayload {
    /// Every event after Pi reports session metadata: the `/name` title.
    pub session_name: Option<String>,
    /// `before_agent_start`: the user's prompt.
    pub prompt: Option<String>,
    /// `agent_end` and the following `agent_settled`: the last assistant
    /// message's `stopReason`
    /// (`stop` | `length` | `toolUse` | `error` | `aborted`).
    pub stop_reason: Option<String>,
    /// `agent_end`: present when the turn died — the in-band death
    /// certificate (no transcript forensics needed, unlike Claude).
    pub error_message: Option<String>,
    /// `agent_end`, retained through `agent_settled`: visible text from the
    /// last assistant message, for durable logs and supervised output.
    pub last_assistant_message: Option<String>,
    /// Every event: the session's model id (`ctx.model.id`); `agent_end`
    /// overrides it with the last assistant message's model when present.
    pub model: Option<String>,
    /// Every event: pi's thinking level (`off` | `minimal` | … | `max`),
    /// the closest pi has to Claude's effort.
    pub effort: Option<String>,
    /// Every event: the model's context window in tokens, from
    /// `ctx.getContextUsage()`.
    pub context_window: Option<u64>,
    /// Every event: cumulative session tokens from `ctx.getContextUsage()`
    /// (rounded on the wire); `agent_end` overrides with the last assistant
    /// message's `usage.totalTokens` when present.
    pub total_tokens: Option<u64>,
    /// Every event after the first turn: cumulative session cost the extension
    /// accumulates from `usage.cost.total`; resumed branches are hydrated.
    pub total_cost_usd: Option<f64>,
    /// Latest provider call: fresh input, excluding cache reads and cache writes.
    pub input_tokens: Option<u64>,
    /// Latest provider call: generated output.
    pub output_tokens: Option<u64>,
    /// Latest provider call: cache-read input.
    pub cache_read_input_tokens: Option<u64>,
    /// Latest provider call: cache-write/cache-creation input.
    pub cache_write_input_tokens: Option<u64>,
    /// Compaction events: manual `/compact`, threshold compaction, or overflow
    /// recovery. Older Pi releases omit this field.
    pub compaction_reason: Option<PiCompactionReason>,
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
        let priced = parse_payload(&json!({ "total_cost_usd": 0.125 }));
        assert_eq!(priced.total_cost_usd, Some(0.125));
        // A type mismatch anywhere degrades the whole payload to default
        // rather than erroring — enrichment, never correctness.
        let typed = parse_payload(&json!({
            "total_cost_usd": "not a number",
            "total_tokens": 10,
            "prompt": "p"
        }));
        assert!(typed.total_cost_usd.is_none());
        assert!(typed.total_tokens.is_none());
        assert!(typed.prompt.is_none());
    }

    #[test]
    fn compaction_reason_maps_manual_and_automatic_causes() {
        for (reason, expected) in [
            ("manual", Some(false)),
            ("threshold", Some(true)),
            ("overflow", Some(true)),
            ("future", None),
        ] {
            let parsed = parse_payload(&json!({ "compaction_reason": reason }));
            assert_eq!(
                parsed
                    .compaction_reason
                    .as_ref()
                    .and_then(PiCompactionReason::auto_flag),
                expected,
                "{reason}"
            );
        }
    }
}
