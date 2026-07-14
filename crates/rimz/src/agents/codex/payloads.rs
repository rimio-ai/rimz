//! Typed input structs for the Codex hook protocol.
//!
//! Structs contain only fields Rimz consumes. All use `#[serde(default)]` so
//! sparse payloads deserialize cleanly; `parse_*` functions are adapter entry
//! points.

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{CompactTrigger, HookEventCommon, SessionSource};

// ── Common ─────────────────────────────────────────────────────────────────

/// Codex-specific wrapper around universal hook identity fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
}

// ── Per-event input structs ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSessionStart {
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexUserPromptSubmit {
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSubagentStart {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
}

/// Silent lifecycle event. `tool_name` and `tool_input` are available for audit
/// enrichment; `tool_use_id` correlates with the corresponding PostToolUse.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPreToolUse {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
}

/// Blocking ask event. `tool_name` and `tool_input` are available in
/// `classify_hook` for naming the waiting kind.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPermissionRequest {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub tool_name: Option<String>,
    /// Full tool input object; `tool_input.description` (if present) is reached
    /// as `tool_input.get("description")`.
    pub tool_input: Option<Value>,
}

/// Silent lifecycle event. `tool_response` is available for audit enrichment.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPostToolUse {
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSubagentStop {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexStop {
    /// Final assistant message text from the completed turn.
    pub last_assistant_message: Option<String>,
}

/// Fires after conversation compaction completes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPostCompact {
    pub trigger: CompactTrigger,
}

// ── Parse helpers ──────────────────────────────────────────────────────────

macro_rules! parse_fn {
    ($fn_name:ident, $ty:ty) => {
        pub fn $fn_name(payload: &Value) -> $ty {
            serde_json::from_value(payload.clone()).unwrap_or_default()
        }
    };
}

parse_fn!(parse_session_start, CodexSessionStart);
parse_fn!(parse_user_prompt_submit, CodexUserPromptSubmit);
parse_fn!(parse_subagent_start, CodexSubagentStart);
parse_fn!(parse_pre_tool_use, CodexPreToolUse);
parse_fn!(parse_permission_request, CodexPermissionRequest);
parse_fn!(parse_post_tool_use, CodexPostToolUse);
parse_fn!(parse_subagent_stop, CodexSubagentStop);
parse_fn!(parse_stop, CodexStop);
parse_fn!(parse_post_compact, CodexPostCompact);

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::hook_types::{CompactTrigger, SessionSource};

    #[test]
    fn wire_catalog_parses_flatten_depth_enums_and_tolerates_drift() {
        // The doubly-flattened common fields reach through `common.common`, an
        // unknown future field is ignored, and the session-source enum maps.
        let session = parse_session_start(&json!({
            "session_id": "s1",
            "source": "startup",
            "future_openai_field": true,
        }));
        assert_eq!(session.source, SessionSource::Startup);
        assert_eq!(
            parse_session_start(&json!({"source": "compact"})).source,
            SessionSource::Compact
        );

        // `#[serde(default)]` makes a sparse payload deserialize cleanly.
        let sparse = parse_stop(&json!({}));
        assert_eq!(sparse.last_assistant_message, None);

        // The richer tool/subagent/compaction catalog entries pick up their
        // enrichment fields and map the compaction-trigger enum.
        let subagent = parse_subagent_stop(&json!({
            "agent_id": "child-1",
            "agent_type": "code-reviewer",
        }));
        assert_eq!(subagent.agent_id.as_deref(), Some("child-1"));
        assert_eq!(subagent.agent_type.as_deref(), Some("code-reviewer"));
        assert!(
            parse_permission_request(
                &json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf /"}})
            )
            .tool_input
            .is_some()
        );
        assert_eq!(
            parse_post_compact(&json!({"trigger": "manual"})).trigger,
            CompactTrigger::Manual
        );
    }
}
