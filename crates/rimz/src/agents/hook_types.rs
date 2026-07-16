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
    pub(crate) test_ask: Option<AskKind>,
}

impl HookRecord {
    pub(crate) const fn lifecycle(event: &'static str, _test_payload: &'static str) -> Self {
        Self {
            event,
            matcher: None,
            lifecycle_fallback: true,
            synchronous: false,
            #[cfg(test)]
            test_payload: _test_payload,
            #[cfg(test)]
            test_ask: None,
        }
    }

    pub(crate) const fn blocking(
        event: &'static str,
        _test_payload: &'static str,
        _test_ask: AskKind,
    ) -> Self {
        Self {
            event,
            matcher: None,
            lifecycle_fallback: false,
            synchronous: false,
            #[cfg(test)]
            test_payload: _test_payload,
            #[cfg(test)]
            test_ask: Some(_test_ask),
        }
    }

    pub(crate) const fn with_matcher(mut self, matcher: &'static str) -> Self {
        self.matcher = Some(matcher);
        self
    }

    pub(crate) const fn synchronous(mut self) -> Self {
        self.synchronous = true;
        self
    }

    pub(crate) const fn with_lifecycle_fallback(mut self) -> Self {
        self.lifecycle_fallback = true;
        self
    }

    #[cfg(test)]
    fn expected_class(self) -> AgentHookClass {
        if self.test_ask.is_some() {
            AgentHookClass::AwaitingUser
        } else if self.lifecycle_fallback {
            AgentHookClass::Lifecycle
        } else {
            AgentHookClass::Unknown
        }
    }
}

macro_rules! hook_record {
    (lifecycle, $event:literal, $payload:literal) => {
        $crate::agents::hook_types::HookRecord::lifecycle($event, $payload)
    };
    (blocking, $event:literal, $payload:literal, $ask:expr) => {
        $crate::agents::hook_types::HookRecord::blocking($event, $payload, $ask)
    };
}
pub(crate) use hook_record;

#[cfg(test)]
pub(crate) fn catalog_event_names(hooks: &[HookRecord]) -> Vec<&'static str> {
    hooks.iter().map(|hook| hook.event).collect()
}

pub(crate) fn catalog_contains(hooks: &[HookRecord], event_name: &str) -> bool {
    hooks.iter().any(|hook| hook.event == event_name)
}

#[cfg(test)]
pub(crate) const fn catalog_event_name_array<const N: usize>(
    hooks: &[HookRecord; N],
) -> [&'static str; N] {
    let mut names = [""; N];
    let mut index = 0;
    while index < N {
        names[index] = hooks[index].event;
        index += 1;
    }
    names
}

#[cfg(test)]
pub(crate) fn catalog_classification_corpus(
    hooks: &[HookRecord],
) -> Vec<super::ClassificationSample> {
    hooks.iter().map(classification_sample).collect()
}

#[cfg(test)]
pub(crate) fn classification_sample(hook: &HookRecord) -> super::ClassificationSample {
    super::ClassificationSample::new(
        hook.event,
        serde_json::from_str(hook.test_payload).expect("valid catalog payload"),
        hook.expected_class(),
        hook.test_ask,
    )
}

pub(crate) fn classify_catalog_hook(
    hooks: &[HookRecord],
    event_name: &str,
    ask_kind: Option<AskKind>,
) -> ClassifiedHook {
    classify_catalog_entry(
        hooks.iter().find(|hook| hook.event == event_name),
        event_name,
        ask_kind,
    )
}

pub(crate) fn classify_catalog_entry(
    hook: Option<&HookRecord>,
    event_name: &str,
    ask_kind: Option<AskKind>,
) -> ClassifiedHook {
    let class = if ask_kind.is_some() {
        AgentHookClass::AwaitingUser
    } else if hook.is_some_and(|hook| hook.lifecycle_fallback) {
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

    #[test]
    fn hook_catalog_records_derive_policy_and_event_names() {
        const HOOKS: [HookRecord; 4] = [
            hook_record!(lifecycle, "Start", r#"{}"#),
            hook_record!(blocking, "Ask", r#"{}"#, AskKind::Question).synchronous(),
            hook_record!(blocking, "Permission", r#"{}"#, AskKind::Permission)
                .with_matcher("shell")
                .synchronous()
                .with_lifecycle_fallback(),
            hook_record!(lifecycle, "Stop", r#"{}"#).with_matcher("done"),
        ];
        const NAMES: [&str; 4] = catalog_event_name_array(&HOOKS);

        assert_eq!(NAMES, ["Start", "Ask", "Permission", "Stop"]);
        assert_eq!(catalog_event_names(&HOOKS), NAMES);
        assert!(catalog_contains(&HOOKS, "Permission"));
        assert!(!catalog_contains(&HOOKS, "Unknown"));
        assert_eq!(HOOKS[1].matcher, None);
        assert!(HOOKS[1].synchronous);
        assert_eq!(HOOKS[2].matcher, Some("shell"));
        assert!(HOOKS[2].lifecycle_fallback);
        assert_eq!(HOOKS[3].matcher, Some("done"));

        let samples = catalog_classification_corpus(&HOOKS);
        assert_eq!(samples[0].expected.class, AgentHookClass::Lifecycle);
        assert_eq!(samples[1].expected.class, AgentHookClass::AwaitingUser);
        assert_eq!(samples[2].expected.class, AgentHookClass::AwaitingUser);
        assert_eq!(samples[2].expected.ask_kind, Some(AskKind::Permission));
    }
}
