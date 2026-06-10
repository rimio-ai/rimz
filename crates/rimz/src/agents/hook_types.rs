//! Shared wire types for hook payload parsing across Claude and Codex adapters.
//!
//! All input enums use `#[serde(other)]` so unknown upstream values never break
//! parsing. All input structs use `#[serde(default)]` so sparse payloads
//! (including `{}`) always deserialize cleanly.

use serde::Deserialize;

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
    Manual,
    Auto,
    #[default]
    #[serde(other)]
    Unknown,
}

impl CompactTrigger {
    pub const fn auto_flag(&self) -> Option<bool> {
        match self {
            Self::Manual => Some(false),
            Self::Auto => Some(true),
            Self::Unknown => None,
        }
    }
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
        assert_eq!(CompactTrigger::default(), CompactTrigger::Unknown);
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
