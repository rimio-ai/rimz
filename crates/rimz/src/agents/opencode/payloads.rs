//! Typed OpenCode plugin wire structs.
//!
//! RimZ owns the OpenCode wire: [`plugin.ts`](./plugin.ts) flattens the
//! in-process OpenCode hook and bus payloads into this snake_case envelope
//! before spawning `rimz hooks feed --source opencode`. Upstream drift is
//! contained inside the plugin; Rust receives the stable adapter-local shape.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct OpencodeQuestion {
    pub question: Option<String>,
    pub options: Option<Vec<OpencodeQuestionOption>>,
    pub multiple: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct OpencodeQuestionOption {
    pub label: Option<String>,
    pub description: Option<String>,
}

/// The flattened payload the RimZ OpenCode plugin posts for every event.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct OpencodeHookPayload {
    pub session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub prompt: Option<String>,
    pub title: Option<String>,
    pub questions: Option<Vec<OpencodeQuestion>>,
    pub reply: Option<String>,
    pub answers: Option<Vec<Vec<String>>>,
    pub plan_proposed: Option<bool>,
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
}
