//! Typed input structs for Factory Droid's native hook protocol.
//!
//! Droid's common fields and lifecycle enums mirror Claude's hook wire. Sparse,
//! malformed, and forward-extended payloads fall back to defaults so hooks stay
//! best-effort enrichment rather than a reason to interrupt the agent.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{CompactTrigger, HookEventCommon, SessionSource};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
    pub permission_mode: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidSessionStart {
    #[serde(flatten)]
    pub common: DroidCommon,
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidUserPromptSubmit {
    #[serde(flatten)]
    pub common: DroidCommon,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidPreCompact {
    #[serde(flatten)]
    pub common: DroidCommon,
    pub trigger: CompactTrigger,
    pub custom_instructions: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidStop {
    #[serde(flatten)]
    pub common: DroidCommon,
    pub stop_hook_active: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DroidSessionEnd {
    #[serde(flatten)]
    pub common: DroidCommon,
    pub reason: Option<String>,
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
parse_fn!(parse_pre_compact, DroidPreCompact);
parse_fn!(parse_stop, DroidStop);
parse_fn!(parse_session_end, DroidSessionEnd);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parsers_flatten_common_fields_and_tolerate_drift() {
        let start = parse_session_start(&json!({
            "session_id": "sess-1",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/tmp/project",
            "permission_mode": "default",
            "source": "startup",
            "future_field": {"nested": true},
        }));
        assert_eq!(start.common.common.session_id.as_deref(), Some("sess-1"));
        assert_eq!(start.common.permission_mode.as_deref(), Some("default"));
        assert_eq!(start.source, SessionSource::Startup);
        assert_eq!(
            parse_session_start(&json!([])).source,
            SessionSource::Startup
        );
        assert_eq!(
            parse_pre_compact(&json!({"trigger": "auto"})).trigger,
            CompactTrigger::Auto
        );
    }
}
