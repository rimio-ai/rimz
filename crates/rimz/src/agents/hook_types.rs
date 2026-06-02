//! Shared wire types for hook payload parsing across Claude and Codex adapters.
//!
//! All input enums use `#[serde(other)]` so unknown upstream values never break
//! parsing. All input structs use `#[serde(default)]` so sparse payloads
//! (including `{}`) always deserialize cleanly.

use serde::Deserialize;

/// Permission slider value as reported on hook payloads. Claude uses all six
/// variants; Codex omits `Auto`. `#[serde(rename_all = "camelCase")]` covers
/// the mixed-case wire values (`acceptEdits`, `dontAsk`, `bypassPermissions`).
/// Unknown future values map to `Unknown` via `#[serde(other)]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    AcceptEdits,
    /// Claude-only; Codex does not emit this value on the wire.
    Auto,
    DontAsk,
    BypassPermissions,
    #[serde(other)]
    Unknown,
}

/// `source` field on `SessionStart` events, shared by both adapters.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    #[default]
    Startup,
    Resume,
    Clear,
    Compact,
    #[serde(other)]
    Unknown,
}

/// `trigger` field on `PreCompact` and `PostCompact` events.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactTrigger {
    #[default]
    Manual,
    Auto,
    #[serde(other)]
    Unknown,
}

/// Universal fields present on every hook event from both adapters. Embedded
/// via `#[serde(flatten)]` in each adapter's per-event common struct.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookEventCommon {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
}

/// One entry in the `background_tasks` array on a Claude `Stop` payload
/// (Claude Code v2.1.145+). An entry whose `status` is not `completed` or
/// `failed` is considered in-flight, which keeps the turn alive.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BackgroundTask {
    pub id: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
    pub command: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn permission_mode_camel_case_variants() {
        let accept: PermissionMode = serde_json::from_str(r#""acceptEdits""#).unwrap();
        assert_eq!(accept, PermissionMode::AcceptEdits);
        let dont: PermissionMode = serde_json::from_str(r#""dontAsk""#).unwrap();
        assert_eq!(dont, PermissionMode::DontAsk);
        let bypass: PermissionMode = serde_json::from_str(r#""bypassPermissions""#).unwrap();
        assert_eq!(bypass, PermissionMode::BypassPermissions);
    }

    #[test]
    fn permission_mode_unknown_variant() {
        let unknown: PermissionMode = serde_json::from_str(r#""someNewMode""#).unwrap();
        assert_eq!(unknown, PermissionMode::Unknown);
    }

    #[test]
    fn session_source_lowercase_variants() {
        let compact: SessionSource = serde_json::from_str(r#""compact""#).unwrap();
        assert_eq!(compact, SessionSource::Compact);
    }

    #[test]
    fn session_source_unknown_variant() {
        let unknown: SessionSource = serde_json::from_str(r#""brandNewSource""#).unwrap();
        assert_eq!(unknown, SessionSource::Unknown);
    }

    #[test]
    fn compact_trigger_variants() {
        let auto: CompactTrigger = serde_json::from_str(r#""auto""#).unwrap();
        assert_eq!(auto, CompactTrigger::Auto);
        let unknown: CompactTrigger = serde_json::from_str(r#""future""#).unwrap();
        assert_eq!(unknown, CompactTrigger::Unknown);
    }

    #[test]
    fn hook_event_common_sparse() {
        let c: HookEventCommon = serde_json::from_value(json!({})).unwrap();
        assert!(c.session_id.is_none());
        assert!(c.cwd.is_none());
    }

    #[test]
    fn hook_event_common_unknown_fields_ignored() {
        let c: HookEventCommon = serde_json::from_value(json!({
            "session_id": "s1",
            "future_field": {"nested": 1}
        }))
        .unwrap();
        assert_eq!(c.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn background_task_sparse() {
        let t: BackgroundTask = serde_json::from_value(json!({})).unwrap();
        assert!(t.id.is_none());
        assert!(t.status.is_none());
    }
}
