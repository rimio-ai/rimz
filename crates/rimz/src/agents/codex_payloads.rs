//! Typed input and output structs for the Codex hook protocol.
//!
//! **Input** (Deserialize): one struct per event in the installed and catalog
//! sets. All use `#[serde(default)]` so sparse payloads always deserialize
//! cleanly. `parse_*` free functions are the entry points called by the adapter.
//! Silent events (PreToolUse, PostToolUse, PermissionRequest in observe_lifecycle)
//! and forwarded-compat events (PreCompact, PostCompact) are parsed to keep the
//! wire surface typed and auditable even when they produce no observation.
//!
//! **Output** (Serialize): the Codex `PermissionRequest` decision shape. Unlike
//! Claude, Codex's `PermissionRequest` decision carries an optional `message`
//! field alongside `behavior` — see adapter/codex-reference.md for the
//! divergence note.
//!
//! Like the Claude catalog, this module is the **complete, parse-ready wire
//! catalog**: every installed and near-term-catalog event (including the
//! forward-compat compaction pair) has a struct and a `parse_*` entry point,
//! even where the adapter doesn't consume it yet — an upcoming agent state
//! machine will wire more events. `#![allow(dead_code)]` keeps that
//! forward-ready surface from tripping the warnings-as-errors gate.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::hook_types::{CompactTrigger, HookEventCommon, PermissionMode, SessionSource};

// ── Common ─────────────────────────────────────────────────────────────────

/// Codex-specific common fields. Wraps the universal [`HookEventCommon`] and
/// adds the model id, permission slider, and `turn_id` (present on turn-scoped
/// events only).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
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

/// Not currently installed (Codex uses `SessionStart(source=compact)` instead
/// of a dedicated `PreCompact` hook). Struct kept for forward-compat should
/// Codex add the event.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CodexPreCompact {
    #[serde(flatten)]
    pub common: CodexCommon,
    pub trigger: CompactTrigger,
}

/// Not currently installed. Struct kept for forward-compat.
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::hook_types::{CompactTrigger, PermissionMode, SessionSource};
    use super::*;

    #[test]
    fn session_start_compact_source() {
        let p = parse_session_start(&json!({"source": "compact"}));
        assert_eq!(p.source, SessionSource::Compact);
    }

    #[test]
    fn session_start_parses_model() {
        let p = parse_session_start(&json!({"model": "gpt-5.5-codex", "source": "startup"}));
        assert_eq!(p.common.model.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(p.source, SessionSource::Startup);
    }

    #[test]
    fn sparse_payload_gives_defaults() {
        let p = parse_stop(&json!({}));
        assert_eq!(p.stop_hook_active, None);
        assert_eq!(p.last_assistant_message, None);
        assert_eq!(p.common.turn_id, None);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let p = parse_session_start(&json!({
            "session_id": "s1",
            "future_openai_field": true
        }));
        assert_eq!(p.common.common.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn unknown_permission_mode_maps_to_unknown() {
        let p = parse_session_start(&json!({"permission_mode": "someNewMode"}));
        assert_eq!(p.common.permission_mode, Some(PermissionMode::Unknown));
    }

    #[test]
    fn pre_tool_use_fields() {
        let p = parse_pre_tool_use(&json!({
            "tool_name": "Bash",
            "tool_use_id": "tu-1",
            "tool_input": {"command": "ls"}
        }));
        assert_eq!(p.tool_name.as_deref(), Some("Bash"));
        assert_eq!(p.tool_use_id.as_deref(), Some("tu-1"));
        assert!(p.tool_input.is_some());
    }

    #[test]
    fn permission_request_tool_name() {
        let p = parse_permission_request(&json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /"}
        }));
        assert_eq!(p.tool_name.as_deref(), Some("Bash"));
        assert!(p.tool_input.is_some());
    }

    #[test]
    fn post_tool_use_tool_response() {
        let p = parse_post_tool_use(&json!({
            "tool_name": "Bash",
            "tool_response": {"output": "ok"}
        }));
        assert_eq!(p.tool_name.as_deref(), Some("Bash"));
        assert!(p.tool_response.is_some());
    }

    #[test]
    fn subagent_stop_drift_fix_fields() {
        // Drift fix #3: agent_transcript_path and last_assistant_message now captured.
        let p = parse_subagent_stop(&json!({
            "agent_id": "child-1",
            "agent_type": "code-reviewer",
            "agent_transcript_path": "/tmp/rollout-child.jsonl",
            "stop_hook_active": false,
            "last_assistant_message": "Done."
        }));
        assert_eq!(p.agent_id.as_deref(), Some("child-1"));
        assert_eq!(p.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            p.agent_transcript_path.as_deref(),
            Some("/tmp/rollout-child.jsonl")
        );
        assert_eq!(p.last_assistant_message.as_deref(), Some("Done."));
    }

    #[test]
    fn stop_drift_fix_last_assistant_message() {
        // Drift fix #4: last_assistant_message now captured on Stop.
        let p = parse_stop(&json!({
            "stop_hook_active": true,
            "last_assistant_message": "All done!"
        }));
        assert_eq!(p.last_assistant_message.as_deref(), Some("All done!"));
    }

    #[test]
    fn pre_compact_trigger() {
        let p = parse_pre_compact(&json!({"trigger": "auto"}));
        assert_eq!(p.trigger, CompactTrigger::Auto);
    }

    #[test]
    fn post_compact_trigger() {
        let p = parse_post_compact(&json!({"trigger": "manual"}));
        assert_eq!(p.trigger, CompactTrigger::Manual);
    }

    #[test]
    fn permission_decision_output_no_message() {
        let output = CodexPermissionDecisionOutput {
            hook_specific_output: CodexPermissionHookOutput {
                hook_event_name: "PermissionRequest",
                decision: CodexPermissionBehavior {
                    behavior: "allow",
                    message: None,
                },
            },
        };
        insta::assert_json_snapshot!(serde_json::to_value(&output).unwrap(), @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "allow"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
    }

    #[test]
    fn permission_decision_output_with_message() {
        // Drift fix #1: message appears in output when resolver provides a reason.
        let output = CodexPermissionDecisionOutput {
            hook_specific_output: CodexPermissionHookOutput {
                hook_event_name: "PermissionRequest",
                decision: CodexPermissionBehavior {
                    behavior: "deny",
                    message: Some("blocked by rimz policy".to_owned()),
                },
            },
        };
        insta::assert_json_snapshot!(serde_json::to_value(&output).unwrap(), @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny",
              "message": "blocked by rimz policy"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
    }
}
