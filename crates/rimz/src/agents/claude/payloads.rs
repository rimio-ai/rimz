//! Typed input and output structs for the Claude Code hook protocol.
//!
//! **Input** (Deserialize): one struct per event in the installed and catalog
//! sets. All use `#[serde(default)]` so sparse or out-of-spec payloads always
//! deserialize cleanly. `parse_*` free functions are the entry points called by
//! the adapter. Silent events (PostToolUse, Notification) and identity events
//! (PermissionRequest, SessionEnd) carry no observation but are parsed to keep
//! the wire surface typed and auditable.
//!
//! **Output** (Serialize): the exact JSON shapes Claude reads back from stdout.
//! Field names use `#[serde(rename)]` to match the camelCase wire protocol.
//! Optional output fields use `#[serde(skip_serializing_if = "Option::is_none")]`
//! so they are absent when `None`, keeping golden-test output byte-identical.
//!
//! This module is the **complete, parse-ready wire catalog**: every installed
//! and near-term-catalog event has a struct and a `parse_*` entry point, even
//! where the adapter doesn't consume it yet (an upcoming agent state machine
//! will wire more events — notably the start/end pairs). `#![allow(dead_code)]`
//! keeps that forward-ready surface from tripping the warnings-as-errors gate
//! without scattering per-item attributes.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
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
    pub session_title: Option<String>,
}

/// `SessionEnd` carries a `reason` field documenting why the session ended.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSessionEnd {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeUserPromptSubmit {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePreToolUse {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
}

/// Silent lifecycle event. `tool_response` is available for audit enrichment;
/// `todos` inside `tool_input` or `tool_response` carry the `TodoWrite` state.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePostToolUse {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeStop {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub stop_hook_active: Option<bool>,
    /// In-flight background tasks (Claude Code v2.1.145+). An empty vec or
    /// absent field means a genuine turn end; any in-flight entry means the
    /// main thread has parked and will reawaken.
    pub background_tasks: Vec<BackgroundTask>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeStopFailure {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub error: Option<String>,
    pub error_details: Option<Value>,
    pub last_assistant_message: Option<String>,
}

/// Silent lifecycle event. `message` is the notification text.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeNotification {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub message: Option<String>,
}

/// `agent_id` and `agent_type` are carried in [`ClaudeCommon`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSubagentStart {
    #[serde(flatten)]
    pub common: ClaudeCommon,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudeSubagentStop {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    /// Exit code of the subagent process.
    pub exit_code: Option<i64>,
}

/// Fires before context compaction. `trigger` distinguishes manual (`/compact`)
/// from automatic compaction.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePreCompact {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub trigger: CompactTrigger,
}

/// Fires after context compaction completes. The `trigger` shape mirrors
/// `PreCompact` and decides whether the row resumes running or rests idle.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePostCompact {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub trigger: CompactTrigger,
}

/// Blocking feed event. `tool_name` and `tool_input` are available in
/// `classify_hook` for enriching the feed item; they don't affect the decision
/// shape (which is `behavior: allow|deny`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ClaudePermissionRequest {
    #[serde(flatten)]
    pub common: ClaudeCommon,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
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
parse_fn!(parse_session_end, ClaudeSessionEnd);
parse_fn!(parse_user_prompt_submit, ClaudeUserPromptSubmit);
parse_fn!(parse_pre_tool_use, ClaudePreToolUse);
parse_fn!(parse_post_tool_use, ClaudePostToolUse);
parse_fn!(parse_stop, ClaudeStop);
parse_fn!(parse_stop_failure, ClaudeStopFailure);
parse_fn!(parse_notification, ClaudeNotification);
parse_fn!(parse_subagent_start, ClaudeSubagentStart);
parse_fn!(parse_subagent_stop, ClaudeSubagentStop);
parse_fn!(parse_pre_compact, ClaudePreCompact);
parse_fn!(parse_post_compact, ClaudePostCompact);
parse_fn!(parse_permission_request, ClaudePermissionRequest);

// ── Decision output structs ────────────────────────────────────────────────

/// Claude `PermissionRequest` decision output.
///
/// Wire: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
///
/// `updated_input` and `applied_rule` are optional upstream output fields
/// (see adapter/claude-reference.md). Both are absent from the wire when `None`
/// via `skip_serializing_if`, keeping existing golden-test output byte-identical.
#[derive(Debug, Serialize)]
pub struct ClaudePermissionDecisionOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: ClaudePermissionHookOutput,
}

#[derive(Debug, Serialize)]
pub struct ClaudePermissionHookOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: &'static str,
    pub decision: ClaudePermissionBehavior,
}

#[derive(Debug, Serialize)]
pub struct ClaudePermissionBehavior {
    pub behavior: &'static str,
    #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    #[serde(rename = "appliedRule", skip_serializing_if = "Option::is_none")]
    pub applied_rule: Option<String>,
}

/// Claude `PreToolUse` decision output for `PlanApproval` and `Question` feed
/// kinds.
///
/// Wire: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{}}}`
///
/// `updated_input` is **required** by the upstream spec. Callers must extract it
/// before constructing this struct; the missing-field error is returned from
/// `render_decision` before construction.
#[derive(Debug, Serialize)]
pub struct ClaudePreToolUseDecisionOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: ClaudePreToolUseHookOutput,
}

#[derive(Debug, Serialize)]
pub struct ClaudePreToolUseHookOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: &'static str,
    #[serde(rename = "updatedInput")]
    pub updated_input: Value,
}

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
            "session_title": "My session",
            "future_field_from_anthropic": {"nested": 1},
        }));
        assert_eq!(session.common.common.session_id.as_deref(), Some("sess-1"));
        assert_eq!(session.common.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(session.source, SessionSource::Startup);
        assert_eq!(session.session_title.as_deref(), Some("My session"));
        // An unknown source variant falls back rather than failing the parse.
        assert_eq!(
            parse_session_start(&json!({"source": "brand_new_source"})).source,
            SessionSource::Unknown
        );

        let sparse = parse_stop(&json!({}));
        assert!(sparse.background_tasks.is_empty());
        assert_eq!(sparse.stop_hook_active, None);
        assert_eq!(sparse.common.common.session_id, None);

        let stop = parse_stop(&json!({
            "background_tasks": [
                {"id": "t1", "status": "running", "description": "linting"},
                {"id": "t2", "status": "completed", "description": "done"}
            ],
            "effort": {"level": "high"}
        }));
        assert_eq!(stop.background_tasks.len(), 2);
        assert_eq!(
            stop.background_tasks[0].description.as_deref(),
            Some("linting")
        );
        assert_eq!(
            stop.common.effort.and_then(|e| e.level).as_deref(),
            Some("high")
        );

        assert_eq!(
            parse_subagent_stop(&json!({"exit_code": 1, "agent_id": "child-1"})).exit_code,
            Some(1)
        );
        assert_eq!(
            parse_pre_compact(&json!({"trigger": "auto"})).trigger,
            CompactTrigger::Auto
        );
        assert_eq!(
            parse_post_compact(&json!({"trigger": "manual"})).trigger,
            CompactTrigger::Manual
        );
    }
}
