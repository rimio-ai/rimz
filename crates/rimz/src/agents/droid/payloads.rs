//! Typed input structs for Factory Droid's native hook protocol.
//!
//! Droid's common fields and lifecycle enums mirror Claude's hook wire. Sparse,
//! malformed, and forward-extended payloads fall back to defaults so hooks stay
//! best-effort enrichment rather than a reason to interrupt the agent.
use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::SessionSource;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidSessionStart {
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidUserPromptSubmit {
    pub prompt: Option<String>,
}

macro_rules! parse_fn {
    ($name:ident, $ty:ty) => {
        pub fn $name(payload: &Value) -> $ty {
            serde_json::from_value(payload.clone()).unwrap_or_default()
        }
    };
}

parse_fn!(parse_session_start, DroidSessionStart);
parse_fn!(parse_user_prompt_submit, DroidUserPromptSubmit);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parsers_keep_consumed_fields_and_tolerate_drift() {
        let start = parse_session_start(&json!({
            "source": "startup",
            "future_field": {"nested": true},
        }));
        assert_eq!(start.source, SessionSource::Startup);
        assert_eq!(
            parse_session_start(&json!([])).source,
            SessionSource::Startup
        );
        assert_eq!(
            parse_user_prompt_submit(&json!({"prompt": "ship it"}))
                .prompt
                .as_deref(),
            Some("ship it")
        );
    }
}
