//! Typed payload for the Rimz-owned Amp plugin wire.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AmpHookPayload {
    pub session_id: Option<String>,
    pub prompt: Option<String>,
    pub files_modified: Option<bool>,
    pub status: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub last_assistant_message: Option<String>,
}

pub(crate) fn parse_payload(payload: &Value) -> AmpHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_parse_is_tolerant() {
        assert!(parse_payload(&json!("junk")).session_id.is_none());
        let parsed = parse_payload(&json!({
            "session_id": "T-abc123",
            "unknown": true,
            "files_modified": true
        }));
        assert_eq!(parsed.session_id.as_deref(), Some("T-abc123"));
        assert_eq!(parsed.files_modified, Some(true));
    }
}
