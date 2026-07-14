//! Shared wire types and managed hook catalogs across agent adapters.
//!
//! All input enums use `#[serde(other)]` so unknown upstream values never break
//! parsing. All input structs use `#[serde(default)]` so sparse payloads
//! (including `{}`) always deserialize cleanly.

use serde::Deserialize;

use super::{AgentHookClass, AskKind, ClassifiedHook};

/// One installed managed hook and its classification policy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HookRecord {
    pub(crate) event: &'static str,
    pub(crate) matcher: Option<&'static str>,
    pub(crate) lifecycle_fallback: bool,
    pub(crate) synchronous: bool,
    #[cfg(test)]
    pub(crate) test_payload: &'static str,
    #[cfg(test)]
    pub(crate) test_class: AgentHookClass,
    #[cfg(test)]
    pub(crate) test_ask: Option<AskKind>,
}

macro_rules! hook_record {
    ($event:literal, $matcher:expr, $lifecycle:expr, $sync:expr, $payload:literal, $class:expr, $ask:expr) => {
        $crate::agents::hook_types::HookRecord {
            event: $event,
            matcher: $matcher,
            lifecycle_fallback: $lifecycle,
            synchronous: $sync,
            #[cfg(test)]
            test_payload: $payload,
            #[cfg(test)]
            test_class: $class,
            #[cfg(test)]
            test_ask: $ask,
        }
    };
}
pub(crate) use hook_record;

pub(crate) fn classify_catalog_hook(
    hooks: &[HookRecord],
    event_name: &str,
    ask_kind: Option<AskKind>,
) -> ClassifiedHook {
    let class = if ask_kind.is_some() {
        AgentHookClass::AwaitingUser
    } else if hooks
        .iter()
        .any(|hook| hook.event == event_name && hook.lifecycle_fallback)
    {
        AgentHookClass::Lifecycle
    } else {
        AgentHookClass::Unknown
    };
    ClassifiedHook {
        class,
        ask_kind,
        event_name: event_name.to_owned(),
    }
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
    fn hook_payloads_parse_unknown_values_and_sparse_objects() {
        // Enums fall back to Unknown for unrecognised upstream values.
        let compact: SessionSource = serde_json::from_str(r#""compact""#).unwrap();
        assert_eq!(compact, SessionSource::Compact);
        let unknown: SessionSource = serde_json::from_str(r#""brandNewSource""#).unwrap();
        assert_eq!(unknown, SessionSource::Unknown);
        let auto: CompactTrigger = serde_json::from_str(r#""auto""#).unwrap();
        assert_eq!(auto, CompactTrigger::Auto);
        let unknown: CompactTrigger = serde_json::from_str(r#""future""#).unwrap();
        assert_eq!(unknown, CompactTrigger::Unknown);
        assert_eq!(CompactTrigger::default(), CompactTrigger::Unknown);

        // Structs deserialize sparse payloads — including unknown fields — cleanly.
        let c: HookEventCommon = serde_json::from_value(json!({})).unwrap();
        assert!(c.session_id.is_none());
        let c: HookEventCommon = serde_json::from_value(json!({
            "session_id": "s1",
            "future_field": {"nested": 1}
        }))
        .unwrap();
        assert_eq!(c.session_id.as_deref(), Some("s1"));
        let t: BackgroundTask = serde_json::from_value(json!({})).unwrap();
        assert!(t.id.is_none());
        assert!(t.status.is_none());
    }
}
