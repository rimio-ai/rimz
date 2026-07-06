//! Typed input structs for the Codex hook protocol.
//!
//! **Input** (Deserialize): one struct per event in the installed and catalog
//! sets. All use `#[serde(default)]` so sparse payloads always deserialize
//! cleanly. `parse_*` free functions are the entry points called by the adapter.
//! Silent events (read-only PostToolUse, PermissionRequest in observe_lifecycle)
//! and compaction events are parsed to keep the wire surface typed and auditable.
//!
//! Like the Claude catalog, this module is the **complete, parse-ready wire
//! catalog**: every installed and near-term-catalog event (including the
//! compaction pair) has a struct and a `parse_*` entry point. `#![allow(dead_code)]`
//! keeps the full catalog from tripping the warnings-as-errors gate when a typed
//! payload is present only for audit or future enrichment.
#![allow(dead_code)]

use serde::Deserialize;
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
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
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
            "model": "gpt-5.5-codex",
            "source": "startup",
            "future_openai_field": true,
        }));
        assert_eq!(session.common.common.session_id.as_deref(), Some("s1"));
        assert_eq!(session.common.model.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(session.source, SessionSource::Startup);
        assert_eq!(
            parse_session_start(&json!({"source": "compact"})).source,
            SessionSource::Compact
        );

        // `#[serde(default)]` makes a sparse payload deserialize cleanly.
        let sparse = parse_stop(&json!({}));
        assert_eq!(sparse.stop_hook_active, None);
        assert_eq!(sparse.last_assistant_message, None);
        assert_eq!(sparse.common.turn_id, None);

        // The richer tool/subagent/compaction catalog entries pick up their
        // enrichment fields and map the compaction-trigger enum.
        let subagent = parse_subagent_stop(&json!({
            "agent_id": "child-1",
            "agent_type": "code-reviewer",
            "agent_transcript_path": "/tmp/rollout-child.jsonl",
            "last_assistant_message": "Done.",
        }));
        assert_eq!(subagent.agent_id.as_deref(), Some("child-1"));
        assert_eq!(subagent.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            subagent.agent_transcript_path.as_deref(),
            Some("/tmp/rollout-child.jsonl")
        );
        assert_eq!(subagent.last_assistant_message.as_deref(), Some("Done."));
        assert!(
            parse_permission_request(
                &json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf /"}})
            )
            .tool_input
            .is_some()
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
