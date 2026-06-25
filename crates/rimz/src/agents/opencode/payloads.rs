//! Typed OpenCode plugin wire structs.
//!
//! Rimz owns the OpenCode wire: [`plugin.ts`](./plugin.ts) flattens the
//! in-process OpenCode hook and bus payloads into this snake_case envelope
//! before spawning `rimz hooks feed --source opencode`. Upstream drift is
//! contained inside the plugin; Rust receives the stable adapter-local shape.

use serde::Deserialize;
use serde_json::Value;

/// The flattened payload the Rimz OpenCode plugin posts for every event.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct OpencodeHookPayload {
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    #[allow(
        dead_code,
        reason = "parsed to pin the plugin envelope; refresh spawning reads raw payload JSON"
    )]
    pub server_url: Option<String>,
    pub prompt: Option<String>,
    pub is_error: Option<bool>,
    pub error_message: Option<String>,
    pub error_class: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub context_window: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
}

/// Tolerant parse: unusable payloads degrade to an empty enrichment record.
pub(crate) fn parse_payload(payload: &Value) -> OpencodeHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

pub(crate) fn errored(parsed: &OpencodeHookPayload) -> bool {
    parsed.is_error.unwrap_or(false)
        || parsed
            .error_message
            .as_deref()
            .is_some_and(|message| !message.is_empty())
        || parsed
            .error_class
            .as_deref()
            .is_some_and(|class| !class.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errored_reads_boolean_message_and_class_markers() {
        assert!(errored(&parse_payload(&json!({ "is_error": true }))));
        assert!(errored(&parse_payload(
            &json!({ "error_message": "provider failed" })
        )));
        assert!(errored(&parse_payload(
            &json!({ "error_class": "ApiError" })
        )));
        assert!(!errored(&parse_payload(&json!({ "is_error": false }))));
        assert!(!errored(&parse_payload(&json!({}))));
    }

    #[test]
    fn tolerant_parse_degrades_to_empty_default() {
        let parsed = parse_payload(&json!("not an object"));
        assert!(parsed.session_id.is_none());
        let typed = parse_payload(&json!({ "total_tokens": "bad", "session_id": "ses_1" }));
        assert!(typed.session_id.is_none());
        assert!(typed.total_tokens.is_none());
    }

    #[test]
    fn parse_payload_reads_server_url() {
        let parsed = parse_payload(&json!({
            "session_id": "ses_1",
            "server_url": "http://127.0.0.1:4096/"
        }));
        assert_eq!(parsed.session_id.as_deref(), Some("ses_1"));
        assert_eq!(parsed.server_url.as_deref(), Some("http://127.0.0.1:4096/"));
    }
}
