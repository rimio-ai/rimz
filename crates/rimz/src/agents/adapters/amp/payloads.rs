//! Typed payload for the RimZ-owned Amp plugin wire.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub(super) struct AmpHookPayload {
    pub session_id: Option<String>,
    pub prompt: Option<String>,
    pub files_modified: Option<bool>,
    pub status: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub last_assistant_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::payload::decode_or_default;
    use serde_json::json;

    #[test]
    fn payload_parse_is_tolerant() {
        assert!(
            decode_or_default::<AmpHookPayload>(&json!("junk"))
                .session_id
                .is_none()
        );
        let parsed = decode_or_default::<AmpHookPayload>(&json!({
            "session_id": "T-abc123",
            "unknown": true,
            "files_modified": true
        }));
        assert_eq!(parsed.session_id.as_deref(), Some("T-abc123"));
        assert_eq!(parsed.files_modified, Some(true));
    }
}
