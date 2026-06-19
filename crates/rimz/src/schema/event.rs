//! Event envelope appended to `events.log.jsonl`.

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::AgentLifecycleObservation;
use crate::feed::FeedItem;
use crate::feed::RuntimeOwner;
use crate::ids::{
    AgentKind, AgentSessionId, EventId, MessageId, MuxName, PaneId, RunId, WorkspaceId,
};
use crate::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use crate::schema::EVENT_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentLifecyclePayload {
    #[serde(default)]
    pub event_name: Option<String>,
    #[serde(flatten)]
    pub observation: AgentLifecycleObservation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLaunchState {
    #[default]
    Starting,
    Bound,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentLaunchPayload {
    pub agent_id: AgentSessionId,
    pub agent_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_ordinal: Option<u32>,
    #[serde(default)]
    pub state: AgentLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_owner: Option<RuntimeOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSteeredPayload {
    pub kind: AgentKind,
    /// The steered session, when one is bound. A bare agent pane addressed by
    /// `@kind` before its first turn has no session id yet, so the audit record
    /// names only the kind and pane rather than minting a placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    pub pane_id: PaneId,
    pub forced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageSender>,
    pub text_len: usize,
}

impl AgentSteeredPayload {
    pub fn new(
        kind: AgentKind,
        agent_id: Option<AgentSessionId>,
        pane_id: PaneId,
        forced: bool,
        sender: Option<MessageSender>,
        text_len: usize,
    ) -> Self {
        Self {
            kind,
            agent_id,
            pane_id,
            forced,
            sender,
            text_len,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageEventMethod {
    Queued,
    Delivered,
    Removed,
    Abandoned,
}

impl MessageEventMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "message.queued",
            Self::Delivered => "message.delivered",
            Self::Removed => "message.removed",
            Self::Abandoned => "message.abandoned",
        }
    }

    pub const fn for_terminal_status(status: MessageStatus) -> Option<Self> {
        match status {
            MessageStatus::Pending => None,
            MessageStatus::Claimed => None,
            MessageStatus::Delivered => Some(Self::Delivered),
            MessageStatus::Removed => Some(Self::Removed),
            MessageStatus::Abandoned => Some(Self::Abandoned),
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "message.queued" => Some(Self::Queued),
            "message.delivered" => Some(Self::Delivered),
            "message.removed" => Some(Self::Removed),
            "message.abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageEventPayload {
    pub message_id: MessageId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub gate: DeliveryGate,
    pub status: MessageStatus,
    pub text_len: usize,
    pub enter: bool,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageSender>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl MessageEventPayload {
    pub fn from_record(message: &MessageRecord, reason: Option<&str>) -> Self {
        Self {
            message_id: message.message_id.clone(),
            kind: message.kind.clone(),
            agent_id: message.agent_id.clone(),
            gate: message.gate,
            status: message.status,
            text_len: message.text.len(),
            enter: message.enter,
            attempts: message.attempts,
            sender: message.sender.attributed(),
            reason: reason.map(ToOwned::to_owned),
        }
    }
}

impl AgentLifecyclePayload {
    pub fn new(event_name: impl Into<String>, observation: &AgentLifecycleObservation) -> Self {
        Self {
            event_name: Some(event_name.into()),
            observation: observation.clone(),
        }
    }

    pub fn from_params(params: &Value) -> Option<Self> {
        let signal = params
            .get("signal")
            .and_then(|value| serde_json::from_value(value.clone()).ok())?;
        Some(Self {
            event_name: optional_string(params, "event_name"),
            observation: AgentLifecycleObservation {
                agent_id: optional_string(params, "agent_id").map(AgentSessionId::from),
                agent_name: optional_string(params, "agent_name"),
                agent_profile: optional_string(params, "agent_profile"),
                agent_role: optional_string(params, "agent_role"),
                kind_ordinal: optional_u64(params, "kind_ordinal").map(clamp_u32),
                signal,
                agent_pid: optional_deserialize(params, "agent_pid"),
                agent_process_start: optional_string(params, "agent_process_start"),
                runtime_owner: optional_deserialize::<RuntimeOwner>(params, "runtime_owner"),
                worktree_path: optional_string(params, "worktree_path"),
                worktree_branch: optional_string(params, "worktree_branch"),
                task: optional_string(params, "task"),
                prompt: optional_string(params, "prompt"),
                transcript_path: optional_string(params, "transcript_path"),
                model: optional_string(params, "model"),
                effort: optional_string(params, "effort"),
                context_pct: optional_u64(params, "context_pct").map(|v| v.min(100) as u8),
                context_window: optional_u64(params, "context_window"),
                total_tokens: optional_u64(params, "total_tokens"),
                turn_error: None,
                cache_read_input_tokens: optional_u64(params, "cache_read_input_tokens"),
                cache_write_input_tokens: optional_u64(params, "cache_write_input_tokens"),
                fresh_input_tokens: optional_u64(params, "fresh_input_tokens"),
                output_tokens: optional_u64(params, "output_tokens"),
                todo_done: optional_u64(params, "todo_done").map(clamp_u32),
                todo_total: optional_u64(params, "todo_total").map(clamp_u32),
                pane_id: optional_string(params, "pane_id")
                    .and_then(|raw| PaneId::parse(&raw).ok()),
                parent_agent_id: optional_string(params, "parent_agent_id")
                    .map(AgentSessionId::from),
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind<'a> {
    AgentLifecycle(Box<AgentLifecyclePayload>),
    AgentLaunch(AgentLaunchPayload),
    AgentSteered(AgentSteeredPayload),
    Message {
        method: MessageEventMethod,
        payload: MessageEventPayload,
    },
    SessionRebirth,
    /// Deliberate carrier for audit/user events that have not graduated to a
    /// folded typed variant yet, including `feed.*`, `event.emit`, and unknown
    /// methods from older or newer binaries.
    Other {
        method: &'a str,
        params: &'a Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: EventId,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub mux: Option<MuxName>,
    pub source: String,
    pub source_kind: String,
    pub method: String,
    pub timestamp: Timestamp,
    pub params: Value,
}

impl EventEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        source: impl Into<String>,
        source_kind: impl Into<String>,
        method: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            event_id: EventId::new(),
            workspace_id,
            session_name: session_name.into(),
            mux: None,
            source: source.into(),
            source_kind: source_kind.into(),
            method: method.into(),
            timestamp: Timestamp::now(),
            params,
        }
    }

    /// Constructor for the `session.rebirth` boundary a genuine mux-session
    /// birth appends. A reborn session renumbers panes from zero, so every
    /// pane stamp recorded before this instant names a pane that no longer
    /// exists — the agent rollup fold clears them all at this point in the
    /// log ([`crate::ledger::snapshot`]'s reducer), keeping a prior
    /// incarnation's session off a reused pane id. The sessions themselves
    /// stay: the boundary unstamps, it never tombstones.
    pub fn session_rebirth(workspace_id: WorkspaceId, session_name: impl Into<String>) -> Self {
        Self::new(
            workspace_id,
            session_name,
            "rimz",
            "runtime",
            "session.rebirth",
            json!({}),
        )
    }

    pub fn agent_launched(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        kind: &AgentKind,
        payload: AgentLaunchPayload,
    ) -> Self {
        let params = serde_json::to_value(&payload)
            .expect("AgentLaunchPayload contains only JSON-serializable fields");
        Self::new(
            workspace_id,
            session_name,
            kind.as_str(),
            "agent",
            "agent.launched",
            params,
        )
    }

    pub fn kind(&self) -> EventKind<'_> {
        match self.method.as_str() {
            "agent.lifecycle" => AgentLifecyclePayload::from_params(&self.params)
                .map(Box::new)
                .map(EventKind::AgentLifecycle)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "agent.launched" => serde_json::from_value(self.params.clone())
                .map(EventKind::AgentLaunch)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "agent.steered" => serde_json::from_value(self.params.clone())
                .map(EventKind::AgentSteered)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "session.rebirth" => EventKind::SessionRebirth,
            method => MessageEventMethod::parse(method)
                .and_then(|method| {
                    serde_json::from_value(self.params.clone())
                        .ok()
                        .map(|payload| EventKind::Message { method, payload })
                })
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
        }
    }

    /// Convenience constructor for a `feed.push` event from a `FeedItem`.
    pub fn feed_pushed(item: &FeedItem, session_name: impl Into<String>) -> Self {
        Self::new(
            item.workspace_id.clone(),
            session_name,
            item.source.clone(),
            item.source_kind.clone(),
            "feed.push",
            json!({
                "request_id": item.request_id,
                "surface": item.surface,
                "kind": item.kind,
            }),
        )
    }

    /// Convenience constructor for an `agent.lifecycle` event. The CLI hook
    /// path calls this after a lifecycle hook fires, so the sidebar's agent
    /// rollup sees the status and enrichment update without each adapter
    /// touching the ledger.
    pub fn agent_lifecycle(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        agent_kind: impl Into<String>,
        event_name: impl Into<String>,
        observation: &AgentLifecycleObservation,
    ) -> Self {
        let agent_kind = agent_kind.into();
        let params = serde_json::to_value(AgentLifecyclePayload::new(event_name, observation))
            .expect("AgentLifecyclePayload contains only JSON-serializable fields");
        Self::new(
            workspace_id,
            session_name,
            agent_kind.clone(),
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    }

    pub fn agent_steered(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        payload: AgentSteeredPayload,
    ) -> Self {
        let params = serde_json::to_value(&payload)
            .expect("AgentSteeredPayload contains only JSON-serializable fields");
        Self::new(
            workspace_id,
            session_name,
            "rimz",
            "cli",
            "agent.steered",
            params,
        )
    }

    /// Audit record for an automated rate-limit resume: the producer typed the
    /// configured nudge into a parked agent's live pane the moment its 5h/7d
    /// window reset. A plain audit event — it rides the [`EventKind::Other`]
    /// carrier like `feed.*`, never folded into the agent rollup, because the
    /// agent's own next hook drives its state back to `running`. The nudge text
    /// never enters the log, mirroring `agent.steered`.
    pub fn agent_resumed(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        pane_id: &PaneId,
        reason: &str,
    ) -> Self {
        Self::new(
            workspace_id,
            session_name,
            "rimz",
            "cli",
            "agent.resumed",
            json!({
                "kind": kind,
                "agent_id": agent_id,
                "pane_id": pane_id,
                "reason": reason,
            }),
        )
    }

    pub fn message_event(
        message: &MessageRecord,
        session_name: impl Into<String>,
        method: MessageEventMethod,
        reason: Option<&str>,
    ) -> Self {
        let params = serde_json::to_value(MessageEventPayload::from_record(message, reason))
            .expect("MessageEventPayload contains only JSON-serializable fields");
        Self::new(
            message.workspace_id.clone(),
            session_name,
            "rimz",
            "cli",
            method.as_str(),
            params,
        )
    }
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn optional_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn optional_deserialize<T: DeserializeOwned>(params: &Value, key: &str) -> Option<T> {
    params
        .get(key)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn clamp_u32(value: u64) -> u32 {
    value.min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::lifecycle::LifecycleSignal;
    use crate::feed::{RuntimeOwner, RuntimeOwnerKind};
    use crate::ids::{AgentSessionId, MuxName, PaneId};

    fn workspace() -> WorkspaceId {
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-event-test"))
    }

    fn lifecycle_observation() -> AgentLifecycleObservation {
        AgentLifecycleObservation {
            agent_id: Some(AgentSessionId::from("sess-1")),
            agent_name: Some("amber-atlas".to_owned()),
            agent_profile: None,
            agent_role: Some("coder".to_owned()),
            kind_ordinal: Some(2),
            signal: LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            agent_pid: Some(42),
            agent_process_start: Some("12345".to_owned()),
            runtime_owner: Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-1",
                42,
                Some("12345".to_owned()),
            )),
            worktree_path: Some("/tmp/project".to_owned()),
            worktree_branch: Some("main".to_owned()),
            task: Some("Review".to_owned()),
            prompt: Some("ship it".to_owned()),
            transcript_path: Some("/tmp/transcript.jsonl".to_owned()),
            model: Some("claude-opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(80),
            context_window: Some(200_000),
            total_tokens: Some(10_000),
            turn_error: None,
            cache_read_input_tokens: Some(7_000),
            cache_write_input_tokens: Some(1_000),
            fresh_input_tokens: Some(2_000),
            output_tokens: Some(1_000),
            todo_done: Some(3),
            todo_total: Some(5),
            pane_id: Some(PaneId::from_parts(MuxName::Tmux, "%1")),
            parent_agent_id: Some(AgentSessionId::from("parent-1")),
        }
    }

    #[test]
    fn agent_lifecycle_constructor_keeps_the_existing_wire_shape() {
        let workspace = workspace();
        let observation = lifecycle_observation();
        let typed = EventEnvelope::agent_lifecycle(
            workspace.clone(),
            "session",
            "claude",
            "Stop",
            &observation,
        );
        let mut legacy = EventEnvelope::new(
            workspace,
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            json!({
                "event_name": "Stop",
                "agent_id": "sess-1",
                "agent_name": "amber-atlas",
                "agent_role": "coder",
                "kind_ordinal": 2,
                "signal": {
                    "signal": "turn_ended",
                    "errored": false,
                    "parked_on_background": false,
                },
                "agent_pid": 42,
                "agent_process_start": "12345",
                "runtime_owner": {
                    "kind": "agent",
                    "subject_id": "sess-1",
                    "pid": 42,
                    "process_start": "12345",
                },
                "worktree_path": "/tmp/project",
                "worktree_branch": "main",
                "task": "Review",
                "prompt": "ship it",
                "transcript_path": "/tmp/transcript.jsonl",
                "model": "claude-opus",
                "effort": "high",
                "context_pct": 80,
                "context_window": 200_000,
                "total_tokens": 10_000,
                "cache_read_input_tokens": 7_000,
                "cache_write_input_tokens": 1_000,
                "fresh_input_tokens": 2_000,
                "output_tokens": 1_000,
                "todo_done": 3,
                "todo_total": 5,
                "pane_id": "tmux:%1",
                "parent_agent_id": "parent-1",
            }),
        );
        legacy.event_id = typed.event_id.clone();
        legacy.timestamp = typed.timestamp;

        assert_eq!(
            serde_json::to_vec(&typed).unwrap(),
            serde_json::to_vec(&legacy).unwrap(),
            "typed construction must not migrate event-log bytes"
        );
        let EventKind::AgentLifecycle(payload) = typed.kind() else {
            panic!("agent.lifecycle decodes to its typed kind");
        };
        let payload = *payload;
        assert_eq!(payload.event_name.as_deref(), Some("Stop"));
        assert_eq!(payload.observation, observation);
    }

    #[test]
    fn agent_lifecycle_constructor_serializes_absent_fields_as_null() {
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-null")),
            LifecycleSignal::Registered,
        );
        let event = EventEnvelope::agent_lifecycle(
            workspace(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        );

        for key in [
            "agent_pid",
            "agent_name",
            "kind_ordinal",
            "agent_process_start",
            "runtime_owner",
            "worktree_path",
            "worktree_branch",
            "task",
            "prompt",
            "transcript_path",
            "model",
            "effort",
            "context_pct",
            "context_window",
            "total_tokens",
            "cache_read_input_tokens",
            "cache_write_input_tokens",
            "fresh_input_tokens",
            "output_tokens",
            "todo_done",
            "todo_total",
            "pane_id",
            "parent_agent_id",
        ] {
            assert_eq!(
                event.params.get(key),
                Some(&Value::Null),
                "{key} must stay present as null to preserve partial-event bytes",
            );
        }
    }

    #[test]
    fn session_rebirth_constructor_keeps_the_existing_wire_shape() {
        let workspace = workspace();
        let typed = EventEnvelope::session_rebirth(workspace.clone(), "session");
        let mut legacy = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "runtime",
            "session.rebirth",
            json!({}),
        );
        legacy.event_id = typed.event_id.clone();
        legacy.timestamp = typed.timestamp;

        assert_eq!(
            serde_json::to_vec(&typed).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        assert!(matches!(typed.kind(), EventKind::SessionRebirth));
    }

    #[test]
    fn agent_steered_constructor_keeps_the_existing_wire_shape() {
        let workspace = workspace();
        let payload = AgentSteeredPayload::new(
            AgentKind::new_unchecked("claude"),
            Some(AgentSessionId::from("sess-1")),
            PaneId::from_parts(MuxName::Tmux, "%1"),
            true,
            None,
            8,
        );
        let typed = EventEnvelope::agent_steered(workspace.clone(), "session", payload.clone());
        let mut legacy = EventEnvelope::new(
            workspace,
            "session",
            "rimz",
            "cli",
            "agent.steered",
            json!({
                "kind": "claude",
                "agent_id": "sess-1",
                "pane_id": "tmux:%1",
                "forced": true,
                "text_len": 8,
            }),
        );
        legacy.event_id = typed.event_id.clone();
        legacy.timestamp = typed.timestamp;

        assert_eq!(
            serde_json::to_vec(&typed).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        let EventKind::AgentSteered(decoded) = typed.kind() else {
            panic!("agent.steered decodes to its typed kind");
        };
        assert_eq!(decoded, payload);
    }

    #[test]
    fn agent_steered_records_agent_sender_without_message_body() {
        let sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: Some("swift-otter".to_owned()),
            profile: None,
            role: None,
            channel: Some("docs".to_owned()),
        };
        let payload = AgentSteeredPayload::new(
            AgentKind::new_unchecked("claude"),
            Some(AgentSessionId::from("sess-1")),
            PaneId::from_parts(MuxName::Tmux, "%1"),
            false,
            Some(sender.clone()),
            17,
        );
        let event = EventEnvelope::agent_steered(workspace(), "session", payload.clone());

        assert_eq!(event.params["sender"]["origin"], "agent");
        assert_eq!(event.params["sender"]["kind"], "codex");
        assert_eq!(event.params["sender"]["name"], "swift-otter");
        assert!(
            !serde_json::to_string(&event.params)
                .unwrap()
                .contains("secret prompt body")
        );
        let EventKind::AgentSteered(decoded) = event.kind() else {
            panic!("agent.steered decodes to its typed kind");
        };
        assert_eq!(decoded.sender, Some(sender));
    }

    #[test]
    fn agent_steered_without_session_omits_the_agent_id() {
        // Steering a bare agent pane before its first turn has no session id; the
        // audit record drops the field rather than carrying an empty placeholder.
        let payload = AgentSteeredPayload::new(
            AgentKind::new_unchecked("codex"),
            None,
            PaneId::from_parts(MuxName::Zellij, "terminal_7"),
            false,
            None,
            4,
        );
        let value = serde_json::to_value(&payload).unwrap();
        assert!(
            value.get("agent_id").is_none(),
            "no session means no agent_id on the wire: {value}"
        );
        let decoded: AgentSteeredPayload = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.agent_id, None);
    }

    #[test]
    fn agent_resumed_records_an_audit_event_on_the_other_carrier() {
        // Auto-continue is audit-only: the event rides the generic `Other`
        // carrier (it never folds into the agent rollup), so the rollup reduce
        // and wakeup paths skip it untouched while `feed list --audit` still
        // surfaces it by method.
        let event = EventEnvelope::agent_resumed(
            workspace(),
            "session",
            &AgentKind::new_unchecked("claude"),
            &AgentSessionId::from("sess-1"),
            &PaneId::from_parts(MuxName::Tmux, "%1"),
            "rate_limit_window_reset",
        );
        assert_eq!(event.method, "agent.resumed");
        let EventKind::Other { method, params } = event.kind() else {
            panic!("agent.resumed rides the Other audit carrier");
        };
        assert_eq!(method, "agent.resumed");
        assert_eq!(params["kind"], "claude");
        assert_eq!(params["agent_id"], "sess-1");
        assert_eq!(params["pane_id"], "tmux:%1");
        assert_eq!(params["reason"], "rate_limit_window_reset");
    }

    #[test]
    fn message_event_constructor_keeps_text_out_of_the_wire_shape() {
        let now = Timestamp::now();
        let message = MessageRecord {
            message_id: MessageId::new(),
            workspace_id: workspace(),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            agent_name: Some("lucid-atlas".to_owned()),
            sender: MessageSender::Human,
            text: "secret prompt body".to_owned(),
            enter: true,
            gate: DeliveryGate::Done,
            force: false,
            status: MessageStatus::Pending,
            enqueued_at: now,
            updated_at: now,
            attempts: 0,
            last_attempt_at: None,
            last_error: None,
            delivered_at: None,
            auto_compact: None,
        };
        let typed =
            EventEnvelope::message_event(&message, "session", MessageEventMethod::Queued, None);
        let mut legacy = EventEnvelope::new(
            message.workspace_id.clone(),
            "session",
            "rimz",
            "cli",
            "message.queued",
            json!({
                "message_id": message.message_id.as_str(),
                "kind": "claude",
                "agent_id": "sess-1",
                "gate": "done",
                "status": "pending",
                "text_len": "secret prompt body".len(),
                "enter": true,
                "attempts": 0,
                "reason": null,
            }),
        );
        legacy.event_id = typed.event_id.clone();
        legacy.timestamp = typed.timestamp;

        assert_eq!(
            serde_json::to_vec(&typed).unwrap(),
            serde_json::to_vec(&legacy).unwrap()
        );
        assert!(
            !serde_json::to_string(&typed.params)
                .unwrap()
                .contains("secret prompt body")
        );
        let EventKind::Message { method, payload } = typed.kind() else {
            panic!("message.queued decodes to its typed kind");
        };
        assert_eq!(method, MessageEventMethod::Queued);
        assert_eq!(payload.message_id, message.message_id);
        assert_eq!(payload.text_len, "secret prompt body".len());
        assert_eq!(payload.reason, None);

        let sender = MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: Some("swift-otter".to_owned()),
            profile: None,
            role: None,
            channel: Some("docs".to_owned()),
        };
        let attributed = message.with_sender(sender.clone());
        let event =
            EventEnvelope::message_event(&attributed, "session", MessageEventMethod::Queued, None);

        assert_eq!(event.params["sender"]["kind"], "codex");
        assert!(
            !serde_json::to_string(&event.params)
                .unwrap()
                .contains("secret prompt body")
        );
        let EventKind::Message { payload, .. } = event.kind() else {
            panic!("message.queued decodes to its typed kind");
        };
        assert_eq!(payload.sender, Some(sender));
    }

    #[test]
    fn unknown_event_kind_carries_the_raw_method_and_params() {
        let params = json!({ "request_id": "req_1" });
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "claude",
            "agent-hook",
            "feed.push",
            params.clone(),
        );

        let EventKind::Other {
            method,
            params: raw,
        } = event.kind()
        else {
            panic!("feed.push stays in the open audit-event channel");
        };
        assert_eq!(method, "feed.push");
        assert_eq!(raw, &params);
    }

    #[test]
    fn lifecycle_kind_tolerates_missing_identity_and_bad_optional_fields() {
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            json!({
                "event_name": "SessionStart",
                "signal": { "signal": "registered" },
                "agent_pid": "not-a-pid",
                "context_pct": 300,
                "todo_done": u64::MAX,
                "pane_id": "not-a-pane",
            }),
        );

        let EventKind::AgentLifecycle(payload) = event.kind() else {
            panic!("partial lifecycle payload with a signal is still typed");
        };
        let payload = *payload;
        assert_eq!(payload.observation.agent_id, None);
        assert_eq!(payload.observation.signal, LifecycleSignal::Registered);
        assert_eq!(payload.observation.agent_pid, None);
        assert_eq!(payload.observation.context_pct, Some(100));
        assert_eq!(payload.observation.todo_done, Some(u32::MAX));
        assert_eq!(payload.observation.pane_id, None);
    }

    #[test]
    fn signal_less_lifecycle_event_decodes_as_other() {
        for params in [
            json!({ "status": "running", "event_name": "UserPromptSubmit" }),
            json!({ "signal": "not-an-object" }),
            json!({}),
        ] {
            let event = EventEnvelope::new(
                workspace(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            );
            assert!(matches!(
                event.kind(),
                EventKind::Other {
                    method: "agent.lifecycle",
                    ..
                }
            ));
        }
    }
}
