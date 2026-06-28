//! Event envelope appended to `events.log.jsonl`.

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::AgentLifecycleObservation;
use crate::feed::FeedItem;
use crate::ids::{
    AgentKind, AgentSessionId, EventId, MessageId, MuxName, PaneId, RunId, WorkspaceId,
};
use crate::message::{DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus};
use crate::pane::RuntimeOwner;
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
            body: message.body,
            pane_id: message.pane_id.clone(),
            forced: message.force,
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
                role: optional_string(params, "role"),
                team: optional_string(params, "team"),
                channel: optional_string(params, "channel"),
                profile: optional_string(params, "profile"),
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
                origin: optional_deserialize(params, "origin"),
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

    /// Audit record for an automated rate-limit resume: the producer typed the
    /// configured nudge into a parked agent's live pane the moment its 5h/7d
    /// window reset. A plain audit event — it rides the [`EventKind::Other`]
    /// carrier like `feed.*`, never folded into the agent rollup, because the
    /// agent's own next hook drives its state back to `running`. The nudge text
    /// never enters the log, mirroring `message.sent`.
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
mod tests;
