//! Shared wire types and managed hook catalogs across agent adapters.
//!
//! All input enums use `#[serde(other)]` so unknown upstream values never break
//! parsing. All input structs use `#[serde(default)]` so sparse payloads
//! (including `{}`) always deserialize cleanly.

use serde::Deserialize;
use serde_json::Value;

use crate::ids::AgentSessionId;
use crate::transcript::{AskAnswer, AskQuestion};

use super::{
    AgentHookClass, AgentLifecycleObservation, AgentTurnError, AskKind, ClassifiedHook,
    ContextObservation,
};

/// Provider-neutral result of decoding one native hook payload.
#[derive(Debug, PartialEq)]
pub struct DecodedHook {
    event_name: String,
    class: AgentHookClass,
    ask_kind: Option<AskKind>,
    routing: HookRouting,
    progress: bool,
    session_ended: bool,
    lifecycle: Option<AgentLifecycleObservation>,
    questions: Vec<AskQuestion>,
    ask_detail: Option<String>,
    native_answers: Option<Vec<AskAnswer>>,
    assistant_message: Option<String>,
    final_message: Option<String>,
    turn_error: Option<AgentTurnError>,
    observed_context: Option<ContextObservation>,
    neutral: Option<Value>,
}

impl DecodedHook {
    pub fn new(classified: ClassifiedHook) -> Self {
        Self {
            event_name: classified.event_name,
            class: classified.class,
            ask_kind: classified.ask_kind,
            routing: HookRouting::default(),
            progress: false,
            session_ended: false,
            lifecycle: None,
            questions: Vec::new(),
            ask_detail: None,
            native_answers: None,
            assistant_message: None,
            final_message: None,
            turn_error: None,
            observed_context: None,
            neutral: None,
        }
    }

    fn with_policy(mut self, hook: Option<&HookRecord>) -> Self {
        self.progress = hook.is_some_and(|hook| hook.progress);
        self.session_ended = hook.is_some_and(|hook| hook.session_ended);
        self
    }

    pub fn event_name(&self) -> &str {
        &self.event_name
    }

    pub const fn class(&self) -> AgentHookClass {
        self.class
    }

    pub const fn ask_kind(&self) -> Option<AskKind> {
        self.ask_kind
    }

    pub const fn routing(&self) -> &HookRouting {
        &self.routing
    }

    pub const fn records_progress(&self) -> bool {
        self.progress
    }

    pub const fn ends_session(&self) -> bool {
        self.session_ended
    }

    pub fn lifecycle(&self) -> Option<&AgentLifecycleObservation> {
        self.lifecycle.as_ref()
    }

    pub fn take_lifecycle(&mut self) -> Option<AgentLifecycleObservation> {
        self.lifecycle.take()
    }

    pub fn questions(&self) -> &[AskQuestion] {
        &self.questions
    }

    pub fn ask_detail(&self) -> Option<&str> {
        self.ask_detail.as_deref()
    }

    pub fn native_answers(&self) -> Option<&[AskAnswer]> {
        self.native_answers.as_deref()
    }

    pub fn assistant_message(&self) -> Option<&str> {
        self.assistant_message.as_deref()
    }

    pub fn final_message(&self) -> Option<&str> {
        self.final_message.as_deref()
    }

    pub fn turn_error(&self) -> Option<&AgentTurnError> {
        self.turn_error.as_ref()
    }

    pub fn observed_context(&self) -> Option<&ContextObservation> {
        self.observed_context.as_ref()
    }

    pub fn take_observed_context(&mut self) -> Option<ContextObservation> {
        self.observed_context.take()
    }

    pub fn neutral(&self) -> Option<&Value> {
        self.neutral.as_ref()
    }

    pub fn event_agent_id(&self) -> Option<&AgentSessionId> {
        self.lifecycle
            .as_ref()
            .and_then(|observation| observation.agent_id.as_ref())
            .or_else(|| self.routing.event_agent_id())
    }

    pub fn context_agent_id(&self) -> Option<&AgentSessionId> {
        self.observed_context
            .as_ref()
            .map(|observation| &observation.agent_id)
            .or_else(|| self.routing.context_agent_id())
    }

    pub fn worktree_path(&self) -> Option<&str> {
        self.lifecycle
            .as_ref()
            .and_then(|observation| observation.worktree_path.as_deref())
            .or_else(|| self.routing.worktree_path())
    }

    pub(crate) fn set_routing(&mut self, routing: HookRouting) {
        self.routing = routing;
    }

    pub(crate) fn set_ask(&mut self, questions: Vec<AskQuestion>, detail: Option<String>) {
        self.questions = questions;
        self.ask_detail = detail;
    }

    pub(crate) fn set_native_answers(&mut self, answers: Option<Vec<AskAnswer>>) {
        self.native_answers = answers;
    }

    pub(crate) fn set_assistant_message(&mut self, message: Option<String>) {
        self.assistant_message = message;
    }

    pub(crate) fn set_final_message(&mut self, message: Option<String>) {
        self.final_message = message;
    }

    pub(crate) fn set_turn_error(&mut self, error: Option<AgentTurnError>) {
        self.turn_error = error;
    }

    pub(crate) fn set_observed_context(&mut self, context: Option<ContextObservation>) {
        self.observed_context = context.filter(|observation| {
            self.routing
                .context_agent_id()
                .is_none_or(|agent_id| agent_id == &observation.agent_id)
        });
    }

    pub(crate) fn set_neutral(&mut self, neutral: Option<Value>) {
        self.neutral = neutral;
    }

    pub(crate) fn attach_lifecycle(&mut self, observation: AgentLifecycleObservation) {
        self.lifecycle = Some(observation);
    }

    pub fn update_lifecycle(&mut self, update: impl FnOnce(&mut AgentLifecycleObservation)) {
        if let Some(observation) = self.lifecycle.as_mut() {
            update(observation);
        }
    }

    pub(crate) fn set_policy(&mut self, progress: bool, session_ended: bool) {
        self.progress = progress;
        self.session_ended = session_ended;
    }
}

/// Stable routing stamped before provider lifecycle identity is resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookRouting {
    root_agent_id: Option<AgentSessionId>,
    event: EventRouting,
    worktree_path: Option<String>,
    server_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EventRouting {
    #[default]
    Root,
    Override(Option<AgentSessionId>),
}

impl HookRouting {
    pub fn session(agent_id: Option<AgentSessionId>) -> Self {
        Self {
            root_agent_id: agent_id,
            ..Self::default()
        }
    }

    pub fn split(
        event_agent_id: Option<AgentSessionId>,
        context_agent_id: Option<AgentSessionId>,
    ) -> Self {
        let event = if event_agent_id == context_agent_id {
            EventRouting::Root
        } else {
            EventRouting::Override(event_agent_id)
        };
        Self {
            root_agent_id: context_agent_id,
            event,
            ..Self::default()
        }
    }

    pub fn with_worktree(mut self, worktree_path: Option<String>) -> Self {
        self.worktree_path = worktree_path;
        self
    }

    pub fn with_server_url(mut self, server_url: Option<String>) -> Self {
        self.server_url = server_url;
        self
    }

    fn event_agent_id(&self) -> Option<&AgentSessionId> {
        match &self.event {
            EventRouting::Root => self.root_agent_id.as_ref(),
            EventRouting::Override(agent_id) => agent_id.as_ref(),
        }
    }

    pub fn context_agent_id(&self) -> Option<&AgentSessionId> {
        self.root_agent_id.as_ref()
    }

    pub fn worktree_path(&self) -> Option<&str> {
        self.worktree_path.as_deref()
    }

    pub fn server_url(&self) -> Option<&str> {
        self.server_url.as_deref()
    }
}

/// One installed managed hook and its classification policy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HookRecord {
    pub(crate) event: &'static str,
    pub(crate) matcher: Option<&'static str>,
    pub(crate) lifecycle_fallback: bool,
    pub(crate) synchronous: bool,
    progress: bool,
    session_ended: bool,
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
            progress: false,
            session_ended: false,
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
            progress: false,
            session_ended: false,
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

    pub(crate) const fn progress(mut self) -> Self {
        self.progress = true;
        self
    }

    pub(crate) const fn session_ended(mut self) -> Self {
        self.session_ended = true;
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

pub(crate) fn decode_catalog_hook(
    hooks: &[HookRecord],
    event_name: &str,
    ask_kind: Option<AskKind>,
) -> DecodedHook {
    let hook = hooks.iter().find(|hook| hook.event == event_name);
    DecodedHook::new(classify_catalog_entry(hook, event_name, ask_kind)).with_policy(hook)
}

pub(crate) fn decode_catalog_entry(
    hook: Option<&HookRecord>,
    event_name: &str,
    ask_kind: Option<AskKind>,
) -> DecodedHook {
    DecodedHook::new(classify_catalog_entry(hook, event_name, ask_kind)).with_policy(hook)
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
            hook_record!(lifecycle, "Start", r#"{}"#).progress(),
            hook_record!(blocking, "Ask", r#"{}"#, AskKind::Question).synchronous(),
            hook_record!(blocking, "Permission", r#"{}"#, AskKind::Permission)
                .with_matcher("shell")
                .synchronous()
                .with_lifecycle_fallback(),
            hook_record!(lifecycle, "Stop", r#"{}"#)
                .with_matcher("done")
                .session_ended(),
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

        let start = decode_catalog_hook(&HOOKS, "Start", None);
        assert!(start.records_progress());
        assert!(!start.ends_session());
        let stop = decode_catalog_hook(&HOOKS, "Stop", None);
        assert!(!stop.records_progress());
        assert!(stop.ends_session());
    }

    #[test]
    fn lifecycle_attachment_preserves_root_routing_and_promotes_event_identity() {
        let mut decoded = DecodedHook::new(ClassifiedHook {
            class: AgentHookClass::Lifecycle,
            ask_kind: None,
            event_name: "SubagentStop".to_owned(),
        });
        decoded.set_routing(
            HookRouting::split(Some("root-event".into()), Some("root-context".into()))
                .with_worktree(Some("/root/worktree".to_owned()))
                .with_server_url(Some("http://localhost".to_owned())),
        );
        let mut observation = AgentLifecycleObservation::new(
            Some(crate::ids::AgentSessionId::from("child")),
            super::super::LifecycleSignal::SubagentStopped { errored: false },
        );
        observation.parent_agent_id = Some(crate::ids::AgentSessionId::from("root"));
        observation.worktree_path = Some("/child/worktree".to_owned());
        decoded.attach_lifecycle(observation);

        assert_eq!(
            decoded.event_agent_id().map(AgentSessionId::as_str),
            Some("child")
        );
        assert_eq!(
            decoded.context_agent_id().map(AgentSessionId::as_str),
            Some("root-context")
        );
        assert_eq!(decoded.worktree_path(), Some("/child/worktree"));
        assert_eq!(decoded.routing().server_url(), Some("http://localhost"));
    }

    #[test]
    fn lifecycle_attachment_keeps_routing_fallbacks_and_lifecycle_less_routing() {
        let classified = ClassifiedHook {
            class: AgentHookClass::Lifecycle,
            ask_kind: None,
            event_name: "Context".to_owned(),
        };
        let mut decoded = DecodedHook::new(classified.clone());
        decoded.set_routing(
            HookRouting::split(Some("root".into()), Some("context".into()))
                .with_worktree(Some("/fallback".to_owned())),
        );
        assert!(decoded.lifecycle().is_none());
        assert_eq!(
            decoded.event_agent_id().map(AgentSessionId::as_str),
            Some("root")
        );
        assert_eq!(
            decoded.context_agent_id().map(AgentSessionId::as_str),
            Some("context")
        );

        let mut attached = DecodedHook::new(classified);
        attached.set_routing(decoded.routing().clone());
        attached.attach_lifecycle(AgentLifecycleObservation::new(
            None,
            super::super::LifecycleSignal::TurnStarted,
        ));
        assert_eq!(
            attached.event_agent_id().map(AgentSessionId::as_str),
            Some("root")
        );
        assert_eq!(attached.worktree_path(), Some("/fallback"));
    }
}
