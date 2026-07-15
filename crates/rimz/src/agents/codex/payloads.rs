//! Typed input structs for the Codex hook protocol.
//!
//! Structs contain only fields RimZ consumes. All use `#[serde(default)]` so
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
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub turn_id: Option<String>,
}

/// Optional identity stamped on hooks that fire inside a Codex child thread.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexChildIdentity {
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
}

// ── Per-event input structs ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSessionStart {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexUserPromptSubmit {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSubagentStart {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
}

/// Silent lifecycle event. `tool_name` and `tool_input` are available for audit
/// enrichment; `tool_use_id` correlates with the corresponding PostToolUse.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPreToolUse {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_input: Option<Value>,
}

/// Blocking ask event. `tool_name` and `tool_input` are available in
/// `classify_hook` for naming the waiting kind.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPermissionRequest {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub tool_name: Option<String>,
    /// Full tool input object; `tool_input.description` (if present) is reached
    /// as `tool_input.get("description")`.
    pub tool_input: Option<Value>,
}

/// Silent lifecycle event. `tool_response` is available for audit enrichment.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPostToolUse {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexSubagentStop {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub agent_transcript_path: Option<String>,
    pub stop_hook_active: Option<bool>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexStop {
    pub stop_hook_active: Option<bool>,
    /// Final assistant message text from the completed turn.
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPreCompact {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
    pub trigger: CompactTrigger,
}

/// Fires after conversation compaction completes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPostCompact {
    #[serde(flatten)]
    pub common: CodexCommon,
    #[serde(flatten)]
    pub child: CodexChildIdentity,
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
parse_fn!(parse_pre_compact, CodexPreCompact);
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
            "agent_transcript_path": "/tmp/child.jsonl",
            "stop_hook_active": true,
            "last_assistant_message": "done",
        }));
        assert_eq!(subagent.child.agent_id.as_deref(), Some("child-1"));
        assert_eq!(subagent.child.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            subagent.agent_transcript_path.as_deref(),
            Some("/tmp/child.jsonl")
        );
        assert_eq!(subagent.stop_hook_active, Some(true));
        assert_eq!(subagent.last_assistant_message.as_deref(), Some("done"));
        assert!(
            parse_permission_request(&json!({
                "session_id": "root",
                "agent_id": "child-1",
                "agent_type": "reviewer",
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf /"}
            }))
            .tool_input
            .is_some()
        );
        let prompt = parse_user_prompt_submit(&json!({
            "session_id": "root",
            "agent_id": "child-1",
            "agent_type": "reviewer",
            "prompt": "continue"
        }));
        assert_eq!(prompt.child.agent_id.as_deref(), Some("child-1"));
        assert_eq!(
            parse_post_compact(&json!({"trigger": "manual"})).trigger,
            CompactTrigger::Manual
        );

        // V2 nickname, task-path, and token usage are rollout/app-server data,
        // not fields in the stable hook contract.
        let hook = json!({
            "session_id": "root",
            "agent_id": "child-1",
            "agent_type": "default"
        });
        assert!(hook.get("agent_nickname").is_none());
        assert!(hook.get("agent_path").is_none());
        assert!(hook.get("last_token_usage").is_none());
    }
}
