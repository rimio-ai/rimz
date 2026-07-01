//! Event envelope appended to `events.log.jsonl`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use serde_json::{Value, json};

use crate::agents::AgentLifecycleObservation;
use crate::feed::FeedItem;
use crate::ids::{
    AgentKind, AgentSessionId, EventId, MessageId, MuxName, PaneId, RunId, WorkspaceId,
};
use crate::message::{DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus};
use crate::pane::RuntimeOwner;
use crate::schema::EVENT_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageEventMethod {
    Queued,
    Sent,
    Delivered,
    TimedOut,
    Errored,
    Removed,
    Abandoned,
    Archived,
}

impl MessageEventMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "message.queued",
            Self::Sent => "message.sent",
            Self::Delivered => "message.delivered",
            Self::TimedOut => "message.timed_out",
            Self::Errored => "message.errored",
            Self::Removed => "message.removed",
            Self::Abandoned => "message.abandoned",
            Self::Archived => "message.archived",
        }
    }

    pub const fn for_terminal_status(status: MessageStatus) -> Option<Self> {
        match status {
            MessageStatus::Created => None,
            MessageStatus::Queued => None,
            MessageStatus::Claimed => None,
            MessageStatus::Sent => None,
            MessageStatus::Delivered => Some(Self::Delivered),
            MessageStatus::TimedOut => Some(Self::TimedOut),
            MessageStatus::Errored => Some(Self::Errored),
            MessageStatus::Removed => Some(Self::Removed),
            MessageStatus::Abandoned => Some(Self::Abandoned),
            MessageStatus::Archived => Some(Self::Archived),
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "message.queued" => Some(Self::Queued),
            "message.sent" => Some(Self::Sent),
            "message.delivered" => Some(Self::Delivered),
            "message.timed_out" => Some(Self::TimedOut),
            "message.errored" => Some(Self::Errored),
            "message.removed" => Some(Self::Removed),
            "message.abandoned" => Some(Self::Abandoned),
            "message.archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageEventPayload {
    pub message_id: MessageId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub gate: DeliveryGate,
    pub status: MessageStatus,
    #[serde(default)]
    pub body: MessageBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    pub forced: bool,
    pub text_len: usize,
    pub enter: bool,
    pub attempts: u32,
    #[serde(default)]
    pub unconfirmed_sends: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageSender>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_context_tokens: Option<u64>,
}

impl MessageEventPayload {
    pub fn from_record(message: &MessageRecord, reason: Option<&str>) -> Self {
        Self {
            message_id: message.message_id.clone(),
            kind: message.kind.clone(),
            agent_id: message.agent_id.clone(),
            agent_name: message.agent_name.clone(),
            channel: message.channel.clone(),
            gate: message.gate,
            status: message.status,
            body: message.body,
            pane_id: message.pane_id.clone(),
            forced: message.force,
            text_len: message.text.len(),
            enter: message.enter,
            attempts: message.attempts,
            unconfirmed_sends: message.unconfirmed_sends,
            sender: message.sender.attributed(),
            reason: reason.map(ToOwned::to_owned),
            enqueued_at: Some(message.enqueued_at),
            delivered_at: message.delivered_at,
            compacted_context_tokens: message.compacted_context_tokens,
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
}

#[derive(Clone, Debug)]
pub enum EventKind<'a> {
    AgentLifecycle(Box<AgentLifecyclePayload>),
    AgentLaunch(AgentLaunchPayload),
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
        params: &'a RawValue,
    },
}

impl PartialEq for EventKind<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AgentLifecycle(left), Self::AgentLifecycle(right)) => left == right,
            (Self::AgentLaunch(left), Self::AgentLaunch(right)) => left == right,
            (
                Self::Message {
                    method: left_method,
                    payload: left_payload,
                },
                Self::Message {
                    method: right_method,
                    payload: right_payload,
                },
            ) => left_method == right_method && left_payload == right_payload,
            (Self::SessionRebirth, Self::SessionRebirth) => true,
            (
                Self::Other {
                    method: left_method,
                    params: left_params,
                },
                Self::Other {
                    method: right_method,
                    params: right_params,
                },
            ) => left_method == right_method && left_params.get() == right_params.get(),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub params: Box<RawValue>,
}

impl PartialEq for EventEnvelope {
    fn eq(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.event_id == other.event_id
            && self.workspace_id == other.workspace_id
            && self.session_name == other.session_name
            && self.mux == other.mux
            && self.source == other.source
            && self.source_kind == other.source_kind
            && self.method == other.method
            && self.timestamp == other.timestamp
            && self.params.get() == other.params.get()
    }
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
            params: to_raw_value(&params).expect("params is valid JSON"),
        }
    }

    /// Decode raw params for audit/reporting call sites that need ad-hoc fields.
    /// Hot reducers use [`kind`](Self::kind) to parse only the typed event they need.
    pub fn params_value(&self) -> Value {
        serde_json::from_str(self.params.get()).expect("RawValue guarantees params JSON is valid")
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
            "agent.lifecycle" => serde_json::from_str::<AgentLifecyclePayload>(self.params.get())
                .ok()
                .map(Box::new)
                .map(EventKind::AgentLifecycle)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "agent.launched" => serde_json::from_str(self.params.get())
                .map(EventKind::AgentLaunch)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "session.rebirth" => EventKind::SessionRebirth,
            method => MessageEventMethod::parse(method)
                .and_then(|method| {
                    serde_json::from_str(self.params.get())
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

#[cfg(test)]
mod tests;
