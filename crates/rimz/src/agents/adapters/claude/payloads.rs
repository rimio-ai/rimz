//! Typed input structs for the Claude Code hook protocol.
//!
//! Structs contain only fields RimZ consumes. All use `#[serde(default)]` so
//! sparse or out-of-spec payloads deserialize cleanly; `parse_*` functions are
//! the adapter entry points.

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{BackgroundTask, CompactTrigger, HookEventCommon, SessionSource};

// ── Common ─────────────────────────────────────────────────────────────────

/// Reasoning-effort marker (`effort: { "level": … }`). Claude carries it on the
/// tool-use-context events (`PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`)
/// when the model supports the parameter, so it rides the common fields.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookEffort {
    pub level: Option<String>,
}

/// Claude-specific common fields. Wraps the universal [`HookEventCommon`] and
/// adds the model id (with optional `[1m]` marker), the reasoning-effort
/// object, and subagent identity (`agent_id` / `agent_type` are present only
/// inside a subagent or under `--agent`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
    pub model: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// Reasoning effort (`effort.level`); present on tool-use-context events when
    /// the model supports the parameter, absent otherwise.
    pub effort: Option<HookEffort>,
}

// ── Per-event input structs ────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSessionStart {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeUserPromptSubmit {
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePreToolUse {
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
}

/// Silent lifecycle event. `tool_response` is available for audit enrichment.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePostToolUse {
    pub tool_name: Option<String>,
    pub tool_response: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeStop {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    /// In-flight background tasks (Claude Code v2.1.145+). An empty vec or
    /// absent field means a genuine turn end; any in-flight entry means the
    /// main thread has parked and will reawaken.
    pub background_tasks: Vec<BackgroundTask>,
    /// Session-scoped scheduled wakeups (Claude Code v2.1.145+). Any entry
    /// means this Stop is a park: Claude submits its prompt when due.
    pub session_crons: Vec<SessionCron>,
    /// Final assistant text, available without racing the transcript writer.
    pub last_assistant_message: Option<String>,
}

/// One scheduled wakeup in a Claude `Stop.session_crons` array.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SessionCron {
    pub id: Option<String>,
    pub schedule: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeStopFailure {
    pub error: Option<String>,
    pub last_assistant_message: Option<String>,
}

/// `agent_id` and `agent_type` are carried in [`ClaudeCommon`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSubagentStart {
    #[serde(flatten)]
    pub common: ClaudeCommon,
}

/// `agent_transcript_path` names the *child's* own transcript. The sibling
/// `transcript_path` on the same payload is the parent's, so a child's tokens
/// and model must be read from this field or not at all.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSubagentStop {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub agent_transcript_path: Option<String>,
}

/// Fires after context compaction completes. The `trigger` shape mirrors
/// `PreCompact` and decides whether the row resumes running or rests idle.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePostCompact {
    pub trigger: CompactTrigger,
}

/// Blocking ask event. `tool_name` and `tool_input` are available in
/// `classify_hook` for naming the waiting kind; they don't affect the decision
/// shape (which is `behavior: allow|deny`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePermissionRequest {
    pub tool_name: Option<String>,
}

// ── Parse helpers ──────────────────────────────────────────────────────────
//
// Each returns `T::default()` on deserialization failure, matching the current
// silent-`None` behavior of the raw `payload.get("field").and_then(...)` chains.

macro_rules! parse_fn {
    ($fn_name:ident, $ty:ty) => {
        pub fn $fn_name(payload: &Value) -> $ty {
            serde_json::from_value(payload.clone()).unwrap_or_default()
        }
    };
}

parse_fn!(parse_session_start, ClaudeSessionStart);
parse_fn!(parse_user_prompt_submit, ClaudeUserPromptSubmit);
parse_fn!(parse_pre_tool_use, ClaudePreToolUse);
parse_fn!(parse_post_tool_use, ClaudePostToolUse);
parse_fn!(parse_stop, ClaudeStop);
parse_fn!(parse_stop_failure, ClaudeStopFailure);
parse_fn!(parse_subagent_start, ClaudeSubagentStart);
parse_fn!(parse_subagent_stop, ClaudeSubagentStop);
parse_fn!(parse_post_compact, ClaudePostCompact);
parse_fn!(parse_permission_request, ClaudePermissionRequest);

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::hook_types::{CompactTrigger, SessionSource};

    /// The parse layer's job beyond serde field-mapping: flatten nested common
    /// fields to depth, fall back on unknown enum variants, tolerate future keys
    /// and sparse payloads, and carry typed vecs / trigger enums.
    #[test]
    fn parse_helpers_flatten_default_and_tolerate_drift() {
        let session = parse_session_start(&json!({
            "session_id": "sess-1",
            "model": "claude-opus-4-8",
            "source": "startup",
            "future_field_from_anthropic": {"nested": 1},
        }));
        assert_eq!(session.common.common.session_id.as_deref(), Some("sess-1"));
        assert_eq!(session.common.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(session.source, SessionSource::Startup);
        // An unknown source variant falls back rather than failing the parse.
        assert_eq!(
            parse_session_start(&json!({"source": "brand_new_source"})).source,
            SessionSource::Unknown
        );

        let sparse = parse_stop(&json!({}));
        assert!(sparse.background_tasks.is_empty());
        assert!(sparse.session_crons.is_empty());
        assert_eq!(sparse.common.common.session_id, None);

        let stop = parse_stop(&json!({
            "background_tasks": [
                {"id": "t1", "status": "running", "description": "linting"},
                {"id": "t2", "status": "completed", "description": "done"}
            ],
            "session_crons": [
                {"id": "cron-1", "schedule": "0 9 * * 1-5", "prompt": "check build"}
            ],
            "effort": {"level": "high"}
        }));
        assert_eq!(stop.background_tasks.len(), 2);
        assert_eq!(stop.session_crons.len(), 1);
        assert_eq!(
            stop.background_tasks[0].description.as_deref(),
            Some("linting")
        );
        assert_eq!(
            stop.common.effort.and_then(|e| e.level).as_deref(),
            Some("high")
        );

        assert_eq!(
            parse_post_compact(&json!({"trigger": "manual"})).trigger,
            CompactTrigger::Manual
        );
    }
}
