//! Typed input and output structs for the Codex hook protocol.
//!
//! **Input** (Deserialize): one struct per event in the installed and catalog
//! sets. All use `#[serde(default)]` so sparse payloads always deserialize
//! cleanly. `parse_*` free functions are the entry points called by the adapter.
//! Silent events (read-only PostToolUse, PermissionRequest in observe_lifecycle)
//! and compaction events are parsed to keep the wire surface typed and auditable.
//!
//! **Output** (Serialize): the Codex `PermissionRequest` and blocking
//! `PreToolUse` decision shapes. Unlike Claude, Codex's `PermissionRequest`
//! decision carries an optional `message` field alongside `behavior` — see
//! adapter/codex-reference.md for the divergence note.
//!
//! Like the Claude catalog, this module is the **complete, parse-ready wire
//! catalog**: every installed and near-term-catalog event (including the
//! compaction pair) has a struct and a `parse_*` entry point. `#![allow(dead_code)]`
//! keeps the full catalog from tripping the warnings-as-errors gate when a typed
//! payload is present only for audit or future enrichment.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::hook_types::{CompactTrigger, HookEventCommon, SessionSource};

// ── Common ─────────────────────────────────────────────────────────────────

/// Codex-specific common fields. Wraps the universal [`HookEventCommon`] and
/// adds the model id and `turn_id` (present on turn-scoped events only).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
    pub model: Option<String>,
    /// Present only on turn-scoped events (`UserPromptSubmit`, `PreToolUse`,
    /// `PostToolUse`, `Stop`, `SubagentStart`, `SubagentStop`, `PermissionRequest`).
    pub turn_id: Option<String>,
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
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_input: Option<Value>,
}

/// Blocking feed event. `tool_name` and `tool_input` are available in
/// `classify_hook` for enriching the feed item.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPermissionRequest {
    #[serde(flatten)]
    pub common: CodexCommon,
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
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// Path to the subagent's own rollout JSONL transcript.
    pub agent_transcript_path: Option<String>,
    pub stop_hook_active: Option<bool>,
    /// Final assistant message text from the subagent's turn.
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexStop {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub stop_hook_active: Option<bool>,
    /// Final assistant message text from the completed turn.
    pub last_assistant_message: Option<String>,
}

/// Fires before conversation compaction. `trigger` distinguishes manual from
/// automatic compaction.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPreCompact {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub trigger: CompactTrigger,
}

/// Fires after conversation compaction completes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPostCompact {
    #[serde(flatten)]
    pub common: CodexCommon,
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

// ── Decision output struct ─────────────────────────────────────────────────

/// Codex `PermissionRequest` decision output.
///
/// Wire: `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
///
/// `message` is populated from the resolver's `reason` when present (drift fix
/// #1: the upstream spec includes `decision.message`). It is absent from the
/// wire when `None` via `skip_serializing_if`, keeping existing golden-test
/// output byte-identical.
///
/// **Divergence from Claude.** Never emit `updatedInput`, `updatedPermissions`,
/// or `interrupt` on a Codex `PermissionRequest` — those belong to *other* Codex
/// hook types. See adapter/codex-reference.md for the full divergence note.
#[derive(Debug, Serialize)]
pub struct CodexPermissionDecisionOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: CodexPermissionHookOutput,
}

#[derive(Debug, Serialize)]
pub struct CodexPermissionHookOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: &'static str,
    pub decision: CodexPermissionBehavior,
}

#[derive(Debug, Serialize)]
pub struct CodexPermissionBehavior {
    pub behavior: &'static str,
    /// Reason surfaced when the resolver blocked the call. Absent when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Codex `PreToolUse` decision output for blocking `request_user_input` feed items.
///
/// Wire: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}`
///
/// `updatedInput` is optional on allow. A denial carries
/// `permissionDecisionReason`; callers supply a default before construction.
#[derive(Debug, Serialize)]
pub struct CodexPreToolUseDecisionOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: CodexPreToolUseHookOutput,
}

#[derive(Debug, Serialize)]
pub struct CodexPreToolUseHookOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: &'static str,
    #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
    #[serde(
        rename = "permissionDecisionReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_decision_reason: Option<String>,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::hook_types::{CompactTrigger, SessionSource};

    #[test]
    fn session_and_stop_payloads_cover_defaults_drift_and_enrichment_fields() {
        assert_eq!(
            parse_session_start(&json!({"source": "compact"})).source,
            SessionSource::Compact
        );
        let session = parse_session_start(&json!({"model": "gpt-5.5-codex", "source": "startup"}));
        assert_eq!(session.common.model.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(session.source, SessionSource::Startup);

        let future = parse_session_start(&json!({
            "session_id": "s1",
            "future_openai_field": true
        }));
        assert_eq!(future.common.common.session_id.as_deref(), Some("s1"));

        let sparse = parse_stop(&json!({}));
        assert_eq!(sparse.stop_hook_active, None);
        assert_eq!(sparse.last_assistant_message, None);
        assert_eq!(sparse.common.turn_id, None);

        let stop = parse_stop(&json!({
            "stop_hook_active": true,
            "last_assistant_message": "All done!"
        }));
        assert_eq!(stop.last_assistant_message.as_deref(), Some("All done!"));
    }

    #[test]
    fn hook_payloads_parse_tool_subagent_and_compaction_fields() {
        let pre_tool = parse_pre_tool_use(&json!({
            "tool_name": "Bash",
            "tool_use_id": "tu-1",
            "tool_input": {"command": "ls"}
        }));
        assert_eq!(pre_tool.tool_name.as_deref(), Some("Bash"));
        assert_eq!(pre_tool.tool_use_id.as_deref(), Some("tu-1"));
        assert!(pre_tool.tool_input.is_some());

        let permission = parse_permission_request(&json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /"}
        }));
        assert_eq!(permission.tool_name.as_deref(), Some("Bash"));
        assert!(permission.tool_input.is_some());

        let post_tool = parse_post_tool_use(&json!({
            "tool_name": "Bash",
            "tool_response": {"output": "ok"}
        }));
        assert_eq!(post_tool.tool_name.as_deref(), Some("Bash"));
        assert!(post_tool.tool_response.is_some());

        let subagent = parse_subagent_stop(&json!({
            "agent_id": "child-1",
            "agent_type": "code-reviewer",
            "agent_transcript_path": "/tmp/rollout-child.jsonl",
            "stop_hook_active": false,
            "last_assistant_message": "Done."
        }));
        assert_eq!(subagent.agent_id.as_deref(), Some("child-1"));
        assert_eq!(subagent.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            subagent.agent_transcript_path.as_deref(),
            Some("/tmp/rollout-child.jsonl")
        );
        assert_eq!(subagent.last_assistant_message.as_deref(), Some("Done."));
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
